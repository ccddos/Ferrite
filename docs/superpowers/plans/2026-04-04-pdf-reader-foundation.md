# PDF Reader Foundation Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build a medium-weight PDF reader path for Ferrite with non-blocking rendering, page jump navigation, outline fallback, and persisted reading state while staying on the pure-Rust `hayro` stack.

**Architecture:** Move PDF reading state onto `Tab` so UI state, session persistence, and keyboard navigation all share one source of truth. Split the current monolithic PDF preview into focused model/runtime/UI modules, then replace ad-hoc thread spawning with a bounded render scheduler and cache budget that prioritizes visible pages over thumbnails.

**Tech Stack:** Rust 2021, egui/eframe, `hayro` + `hayro-interpret`, `serde`, `std::thread`, `std::sync::{Arc, Mutex, mpsc}`, `cargo test`, `cargo check`

---

## File Structure

- Create: `src/preview/pdf/mod.rs`
  - Module wiring for the PDF reader submodules and public exports.
- Create: `src/preview/pdf/model.rs`
  - Shared PDF reader types: spread/layout/sidebar enums, persisted snapshot, transient page-jump input helpers.
- Create: `src/preview/pdf/runtime.rs`
  - Bounded render queue, request priority, zoom bucketing, bitmap/texture cache budgets, document lifetime management.
- Create: `src/preview/pdf/ui.rs`
  - `render_pdf_preview`, toolbar/sidebar/main viewport rendering, page jump UI, outline fallback UI.
- Create: `docs/technical/viewers/pdf-preview.md`
  - Reader architecture note plus manual verification scenarios.
- Modify: `src/preview/mod.rs`
  - Swap `pdf_preview.rs` export to the new `preview/pdf/` module.
- Modify: `src/state.rs`
  - Store PDF view state on `Tab`, persist it to both config/session models, restore it on open/session restore, add regression tests.
- Modify: `src/config/settings.rs`
  - Extend `TabInfo` with optional persisted PDF view state and serde backward-compatibility tests.
- Modify: `src/config/session.rs`
  - Extend `SessionTabState` with optional persisted PDF view state and session round-trip tests.
- Modify: `src/app/mod.rs`
  - Remove the separate `pdf_viewer_states` map once `Tab` owns PDF state.
- Modify: `src/app/central_panel.rs`
  - Render PDF preview using `tab.pdf_view_state`, wire page jump toolbar input.
- Modify: `src/app/keyboard.rs`
  - Update PDF next/prev-page shortcuts to mutate the active tab’s owned PDF state.
- Delete: `src/preview/pdf_preview.rs`
  - Remove the monolithic implementation after the new module tree compiles and exports the same public entry point.

## Task 1: Define the Shared PDF View State Model

**Files:**
- Create: `src/preview/pdf/model.rs`
- Modify: `src/preview/pdf/mod.rs`
- Modify: `src/preview/mod.rs`
- Modify: `src/config/settings.rs`
- Modify: `src/config/session.rs`
- Test: `src/config/settings.rs`
- Test: `src/config/session.rs`

- [ ] **Step 1: Write the failing persistence tests for the new PDF snapshot type**

```rust
#[test]
fn test_tab_info_pdf_view_state_roundtrip() {
    use crate::preview::pdf::{
        PdfScrollLayout, PdfSidebarMode, PdfSpreadMode, PdfViewStateSnapshot,
    };

    let tab = TabInfo {
        path: Some(PathBuf::from("/tmp/sample.pdf")),
        modified: false,
        cursor_position: (0, 0),
        scroll_offset: 0.0,
        view_mode: ViewMode::Rendered,
        split_ratio: 0.5,
        pdf_view_state: Some(PdfViewStateSnapshot {
            current_page: 7,
            zoom_percent: 150,
            spread_mode: PdfSpreadMode::TwoPageOdd,
            scroll_layout: PdfScrollLayout::Vertical,
            sidebar_visible: true,
            sidebar_mode: PdfSidebarMode::Outline,
        }),
    };

    let json = serde_json::to_string(&tab).unwrap();
    let restored: TabInfo = serde_json::from_str(&json).unwrap();
    assert_eq!(restored.pdf_view_state, tab.pdf_view_state);
}

#[test]
fn test_tab_info_pdf_view_state_backward_compatibility() {
    let json = r#"{
        "path": "/tmp/sample.pdf",
        "modified": false,
        "cursor_position": [0, 0],
        "scroll_offset": 0.0,
        "view_mode": "rendered",
        "split_ratio": 0.5
    }"#;

    let restored: TabInfo = serde_json::from_str(json).unwrap();
    assert!(restored.pdf_view_state.is_none());
}
```

```rust
#[test]
fn test_session_tab_state_pdf_view_state_roundtrip() {
    use crate::preview::pdf::{
        PdfScrollLayout, PdfSidebarMode, PdfSpreadMode, PdfViewStateSnapshot,
    };

    let session_tab = SessionTabState {
        tab_id: 9,
        path: Some(PathBuf::from("/tmp/sample.pdf")),
        display_title: "sample.pdf".to_string(),
        view_mode: ViewMode::Rendered,
        cursor_char_index: 0,
        cursor_position: (0, 0),
        selection: None,
        scroll_offset: 0.0,
        rendered_scroll_offset: 0.0,
        has_unsaved_content: false,
        file_mtime: None,
        original_content_hash: None,
        csv_delimiter: None,
        pdf_view_state: Some(PdfViewStateSnapshot {
            current_page: 42,
            zoom_percent: 125,
            spread_mode: PdfSpreadMode::TwoPageOdd,
            scroll_layout: PdfScrollLayout::Horizontal,
            sidebar_visible: false,
            sidebar_mode: PdfSidebarMode::Thumbnails,
        }),
    };

    let json = serde_json::to_string(&session_tab).unwrap();
    let restored: SessionTabState = serde_json::from_str(&json).unwrap();
    assert_eq!(restored.pdf_view_state, session_tab.pdf_view_state);
}
```

- [ ] **Step 2: Run the narrow tests and verify they fail because the field/type does not exist yet**

Run: `cargo test test_tab_info_pdf_view_state_roundtrip test_tab_info_pdf_view_state_backward_compatibility test_session_tab_state_pdf_view_state_roundtrip -- --nocapture`

Expected: FAIL with errors like `no field 'pdf_view_state' on type 'TabInfo'` and unresolved import errors for `PdfViewStateSnapshot`.

- [ ] **Step 3: Add the shared PDF model module**

```rust
// src/preview/pdf/model.rs
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PdfSpreadMode {
    SinglePage,
    TwoPageOdd,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PdfScrollLayout {
    Vertical,
    Horizontal,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PdfSidebarMode {
    Thumbnails,
    Outline,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct PdfViewStateSnapshot {
    pub current_page: usize,
    pub zoom_percent: u16,
    pub spread_mode: PdfSpreadMode,
    pub scroll_layout: PdfScrollLayout,
    pub sidebar_visible: bool,
    pub sidebar_mode: PdfSidebarMode,
}

impl Default for PdfViewStateSnapshot {
    fn default() -> Self {
        Self {
            current_page: 0,
            zoom_percent: 100,
            spread_mode: PdfSpreadMode::TwoPageOdd,
            scroll_layout: PdfScrollLayout::Vertical,
            sidebar_visible: true,
            sidebar_mode: PdfSidebarMode::Thumbnails,
        }
    }
}
```

```rust
// src/preview/pdf/mod.rs
mod model;
mod runtime;
mod ui;

pub use model::{
    PdfScrollLayout, PdfSidebarMode, PdfSpreadMode, PdfViewStateSnapshot,
};
pub use ui::{render_pdf_preview, PdfPreviewState};
```

```rust
// src/preview/mod.rs
mod image_preview;
mod pdf;
mod sync_scroll;

pub use pdf::{
    render_pdf_preview, PdfPreviewState, PdfScrollLayout, PdfSidebarMode, PdfSpreadMode,
    PdfViewStateSnapshot,
};
```

- [ ] **Step 4: Extend the config/session persistence structs**

```rust
// src/config/settings.rs
use crate::preview::PdfViewStateSnapshot;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TabInfo {
    pub path: Option<PathBuf>,
    #[serde(default)]
    pub modified: bool,
    #[serde(default)]
    pub cursor_position: (usize, usize),
    #[serde(default)]
    pub scroll_offset: f32,
    #[serde(default)]
    pub view_mode: ViewMode,
    #[serde(default = "default_split_ratio")]
    pub split_ratio: f32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pdf_view_state: Option<PdfViewStateSnapshot>,
}

impl Default for TabInfo {
    fn default() -> Self {
        Self {
            path: None,
            modified: false,
            cursor_position: (0, 0),
            scroll_offset: 0.0,
            view_mode: ViewMode::Raw,
            split_ratio: 0.5,
            pdf_view_state: None,
        }
    }
}
```

```rust
// src/config/session.rs
use crate::preview::PdfViewStateSnapshot;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionTabState {
    pub tab_id: usize,
    pub path: Option<PathBuf>,
    pub display_title: String,
    pub view_mode: ViewMode,
    pub cursor_char_index: usize,
    pub cursor_position: (usize, usize),
    #[serde(skip_serializing_if = "Option::is_none")]
    pub selection: Option<(usize, usize)>,
    pub scroll_offset: f32,
    #[serde(default)]
    pub rendered_scroll_offset: f32,
    pub has_unsaved_content: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub file_mtime: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub original_content_hash: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub csv_delimiter: Option<u8>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pdf_view_state: Option<PdfViewStateSnapshot>,
}
```

- [ ] **Step 5: Run the narrow tests again**

Run: `cargo test test_tab_info_pdf_view_state_roundtrip test_tab_info_pdf_view_state_backward_compatibility test_session_tab_state_pdf_view_state_roundtrip -- --nocapture`

Expected: PASS for all three tests.

- [ ] **Step 6: Commit the model/persistence layer**

```bash
git add src/preview/mod.rs src/preview/pdf/mod.rs src/preview/pdf/model.rs src/config/settings.rs src/config/session.rs
git commit -F - <<'EOF'
Define a persisted PDF reader view-state model

Ferrite now needs one schema for PDF reader state that can survive
session restore and config-based tab restore without coupling the
runtime cache to persistence.

Constraint: Preserve backward compatibility for existing config/session JSON
Rejected: Store egui-only preview state structs directly | mixes UI runtime with persistence schema
Confidence: high
Scope-risk: narrow
Reversibility: clean
Directive: Keep persisted PDF state limited to durable reading preferences, not transient UI input buffers
Tested: cargo test test_tab_info_pdf_view_state_roundtrip test_tab_info_pdf_view_state_backward_compatibility test_session_tab_state_pdf_view_state_roundtrip -- --nocapture
Not-tested: Full app restart path
EOF
```

### Task 2: Move PDF Reader State Onto `Tab`

**Files:**
- Modify: `src/state.rs`
- Modify: `src/app/mod.rs`
- Modify: `src/app/central_panel.rs`
- Modify: `src/app/keyboard.rs`
- Test: `src/state.rs`

- [ ] **Step 1: Write the failing state ownership tests**

```rust
#[test]
fn test_with_pdf_file_initializes_pdf_view_state() {
    let tab = Tab::with_pdf_file(1, PathBuf::from("/tmp/doc.pdf"), false);
    let pdf = tab.pdf_view_state.as_ref().expect("pdf state");
    assert_eq!(pdf.current_page, 0);
    assert_eq!(pdf.zoom, 1.0);
    assert!(pdf.sidebar_visible);
}

#[test]
fn test_tab_to_tab_info_carries_pdf_view_state() {
    let mut tab = Tab::with_pdf_file(1, PathBuf::from("/tmp/doc.pdf"), false);
    let pdf = tab.pdf_view_state.as_mut().unwrap();
    pdf.current_page = 11;
    pdf.zoom = 1.5;
    pdf.sidebar_visible = false;

    let info = tab.to_tab_info();
    let snapshot = info.pdf_view_state.expect("snapshot");
    assert_eq!(snapshot.current_page, 11);
    assert_eq!(snapshot.zoom_percent, 150);
    assert!(!snapshot.sidebar_visible);
}

#[test]
fn test_restore_session_result_restores_pdf_view_state() {
    use crate::config::{SessionRestoreResult, SessionState, SessionTabState};
    use crate::preview::pdf::PdfViewStateSnapshot;

    let mut state = AppState::with_settings(Settings::default());
    let mut session = SessionState::default();
    session.tabs.push(SessionTabState {
        tab_id: 99,
        path: Some(PathBuf::from("/tmp/doc.pdf")),
        display_title: "doc.pdf".to_string(),
        view_mode: ViewMode::Rendered,
        pdf_view_state: Some(PdfViewStateSnapshot {
            current_page: 3,
            zoom_percent: 125,
            ..Default::default()
        }),
        ..Default::default()
    });

    let result = SessionRestoreResult {
        session: Some(session),
        ..Default::default()
    };

    state.restore_from_session_result(&result);
    let tab = state.active_tab().unwrap();
    assert_eq!(tab.file_type(), FileType::Pdf);
    assert_eq!(tab.pdf_view_state.as_ref().unwrap().current_page, 3);
}
```

- [ ] **Step 2: Run the narrow tests and verify they fail**

Run: `cargo test test_with_pdf_file_initializes_pdf_view_state test_tab_to_tab_info_carries_pdf_view_state test_restore_session_result_restores_pdf_view_state -- --nocapture`

Expected: FAIL with errors about missing `pdf_view_state` on `Tab` and missing restore plumbing.

- [ ] **Step 3: Add `pdf_view_state` to `Tab` and initialize it only for PDFs**

```rust
// src/state.rs
use crate::preview::{PdfPreviewState, PdfViewStateSnapshot};

pub struct Tab {
    pub id: usize,
    pub path: Option<PathBuf>,
    pub content: String,
    pub split_ratio: f32,
    pub pdf_view_state: Option<PdfPreviewState>,
    pub pipeline_state: TabPipelineState,
    pub detected_encoding: Option<&'static str>,
}
```

```rust
// src/state.rs
pub fn with_pdf_file(id: usize, path: PathBuf, auto_save_default: bool) -> Self {
    let mut tab = Self::with_file(id, path, String::new());
    tab.file_type = FileType::Pdf;
    tab.view_mode = ViewMode::Rendered;
    tab.auto_save_enabled = auto_save_default;
    tab.detected_encoding = None;
    tab.current_encoding = "binary";
    tab.original_bytes.clear();
    tab.pdf_view_state = Some(PdfPreviewState::default());
    tab
}
```

```rust
// src/state.rs
pub fn with_image_file(id: usize, path: PathBuf, auto_save_default: bool) -> Self {
    let mut tab = Self::with_file(id, path, String::new());
    tab.file_type = FileType::Image;
    tab.view_mode = ViewMode::Rendered;
    tab.auto_save_enabled = auto_save_default;
    tab.pdf_view_state = None;
    tab
}
```

- [ ] **Step 4: Serialize/restore the owned state through `TabInfo` and `SessionTabState`**

```rust
// src/state.rs
pub fn to_tab_info(&self) -> TabInfo {
    TabInfo {
        path: self.path.clone(),
        modified: self.is_modified(),
        cursor_position: self.cursor_position,
        scroll_offset: self.scroll_offset,
        view_mode: self.view_mode,
        split_ratio: self.split_ratio,
        pdf_view_state: self
            .pdf_view_state
            .as_ref()
            .map(PdfPreviewState::to_snapshot),
    }
}
```

```rust
// src/state.rs, inside `from_tab_info`, immediately before the final `tab` return
if tab.file_type() == FileType::Pdf {
    tab.pdf_view_state = Some(
        info.pdf_view_state
            .clone()
            .map(PdfPreviewState::from_snapshot)
            .unwrap_or_default(),
    );
}
```

```rust
// src/state.rs capture_session_state
SessionTabState {
    tab_id: tab.id,
    path: tab.path.clone(),
    display_title: tab.title(),
    view_mode: tab.view_mode,
    cursor_char_index: tab.cursors.primary().head,
    cursor_position: tab.cursor_position,
    selection: tab.cursors.selection_range(),
    scroll_offset: tab.scroll_offset,
    rendered_scroll_offset: 0.0,
    has_unsaved_content: tab.is_modified(),
    file_mtime,
    original_content_hash,
    csv_delimiter: None,
    pdf_view_state: tab.pdf_view_state.as_ref().map(PdfPreviewState::to_snapshot),
}
```

- [ ] **Step 5: Remove the duplicate app-level map and read/write state through the active tab**

```rust
// src/app/mod.rs
pub struct FerriteApp {
    sync_scroll_states: HashMap<usize, SyncScrollState>,
    should_exit: bool,
    last_window_size: Option<egui::Vec2>,
}
```

```rust
// src/app/central_panel.rs
} else if active_file_type.is_pdf() {
    if let Some(tab) = self.state.active_tab_mut() {
        if let (Some(path), Some(pdf_state)) = (tab.path.clone(), tab.pdf_view_state.as_mut()) {
            render_pdf_preview(ui, &path, pdf_state);
        }
    }
}
```

```rust
// src/app/keyboard.rs
if let Some(tab) = self.state.active_tab_mut() {
    if let Some(pdf_state) = tab.pdf_view_state.as_mut() {
        if pdf_state.current_page > 0 {
            pdf_state.current_page -= 1;
        }
    }
}
```

- [ ] **Step 6: Run the state tests plus a compile check**

Run: `cargo test test_with_pdf_file_initializes_pdf_view_state test_tab_to_tab_info_carries_pdf_view_state test_restore_session_result_restores_pdf_view_state test_open_pdf_file_as_preview -- --nocapture`

Expected: PASS

Run: `cargo check`

Expected: command exits 0 and prints a `Finished 'dev' profile` line.

- [ ] **Step 7: Commit the state-ownership migration**

```bash
git add src/state.rs src/app/mod.rs src/app/central_panel.rs src/app/keyboard.rs
git commit -F - <<'EOF'
Unify PDF reader state under each tab

Ferrite no longer needs a second PDF-state map in the app shell now
that PDF reader state must survive persistence and keyboard-driven
navigation.

Constraint: Session capture APIs only see AppState tabs, not FerriteApp transient maps
Rejected: Keep pdf_viewer_states and copy into tabs during save | easy to desynchronize
Confidence: high
Scope-risk: moderate
Reversibility: clean
Directive: Any future PDF interaction state that must survive tab restore belongs on Tab, not in FerriteApp side maps
Tested: cargo test test_with_pdf_file_initializes_pdf_view_state test_tab_to_tab_info_carries_pdf_view_state test_restore_session_result_restores_pdf_view_state test_open_pdf_file_as_preview -- --nocapture; cargo check
Not-tested: Crash-recovery restore with a real PDF file on disk
EOF
```

### Task 3: Replace Ad-Hoc Rendering Threads with a Bounded PDF Runtime

**Files:**
- Create: `src/preview/pdf/runtime.rs`
- Create: `src/preview/pdf/ui.rs`
- Create: `src/preview/pdf/mod.rs`
- Modify: `src/preview/mod.rs`
- Delete: `src/preview/pdf_preview.rs`
- Test: `src/preview/pdf/runtime.rs`

- [ ] **Step 1: Write the failing pure-runtime tests**

```rust
#[test]
fn test_zoom_bucket_rounds_to_stable_cache_keys() {
    assert_eq!(zoom_bucket(1.0), 100);
    assert_eq!(zoom_bucket(1.249), 125);
    assert_eq!(zoom_bucket(1.251), 125);
}

#[test]
fn test_request_queue_orders_visible_pages_before_thumbnails() {
    let mut queue = PdfRequestQueue::default();
    queue.push(PdfRenderRequest::thumbnail(PathBuf::from("/tmp/doc.pdf"), 8, 20));
    queue.push(PdfRenderRequest::visible(PathBuf::from("/tmp/doc.pdf"), 2, 125));
    queue.push(PdfRenderRequest::prefetch(PathBuf::from("/tmp/doc.pdf"), 3, 125));

    assert_eq!(queue.pop().unwrap().kind, PdfRenderKind::VisiblePage);
    assert_eq!(queue.pop().unwrap().kind, PdfRenderKind::PrefetchPage);
    assert_eq!(queue.pop().unwrap().kind, PdfRenderKind::Thumbnail);
}

#[test]
fn test_cache_budget_evicts_thumbnail_entries_first() {
    let mut cache = PdfPageCache::new(2, 1);
    cache.insert_page(sample_key(1), sample_bitmap());
    cache.insert_page(sample_key(2), sample_bitmap());
    cache.insert_thumbnail(sample_thumb_key(1), sample_bitmap());
    cache.insert_page(sample_key(3), sample_bitmap());

    assert!(cache.get_thumbnail(&sample_thumb_key(1)).is_none());
    assert!(cache.get_page(&sample_key(3)).is_some());
}
```

- [ ] **Step 2: Run the new runtime tests and verify they fail**

Run: `cargo test test_zoom_bucket_rounds_to_stable_cache_keys test_request_queue_orders_visible_pages_before_thumbnails test_cache_budget_evicts_thumbnail_entries_first -- --nocapture`

Expected: FAIL because `PdfRequestQueue`, `PdfPageCache`, and `zoom_bucket` are not implemented yet.

- [ ] **Step 3: Implement the bounded scheduler and cache budget helpers**

```rust
// src/preview/pdf/runtime.rs
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum PdfRenderKind {
    VisiblePage,
    PrefetchPage,
    Thumbnail,
}

#[derive(Debug, Clone)]
pub struct PdfRenderRequest {
    pub kind: PdfRenderKind,
    pub path: PathBuf,
    pub page_index: usize,
    pub zoom_bucket: u16,
}

pub fn zoom_bucket(zoom: f32) -> u16 {
    ((zoom * 100.0).round() as i32).clamp(50, 400) as u16
}

#[derive(Default)]
pub struct PdfRequestQueue {
    heap: BinaryHeap<QueuedRequest>,
    next_sequence: u64,
}

fn kind_rank(kind: PdfRenderKind) -> u8 {
    match kind {
        PdfRenderKind::VisiblePage => 0,
        PdfRenderKind::PrefetchPage => 1,
        PdfRenderKind::Thumbnail => 2,
    }
}

impl PdfRequestQueue {
    pub fn push(&mut self, req: PdfRenderRequest) {
        let queued = QueuedRequest::new(kind_rank(req.kind), self.next_sequence, req);
        self.heap.push(queued);
        self.next_sequence += 1;
    }

    pub fn pop(&mut self) -> Option<PdfRenderRequest> {
        self.heap.pop().map(|queued| queued.request)
    }
}

pub struct PdfPageCache {
    page_limit: usize,
    thumbnail_limit: usize,
    pages: LruCache<PageRenderKey, RenderedPdfBitmap>,
    thumbnails: LruCache<PageRenderKey, RenderedPdfBitmap>,
}
```

- [ ] **Step 4: Move the current document/page loading code behind a `PdfRuntime` facade**

```rust
// src/preview/pdf/runtime.rs
pub struct PdfRuntime {
    documents: HashMap<PathBuf, PdfDocumentHandle>,
    queue: PdfRequestQueue,
    cache: PdfPageCache,
    worker_count: usize,
}

impl PdfRuntime {
    pub fn request_visible_range(
        &mut self,
        path: &Path,
        zoom: f32,
        visible_pages: impl IntoIterator<Item = usize>,
        prefetch_pages: impl IntoIterator<Item = usize>,
    ) {
        let zoom_bucket = zoom_bucket(zoom);
        for page_index in visible_pages {
            self.queue.push(PdfRenderRequest::visible(path.to_path_buf(), page_index, zoom_bucket));
        }
        for page_index in prefetch_pages {
            self.queue.push(PdfRenderRequest::prefetch(path.to_path_buf(), page_index, zoom_bucket));
        }
    }

    pub fn request_thumbnail_range(
        &mut self,
        path: &Path,
        pages: impl IntoIterator<Item = usize>,
    ) {
        for page_index in pages {
            self.queue.push(PdfRenderRequest::thumbnail(path.to_path_buf(), page_index, 20));
        }
    }
}
```

```rust
// src/preview/pdf/ui.rs
let runtime = PdfRuntime::for_ui(ui);
runtime.request_visible_range(path, state.zoom, visible_pages, prefetch_pages);
runtime.request_thumbnail_range(path, thumbnail_pages);
```

- [ ] **Step 5: Split the current monolithic file into `model`, `runtime`, and `ui` modules**

```rust
// src/preview/pdf/mod.rs
mod model;
mod runtime;
mod ui;

pub use model::{
    PdfMouseMode, PdfPreviewState, PdfScrollLayout, PdfSidebarMode, PdfSpreadMode,
    PdfViewStateSnapshot,
};
pub use ui::render_pdf_preview;
```

```bash
git mv src/preview/pdf_preview.rs src/preview/pdf/ui.rs
```

Then shrink `src/preview/pdf/ui.rs` so it imports queue/cache/document helpers from `runtime.rs` instead of owning them inline.

- [ ] **Step 6: Run the runtime tests plus a compile check**

Run: `cargo test test_zoom_bucket_rounds_to_stable_cache_keys test_request_queue_orders_visible_pages_before_thumbnails test_cache_budget_evicts_thumbnail_entries_first -- --nocapture`

Expected: PASS

Run: `cargo check`

Expected: command exits 0 and prints a `Finished 'dev' profile` line.

- [ ] **Step 7: Commit the runtime split**

```bash
git add src/preview/mod.rs src/preview/pdf/mod.rs src/preview/pdf/runtime.rs src/preview/pdf/ui.rs
git rm src/preview/pdf_preview.rs
git commit -F - <<'EOF'
Bound PDF rendering behind a scheduler and cache budget

Ferrite's PDF path now needs predictable worker usage so large PDFs can
scroll smoothly without spawning an unbounded number of page render
threads.

Constraint: Stay on the pure-Rust hayro renderer with no native PDF runtime
Rejected: Keep per-page thread::spawn from the UI path | causes request storms during fast scroll/zoom
Confidence: medium
Scope-risk: moderate
Reversibility: clean
Directive: Visible page work must always outrank thumbnail work in the queue
Tested: cargo test test_zoom_bucket_rounds_to_stable_cache_keys test_request_queue_orders_visible_pages_before_thumbnails test_cache_budget_evicts_thumbnail_entries_first -- --nocapture; cargo check
Not-tested: Long manual scroll on a 500+ page PDF
EOF
```

### Task 4: Add Page Jump and Robust Outline Fallback Navigation

**Files:**
- Modify: `src/preview/pdf/model.rs`
- Modify: `src/preview/pdf/ui.rs`
- Modify: `src/app/keyboard.rs`
- Test: `src/preview/pdf/model.rs`

- [ ] **Step 1: Write the failing navigation helper tests**

```rust
#[test]
fn test_parse_page_jump_input_accepts_one_based_page_numbers() {
    assert_eq!(parse_page_jump_input("12", 200), Some(11));
}

#[test]
fn test_parse_page_jump_input_rejects_out_of_range_page_numbers() {
    assert_eq!(parse_page_jump_input("0", 200), None);
    assert_eq!(parse_page_jump_input("201", 200), None);
    assert_eq!(parse_page_jump_input("abc", 200), None);
}

#[test]
fn test_outline_or_page_list_returns_page_list_when_outline_is_empty() {
    let items = outline_or_page_list(&[], 3);
    assert_eq!(items.len(), 3);
    assert_eq!(items[0].label, "Page 1");
    assert_eq!(items[2].page_index, 2);
}
```

- [ ] **Step 2: Run the helper tests and verify they fail**

Run: `cargo test test_parse_page_jump_input_accepts_one_based_page_numbers test_parse_page_jump_input_rejects_out_of_range_page_numbers test_outline_or_page_list_returns_page_list_when_outline_is_empty -- --nocapture`

Expected: FAIL because `parse_page_jump_input` and `outline_or_page_list` do not exist.

- [ ] **Step 3: Add the small pure helpers and transient input state**

```rust
// src/preview/pdf/model.rs
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PdfNavItem {
    pub label: String,
    pub page_index: usize,
    pub depth: usize,
}

pub fn parse_page_jump_input(input: &str, total_pages: usize) -> Option<usize> {
    let page = input.trim().parse::<usize>().ok()?;
    if page == 0 || page > total_pages {
        return None;
    }
    Some(page - 1)
}

pub fn outline_or_page_list(outline: &[PdfOutlineItem], total_pages: usize) -> Vec<PdfNavItem> {
    if outline.is_empty() {
        return (0..total_pages)
            .map(|page_index| PdfNavItem {
                label: format!("Page {}", page_index + 1),
                page_index,
                depth: 0,
            })
            .collect();
    }

    outline
        .iter()
        .map(|item| PdfNavItem {
            label: item.title.clone(),
            page_index: item.page_index,
            depth: item.depth,
        })
        .collect()
}
```

```rust
// src/preview/pdf/model.rs
pub struct PdfPreviewState {
    pub current_page: usize,
    pub zoom: f32,
    pub total_pages: Option<usize>,
    pub spread_mode: PdfSpreadMode,
    pub scroll_layout: PdfScrollLayout,
    pub sidebar_visible: bool,
    pub sidebar_mode: PdfSidebarMode,
    pub mouse_mode: PdfMouseMode,
    pub page_jump_input: String,
}
```

- [ ] **Step 4: Add page jump UI and explicit outline fallback**

```rust
// src/preview/pdf/ui.rs toolbar fragment
ui.separator();
let response = ui.add(
    egui::TextEdit::singleline(&mut state.page_jump_input)
        .desired_width(56.0)
        .hint_text("Page"),
);
if response.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
    if let Some(target) = parse_page_jump_input(&state.page_jump_input, total_pages) {
        state.current_page = target;
    }
}
if ui.button("Go").clicked() {
    if let Some(target) = parse_page_jump_input(&state.page_jump_input, total_pages) {
        state.current_page = target;
    }
}
```

```rust
// src/preview/pdf/ui.rs sidebar fragment
for item in outline_or_page_list(&doc.outline, total_pages) {
    let selected = state.current_page == item.page_index;
    if ui.selectable_label(selected, item.label).clicked() {
        state.current_page = item.page_index;
    }
}
```

- [ ] **Step 5: Re-run the helper tests and the PDF open regression**

Run: `cargo test test_parse_page_jump_input_accepts_one_based_page_numbers test_parse_page_jump_input_rejects_out_of_range_page_numbers test_outline_or_page_list_returns_page_list_when_outline_is_empty test_open_pdf_file_as_preview -- --nocapture`

Expected: PASS

Run: `cargo check`

Expected: command exits 0 and prints a `Finished 'dev' profile` line.

- [ ] **Step 6: Commit the navigation layer**

```bash
git add src/preview/pdf/model.rs src/preview/pdf/ui.rs src/app/keyboard.rs
git commit -F - <<'EOF'
Add baseline PDF page-jump and outline fallback navigation

Ferrite's PDF reader now needs a reliable navigation floor even when a
document has no outline tree, and page jump is the smallest useful
navigation affordance after performance stabilization.

Constraint: Do not implement full-text PDF search in this iteration
Rejected: Add search-first navigation instead of page jump | broader scope than the approved medium-reader target
Confidence: medium
Scope-risk: narrow
Reversibility: clean
Directive: Keep page-jump parsing one-based in the UI and zero-based internally
Tested: cargo test test_parse_page_jump_input_accepts_one_based_page_numbers test_parse_page_jump_input_rejects_out_of_range_page_numbers test_outline_or_page_list_returns_page_list_when_outline_is_empty test_open_pdf_file_as_preview -- --nocapture; cargo check
Not-tested: Manual keyboard-only page jump flow
EOF
```

### Task 5: Document the PDF Reader Architecture and Verification Matrix

**Files:**
- Create: `docs/technical/viewers/pdf-preview.md`
- Modify: `src/state.rs`
- Test: `src/state.rs`

- [ ] **Step 1: Add one regression test for round-tripping persisted PDF state through `TabInfo`**

```rust
#[test]
fn test_pdf_tab_roundtrip_via_tab_info_preserves_page_zoom_and_sidebar() {
    let mut tab = Tab::with_pdf_file(1, PathBuf::from("/tmp/spec.pdf"), false);
    let pdf = tab.pdf_view_state.as_mut().unwrap();
    pdf.current_page = 15;
    pdf.zoom = 1.25;
    pdf.sidebar_visible = false;

    let info = tab.to_tab_info();
    let restored = Tab::from_tab_info(2, &info, String::new());
    let restored_pdf = restored.pdf_view_state.as_ref().unwrap();

    assert_eq!(restored_pdf.current_page, 15);
    assert_eq!(restored_pdf.zoom, 1.25);
    assert!(!restored_pdf.sidebar_visible);
}
```

- [ ] **Step 2: Run the regression test before writing docs**

Run: `cargo test test_pdf_tab_roundtrip_via_tab_info_preserves_page_zoom_and_sidebar -- --nocapture`

Expected: PASS

- [ ] **Step 3: Write the technical note and manual QA checklist**

```markdown
# PDF Preview / Reader Architecture

## Scope
- Pure-Rust read-only PDF reader path
- Non-blocking open/render flow
- Page jump, outline fallback, persisted reading state

## Runtime
- `Tab.pdf_view_state` owns durable reading state
- `PdfRuntime` owns transient document/cache/worker state
- Visible pages outrank prefetch pages
- Prefetch pages outrank thumbnails

## Cache Budget
- Page bitmap cache: bounded LRU
- Thumbnail cache: smaller bounded LRU
- Texture cache: only promote bitmaps once the UI consumes them

## Manual Verification
1. Open a 300+ page PDF and confirm the UI stays interactive immediately.
2. Scroll quickly while thumbnails are visible and confirm body pages win over thumbnails.
3. Enter a page number in the jump field and confirm the target page loads.
4. Restart Ferrite and confirm the same PDF restores page, zoom, and sidebar/layout state.
5. Open a PDF with no outline and confirm the outline pane falls back to a page list.
```

- [ ] **Step 4: Run the focused regression tests plus a full compile**

Run: `cargo test test_pdf_tab_roundtrip_via_tab_info_preserves_page_zoom_and_sidebar test_open_pdf_file_as_preview test_file_type_helpers -- --nocapture`

Expected: PASS

Run: `cargo check`

Expected: command exits 0 and prints a `Finished 'dev' profile` line.

- [ ] **Step 5: Commit the docs + verification pass**

```bash
git add docs/technical/viewers/pdf-preview.md src/state.rs
git commit -F - <<'EOF'
Document the PDF reader foundation and its regression checks

The PDF reader path now has enough moving parts that workers need one
technical note describing the runtime split, cache priorities, and the
manual checks that prove the medium-reader scope still holds.

Constraint: Keep documentation aligned to the current pure-Rust reader scope
Rejected: Fold PDF notes into generic preview docs | hides PDF-specific runtime constraints
Confidence: high
Scope-risk: narrow
Reversibility: clean
Directive: Update this doc whenever queue priority or persistence fields change
Tested: cargo test test_pdf_tab_roundtrip_via_tab_info_preserves_page_zoom_and_sidebar test_open_pdf_file_as_preview test_file_type_helpers -- --nocapture; cargo check
Not-tested: Cross-platform manual QA on Linux and Windows
EOF
```

## Self-Review

- Spec coverage:
  - Performance stabilization: covered by Task 3.
  - Page jump + outline fallback: covered by Task 4.
  - Reading state persistence: covered by Tasks 1, 2, and 5.
  - Pure-Rust / `hayro` constraint: preserved across all tasks, especially Task 3.
- Placeholder scan:
  - No open placeholder markers remain.
- Type consistency:
  - The same names are used throughout: `PdfViewStateSnapshot`, `PdfPreviewState`, `PdfRequestQueue`, `PdfPageCache`, `parse_page_jump_input`.

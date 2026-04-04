# PDF Preview / Reader Architecture

## Scope

- Pure-Rust read-only PDF reader path built on `hayro`
- Non-blocking open and render flow for large documents
- Page jump, outline fallback, and persisted reading state

## Runtime Split

- `Tab.pdf_view_state` owns durable reader state such as current page, zoom, and sidebar layout.
- `preview/pdf/runtime.rs` owns transient document metadata, bounded page workers, and bitmap caches.
- `preview/pdf/ui.rs` only decides which pages are visible, requests work from the runtime, and renders textures/placeholders.

## Scheduling Rules

- Document metadata loads first so the reader knows total page count and outline shape.
- Visible pages are highest priority.
- Prefetch pages adjacent to the viewport are next priority.
- Thumbnail renders are lowest priority and should never starve visible content.

## Cache Budget

- Page bitmap cache keeps a small working set around the viewport.
- Thumbnail bitmap cache is budgeted separately and is evicted first when the combined budget is exceeded.
- egui textures are uploaded lazily from cached bitmaps when the UI actually needs them.

## Persisted Reader State

- `TabInfo.pdf_view_state` stores page, zoom, spread/layout, and sidebar choices for config/session restore.
- `SessionTabState.pdf_view_state` mirrors the same durable fields for crash-safe recovery.
- Transient inputs, in-flight jobs, and textures are intentionally excluded from persistence.

## Manual Verification

1. Open a 300+ page PDF and confirm the UI stays interactive immediately.
2. Scroll quickly with the thumbnail rail open and confirm body pages appear before thumbnails catch up.
3. Enter a page number in the jump field and confirm the reader lands on the expected page.
4. Open a PDF with no outline and confirm the outline pane falls back to a page list.
5. Restart Ferrite with a PDF tab open and confirm page, zoom, and sidebar/layout state restore correctly.

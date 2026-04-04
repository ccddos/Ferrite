//! Standalone PDF preview rendering for PDF document tabs.
//!
//! This is a best-effort read-only viewer built on `hayro`. Large documents are
//! loaded and rasterized on background threads so egui stays responsive while
//! page data arrives incrementally.

use eframe::egui::{
    self, Color32, ColorImage, RichText, Stroke, TextEdit, TextureHandle, TextureOptions, Ui,
    Vec2,
};
use hayro::hayro_interpret::InterpreterSettings;
use hayro::hayro_syntax::object::dict::keys::{A, DEST, FIRST, NEXT, OUTLINES, TITLE};
use hayro::hayro_syntax::object::{Dict, ObjRef, Object, ObjectIdentifier, String as PdfString};
use hayro::hayro_syntax::Pdf;
use hayro::{render, RenderSettings};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

const PAGE_GAP: f32 = 24.0;
const VIEWPORT_BUFFER_PAGES: usize = 1;
const SIDEBAR_BUFFER_ROWS: usize = 2;
const PDF_SIDEBAR_DEFAULT_WIDTH: f32 = 280.0;
const PDF_SIDEBAR_MIN_WIDTH: f32 = 220.0;
const PDF_SIDEBAR_MAX_WIDTH: f32 = 420.0;
const PDF_SIDEBAR_RESIZE_HANDLE_WIDTH: f32 = 8.0;
const PDF_THUMBNAIL_WIDTH: f32 = 150.0;
const PDF_THUMBNAIL_HEIGHT: f32 = 210.0;
const PDF_THUMBNAIL_ROW_HEIGHT: f32 = PDF_THUMBNAIL_HEIGHT + 58.0;
const FALLBACK_PAGE_WIDTH: f32 = 900.0;
const FALLBACK_PAGE_HEIGHT: f32 = 1200.0;
const PDF_ASYNC_POLL_INTERVAL: Duration = Duration::from_millis(16);
const PDF_ZOOM_DEBOUNCE: Duration = Duration::from_millis(45);
const PDF_ZOOM_EPSILON: f32 = 0.01;
const PDF_ZOOM_APPLY_THRESHOLD: f32 = 0.08;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PdfSpreadMode {
    SinglePage,
    TwoPageOdd,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PdfScrollLayout {
    Vertical,
    Horizontal,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PdfSidebarMode {
    Thumbnails,
    Outline,
}

/// Per-tab UI state for PDF preview.
#[derive(Debug, Clone)]
pub struct PdfPreviewState {
    pub current_page: usize,
    pub zoom: f32,
    pub total_pages: Option<usize>,
    pub spread_mode: PdfSpreadMode,
    pub scroll_layout: PdfScrollLayout,
    pub sidebar_visible: bool,
    pub sidebar_mode: PdfSidebarMode,
    pub sidebar_width: f32,
    pending_jump_to_page: Option<usize>,
    pending_zoom_factor: f32,
    last_zoom_input_at: Option<Instant>,
    page_input_buffer: String,
    page_input_editing: bool,
}

impl Default for PdfPreviewState {
    fn default() -> Self {
        Self {
            current_page: 0,
            zoom: 1.0,
            total_pages: None,
            spread_mode: PdfSpreadMode::TwoPageOdd,
            scroll_layout: PdfScrollLayout::Vertical,
            sidebar_visible: true,
            sidebar_mode: PdfSidebarMode::Thumbnails,
            sidebar_width: PDF_SIDEBAR_DEFAULT_WIDTH,
            pending_jump_to_page: None,
            pending_zoom_factor: 1.0,
            last_zoom_input_at: None,
            page_input_buffer: "1".to_string(),
            page_input_editing: false,
        }
    }
}

#[derive(Clone, Copy)]
struct PdfChromeColors {
    toolbar_fill: Color32,
    sidebar_fill: Color32,
    section_fill: Color32,
    border: Color32,
    text: Color32,
    muted_text: Color32,
    selection_fill: Color32,
    selection_text: Color32,
    hover_fill: Color32,
    handle: Color32,
}

#[derive(Clone)]
struct CachedPdfTexture {
    texture: TextureHandle,
    page_width: u32,
    page_height: u32,
    display_width: f32,
    display_height: f32,
}

struct CachedPdfDocument {
    pdf: Pdf,
    total_pages: usize,
    outline: Vec<PdfOutlineItem>,
}

#[derive(Clone)]
enum PdfLoadResult {
    Loading,
    Loaded(CachedPdfTexture),
    Failed(String),
}

#[derive(Debug, Clone)]
struct PdfOutlineItem {
    title: String,
    page_index: usize,
    depth: usize,
}

#[derive(Clone)]
struct RenderedPdfBitmap {
    width: u32,
    height: u32,
    rgba: Arc<[u8]>,
    display_width: f32,
    display_height: f32,
}

#[derive(Clone)]
enum PdfDocumentTaskState {
    Loading,
    Loaded(Arc<CachedPdfDocument>),
    Failed(String),
}

#[derive(Clone)]
struct PdfDocumentTask {
    state: Arc<Mutex<PdfDocumentTaskState>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct PageRenderKey {
    path: PathBuf,
    page_index: usize,
    render_scale_milli: i32,
}

#[derive(Clone)]
enum PdfPageTaskState {
    Loading,
    ReadyBitmap(RenderedPdfBitmap),
    Loaded(CachedPdfTexture),
    Failed(String),
}

#[derive(Clone)]
struct PdfPageTask {
    state: Arc<Mutex<PdfPageTaskState>>,
}

#[derive(Clone, Default)]
struct PdfPreviewStorage {
    documents: HashMap<PathBuf, PdfDocumentTask>,
    pages: HashMap<PageRenderKey, PdfPageTask>,
}

fn zoom_percent_label(zoom: f32) -> String {
    format!("{:.0}%", zoom * 100.0)
}

fn quantize_zoom(zoom: f32) -> f32 {
    ((zoom * 100.0).round() / 100.0).clamp(0.5, 4.0)
}

fn requested_page_index(input: &str, total_pages: usize) -> Option<usize> {
    if total_pages == 0 {
        return None;
    }

    let requested = input.trim().parse::<usize>().ok()?;
    Some(requested.saturating_sub(1).min(total_pages.saturating_sub(1)))
}

fn sync_page_input(state: &mut PdfPreviewState) {
    if !state.page_input_editing {
        state.page_input_buffer = state.current_page.saturating_add(1).to_string();
    }
}

fn jump_to_page(state: &mut PdfPreviewState, target_page: usize, total_pages: usize) {
    let clamped = target_page.min(total_pages.saturating_sub(1));
    state.current_page = clamped;
    state.pending_jump_to_page = Some(clamped);
    state.page_input_buffer = clamped.saturating_add(1).to_string();
    state.page_input_editing = false;
}

fn commit_page_input(state: &mut PdfPreviewState, total_pages: usize) {
    if let Some(page_index) = requested_page_index(&state.page_input_buffer, total_pages) {
        jump_to_page(state, page_index, total_pages);
    } else {
        sync_page_input(state);
    }
}

fn pdf_chrome_colors(ui: &Ui) -> PdfChromeColors {
    let visuals = ui.visuals();
    let text = visuals.text_color();
    PdfChromeColors {
        toolbar_fill: visuals.panel_fill,
        sidebar_fill: visuals.faint_bg_color,
        section_fill: visuals.extreme_bg_color,
        border: visuals.widgets.noninteractive.bg_stroke.color,
        text,
        muted_text: text.gamma_multiply(0.72),
        selection_fill: visuals.selection.bg_fill,
        selection_text: visuals.strong_text_color(),
        hover_fill: visuals.widgets.hovered.bg_fill,
        handle: visuals.widgets.noninteractive.bg_stroke.color.gamma_multiply(0.9),
    }
}

fn outline_row_height(font_size: f32) -> f32 {
    (font_size + 12.0).max(28.0)
}

fn outline_row(
    ui: &mut Ui,
    title: &str,
    selected: bool,
    colors: &PdfChromeColors,
) -> egui::Response {
    let font_id = egui::TextStyle::Body.resolve(ui.style());
    let row_height = outline_row_height(font_id.size);
    let fill = if selected {
        colors.selection_fill
    } else {
        Color32::TRANSPARENT
    };

    let frame = egui::Frame::none()
        .fill(fill)
        .stroke(Stroke::NONE)
        .rounding(10.0)
        .inner_margin(egui::Margin::symmetric(12.0, 6.0));
    let response = frame
        .show(ui, |ui| {
            ui.set_min_height(row_height);
            let label = egui::Label::new(
                RichText::new(title)
                    .size(font_id.size)
                    .color(if selected {
                        colors.selection_text
                    } else {
                        colors.text
                    }),
            )
            .truncate()
            .sense(egui::Sense::click());
            ui.add_sized(egui::vec2(ui.available_width(), row_height), label)
        })
        .inner;

    if response.hovered() && !selected {
        ui.painter().rect_stroke(
            response.rect,
            10.0,
            Stroke::new(1.0, colors.hover_fill),
        );
    }

    response
}

fn flush_pending_zoom(
    ui: &mut Ui,
    state: &mut PdfPreviewState,
    force: bool,
) -> bool {
    let Some(last_input) = state.last_zoom_input_at else {
        return false;
    };

    let now = Instant::now();
    let pending_change = (state.pending_zoom_factor - 1.0).abs();
    let ready = force
        || now.duration_since(last_input) >= PDF_ZOOM_DEBOUNCE
        || pending_change >= PDF_ZOOM_APPLY_THRESHOLD;

    if !ready {
        ui.ctx().request_repaint_after(PDF_ZOOM_DEBOUNCE);
        return false;
    }

    let next_zoom = quantize_zoom((state.zoom * state.pending_zoom_factor).clamp(0.5, 4.0));
    state.pending_zoom_factor = 1.0;
    state.last_zoom_input_at = None;

    if (next_zoom - state.zoom).abs() <= PDF_ZOOM_EPSILON {
        return false;
    }

    state.zoom = next_zoom;
    true
}

fn render_scale_key(zoom: f32, pixels_per_point: f32) -> i32 {
    ((zoom * pixels_per_point) * 100.0).round() as i32
}

fn page_render_key(
    path: &Path,
    page_index: usize,
    zoom: f32,
    pixels_per_point: f32,
) -> PageRenderKey {
    PageRenderKey {
        path: path.to_path_buf(),
        page_index,
        render_scale_milli: render_scale_key(zoom, pixels_per_point),
    }
}

fn with_pdf_preview_storage<R>(ui: &mut Ui, f: impl FnOnce(&mut PdfPreviewStorage) -> R) -> R {
    ui.data_mut(|data| {
        let storage =
            data.get_temp_mut_or_default::<PdfPreviewStorage>(egui::Id::new("pdf_preview_storage"));
        f(storage)
    })
}

impl PdfDocumentTask {
    fn spawn(ctx: &egui::Context, path: &Path) -> Self {
        let state = Arc::new(Mutex::new(PdfDocumentTaskState::Loading));
        let state_for_thread = Arc::clone(&state);
        let path = path.to_path_buf();
        let ctx = ctx.clone();

        thread::Builder::new()
            .name("ferrite-pdf-load".to_string())
            .spawn(move || {
                let next_state = match load_pdf_document(&path) {
                    Ok(doc) => PdfDocumentTaskState::Loaded(doc),
                    Err(msg) => PdfDocumentTaskState::Failed(msg),
                };

                if let Ok(mut guard) = state_for_thread.lock() {
                    *guard = next_state;
                }
                ctx.request_repaint();
            })
            .expect("Failed to spawn PDF document loader");

        Self { state }
    }

    fn snapshot(&self) -> PdfDocumentTaskState {
        self.state
            .lock()
            .map(|guard| guard.clone())
            .unwrap_or_else(|_| PdfDocumentTaskState::Failed("PDF loader state poisoned".into()))
    }
}

impl PdfPageTask {
    fn spawn(
        ctx: &egui::Context,
        path: &Path,
        page_index: usize,
        zoom: f32,
        pixels_per_point: f32,
        doc: Arc<CachedPdfDocument>,
    ) -> Self {
        let state = Arc::new(Mutex::new(PdfPageTaskState::Loading));
        let state_for_thread = Arc::clone(&state);
        let path = path.to_path_buf();
        let ctx = ctx.clone();

        thread::Builder::new()
            .name(format!("ferrite-pdf-page-{page_index}"))
            .spawn(move || {
                let next_state =
                    match render_pdf_page_bitmap(&path, page_index, zoom, pixels_per_point, &doc) {
                    Ok(bitmap) => PdfPageTaskState::ReadyBitmap(bitmap),
                    Err(msg) => PdfPageTaskState::Failed(msg),
                };

                if let Ok(mut guard) = state_for_thread.lock() {
                    *guard = next_state;
                }
                ctx.request_repaint();
            })
            .expect("Failed to spawn PDF page renderer");

        Self { state }
    }

    fn snapshot(&self) -> PdfPageTaskState {
        self.state
            .lock()
            .map(|guard| guard.clone())
            .unwrap_or_else(|_| PdfPageTaskState::Failed("PDF page renderer state poisoned".into()))
    }

    fn promote_loaded(&self, texture: CachedPdfTexture) {
        if let Ok(mut guard) = self.state.lock() {
            *guard = PdfPageTaskState::Loaded(texture);
        }
    }

    fn dimensions(&self) -> Option<(f32, f32)> {
        match self.snapshot() {
            PdfPageTaskState::ReadyBitmap(bitmap) => {
                Some((bitmap.display_width, bitmap.display_height))
            }
            PdfPageTaskState::Loaded(texture) => {
                Some((texture.display_width, texture.display_height))
            }
            PdfPageTaskState::Loading | PdfPageTaskState::Failed(_) => None,
        }
    }
}

fn decode_pdf_string(value: PdfString<'_>) -> String {
    String::from_utf8_lossy(value.get().as_ref())
        .trim()
        .to_string()
}

fn page_index_from_dest_array(
    array: hayro::hayro_syntax::object::Array<'_>,
    page_map: &HashMap<ObjectIdentifier, usize>,
) -> Option<usize> {
    let mut iter = array.raw_iter();
    let first = iter.next()?;
    match first {
        hayro::hayro_syntax::object::MaybeRef::Ref(r) => page_map.get(&r.into()).copied(),
        hayro::hayro_syntax::object::MaybeRef::NotRef(Object::Dict(d)) => {
            d.obj_id().and_then(|id| page_map.get(&id).copied())
        }
        _ => None,
    }
}

fn outline_dest_page(
    item: &Dict<'_>,
    page_map: &HashMap<ObjectIdentifier, usize>,
) -> Option<usize> {
    if let Some(dest) = item.get::<hayro::hayro_syntax::object::Array<'_>>(DEST) {
        return page_index_from_dest_array(dest, page_map);
    }

    if let Some(action) = item.get::<Dict<'_>>(A) {
        if let Some(dest) = action.get::<hayro::hayro_syntax::object::Array<'_>>(b"D".as_ref()) {
            return page_index_from_dest_array(dest, page_map);
        }
    }

    None
}

fn collect_outline_items(
    xref: &hayro::hayro_syntax::xref::XRef,
    current: Option<ObjRef>,
    depth: usize,
    page_map: &HashMap<ObjectIdentifier, usize>,
    items: &mut Vec<PdfOutlineItem>,
    visited: &mut HashSet<ObjectIdentifier>,
) {
    let mut next_ref = current;
    let mut steps = 0usize;

    while let Some(obj_ref) = next_ref {
        let id: ObjectIdentifier = obj_ref.into();
        if !visited.insert(id) {
            break;
        }
        steps += 1;
        if steps > 2048 {
            break;
        }

        let Some(dict) = xref.get::<Dict<'_>>(id) else {
            break;
        };

        if let Some(title) = dict.get::<PdfString<'_>>(TITLE) {
            if let Some(page_index) = outline_dest_page(&dict, page_map) {
                let title_text = decode_pdf_string(title);
                if !title_text.is_empty() {
                    items.push(PdfOutlineItem {
                        title: title_text,
                        page_index,
                        depth,
                    });
                }
            }
        }

        collect_outline_items(
            xref,
            dict.get_ref(FIRST),
            depth + 1,
            page_map,
            items,
            visited,
        );
        next_ref = dict.get_ref(NEXT);
    }
}

fn parse_pdf_outline(pdf: &Pdf) -> Vec<PdfOutlineItem> {
    let mut page_map = HashMap::new();
    for (index, page) in pdf.pages().iter().enumerate() {
        if let Some(id) = page.raw().obj_id() {
            page_map.insert(id, index);
        }
    }

    let xref = pdf.xref();
    let Some(catalog) = xref.get::<Dict<'_>>(xref.root_id()) else {
        return Vec::new();
    };
    let Some(outlines) = catalog.get::<Dict<'_>>(OUTLINES) else {
        return Vec::new();
    };

    let mut items = Vec::new();
    let mut visited = HashSet::new();
    collect_outline_items(
        xref,
        outlines.get_ref(FIRST),
        0,
        &page_map,
        &mut items,
        &mut visited,
    );
    items
}

fn active_outline_index(outline: &[PdfOutlineItem], current_page: usize) -> Option<usize> {
    outline
        .iter()
        .enumerate()
        .rfind(|(_, item)| item.page_index <= current_page)
        .map(|(index, _)| index)
        .or_else(|| (!outline.is_empty()).then_some(0))
}

fn load_pdf_document(path: &Path) -> Result<Arc<CachedPdfDocument>, String> {
    let file = std::fs::read(path).map_err(|e| format!("Failed to read: {}", e))?;
    let pdf = Pdf::new(Arc::new(file)).map_err(|e| format!("Failed to parse PDF: {e:?}"))?;
    let total_pages = pdf.pages().len();
    if total_pages == 0 {
        return Err("PDF has no pages".to_string());
    }
    let outline = parse_pdf_outline(&pdf);
    Ok(Arc::new(CachedPdfDocument {
        pdf,
        total_pages,
        outline,
    }))
}

fn load_cached_pdf_document(ui: &mut Ui, path: &Path) -> PdfDocumentTaskState {
    let ctx = ui.ctx().clone();
    let task = with_pdf_preview_storage(ui, |storage| {
        storage
            .documents
            .entry(path.to_path_buf())
            .or_insert_with(|| PdfDocumentTask::spawn(&ctx, path))
            .clone()
    });

    let state = task.snapshot();
    if matches!(state, PdfDocumentTaskState::Loading) {
        ui.ctx().request_repaint_after(PDF_ASYNC_POLL_INTERVAL);
    }
    state
}

fn render_pdf_page_bitmap(
    _path: &Path,
    page_index: usize,
    zoom: f32,
    pixels_per_point: f32,
    doc: &CachedPdfDocument,
) -> Result<RenderedPdfBitmap, String> {
    let page = doc
        .pdf
        .pages()
        .get(page_index)
        .ok_or_else(|| format!("Page {} out of range", page_index + 1))?;

    let render_scale = zoom * pixels_per_point;
    let render_settings = RenderSettings {
        x_scale: render_scale,
        y_scale: render_scale,
        ..Default::default()
    };

    let interpreter_settings = InterpreterSettings::default();
    let pixmap = render(page, &interpreter_settings, &render_settings);
    let png_bytes = pixmap
        .into_png()
        .map_err(|e| format!("Failed to encode rendered page: {e:?}"))?;

    let img = image::load_from_memory(&png_bytes)
        .map_err(|e| format!("Failed to decode rendered page: {}", e))?;

    let rgba = img.to_rgba8();
    let (width, height) = rgba.dimensions();

    Ok(RenderedPdfBitmap {
        width,
        height,
        rgba: Arc::from(rgba.into_raw()),
        display_width: width as f32 / pixels_per_point,
        display_height: height as f32 / pixels_per_point,
    })
}

fn upload_pdf_texture(
    ctx: &egui::Context,
    path: &Path,
    page_index: usize,
    zoom: f32,
    pixels_per_point: f32,
    bitmap: &RenderedPdfBitmap,
) -> CachedPdfTexture {
    let color_image = ColorImage::from_rgba_unmultiplied(
        [bitmap.width as usize, bitmap.height as usize],
        bitmap.rgba.as_ref(),
    );

    let texture_name = format!(
        "pdf_preview_{}:{}:{:.2}:{:.2}",
        path.display(),
        page_index,
        zoom,
        pixels_per_point
    );
    let texture = ctx.load_texture(&texture_name, color_image, TextureOptions::LINEAR);

    CachedPdfTexture {
        texture,
        page_width: bitmap.width,
        page_height: bitmap.height,
        display_width: bitmap.display_width,
        display_height: bitmap.display_height,
    }
}

fn load_result_for_page(
    ui: &mut Ui,
    path: &Path,
    page_index: usize,
    zoom: f32,
    doc: &Arc<CachedPdfDocument>,
) -> PdfLoadResult {
    let pixels_per_point = ui.ctx().pixels_per_point();
    let key = page_render_key(path, page_index, zoom, pixels_per_point);
    let ctx = ui.ctx().clone();
    let doc = Arc::clone(doc);
    let task = with_pdf_preview_storage(ui, |storage| {
        storage
            .pages
            .entry(key.clone())
            .or_insert_with(|| {
                PdfPageTask::spawn(
                    &ctx,
                    path,
                    page_index,
                    zoom,
                    pixels_per_point,
                    Arc::clone(&doc),
                )
            })
            .clone()
    });

    match task.snapshot() {
        PdfPageTaskState::Loading => {
            ui.ctx().request_repaint_after(PDF_ASYNC_POLL_INTERVAL);
            PdfLoadResult::Loading
        }
        PdfPageTaskState::ReadyBitmap(bitmap) => {
            let texture =
                upload_pdf_texture(ui.ctx(), path, page_index, zoom, pixels_per_point, &bitmap);
            task.promote_loaded(texture.clone());
            PdfLoadResult::Loaded(texture)
        }
        PdfPageTaskState::Loaded(texture) => PdfLoadResult::Loaded(texture),
        PdfPageTaskState::Failed(msg) => PdfLoadResult::Failed(msg),
    }
}

fn estimated_page_size(ui: &mut Ui, path: &Path, page_index: usize, zoom: f32) -> (f32, f32) {
    let pixels_per_point = ui.ctx().pixels_per_point();
    let size = with_pdf_preview_storage(ui, |storage| {
        [page_index, 0].into_iter().find_map(|candidate| {
            storage
                .pages
                .get(&page_render_key(path, candidate, zoom, pixels_per_point))
                .and_then(PdfPageTask::dimensions)
        })
    });

    size.unwrap_or((FALLBACK_PAGE_WIDTH * zoom, FALLBACK_PAGE_HEIGHT * zoom))
}

fn render_page_image(ui: &mut Ui, cached_tex: &CachedPdfTexture) {
    let display_size = Vec2::new(cached_tex.display_width, cached_tex.display_height);
    let sized = egui::load::SizedTexture::new(cached_tex.texture.id(), display_size);
    ui.add(egui::Image::from_texture(sized));
}

fn render_thumbnail_image(ui: &mut Ui, cached_tex: &CachedPdfTexture) {
    let orig_w = cached_tex.page_width as f32;
    let orig_h = cached_tex.page_height as f32;
    let scale = (PDF_THUMBNAIL_WIDTH / orig_w)
        .min(PDF_THUMBNAIL_HEIGHT / orig_h)
        .min(1.0);
    let display_size = Vec2::new(orig_w * scale, orig_h * scale);
    let sized = egui::load::SizedTexture::new(cached_tex.texture.id(), display_size);
    ui.add(egui::Image::from_texture(sized));
}

fn render_page_placeholder(ui: &mut Ui, width: f32, height: f32, label: &str) {
    egui::Frame::none()
        .fill(Color32::from_rgb(248, 249, 251))
        .stroke(egui::Stroke::new(1.0, Color32::from_rgb(216, 220, 226)))
        .rounding(8.0)
        .show(ui, |ui| {
            ui.set_min_size(egui::vec2(width.max(120.0), height.max(160.0)));
            ui.vertical_centered(|ui| {
                ui.add_space((height * 0.35).clamp(24.0, 200.0));
                ui.spinner();
                ui.add_space(10.0);
                ui.label(
                    RichText::new(label)
                        .small()
                        .color(Color32::from_rgb(95, 105, 120)),
                );
                ui.add_space(12.0);
            });
        });
}

fn render_thumbnail_placeholder(ui: &mut Ui) {
    egui::Frame::none()
        .fill(Color32::from_rgb(248, 249, 251))
        .stroke(egui::Stroke::new(1.0, Color32::from_rgb(216, 220, 226)))
        .rounding(6.0)
        .show(ui, |ui| {
            ui.set_min_size(egui::vec2(PDF_THUMBNAIL_WIDTH, PDF_THUMBNAIL_HEIGHT));
            ui.vertical_centered(|ui| {
                ui.add_space(72.0);
                ui.spinner();
                ui.add_space(8.0);
                ui.label(RichText::new("Loading").small().weak());
            });
        });
}

fn render_toolbar(ui: &mut Ui, state: &mut PdfPreviewState, total_pages: usize) -> bool {
    let mut zoom_changed = false;
    sync_page_input(state);
    let colors = pdf_chrome_colors(ui);

    egui::Frame::none()
        .fill(colors.toolbar_fill)
        .stroke(Stroke::new(1.0, colors.border))
        .inner_margin(egui::Margin::symmetric(12.0, 10.0))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                let sidebar_btn = if state.sidebar_visible {
                    RichText::new("Sidebar").color(colors.selection_text)
                } else {
                    RichText::new("Sidebar").color(colors.text)
                };
                let sidebar_fill = if state.sidebar_visible {
                    colors.selection_fill
                } else {
                    colors.section_fill
                };
                if ui.add(egui::Button::new(sidebar_btn).fill(sidebar_fill)).clicked() {
                    state.sidebar_visible = !state.sidebar_visible;
                }

                ui.separator();

                ui.menu_button("View", |ui| {
                    ui.label(RichText::new("SPREAD MODE").strong().small());
                    ui.add_space(4.0);
                    ui.selectable_value(
                        &mut state.spread_mode,
                        PdfSpreadMode::SinglePage,
                        "Single Page",
                    );
                    ui.selectable_value(
                        &mut state.spread_mode,
                        PdfSpreadMode::TwoPageOdd,
                        "Two Page (Odd)",
                    );
                    ui.separator();
                    ui.label(RichText::new("SCROLL LAYOUT").strong().small());
                    ui.add_space(4.0);
                    ui.selectable_value(
                        &mut state.scroll_layout,
                        PdfScrollLayout::Vertical,
                        "Vertical",
                    );
                    ui.selectable_value(
                        &mut state.scroll_layout,
                        PdfScrollLayout::Horizontal,
                        "Horizontal",
                    );
                });

                ui.separator();

                egui::ComboBox::from_id_source("pdf_zoom_combo")
                    .selected_text(zoom_percent_label(state.zoom))
                    .show_ui(ui, |ui| {
                        for zoom in [0.5_f32, 0.75, 1.0, 1.25, 1.5, 2.0] {
                            if ui
                                .selectable_value(&mut state.zoom, zoom, zoom_percent_label(zoom))
                                .changed()
                            {
                                zoom_changed = true;
                            }
                        }
                    });

                if ui.button("−").clicked() {
                    state.zoom = (state.zoom - 0.25).max(0.5);
                    zoom_changed = true;
                }
                if ui.button("+").clicked() {
                    state.zoom = (state.zoom + 0.25).min(4.0);
                    zoom_changed = true;
                }

                ui.separator();
                egui::Frame::none()
                    .fill(colors.section_fill)
                    .stroke(Stroke::new(1.0, colors.border))
                    .rounding(8.0)
                    .inner_margin(egui::Margin::symmetric(8.0, 2.0))
                    .show(ui, |ui| {
                        ui.horizontal(|ui| {
                            let response = ui.add_sized(
                                egui::vec2(48.0, 22.0),
                                TextEdit::singleline(&mut state.page_input_buffer)
                                    .horizontal_align(egui::Align::Center),
                            );
                            if response.changed() || response.has_focus() {
                                state.page_input_editing = true;
                            }
                            let commit_requested = response.lost_focus()
                                || (response.has_focus()
                                    && ui.input(|i| i.key_pressed(egui::Key::Enter)));
                            if commit_requested {
                                commit_page_input(state, total_pages);
                                ui.memory_mut(|memory| memory.surrender_focus(response.id));
                            }
                            ui.label(
                                RichText::new(format!("/ {}", total_pages.max(1)))
                                    .strong()
                                    .color(colors.text),
                            );
                        });
                    });
            });
        });

    zoom_changed
}

fn clamp_sidebar_width(width: f32) -> f32 {
    width.clamp(PDF_SIDEBAR_MIN_WIDTH, PDF_SIDEBAR_MAX_WIDTH)
}

fn update_current_page_from_vertical_viewport(
    state: &mut PdfPreviewState,
    viewport: &egui::Rect,
    row_height: f32,
    total_pages: usize,
    pages_per_row: usize,
) {
    if total_pages == 0 || row_height <= 0.0 {
        return;
    }

    let center_row = (viewport.center().y / row_height).floor().max(0.0) as usize;
    let page_index = center_row
        .saturating_mul(pages_per_row)
        .min(total_pages.saturating_sub(1));
    state.current_page = page_index;
}

fn update_current_page_from_horizontal_viewport(
    state: &mut PdfPreviewState,
    viewport: &egui::Rect,
    col_width: f32,
    total_pages: usize,
) {
    if total_pages == 0 || col_width <= 0.0 {
        return;
    }

    let center_col = (viewport.center().x / col_width).floor().max(0.0) as usize;
    state.current_page = center_col.min(total_pages.saturating_sub(1));
}

fn render_continuous_vertical(
    ui: &mut Ui,
    path: &Path,
    total_pages: usize,
    zoom: f32,
    doc: &Arc<CachedPdfDocument>,
    state: &mut PdfPreviewState,
    drag_to_scroll: bool,
) {
    let (_, estimated_h) = estimated_page_size(ui, path, state.current_page, zoom);
    let row_height = estimated_h + PAGE_GAP;

    egui::ScrollArea::both()
        .auto_shrink([false, false])
        .drag_to_scroll(drag_to_scroll)
        .show_viewport(ui, |ui, viewport| {
            if let Some(target_page) = state.pending_jump_to_page {
                let target_y = (target_page as f32) * row_height;
                let jump_rect = egui::Rect::from_min_size(
                    egui::pos2(0.0, target_y),
                    egui::vec2(ui.available_width().max(1.0), row_height.max(1.0)),
                );
                ui.scroll_to_rect(jump_rect, Some(egui::Align::Center));
                state.current_page = target_page.min(total_pages.saturating_sub(1));
                state.pending_jump_to_page = None;
            }
            update_current_page_from_vertical_viewport(state, &viewport, row_height, total_pages, 1);
            let first_visible = ((viewport.min.y / row_height).floor().max(0.0) as usize)
                .saturating_sub(VIEWPORT_BUFFER_PAGES);
            let last_visible = (((viewport.max.y / row_height).ceil().max(0.0)) as usize
                + VIEWPORT_BUFFER_PAGES)
                .min(total_pages);

            ui.add_space(first_visible as f32 * row_height);

            for page_index in first_visible..last_visible {
                let (page_w, page_h) = estimated_page_size(ui, path, page_index, zoom);
                match load_result_for_page(ui, path, page_index, zoom, doc) {
                    PdfLoadResult::Loading => {
                        ui.vertical_centered(|ui| {
                            render_page_placeholder(ui, page_w, page_h, "Rendering page...");
                        });
                        ui.add_space(PAGE_GAP);
                    }
                    PdfLoadResult::Loaded(cached_tex) => {
                        ui.vertical_centered(|ui| {
                            render_page_image(ui, &cached_tex);
                        });
                        ui.add_space(PAGE_GAP);
                    }
                    PdfLoadResult::Failed(msg) => {
                        ui.label(format!("Page {} failed: {}", page_index + 1, msg));
                        ui.add_space(12.0);
                    }
                }
            }

            ui.add_space((total_pages.saturating_sub(last_visible)) as f32 * row_height);
        });
}

fn render_continuous_two_column(
    ui: &mut Ui,
    path: &Path,
    total_pages: usize,
    zoom: f32,
    doc: &Arc<CachedPdfDocument>,
    state: &mut PdfPreviewState,
    drag_to_scroll: bool,
) {
    let (_, estimated_h) = estimated_page_size(ui, path, state.current_page, zoom);
    let row_height = estimated_h + PAGE_GAP;
    let total_rows = total_pages.div_ceil(2);

    egui::ScrollArea::both()
        .auto_shrink([false, false])
        .drag_to_scroll(drag_to_scroll)
        .show_viewport(ui, |ui, viewport| {
            if let Some(target_page) = state.pending_jump_to_page {
                let target_row = target_page / 2;
                let target_y = (target_row as f32) * row_height;
                let jump_rect = egui::Rect::from_min_size(
                    egui::pos2(0.0, target_y),
                    egui::vec2(ui.available_width().max(1.0), row_height.max(1.0)),
                );
                ui.scroll_to_rect(jump_rect, Some(egui::Align::Center));
                state.current_page = target_page.min(total_pages.saturating_sub(1));
                state.pending_jump_to_page = None;
            }
            update_current_page_from_vertical_viewport(state, &viewport, row_height, total_pages, 2);
            let first_row = ((viewport.min.y / row_height).floor().max(0.0) as usize)
                .saturating_sub(VIEWPORT_BUFFER_PAGES);
            let last_row = (((viewport.max.y / row_height).ceil().max(0.0)) as usize
                + VIEWPORT_BUFFER_PAGES)
                .min(total_rows);

            ui.add_space(first_row as f32 * row_height);

            let mut page_index = first_row * 2;
            while page_index < (last_row * 2).min(total_pages) {
                ui.horizontal_top(|ui| {
                    for col in 0..2 {
                        let current_index = page_index + col;
                        if current_index < total_pages {
                            let (page_w, page_h) =
                                estimated_page_size(ui, path, current_index, zoom);
                            match load_result_for_page(ui, path, current_index, zoom, doc) {
                                PdfLoadResult::Loading => {
                                    ui.vertical(|ui| {
                                        render_page_placeholder(
                                            ui,
                                            page_w,
                                            page_h,
                                            "Rendering page...",
                                        );
                                    });
                                }
                                PdfLoadResult::Loaded(cached_tex) => {
                                    ui.vertical(|ui| {
                                        render_page_image(ui, &cached_tex);
                                    });
                                }
                                PdfLoadResult::Failed(msg) => {
                                    ui.vertical(|ui| {
                                        ui.label(format!(
                                            "Page {} failed: {}",
                                            current_index + 1,
                                            msg
                                        ));
                                    });
                                }
                            }
                        } else {
                            ui.add_space(24.0);
                        }

                        if col == 0 {
                            ui.add_space(PAGE_GAP);
                        }
                    }
                });
                ui.add_space(PAGE_GAP);
                page_index += 2;
            }

            ui.add_space((total_rows.saturating_sub(last_row)) as f32 * row_height);
        });
}

fn render_continuous_horizontal(
    ui: &mut Ui,
    path: &Path,
    total_pages: usize,
    zoom: f32,
    doc: &Arc<CachedPdfDocument>,
    state: &mut PdfPreviewState,
    drag_to_scroll: bool,
) {
    let (estimated_w, _) = estimated_page_size(ui, path, state.current_page, zoom);
    let col_width = estimated_w + PAGE_GAP;

    egui::ScrollArea::both()
        .auto_shrink([false, false])
        .drag_to_scroll(drag_to_scroll)
        .show_viewport(ui, |ui, viewport| {
            if let Some(target_page) = state.pending_jump_to_page {
                let target_x = (target_page as f32) * col_width;
                let jump_rect = egui::Rect::from_min_size(
                    egui::pos2(target_x, 0.0),
                    egui::vec2(col_width.max(1.0), ui.available_height().max(1.0)),
                );
                ui.scroll_to_rect(jump_rect, Some(egui::Align::Center));
                state.current_page = target_page.min(total_pages.saturating_sub(1));
                state.pending_jump_to_page = None;
            }
            update_current_page_from_horizontal_viewport(state, &viewport, col_width, total_pages);
            let first_visible = ((viewport.min.x / col_width).floor().max(0.0) as usize)
                .saturating_sub(VIEWPORT_BUFFER_PAGES);
            let last_visible = (((viewport.max.x / col_width).ceil().max(0.0)) as usize
                + VIEWPORT_BUFFER_PAGES)
                .min(total_pages);

            ui.horizontal(|ui| {
                ui.add_space(first_visible as f32 * col_width);

                for page_index in first_visible..last_visible {
                    let (page_w, page_h) = estimated_page_size(ui, path, page_index, zoom);
                    match load_result_for_page(ui, path, page_index, zoom, doc) {
                        PdfLoadResult::Loading => {
                            ui.vertical(|ui| {
                                render_page_placeholder(ui, page_w, page_h, "Rendering page...");
                            });
                            ui.add_space(PAGE_GAP);
                        }
                        PdfLoadResult::Loaded(cached_tex) => {
                            ui.vertical(|ui| {
                                render_page_image(ui, &cached_tex);
                            });
                            ui.add_space(PAGE_GAP);
                        }
                        PdfLoadResult::Failed(msg) => {
                            ui.label(format!("Page {} failed: {}", page_index + 1, msg));
                            ui.add_space(12.0);
                        }
                    }
                }

                ui.add_space((total_pages.saturating_sub(last_visible)) as f32 * col_width);
            });
        });
}

fn render_sidebar(
    ui: &mut Ui,
    path: &Path,
    total_pages: usize,
    state: &mut PdfPreviewState,
    doc: &Arc<CachedPdfDocument>,
) {
    let sidebar_width = clamp_sidebar_width(state.sidebar_width);
    let colors = pdf_chrome_colors(ui);
    ui.set_width(sidebar_width);
    ui.set_min_width(sidebar_width);
    ui.set_max_width(sidebar_width);
    egui::Frame::none()
        .fill(colors.sidebar_fill)
        .stroke(Stroke::new(1.0, colors.border))
        .inner_margin(egui::Margin::symmetric(12.0, 12.0))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.selectable_value(&mut state.sidebar_mode, PdfSidebarMode::Thumbnails, "Pages");
                ui.selectable_value(&mut state.sidebar_mode, PdfSidebarMode::Outline, "Outline");
            });

            ui.add_space(12.0);

            match state.sidebar_mode {
                PdfSidebarMode::Thumbnails => {
                    egui::ScrollArea::vertical().show_viewport(ui, |ui, viewport| {
                        let first_visible =
                            ((viewport.min.y / PDF_THUMBNAIL_ROW_HEIGHT).floor().max(0.0) as usize)
                                .saturating_sub(SIDEBAR_BUFFER_ROWS);
                        let last_visible = (((viewport.max.y / PDF_THUMBNAIL_ROW_HEIGHT)
                            .ceil()
                            .max(0.0)) as usize
                            + SIDEBAR_BUFFER_ROWS)
                            .min(total_pages);

                        ui.add_space(first_visible as f32 * PDF_THUMBNAIL_ROW_HEIGHT);

                        for page_index in first_visible..last_visible {
                            let selected = state.current_page == page_index;
                            let frame_fill = if selected {
                                colors.selection_fill
                            } else {
                                colors.section_fill
                            };
                            let text_color = if selected {
                                colors.selection_text
                            } else {
                                colors.text
                            };

                            let stroke = if selected {
                                Stroke::new(2.0, colors.selection_fill.gamma_multiply(1.15))
                            } else {
                                Stroke::new(1.0, colors.border)
                            };

                            egui::Frame::none()
                                .fill(frame_fill)
                                .stroke(stroke)
                                .rounding(10.0)
                                .inner_margin(egui::Margin::same(10.0))
                                .show(ui, |ui| {
                                    ui.vertical_centered(|ui| {
                                        let button = egui::Button::new("")
                                            .fill(Color32::from_rgb(248, 249, 251))
                                            .stroke(egui::Stroke::new(
                                                1.0,
                                                Color32::from_rgb(210, 214, 220),
                                            ))
                                            .min_size(egui::vec2(
                                                PDF_THUMBNAIL_WIDTH,
                                                PDF_THUMBNAIL_HEIGHT,
                                            ));

                                        let response = ui.add(button);
                                        let thumb_rect =
                                            response.rect.shrink2(egui::vec2(8.0, 8.0));
                                        ui.allocate_ui_at_rect(thumb_rect, |ui| {
                                            ui.centered_and_justified(|ui| {
                                                match load_result_for_page(
                                                    ui, path, page_index, 0.2, doc,
                                                ) {
                                                    PdfLoadResult::Loading => {
                                                        render_thumbnail_placeholder(ui);
                                                    }
                                                    PdfLoadResult::Loaded(cached_tex) => {
                                                        render_thumbnail_image(ui, &cached_tex);
                                                    }
                                                    PdfLoadResult::Failed(_) => {
                                                        ui.label(
                                                            RichText::new("Failed").small().weak(),
                                                        );
                                                    }
                                                }
                                            });
                                        });

                                        if response.clicked() {
                                            jump_to_page(state, page_index, total_pages);
                                        }
                                        if selected {
                                            response.scroll_to_me(Some(egui::Align::Center));
                                        }

                                        ui.add_space(8.0);
                                        ui.label(
                                            RichText::new((page_index + 1).to_string())
                                                .small()
                                                .color(text_color),
                                        );
                                    });
                                });
                            ui.add_space(12.0);
                        }

                        ui.add_space(
                            (total_pages.saturating_sub(last_visible)) as f32
                                * PDF_THUMBNAIL_ROW_HEIGHT,
                        );
                    });
                }
                PdfSidebarMode::Outline => {
                    egui::ScrollArea::vertical().show(ui, |ui| {
                        ui.label(
                            RichText::new("Outline")
                                .strong()
                                .color(colors.text),
                        );
                        ui.add_space(8.0);
                        if doc.outline.is_empty() {
                            ui.label(
                                RichText::new("No outline")
                                    .small()
                                    .color(colors.muted_text),
                            );
                        } else {
                            let active_index =
                                active_outline_index(&doc.outline, state.current_page);
                            for (index, item) in doc.outline.iter().enumerate() {
                                let selected = active_index == Some(index);
                                let response = outline_row(ui, &item.title, selected, &colors);
                                if response.clicked() {
                                    jump_to_page(state, item.page_index, total_pages);
                                }
                                if selected {
                                    response.scroll_to_me(Some(egui::Align::Center));
                                }
                                ui.add_space(6.0);
                            }
                        }
                    });
                }
            }
        });
}

fn render_loading_screen(ui: &mut Ui, path: &Path) {
    ui.ctx().request_repaint_after(PDF_ASYNC_POLL_INTERVAL);
    ui.vertical_centered(|ui| {
        ui.add_space(48.0);
        ui.spinner();
        ui.add_space(12.0);
        ui.label(RichText::new("Loading PDF preview...").strong());
        ui.add_space(6.0);
        ui.label(RichText::new(path.display().to_string()).small().weak());
    });
}

/// Render a PDF preview for a top-level PDF document tab.
pub fn render_pdf_preview(ui: &mut Ui, path: &Path, state: &mut PdfPreviewState) {
    let doc = match load_cached_pdf_document(ui, path) {
        PdfDocumentTaskState::Loading => {
            state.total_pages = None;
            render_loading_screen(ui, path);
            return;
        }
        PdfDocumentTaskState::Loaded(doc) => doc,
        PdfDocumentTaskState::Failed(msg) => {
            state.total_pages = None;
            ui.vertical_centered(|ui| {
                ui.add_space(24.0);
                ui.label(RichText::new("PDF preview unavailable").strong());
                ui.add_space(8.0);
                ui.label(msg);
                ui.add_space(8.0);
                ui.label(path.display().to_string());
            });
            return;
        }
    };

    let total_pages = doc.total_pages;
    state.current_page = state.current_page.min(total_pages.saturating_sub(1));
    state.total_pages = Some(total_pages);
    sync_page_input(state);

    let mut zoom_changed = render_toolbar(ui, state, total_pages);
    if zoom_changed {
        state.pending_zoom_factor = 1.0;
        state.last_zoom_input_at = None;
        state.zoom = quantize_zoom(state.zoom);
    }

    let gesture_zoom = ui.input(|i| i.zoom_delta());
    if (gesture_zoom - 1.0).abs() > PDF_ZOOM_EPSILON {
        state.pending_zoom_factor *= gesture_zoom;
        state.last_zoom_input_at = Some(Instant::now());
        ui.ctx().request_repaint_after(PDF_ZOOM_DEBOUNCE);
    }
    zoom_changed |= flush_pending_zoom(ui, state, false);

    let _ = zoom_changed;
    ui.add_space(8.0);
    let drag_to_scroll = true;
    state.sidebar_width = clamp_sidebar_width(state.sidebar_width);

    let body_size = ui.available_size_before_wrap();
    ui.allocate_ui_with_layout(
        body_size,
        egui::Layout::left_to_right(egui::Align::Min),
        |ui| {
            if state.sidebar_visible {
                let sidebar_width = clamp_sidebar_width(state.sidebar_width);
                ui.allocate_ui_with_layout(
                    egui::vec2(sidebar_width, body_size.y),
                    egui::Layout::top_down(egui::Align::Min),
                    |ui| {
                        ui.set_width(sidebar_width);
                        render_sidebar(ui, path, total_pages, state, &doc)
                    },
                );

                let (handle_rect, handle_response) = ui.allocate_exact_size(
                    egui::vec2(PDF_SIDEBAR_RESIZE_HANDLE_WIDTH, body_size.y),
                    egui::Sense::drag(),
                );
                if handle_response.hovered() || handle_response.dragged() {
                    ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeHorizontal);
                }
                if handle_response.dragged() {
                    let delta_x = ui.input(|i| i.pointer.delta().x);
                    state.sidebar_width = clamp_sidebar_width(state.sidebar_width + delta_x);
                }
                ui.painter().rect_filled(
                    handle_rect.shrink2(egui::vec2(2.0, 0.0)),
                    0.0,
                    pdf_chrome_colors(ui).handle,
                );
            }

            let main_width = if state.sidebar_visible {
                (body_size.x - clamp_sidebar_width(state.sidebar_width) - PDF_SIDEBAR_RESIZE_HANDLE_WIDTH)
                    .max(0.0)
            } else {
                body_size.x
            };

            ui.allocate_ui_with_layout(
                egui::vec2(main_width, body_size.y),
                egui::Layout::top_down(egui::Align::Min),
                |ui| match (state.spread_mode, state.scroll_layout) {
                    (PdfSpreadMode::SinglePage, PdfScrollLayout::Vertical) => {
                        render_continuous_vertical(
                            ui,
                            path,
                            total_pages,
                            state.zoom,
                            &doc,
                            state,
                            drag_to_scroll,
                        );
                    }
                    (PdfSpreadMode::SinglePage, PdfScrollLayout::Horizontal) => {
                        render_continuous_horizontal(
                            ui,
                            path,
                            total_pages,
                            state.zoom,
                            &doc,
                            state,
                            drag_to_scroll,
                        );
                    }
                    (PdfSpreadMode::TwoPageOdd, PdfScrollLayout::Vertical) => {
                        render_continuous_two_column(
                            ui,
                            path,
                            total_pages,
                            state.zoom,
                            &doc,
                            state,
                            drag_to_scroll,
                        );
                    }
                    (PdfSpreadMode::TwoPageOdd, PdfScrollLayout::Horizontal) => {
                        render_continuous_horizontal(
                            ui,
                            path,
                            total_pages,
                            state.zoom,
                            &doc,
                            state,
                            drag_to_scroll,
                        );
                    }
                },
            );
        },
    );
}

#[cfg(test)]
mod tests {
    use super::{outline_row_height, requested_page_index};

    #[test]
    fn requested_page_index_trims_and_converts_to_zero_based_page() {
        assert_eq!(requested_page_index(" 7 ", 12), Some(6));
    }

    #[test]
    fn requested_page_index_clamps_to_document_bounds() {
        assert_eq!(requested_page_index("0", 8), Some(0));
        assert_eq!(requested_page_index("99", 8), Some(7));
    }

    #[test]
    fn requested_page_index_rejects_non_numeric_input() {
        assert_eq!(requested_page_index("", 8), None);
        assert_eq!(requested_page_index("abc", 8), None);
    }

    #[test]
    fn outline_row_height_scales_with_font_size_but_keeps_single_line_minimum() {
        assert_eq!(outline_row_height(10.0), 28.0);
        assert!(outline_row_height(18.0) > outline_row_height(12.0));
    }
}

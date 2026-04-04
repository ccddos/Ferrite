//! Standalone PDF preview rendering for PDF document tabs.
//!
//! This is a best-effort read-only viewer built on `hayro`. It renders pages
//! to bitmaps and uploads them as egui textures. Continuous modes only rasterize
//! pages near the current viewport to keep scrolling and zooming responsive.

use eframe::egui::{self, Color32, ColorImage, RichText, TextureHandle, TextureOptions, Ui, Vec2};
use hayro::hayro_interpret::InterpreterSettings;
use hayro::hayro_syntax::Pdf;
use hayro::{render, RenderSettings};
use std::path::Path;
use std::sync::Arc;

use super::pdf::{PdfScrollLayout, PdfSidebarMode, PdfSpreadMode, PdfViewStateSnapshot};

const PAGE_GAP: f32 = 24.0;
const VIEWPORT_BUFFER_PAGES: usize = 1;
const PDF_SIDEBAR_WIDTH: f32 = 220.0;
const PDF_THUMBNAIL_WIDTH: f32 = 150.0;
const PDF_THUMBNAIL_HEIGHT: f32 = 210.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PdfMouseMode {
    Pan,
    Cursor,
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
    pub mouse_mode: PdfMouseMode,
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
            mouse_mode: PdfMouseMode::Pan,
        }
    }
}

impl PdfPreviewState {
    pub fn to_snapshot(&self) -> PdfViewStateSnapshot {
        PdfViewStateSnapshot {
            current_page: self.current_page,
            zoom_percent: (self.zoom * 100.0).round() as u16,
            spread_mode: self.spread_mode,
            scroll_layout: self.scroll_layout,
            sidebar_visible: self.sidebar_visible,
            sidebar_mode: self.sidebar_mode,
        }
    }

    pub fn from_snapshot(snapshot: PdfViewStateSnapshot) -> Self {
        Self {
            current_page: snapshot.current_page,
            zoom: (snapshot.zoom_percent as f32 / 100.0).clamp(0.5, 4.0),
            total_pages: None,
            spread_mode: snapshot.spread_mode,
            scroll_layout: snapshot.scroll_layout,
            sidebar_visible: snapshot.sidebar_visible,
            sidebar_mode: snapshot.sidebar_mode,
            mouse_mode: PdfMouseMode::Pan,
        }
    }
}

#[derive(Clone)]
struct CachedPdfTexture {
    texture: TextureHandle,
    page_width: u32,
    page_height: u32,
}

struct CachedPdfDocument {
    pdf: Pdf,
    total_pages: usize,
}

#[derive(Clone)]
enum PdfLoadResult {
    Loaded(CachedPdfTexture),
    Failed(String),
}

fn zoom_percent_label(zoom: f32) -> String {
    format!("{:.0}%", zoom * 100.0)
}

fn load_pdf_document(path: &Path) -> Result<Arc<CachedPdfDocument>, String> {
    let file = std::fs::read(path).map_err(|e| format!("Failed to read: {}", e))?;
    let pdf = Pdf::new(Arc::new(file)).map_err(|e| format!("Failed to parse PDF: {e:?}"))?;
    let total_pages = pdf.pages().len();
    if total_pages == 0 {
        return Err("PDF has no pages".to_string());
    }
    Ok(Arc::new(CachedPdfDocument { pdf, total_pages }))
}

fn load_cached_pdf_document(ui: &mut Ui, path: &Path) -> Result<Arc<CachedPdfDocument>, String> {
    let cache_id = egui::Id::new("pdf_document_cache").with(path);
    if let Some(doc) = ui.data(|d| d.get_temp::<Arc<CachedPdfDocument>>(cache_id)) {
        return Ok(doc);
    }

    let doc = load_pdf_document(path)?;
    ui.data_mut(|d| d.insert_temp(cache_id, doc.clone()));
    Ok(doc)
}

fn render_pdf_page(
    ctx: &egui::Context,
    path: &Path,
    page_index: usize,
    zoom: f32,
    doc: &CachedPdfDocument,
) -> Result<CachedPdfTexture, String> {
    let page = doc
        .pdf
        .pages()
        .get(page_index)
        .ok_or_else(|| format!("Page {} out of range", page_index + 1))?;

    let render_settings = RenderSettings {
        x_scale: zoom,
        y_scale: zoom,
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
    let pixels: Vec<Color32> = rgba
        .pixels()
        .map(|p| Color32::from_rgba_unmultiplied(p[0], p[1], p[2], p[3]))
        .collect();

    let color_image = ColorImage {
        size: [width as usize, height as usize],
        pixels,
    };

    let texture_name = format!("pdf_preview_{}:{}:{:.2}", path.display(), page_index, zoom);
    let texture = ctx.load_texture(&texture_name, color_image, TextureOptions::LINEAR);

    Ok(CachedPdfTexture {
        texture,
        page_width: width,
        page_height: height,
    })
}

fn load_result_for_page(
    ui: &mut Ui,
    path: &Path,
    page_index: usize,
    zoom: f32,
    doc: &CachedPdfDocument,
) -> PdfLoadResult {
    let cache_id = egui::Id::new("pdf_preview_cache").with((
        path.to_path_buf(),
        page_index,
        (zoom * 100.0).round() as i32,
    ));

    if let Some(cached) = ui.data(|d| d.get_temp(cache_id)) {
        return cached;
    }

    let result = match render_pdf_page(ui.ctx(), path, page_index, zoom, doc) {
        Ok(tex) => PdfLoadResult::Loaded(tex),
        Err(msg) => PdfLoadResult::Failed(msg),
    };
    ui.data_mut(|d| d.insert_temp(cache_id, result.clone()));
    result
}

fn render_page_image(ui: &mut Ui, cached_tex: &CachedPdfTexture) {
    let display_size = Vec2::new(cached_tex.page_width as f32, cached_tex.page_height as f32);
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

fn estimated_page_size(ui: &mut Ui, path: &Path, zoom: f32, doc: &CachedPdfDocument) -> (f32, f32) {
    match load_result_for_page(ui, path, 0, zoom, doc) {
        PdfLoadResult::Loaded(tex) => (tex.page_width as f32, tex.page_height as f32),
        PdfLoadResult::Failed(_) => (900.0 * zoom, 1200.0 * zoom),
    }
}

fn render_toolbar(ui: &mut Ui, state: &mut PdfPreviewState, total_pages: usize) -> bool {
    let mut zoom_changed = false;

    egui::Frame::none()
        .fill(Color32::from_rgb(33, 41, 54))
        .inner_margin(egui::Margin::symmetric(12.0, 10.0))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                let sidebar_btn = if state.sidebar_visible { "▣" } else { "☰" };
                if ui.button(sidebar_btn).clicked() {
                    state.sidebar_visible = !state.sidebar_visible;
                }

                ui.separator();

                ui.menu_button("View", |ui| {
                    ui.label(RichText::new("SPREAD MODE").strong().small());
                    ui.add_space(4.0);
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
                ui.label(
                    RichText::new(format!(
                        "{} / {}",
                        state.current_page.saturating_add(1).min(total_pages.max(1)),
                        total_pages.max(1)
                    ))
                    .strong(),
                );

                ui.separator();

                ui.selectable_value(&mut state.mouse_mode, PdfMouseMode::Pan, "✋");
                ui.selectable_value(&mut state.mouse_mode, PdfMouseMode::Cursor, "↖");
            });
        });

    zoom_changed
}

fn render_continuous_vertical(
    ui: &mut Ui,
    path: &Path,
    total_pages: usize,
    zoom: f32,
    doc: &CachedPdfDocument,
    drag_to_scroll: bool,
) {
    let (_, estimated_h) = estimated_page_size(ui, path, zoom, doc);
    let row_height = estimated_h + PAGE_GAP;

    egui::ScrollArea::both()
        .auto_shrink([false, false])
        .drag_to_scroll(drag_to_scroll)
        .show_viewport(ui, |ui, viewport| {
            let first_visible = ((viewport.min.y / row_height).floor().max(0.0) as usize)
                .saturating_sub(VIEWPORT_BUFFER_PAGES);
            let last_visible = (((viewport.max.y / row_height).ceil().max(0.0)) as usize
                + VIEWPORT_BUFFER_PAGES)
                .min(total_pages);

            ui.add_space(first_visible as f32 * row_height);

            for page_index in first_visible..last_visible {
                match load_result_for_page(ui, path, page_index, zoom, doc) {
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
    doc: &CachedPdfDocument,
    drag_to_scroll: bool,
) {
    let (_, estimated_h) = estimated_page_size(ui, path, zoom, doc);
    let row_height = estimated_h + PAGE_GAP;
    let total_rows = total_pages.div_ceil(2);

    egui::ScrollArea::both()
        .auto_shrink([false, false])
        .drag_to_scroll(drag_to_scroll)
        .show_viewport(ui, |ui, viewport| {
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
                            match load_result_for_page(ui, path, current_index, zoom, doc) {
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
    doc: &CachedPdfDocument,
    drag_to_scroll: bool,
) {
    let (estimated_w, _) = estimated_page_size(ui, path, zoom, doc);
    let col_width = estimated_w + PAGE_GAP;

    egui::ScrollArea::both()
        .auto_shrink([false, false])
        .drag_to_scroll(drag_to_scroll)
        .show_viewport(ui, |ui, viewport| {
            let first_visible = ((viewport.min.x / col_width).floor().max(0.0) as usize)
                .saturating_sub(VIEWPORT_BUFFER_PAGES);
            let last_visible = (((viewport.max.x / col_width).ceil().max(0.0)) as usize
                + VIEWPORT_BUFFER_PAGES)
                .min(total_pages);

            ui.horizontal(|ui| {
                ui.add_space(first_visible as f32 * col_width);

                for page_index in first_visible..last_visible {
                    match load_result_for_page(ui, path, page_index, zoom, doc) {
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
    doc: &CachedPdfDocument,
) {
    egui::Frame::none()
        .fill(Color32::from_rgb(33, 41, 54))
        .inner_margin(egui::Margin::symmetric(12.0, 12.0))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.selectable_value(&mut state.sidebar_mode, PdfSidebarMode::Thumbnails, "⧉");
                ui.selectable_value(&mut state.sidebar_mode, PdfSidebarMode::Outline, "☷");
            });

            ui.add_space(12.0);

            match state.sidebar_mode {
                PdfSidebarMode::Thumbnails => {
                    egui::ScrollArea::vertical().show(ui, |ui| {
                        for page_index in 0..total_pages {
                            match load_result_for_page(ui, path, page_index, 0.2, doc) {
                                PdfLoadResult::Loaded(cached_tex) => {
                                    let selected = state.current_page == page_index;
                                    let frame_fill = if selected {
                                        Color32::from_rgb(88, 156, 255)
                                    } else {
                                        Color32::from_rgb(39, 48, 61)
                                    };
                                    let text_color = if selected {
                                        Color32::WHITE
                                    } else {
                                        Color32::from_rgb(210, 214, 220)
                                    };

                                    let stroke = if selected {
                                        egui::Stroke::new(2.0, Color32::from_rgb(120, 180, 255))
                                    } else {
                                        egui::Stroke::new(1.0, Color32::from_rgb(58, 68, 84))
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
                                                        render_thumbnail_image(ui, &cached_tex);
                                                    });
                                                });

                                                if response.clicked() {
                                                    state.current_page = page_index;
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
                                PdfLoadResult::Failed(_) => {}
                            }
                        }
                    });
                }
                PdfSidebarMode::Outline => {
                    egui::ScrollArea::vertical().show(ui, |ui| {
                        ui.label(
                            RichText::new("Outline")
                                .strong()
                                .color(Color32::from_rgb(210, 214, 220)),
                        );
                        ui.add_space(8.0);
                        for page_index in 0..total_pages {
                            let selected = state.current_page == page_index;
                            if ui
                                .selectable_label(selected, format!("Page {}", page_index + 1))
                                .clicked()
                            {
                                state.current_page = page_index;
                            }
                        }
                    });
                }
            }
        });
}

/// Render a PDF preview for a top-level PDF document tab.
pub fn render_pdf_preview(ui: &mut Ui, path: &Path, state: &mut PdfPreviewState) {
    let doc = match load_cached_pdf_document(ui, path) {
        Ok(doc) => doc,
        Err(msg) => {
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
    state.total_pages = Some(total_pages);

    let mut zoom_changed = render_toolbar(ui, state, total_pages);

    let gesture_zoom = ui.input(|i| i.zoom_delta());
    if (gesture_zoom - 1.0).abs() > f32::EPSILON {
        log::debug!("PDF gesture zoom delta: {}", gesture_zoom);
        state.zoom = (state.zoom * gesture_zoom).clamp(0.5, 4.0);
        zoom_changed = true;
    }

    let _ = zoom_changed;
    ui.add_space(8.0);
    let drag_to_scroll = matches!(state.mouse_mode, PdfMouseMode::Pan);

    let body_size = ui.available_size_before_wrap();
    ui.allocate_ui_with_layout(
        body_size,
        egui::Layout::left_to_right(egui::Align::Min),
        |ui| {
            if state.sidebar_visible {
                ui.allocate_ui_with_layout(
                    egui::vec2(PDF_SIDEBAR_WIDTH, body_size.y),
                    egui::Layout::top_down(egui::Align::Min),
                    |ui| render_sidebar(ui, path, total_pages, state, &doc),
                );
                ui.add_space(12.0);
            }

            let main_width = if state.sidebar_visible {
                (body_size.x - PDF_SIDEBAR_WIDTH - 12.0).max(0.0)
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
                            drag_to_scroll,
                        );
                    }
                },
            );
        },
    );
}

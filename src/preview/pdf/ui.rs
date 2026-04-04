use eframe::egui::{self, Color32, ColorImage, RichText, TextureHandle, TextureOptions, Ui, Vec2};
use std::collections::HashMap;
use std::path::Path;

use super::model::{PdfMouseMode, PdfPreviewState, PdfScrollLayout, PdfSidebarMode, PdfSpreadMode};
use super::runtime::{
    thumbnail_zoom_bucket, with_pdf_runtime, PageRenderKey, PdfDocumentLoadState,
    PdfDocumentMetadata, RenderedPdfBitmap,
};

const PAGE_GAP: f32 = 24.0;
const VIEWPORT_BUFFER_PAGES: usize = 1;
const SIDEBAR_BUFFER_ROWS: usize = 2;
const PDF_SIDEBAR_WIDTH: f32 = 220.0;
const PDF_THUMBNAIL_WIDTH: f32 = 150.0;
const PDF_THUMBNAIL_HEIGHT: f32 = 210.0;
const PDF_THUMBNAIL_ROW_HEIGHT: f32 = PDF_THUMBNAIL_HEIGHT + 58.0;
const FALLBACK_PAGE_WIDTH: f32 = 900.0;
const FALLBACK_PAGE_HEIGHT: f32 = 1200.0;
const PDF_ASYNC_POLL_INTERVAL: std::time::Duration = std::time::Duration::from_millis(16);

#[derive(Clone)]
struct CachedPdfTexture {
    texture: TextureHandle,
    page_width: u32,
    page_height: u32,
}

#[derive(Clone, Default)]
struct PdfTextureCache {
    textures: HashMap<PageRenderKey, CachedPdfTexture>,
}

#[derive(Clone)]
enum PdfLoadResult {
    Loading,
    Loaded(CachedPdfTexture),
    Failed(String),
}

fn with_pdf_texture_cache<R>(ui: &mut Ui, f: impl FnOnce(&mut PdfTextureCache) -> R) -> R {
    ui.data_mut(|data| {
        let cache =
            data.get_temp_mut_or_default::<PdfTextureCache>(egui::Id::new("pdf_texture_cache"));
        f(cache)
    })
}

fn zoom_percent_label(zoom: f32) -> String {
    format!("{:.0}%", zoom * 100.0)
}

fn upload_pdf_texture(
    ctx: &egui::Context,
    key: &PageRenderKey,
    bitmap: &RenderedPdfBitmap,
) -> CachedPdfTexture {
    let color_image = ColorImage::from_rgba_unmultiplied(
        [bitmap.width as usize, bitmap.height as usize],
        bitmap.rgba.as_ref(),
    );
    let texture_name = format!(
        "pdf_preview_{}:{}:{}",
        key.path.display(),
        key.page_index,
        key.zoom_bucket
    );
    let texture = ctx.load_texture(&texture_name, color_image, TextureOptions::LINEAR);

    CachedPdfTexture {
        texture,
        page_width: bitmap.width,
        page_height: bitmap.height,
    }
}

fn load_document_metadata(ui: &mut Ui, path: &Path) -> PdfDocumentLoadState {
    let state = with_pdf_runtime(ui.ctx(), |runtime| runtime.document_state(path));
    if matches!(state, PdfDocumentLoadState::Loading) {
        ui.ctx().request_repaint_after(PDF_ASYNC_POLL_INTERVAL);
    }
    state
}

fn load_result_for_page(
    ui: &mut Ui,
    path: &Path,
    page_index: usize,
    zoom_bucket: u16,
) -> PdfLoadResult {
    let key = PageRenderKey::new(path, page_index, zoom_bucket);

    if let Some(texture) =
        with_pdf_texture_cache(ui, |cache| cache.textures.get(&key).cloned())
    {
        return PdfLoadResult::Loaded(texture);
    }

    let bitmap = with_pdf_runtime(ui.ctx(), |runtime| runtime.page_bitmap(&key));
    if let Some(bitmap) = bitmap {
        let texture = upload_pdf_texture(ui.ctx(), &key, &bitmap);
        with_pdf_texture_cache(ui, |cache| {
            cache.textures.insert(key.clone(), texture.clone());
        });
        return PdfLoadResult::Loaded(texture);
    }

    if let Some(msg) = with_pdf_runtime(ui.ctx(), |runtime| runtime.page_error(&key)) {
        return PdfLoadResult::Failed(msg);
    }

    ui.ctx().request_repaint_after(PDF_ASYNC_POLL_INTERVAL);
    PdfLoadResult::Loading
}

fn estimated_page_size(ui: &mut Ui, path: &Path, page_index: usize, zoom: f32) -> (f32, f32) {
    let key = PageRenderKey::new(path, page_index, super::runtime::zoom_bucket(zoom));
    let fallback_key = PageRenderKey::new(path, 0, super::runtime::zoom_bucket(zoom));
    with_pdf_runtime(ui.ctx(), |runtime| {
        runtime
            .page_dimensions(&key)
            .or_else(|| runtime.page_dimensions(&fallback_key))
    })
    .unwrap_or((FALLBACK_PAGE_WIDTH * zoom, FALLBACK_PAGE_HEIGHT * zoom))
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

fn render_toolbar(ui: &mut Ui, state: &mut PdfPreviewState, total_pages: usize) {
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
                            ui.selectable_value(&mut state.zoom, zoom, zoom_percent_label(zoom));
                        }
                    });

                if ui.button("−").clicked() {
                    state.zoom = (state.zoom - 0.25).max(0.5);
                }
                if ui.button("+").clicked() {
                    state.zoom = (state.zoom + 0.25).min(4.0);
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
}

fn prefetch_pages(first_visible: usize, last_visible: usize, total_pages: usize) -> Vec<usize> {
    let mut pages = Vec::new();
    if first_visible > 0 {
        pages.push(first_visible - 1);
    }
    if last_visible < total_pages {
        pages.push(last_visible);
    }
    pages
}

fn render_continuous_vertical(
    ui: &mut Ui,
    path: &Path,
    total_pages: usize,
    zoom: f32,
    current_page: usize,
    drag_to_scroll: bool,
) {
    let (_, estimated_h) = estimated_page_size(ui, path, current_page, zoom);
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

            with_pdf_runtime(ui.ctx(), |runtime| {
                runtime.request_visible_range(
                    path,
                    zoom,
                    first_visible..last_visible,
                    prefetch_pages(first_visible, last_visible, total_pages),
                )
            });

            ui.add_space(first_visible as f32 * row_height);

            for page_index in first_visible..last_visible {
                let (page_w, page_h) = estimated_page_size(ui, path, page_index, zoom);
                match load_result_for_page(
                    ui,
                    path,
                    page_index,
                    super::runtime::zoom_bucket(zoom),
                ) {
                    PdfLoadResult::Loading => {
                        ui.vertical_centered(|ui| {
                            render_page_placeholder(ui, page_w, page_h, "Rendering page...");
                        });
                        ui.add_space(PAGE_GAP);
                    }
                    PdfLoadResult::Loaded(cached_tex) => {
                        ui.vertical_centered(|ui| render_page_image(ui, &cached_tex));
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
    current_page: usize,
    drag_to_scroll: bool,
) {
    let (_, estimated_h) = estimated_page_size(ui, path, current_page, zoom);
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

            let first_page = first_row * 2;
            let last_page = (last_row * 2).min(total_pages);
            with_pdf_runtime(ui.ctx(), |runtime| {
                runtime.request_visible_range(
                    path,
                    zoom,
                    first_page..last_page,
                    prefetch_pages(first_page, last_page, total_pages),
                )
            });

            ui.add_space(first_row as f32 * row_height);

            let mut page_index = first_page;
            while page_index < last_page {
                ui.horizontal_top(|ui| {
                    for col in 0..2 {
                        let current_index = page_index + col;
                        if current_index < total_pages {
                            let (page_w, page_h) =
                                estimated_page_size(ui, path, current_index, zoom);
                            match load_result_for_page(
                                ui,
                                path,
                                current_index,
                                super::runtime::zoom_bucket(zoom),
                            ) {
                                PdfLoadResult::Loading => ui.vertical(|ui| {
                                    render_page_placeholder(
                                        ui,
                                        page_w,
                                        page_h,
                                        "Rendering page...",
                                    );
                                }),
                                PdfLoadResult::Loaded(cached_tex) => ui.vertical(|ui| {
                                    render_page_image(ui, &cached_tex);
                                }),
                                PdfLoadResult::Failed(msg) => ui.vertical(|ui| {
                                    ui.label(format!(
                                        "Page {} failed: {}",
                                        current_index + 1,
                                        msg
                                    ));
                                }),
                            };
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
    current_page: usize,
    drag_to_scroll: bool,
) {
    let (estimated_w, _) = estimated_page_size(ui, path, current_page, zoom);
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

            with_pdf_runtime(ui.ctx(), |runtime| {
                runtime.request_visible_range(
                    path,
                    zoom,
                    first_visible..last_visible,
                    prefetch_pages(first_visible, last_visible, total_pages),
                )
            });

            ui.horizontal(|ui| {
                ui.add_space(first_visible as f32 * col_width);

                for page_index in first_visible..last_visible {
                    let (page_w, page_h) = estimated_page_size(ui, path, page_index, zoom);
                    match load_result_for_page(
                        ui,
                        path,
                        page_index,
                        super::runtime::zoom_bucket(zoom),
                    ) {
                        PdfLoadResult::Loading => {
                            ui.vertical(|ui| {
                                render_page_placeholder(ui, page_w, page_h, "Rendering page...");
                            });
                            ui.add_space(PAGE_GAP);
                        }
                        PdfLoadResult::Loaded(cached_tex) => {
                            ui.vertical(|ui| render_page_image(ui, &cached_tex));
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
    doc: &PdfDocumentMetadata,
    state: &mut PdfPreviewState,
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
                    egui::ScrollArea::vertical().show_viewport(ui, |ui, viewport| {
                        let first_visible =
                            ((viewport.min.y / PDF_THUMBNAIL_ROW_HEIGHT).floor().max(0.0) as usize)
                                .saturating_sub(SIDEBAR_BUFFER_ROWS);
                        let last_visible = (((viewport.max.y / PDF_THUMBNAIL_ROW_HEIGHT)
                            .ceil()
                            .max(0.0)) as usize
                            + SIDEBAR_BUFFER_ROWS)
                            .min(doc.total_pages);

                        with_pdf_runtime(ui.ctx(), |runtime| {
                            runtime.request_thumbnail_range(path, first_visible..last_visible)
                        });

                        ui.add_space(first_visible as f32 * PDF_THUMBNAIL_ROW_HEIGHT);

                        for page_index in first_visible..last_visible {
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
                                                match load_result_for_page(
                                                    ui,
                                                    path,
                                                    page_index,
                                                    thumbnail_zoom_bucket(),
                                                ) {
                                                    PdfLoadResult::Loading => {
                                                        render_thumbnail_placeholder(ui);
                                                    }
                                                    PdfLoadResult::Loaded(cached_tex) => {
                                                        render_thumbnail_image(ui, &cached_tex);
                                                    }
                                                    PdfLoadResult::Failed(_) => {
                                                        ui.label(
                                                            RichText::new("Failed")
                                                                .small()
                                                                .weak(),
                                                        );
                                                    }
                                                }
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

                        ui.add_space(
                            (doc.total_pages.saturating_sub(last_visible)) as f32
                                * PDF_THUMBNAIL_ROW_HEIGHT,
                        );
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
                        if doc.outline.is_empty() {
                            ui.label(
                                RichText::new("No outline")
                                    .small()
                                    .color(Color32::from_rgb(160, 168, 180)),
                            );
                        } else {
                            for item in &doc.outline {
                                let selected = state.current_page == item.page_index;
                                let fill = if selected {
                                    Color32::from_rgb(88, 156, 255)
                                } else {
                                    Color32::TRANSPARENT
                                };
                                let text_color = if selected {
                                    Color32::WHITE
                                } else {
                                    Color32::from_rgb(210, 214, 220)
                                };
                                ui.horizontal(|ui| {
                                    ui.add_space((item.depth as f32) * 18.0);
                                    let button = egui::Button::new(
                                        RichText::new(&item.title).color(text_color),
                                    )
                                    .fill(fill)
                                    .stroke(egui::Stroke::NONE)
                                    .min_size(egui::vec2(
                                        (ui.available_width() - (item.depth as f32) * 18.0)
                                            .max(0.0),
                                        34.0,
                                    ));
                                    if ui.add(button).clicked() {
                                        state.current_page = item.page_index;
                                    }
                                });
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

pub fn render_pdf_preview(ui: &mut Ui, path: &Path, state: &mut PdfPreviewState) {
    let doc = match load_document_metadata(ui, path) {
        PdfDocumentLoadState::Loading => {
            state.total_pages = None;
            render_loading_screen(ui, path);
            return;
        }
        PdfDocumentLoadState::Loaded(doc) => doc,
        PdfDocumentLoadState::Failed(msg) => {
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

    render_toolbar(ui, state, total_pages);

    let gesture_zoom = ui.input(|i| i.zoom_delta());
    if (gesture_zoom - 1.0).abs() > f32::EPSILON {
        state.zoom = (state.zoom * gesture_zoom).clamp(0.5, 4.0);
    }

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
                    |ui| render_sidebar(ui, path, &doc, state),
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
                            state.current_page,
                            drag_to_scroll,
                        );
                    }
                    (PdfSpreadMode::SinglePage, PdfScrollLayout::Horizontal) => {
                        render_continuous_horizontal(
                            ui,
                            path,
                            total_pages,
                            state.zoom,
                            state.current_page,
                            drag_to_scroll,
                        );
                    }
                    (PdfSpreadMode::TwoPageOdd, PdfScrollLayout::Vertical) => {
                        render_continuous_two_column(
                            ui,
                            path,
                            total_pages,
                            state.zoom,
                            state.current_page,
                            drag_to_scroll,
                        );
                    }
                    (PdfSpreadMode::TwoPageOdd, PdfScrollLayout::Horizontal) => {
                        render_continuous_horizontal(
                            ui,
                            path,
                            total_pages,
                            state.zoom,
                            state.current_page,
                            drag_to_scroll,
                        );
                    }
                },
            );
        },
    );
}

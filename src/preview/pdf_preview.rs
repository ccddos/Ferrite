//! Standalone PDF preview rendering for PDF document tabs.
//!
//! This is a best-effort read-only viewer built on `hayro`. It renders the
//! selected page to a bitmap and uploads it as an egui texture.

use eframe::egui::{self, Color32, ColorImage, RichText, TextureHandle, TextureOptions, Ui, Vec2};
use hayro::hayro_interpret::InterpreterSettings;
use hayro::hayro_syntax::Pdf;
use hayro::{render, RenderSettings};
use std::path::Path;
use std::sync::Arc;

/// Per-tab UI state for PDF preview.
#[derive(Debug, Clone)]
pub struct PdfPreviewState {
    pub current_page: usize,
    pub zoom: f32,
    pub total_pages: Option<usize>,
}

impl Default for PdfPreviewState {
    fn default() -> Self {
        Self {
            current_page: 0,
            zoom: 1.0,
            total_pages: None,
        }
    }
}

#[derive(Clone)]
struct CachedPdfTexture {
    texture: TextureHandle,
    page_width: u32,
    page_height: u32,
    total_pages: usize,
}

#[derive(Clone)]
enum PdfLoadResult {
    Loaded(CachedPdfTexture),
    Failed(String),
}

fn render_pdf_page(
    ctx: &egui::Context,
    path: &Path,
    page_index: usize,
    zoom: f32,
) -> Result<CachedPdfTexture, String> {
    let file = std::fs::read(path).map_err(|e| format!("Failed to read: {}", e))?;
    let pdf = Pdf::new(Arc::new(file)).map_err(|e| format!("Failed to parse PDF: {e:?}"))?;
    let total_pages = pdf.pages().len();

    if total_pages == 0 {
        return Err("PDF has no pages".to_string());
    }

    let page = pdf
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
        total_pages,
    })
}

/// Render a PDF preview for a top-level PDF document tab.
pub fn render_pdf_preview(ui: &mut Ui, path: &Path, state: &mut PdfPreviewState) {
    let total_pages = match std::fs::read(path)
        .ok()
        .and_then(|file| Pdf::new(Arc::new(file)).ok())
        .map(|pdf| pdf.pages().len())
    {
        Some(count) if count > 0 => count,
        _ => 0,
    };
    state.total_pages = if total_pages > 0 {
        Some(total_pages)
    } else {
        None
    };

    let mut zoom_changed = false;

    ui.horizontal(|ui| {
        if ui.button("←").clicked() && state.current_page > 0 {
            state.current_page -= 1;
        }
        if ui.button("→").clicked() && total_pages > 0 && state.current_page + 1 < total_pages {
            state.current_page = state.current_page.saturating_add(1);
        }

        ui.add_space(8.0);

        if ui.button("−").clicked() {
            state.zoom = (state.zoom - 0.25).max(0.5);
            zoom_changed = true;
        }
        if ui.button("+").clicked() {
            state.zoom = (state.zoom + 0.25).min(4.0);
            zoom_changed = true;
        }

        ui.add_space(8.0);
        ui.label(RichText::new(format!("{:.0}%", state.zoom * 100.0)).weak());
    });

    let gesture_zoom = ui.input(|i| i.zoom_delta());
    if (gesture_zoom - 1.0).abs() > f32::EPSILON {
        log::debug!("PDF gesture zoom delta: {}", gesture_zoom);
        state.zoom = (state.zoom * gesture_zoom).clamp(0.5, 4.0);
        zoom_changed = true;
    }

    let cache_id = egui::Id::new("pdf_preview_cache").with((
        path.to_path_buf(),
        state.current_page,
        (state.zoom * 100.0).round() as i32,
    ));
    let cached: Option<PdfLoadResult> = ui.data(|d| d.get_temp(cache_id));

    let load_result = if zoom_changed { None } else { cached }.unwrap_or_else(|| {
        let result = match render_pdf_page(ui.ctx(), path, state.current_page, state.zoom) {
            Ok(tex) => PdfLoadResult::Loaded(tex),
            Err(msg) => PdfLoadResult::Failed(msg),
        };
        ui.data_mut(|d| d.insert_temp(cache_id, result.clone()));
        result
    });

    match load_result {
        PdfLoadResult::Loaded(cached_tex) => {
            if cached_tex.total_pages > 0 && state.current_page >= cached_tex.total_pages {
                state.current_page = cached_tex.total_pages.saturating_sub(1);
            }

            ui.horizontal(|ui| {
                ui.label(
                    RichText::new(format!(
                        "Page {} / {}",
                        state.current_page + 1,
                        cached_tex.total_pages
                    ))
                    .small()
                    .weak(),
                );
                ui.add_space(8.0);
                ui.label(
                    RichText::new(format!(
                        "{} x {}",
                        cached_tex.page_width, cached_tex.page_height
                    ))
                    .small()
                    .weak(),
                );
            });
            ui.add_space(8.0);

            let display_size =
                Vec2::new(cached_tex.page_width as f32, cached_tex.page_height as f32);

            egui::ScrollArea::both()
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    ui.vertical_centered(|ui| {
                        let sized =
                            egui::load::SizedTexture::new(cached_tex.texture.id(), display_size);
                        ui.add(egui::Image::from_texture(sized));
                    });
                });
        }
        PdfLoadResult::Failed(msg) => {
            ui.vertical_centered(|ui| {
                ui.add_space(24.0);
                ui.label(RichText::new("PDF preview unavailable").strong());
                ui.add_space(8.0);
                ui.label(msg);
                ui.add_space(8.0);
                ui.label(path.display().to_string());
            });
        }
    }
}

//! Standalone image preview rendering for image document tabs.
//!
//! This reuses the same decode-and-cache approach as markdown image rendering,
//! but works directly from a file path for top-level image tabs.

use eframe::egui::{self, Color32, ColorImage, RichText, TextureHandle, TextureOptions, Ui, Vec2};
use std::path::Path;

#[derive(Clone)]
struct CachedImageTexture {
    texture: TextureHandle,
    original_width: u32,
    original_height: u32,
}

#[derive(Clone)]
enum ImageLoadResult {
    Loaded(CachedImageTexture),
    Failed(String),
}

fn load_image_texture(ctx: &egui::Context, path: &Path) -> Result<CachedImageTexture, String> {
    let bytes = std::fs::read(path).map_err(|e| format!("Failed to read: {}", e))?;
    let img = image::load_from_memory(&bytes).map_err(|e| format!("Failed to decode: {}", e))?;

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

    let texture_name = format!("image_preview_{}", path.display());
    let texture = ctx.load_texture(&texture_name, color_image, TextureOptions::LINEAR);

    Ok(CachedImageTexture {
        texture,
        original_width: width,
        original_height: height,
    })
}

/// Render an image preview for a top-level image document tab.
pub fn render_image_preview(ui: &mut Ui, path: &Path) {
    let cache_id = egui::Id::new("image_preview_cache").with(path);
    let cached: Option<ImageLoadResult> = ui.data(|d| d.get_temp(cache_id));

    let load_result = cached.unwrap_or_else(|| {
        let result = match load_image_texture(ui.ctx(), path) {
            Ok(tex) => ImageLoadResult::Loaded(tex),
            Err(msg) => ImageLoadResult::Failed(msg),
        };
        ui.data_mut(|d| d.insert_temp(cache_id, result.clone()));
        result
    });

    match load_result {
        ImageLoadResult::Loaded(cached_tex) => {
            let available = ui.available_size_before_wrap();
            let max_width = available.x.max(1.0);
            let max_height = available.y.max(1.0);
            let orig_w = cached_tex.original_width as f32;
            let orig_h = cached_tex.original_height as f32;
            let scale = (max_width / orig_w).min(max_height / orig_h).min(1.0);
            let display_size = Vec2::new(orig_w * scale, orig_h * scale);

            ui.vertical_centered(|ui| {
                ui.label(
                    RichText::new(format!(
                        "{} x {}",
                        cached_tex.original_width, cached_tex.original_height
                    ))
                    .small()
                    .weak(),
                );
                ui.add_space(8.0);

                let sized = egui::load::SizedTexture::new(cached_tex.texture.id(), display_size);
                ui.add(egui::Image::from_texture(sized));
            });
        }
        ImageLoadResult::Failed(msg) => {
            ui.vertical_centered(|ui| {
                ui.add_space(24.0);
                ui.label(RichText::new("Image preview unavailable").strong());
                ui.add_space(8.0);
                ui.label(msg);
                ui.add_space(8.0);
                ui.label(path.display().to_string());
            });
        }
    }
}

use std::sync::Arc;

/// Convert a grayscale IR frame to RGBA bytes for egui texture upload.
pub fn frame_to_rgba(frame: &face_auth_camera::Frame) -> Vec<u8> {
    frame
        .data
        .iter()
        .flat_map(|&g| [g, g, g, 255u8])
        .collect()
}

/// Upload a camera frame as an egui texture, returning a handle.
/// Re-created every frame; egui handles GPU upload internally.
pub fn upload_frame(
    frame: &Arc<face_auth_camera::Frame>,
    ctx: &egui::Context,
    name: &str,
) -> egui::TextureHandle {
    let rgba = frame_to_rgba(frame);
    let image = egui::ColorImage::from_rgba_unmultiplied(
        [frame.width as usize, frame.height as usize],
        &rgba,
    );
    ctx.load_texture(name, image, egui::TextureOptions::LINEAR)
}

/// Render a texture handle into the current UI, scaled to fit available width.
pub fn show_texture(ui: &mut egui::Ui, texture: &egui::TextureHandle, native_w: u32, native_h: u32) {
    let available_w = ui.available_width();
    if available_w <= 0.0 || native_w == 0 {
        return;
    }
    let scale = available_w / native_w as f32;
    let display_size = egui::Vec2::new(available_w, native_h as f32 * scale);
    ui.image(egui::load::SizedTexture::new(texture.id(), display_size));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grayscale_to_rgba_conversion() {
        let frame = face_auth_camera::Frame {
            data: vec![0u8, 128u8, 255u8, 64u8],
            width: 2,
            height: 2,
            timestamp: std::time::Instant::now(),
        };
        let rgba = frame_to_rgba(&frame);
        assert_eq!(rgba.len(), 4 * 4); // 4 pixels × 4 channels
        // Pixel 0: value 0 → [0,0,0,255]
        assert_eq!(&rgba[0..4], &[0u8, 0, 0, 255]);
        // Pixel 1: value 128 → [128,128,128,255]
        assert_eq!(&rgba[4..8], &[128u8, 128, 128, 255]);
        // Pixel 3: value 64 → [64,64,64,255]
        assert_eq!(&rgba[12..16], &[64u8, 64, 64, 255]);
    }
}

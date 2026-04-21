//! Debug visualization UI using minifb.
//! Renders live camera feed with overlays: bounding box, landmarks,
//! metrics text, aligned face crops, and state indicators.

use face_auth_core::geometry::{BBox, Landmarks};
use minifb::{Key, Window, WindowOptions};

/// Window dimensions — camera is 640×360, we add a right panel for crops/info.
const WIN_W: usize = 840;
const WIN_H: usize = 360;
const CAM_W: usize = 640;
const CAM_H: usize = 360;
/// Panel starts at x=640, width=200.
const PANEL_X: usize = 640;


/// ARGB pixel buffer for minifb (0xAARRGGBB format, but minifb ignores alpha).
pub struct DebugWindow {
    window: Window,
    buf: Vec<u32>,
    border_color: u32,
}

/// Per-frame debug info to render.
pub struct DebugFrame {
    /// Raw grayscale camera frame (640×360).
    pub frame_data: Vec<u8>,
    pub frame_w: u32,
    pub frame_h: u32,
    /// Detection results (if face found).
    pub detection: Option<DebugDetection>,
    /// Current state label.
    pub state: String,
    /// FPS counter.
    pub fps: f32,
    /// Border color: 0=neutral, 1=scanning(yellow), 2=auth(blue), 3=success(green), 4=fail(red).
    pub border_mode: u8,
}

pub struct DebugDetection {
    pub bbox: BBox,
    pub landmarks: Landmarks,
    pub confidence: f32,
    /// Geometry metrics.
    pub yaw: f32,
    pub pitch: f32,
    pub roll: f32,
    pub face_ratio: f32,
    /// Quality.
    pub blur_score: f32,
    pub ir_saturated: bool,
    /// Liveness.
    pub lbp_entropy: f32,
    pub contrast_cv: f32,
    pub liveness_pass: bool,
    /// Recognition (if available).
    pub similarity: Option<f32>,
    /// Aligned 112×112 grayscale crop.
    pub aligned_crop: Option<Vec<u8>>,
    /// CLAHE-processed 112×112 crop.
    pub clahe_crop: Option<Vec<u8>>,
}

impl DebugWindow {
    pub fn new(title: &str) -> Self {
        let window = Window::new(
            title,
            WIN_W,
            WIN_H,
            WindowOptions {
                resize: false,
                ..WindowOptions::default()
            },
        )
        .expect("failed to create debug window");

        Self {
            window,
            buf: vec![0u32; WIN_W * WIN_H],
            border_color: 0x00333333,
        }
    }

    pub fn is_open(&self) -> bool {
        self.window.is_open() && !self.window.is_key_down(Key::Escape)
    }

    pub fn render(&mut self, frame: &DebugFrame) {
        // Clear buffer
        self.buf.fill(0x00111111);

        // Draw camera frame (grayscale → RGB)
        let fw = frame.frame_w as usize;
        let fh = frame.frame_h as usize;
        for row in 0..fh.min(CAM_H) {
            for col in 0..fw.min(CAM_W) {
                let g = frame.frame_data[row * fw + col] as u32;
                self.buf[row * WIN_W + col] = (g << 16) | (g << 8) | g;
            }
        }

        // Border color based on state
        self.border_color = match frame.border_mode {
            1 => 0x00CCCC00, // yellow - scanning
            2 => 0x004488FF, // blue - authenticating
            3 => 0x0044CC44, // green - success
            4 => 0x00CC4444, // red - fail/timeout
            _ => 0x00333333, // neutral
        };

        // Draw border (3px)
        draw_rect(&mut self.buf, WIN_W, 0, 0, CAM_W, CAM_H, self.border_color, false);
        draw_rect(&mut self.buf, WIN_W, 1, 1, CAM_W - 2, CAM_H - 2, self.border_color, false);
        draw_rect(&mut self.buf, WIN_W, 2, 2, CAM_W - 4, CAM_H - 4, self.border_color, false);

        if let Some(ref det) = frame.detection {
            self.draw_detection(det);
        }

        // Right panel
        self.draw_panel(frame);

        self.window
            .update_with_buffer(&self.buf, WIN_W, WIN_H)
            .unwrap_or(());
    }

    fn draw_detection(&mut self, det: &DebugDetection) {
        // Bbox color: green=match, blue=authenticating/liveness pass, yellow=detecting, red=spoof
        let bbox_color = if det.similarity.is_some_and(|s| s > 0.0) {
            0x0044CC44 // green - got a match score
        } else if det.liveness_pass {
            0x004488FF // blue - liveness OK
        } else if !det.liveness_pass && det.lbp_entropy > 0.0 {
            0x00CC4444 // red - liveness fail
        } else {
            0x00CCCC00 // yellow - detecting
        };

        // Draw bounding box
        let bx = det.bbox.x1 as usize;
        let by = det.bbox.y1 as usize;
        let bw = det.bbox.width() as usize;
        let bh = (det.bbox.y2 - det.bbox.y1) as usize;
        draw_rect(&mut self.buf, WIN_W, bx, by, bw, bh, bbox_color, false);
        draw_rect(
            &mut self.buf,
            WIN_W,
            bx.saturating_sub(1),
            by.saturating_sub(1),
            bw + 2,
            bh + 2,
            bbox_color,
            false,
        );

        // Draw 5-point landmarks
        let lm_color = 0x00FF4444;
        let landmarks = [
            det.landmarks.left_eye,
            det.landmarks.right_eye,
            det.landmarks.nose,
            det.landmarks.left_mouth,
            det.landmarks.right_mouth,
        ];
        for (lx, ly) in landmarks {
            draw_filled_circle(&mut self.buf, WIN_W, WIN_H, lx as i32, ly as i32, 3, lm_color);
        }

        // Confidence + similarity text above bbox
        let label = if let Some(sim) = det.similarity {
            format!("{:.0}% sim:{:.3}", det.confidence * 100.0, sim)
        } else {
            format!("{:.0}%", det.confidence * 100.0)
        };
        draw_text(&mut self.buf, WIN_W, WIN_H, bx, by.saturating_sub(12), &label, bbox_color);
    }

    fn draw_panel(&mut self, frame: &DebugFrame) {
        let x = PANEL_X + 8;
        let mut y = 8;
        let white = 0x00DDDDDD;
        let dim = 0x00888888;
        let green = 0x0044CC44;
        let red = 0x00CC4444;
        let yellow = 0x00CCCC00;

        // State
        draw_text(&mut self.buf, WIN_W, WIN_H, x, y, &frame.state, self.border_color);
        y += 14;

        // FPS
        draw_text(&mut self.buf, WIN_W, WIN_H, x, y, &format!("FPS: {:.0}", frame.fps), dim);
        y += 14;

        if let Some(ref det) = frame.detection {
            y += 4;

            // Geometry
            draw_text(
                &mut self.buf, WIN_W, WIN_H, x, y,
                &format!("yaw:{:.0} pit:{:.0} rol:{:.0}", det.yaw, det.pitch, det.roll),
                white,
            );
            y += 12;
            draw_text(
                &mut self.buf, WIN_W, WIN_H, x, y,
                &format!("face: {:.0}%", det.face_ratio * 100.0),
                white,
            );
            y += 12;

            // Quality
            let blur_c = if det.blur_score >= 50.0 { green } else { red };
            draw_text(
                &mut self.buf, WIN_W, WIN_H, x, y,
                &format!("blur: {:.0}", det.blur_score),
                blur_c,
            );
            y += 12;

            let sat_c = if det.ir_saturated { red } else { green };
            draw_text(
                &mut self.buf, WIN_W, WIN_H, x, y,
                &format!("IR sat: {}", if det.ir_saturated { "YES" } else { "no" }),
                sat_c,
            );
            y += 14;

            // Liveness
            let live_c = if det.liveness_pass { green } else { yellow };
            draw_text(
                &mut self.buf, WIN_W, WIN_H, x, y,
                &format!("LBP: {:.2}", det.lbp_entropy),
                live_c,
            );
            y += 12;
            draw_text(
                &mut self.buf, WIN_W, WIN_H, x, y,
                &format!("CV: {:.3}", det.contrast_cv),
                live_c,
            );
            y += 12;
            draw_text(
                &mut self.buf, WIN_W, WIN_H, x, y,
                &format!("live: {}", if det.liveness_pass { "PASS" } else { "FAIL" }),
                live_c,
            );
            y += 14;

            // Similarity
            if let Some(sim) = det.similarity {
                let sim_c = if sim >= 0.70 { green } else if sim >= 0.50 { yellow } else { red };
                draw_text(
                    &mut self.buf, WIN_W, WIN_H, x, y,
                    &format!("sim: {:.3}", sim),
                    sim_c,
                );
                y += 14;
            }

            // Draw aligned crops in panel
            if let Some(ref crop) = det.aligned_crop {
                y += 4;
                draw_text(&mut self.buf, WIN_W, WIN_H, x, y, "raw crop:", dim);
                y += 12;
                draw_gray_thumbnail(&mut self.buf, WIN_W, WIN_H, x, y, crop, 112, 56);
                y += 60;
            }

            if let Some(ref crop) = det.clahe_crop {
                draw_text(&mut self.buf, WIN_W, WIN_H, x, y, "CLAHE:", dim);
                y += 12;
                draw_gray_thumbnail(&mut self.buf, WIN_W, WIN_H, x, y, crop, 112, 56);
            }
        }
    }
}

// --- Drawing primitives ---

fn draw_rect(buf: &mut [u32], buf_w: usize, x: usize, y: usize, w: usize, h: usize, color: u32, filled: bool) {
    if filled {
        for row in y..y + h {
            if row >= CAM_H.max(WIN_H) { break; }
            for col in x..x + w {
                if col >= WIN_W { break; }
                buf[row * buf_w + col] = color;
            }
        }
    } else {
        // Top and bottom edges
        for col in x..x + w {
            if col >= WIN_W { break; }
            if y < WIN_H { buf[y * buf_w + col] = color; }
            let bottom = y + h.saturating_sub(1);
            if bottom < WIN_H { buf[bottom * buf_w + col] = color; }
        }
        // Left and right edges
        for row in y..y + h {
            if row >= WIN_H { break; }
            if x < WIN_W { buf[row * buf_w + x] = color; }
            let right = x + w.saturating_sub(1);
            if right < WIN_W { buf[row * buf_w + right] = color; }
        }
    }
}

fn draw_filled_circle(buf: &mut [u32], buf_w: usize, buf_h: usize, cx: i32, cy: i32, r: i32, color: u32) {
    for dy in -r..=r {
        for dx in -r..=r {
            if dx * dx + dy * dy <= r * r {
                let px = cx + dx;
                let py = cy + dy;
                if px >= 0 && py >= 0 && (px as usize) < buf_w && (py as usize) < buf_h {
                    buf[py as usize * buf_w + px as usize] = color;
                }
            }
        }
    }
}

/// Draw a 112×112 grayscale image as a thumbnail at the given size.
fn draw_gray_thumbnail(
    buf: &mut [u32], buf_w: usize, buf_h: usize,
    x: usize, y: usize,
    data: &[u8], src_size: usize, dst_size: usize,
) {
    let scale = src_size as f32 / dst_size as f32;
    for row in 0..dst_size {
        for col in 0..dst_size {
            let sx = (col as f32 * scale) as usize;
            let sy = (row as f32 * scale) as usize;
            if sx < src_size && sy < src_size {
                let g = data[sy * src_size + sx] as u32;
                let px = x + col;
                let py = y + row;
                if px < buf_w && py < buf_h {
                    buf[py * buf_w + px] = (g << 16) | (g << 8) | g;
                }
            }
        }
    }
}

// --- Bitmap font (5×7, ASCII 32-126) ---

/// Render text using an embedded 5×7 bitmap font.
fn draw_text(buf: &mut [u32], buf_w: usize, buf_h: usize, x: usize, y: usize, text: &str, color: u32) {
    let mut cx = x;
    for ch in text.chars() {
        let idx = ch as usize;
        if idx >= 32 && idx <= 126 {
            let glyph = &FONT_5X7[idx - 32];
            for (row, &bits) in glyph.iter().enumerate() {
                for col in 0..5 {
                    if bits & (1 << (4 - col)) != 0 {
                        let px = cx + col;
                        let py = y + row;
                        if px < buf_w && py < buf_h {
                            buf[py * buf_w + px] = color;
                        }
                    }
                }
            }
        }
        cx += 6; // 5px + 1px gap
    }
}

/// 5×7 bitmap font for ASCII 32–126. Each glyph is 7 bytes (rows), 5 bits wide (MSB=left).
#[rustfmt::skip]
const FONT_5X7: [[u8; 7]; 95] = [
    [0x00,0x00,0x00,0x00,0x00,0x00,0x00], // ' '
    [0x04,0x04,0x04,0x04,0x04,0x00,0x04], // '!'
    [0x0A,0x0A,0x00,0x00,0x00,0x00,0x00], // '"'
    [0x0A,0x1F,0x0A,0x0A,0x1F,0x0A,0x00], // '#'
    [0x04,0x0F,0x14,0x0E,0x05,0x1E,0x04], // '$'
    [0x18,0x19,0x02,0x04,0x08,0x13,0x03], // '%'
    [0x08,0x14,0x14,0x08,0x15,0x12,0x0D], // '&'
    [0x04,0x04,0x00,0x00,0x00,0x00,0x00], // '''
    [0x02,0x04,0x08,0x08,0x08,0x04,0x02], // '('
    [0x08,0x04,0x02,0x02,0x02,0x04,0x08], // ')'
    [0x00,0x04,0x15,0x0E,0x15,0x04,0x00], // '*'
    [0x00,0x04,0x04,0x1F,0x04,0x04,0x00], // '+'
    [0x00,0x00,0x00,0x00,0x00,0x04,0x08], // ','
    [0x00,0x00,0x00,0x1F,0x00,0x00,0x00], // '-'
    [0x00,0x00,0x00,0x00,0x00,0x00,0x04], // '.'
    [0x00,0x01,0x02,0x04,0x08,0x10,0x00], // '/'
    [0x0E,0x11,0x13,0x15,0x19,0x11,0x0E], // '0'
    [0x04,0x0C,0x04,0x04,0x04,0x04,0x0E], // '1'
    [0x0E,0x11,0x01,0x06,0x08,0x10,0x1F], // '2'
    [0x0E,0x11,0x01,0x06,0x01,0x11,0x0E], // '3'
    [0x02,0x06,0x0A,0x12,0x1F,0x02,0x02], // '4'
    [0x1F,0x10,0x1E,0x01,0x01,0x11,0x0E], // '5'
    [0x06,0x08,0x10,0x1E,0x11,0x11,0x0E], // '6'
    [0x1F,0x01,0x02,0x04,0x08,0x08,0x08], // '7'
    [0x0E,0x11,0x11,0x0E,0x11,0x11,0x0E], // '8'
    [0x0E,0x11,0x11,0x0F,0x01,0x02,0x0C], // '9'
    [0x00,0x00,0x04,0x00,0x00,0x04,0x00], // ':'
    [0x00,0x00,0x04,0x00,0x00,0x04,0x08], // ';'
    [0x02,0x04,0x08,0x10,0x08,0x04,0x02], // '<'
    [0x00,0x00,0x1F,0x00,0x1F,0x00,0x00], // '='
    [0x08,0x04,0x02,0x01,0x02,0x04,0x08], // '>'
    [0x0E,0x11,0x01,0x02,0x04,0x00,0x04], // '?'
    [0x0E,0x11,0x17,0x15,0x17,0x10,0x0E], // '@'
    [0x0E,0x11,0x11,0x1F,0x11,0x11,0x11], // 'A'
    [0x1E,0x11,0x11,0x1E,0x11,0x11,0x1E], // 'B'
    [0x0E,0x11,0x10,0x10,0x10,0x11,0x0E], // 'C'
    [0x1E,0x11,0x11,0x11,0x11,0x11,0x1E], // 'D'
    [0x1F,0x10,0x10,0x1E,0x10,0x10,0x1F], // 'E'
    [0x1F,0x10,0x10,0x1E,0x10,0x10,0x10], // 'F'
    [0x0E,0x11,0x10,0x17,0x11,0x11,0x0E], // 'G'
    [0x11,0x11,0x11,0x1F,0x11,0x11,0x11], // 'H'
    [0x0E,0x04,0x04,0x04,0x04,0x04,0x0E], // 'I'
    [0x07,0x02,0x02,0x02,0x02,0x12,0x0C], // 'J'
    [0x11,0x12,0x14,0x18,0x14,0x12,0x11], // 'K'
    [0x10,0x10,0x10,0x10,0x10,0x10,0x1F], // 'L'
    [0x11,0x1B,0x15,0x15,0x11,0x11,0x11], // 'M'
    [0x11,0x19,0x15,0x13,0x11,0x11,0x11], // 'N'
    [0x0E,0x11,0x11,0x11,0x11,0x11,0x0E], // 'O'
    [0x1E,0x11,0x11,0x1E,0x10,0x10,0x10], // 'P'
    [0x0E,0x11,0x11,0x11,0x15,0x12,0x0D], // 'Q'
    [0x1E,0x11,0x11,0x1E,0x14,0x12,0x11], // 'R'
    [0x0E,0x11,0x10,0x0E,0x01,0x11,0x0E], // 'S'
    [0x1F,0x04,0x04,0x04,0x04,0x04,0x04], // 'T'
    [0x11,0x11,0x11,0x11,0x11,0x11,0x0E], // 'U'
    [0x11,0x11,0x11,0x11,0x0A,0x0A,0x04], // 'V'
    [0x11,0x11,0x11,0x15,0x15,0x1B,0x11], // 'W'
    [0x11,0x11,0x0A,0x04,0x0A,0x11,0x11], // 'X'
    [0x11,0x11,0x0A,0x04,0x04,0x04,0x04], // 'Y'
    [0x1F,0x01,0x02,0x04,0x08,0x10,0x1F], // 'Z'
    [0x0E,0x08,0x08,0x08,0x08,0x08,0x0E], // '['
    [0x00,0x10,0x08,0x04,0x02,0x01,0x00], // '\'
    [0x0E,0x02,0x02,0x02,0x02,0x02,0x0E], // ']'
    [0x04,0x0A,0x11,0x00,0x00,0x00,0x00], // '^'
    [0x00,0x00,0x00,0x00,0x00,0x00,0x1F], // '_'
    [0x08,0x04,0x00,0x00,0x00,0x00,0x00], // '`'
    [0x00,0x00,0x0E,0x01,0x0F,0x11,0x0F], // 'a'
    [0x10,0x10,0x1E,0x11,0x11,0x11,0x1E], // 'b'
    [0x00,0x00,0x0E,0x11,0x10,0x11,0x0E], // 'c'
    [0x01,0x01,0x0F,0x11,0x11,0x11,0x0F], // 'd'
    [0x00,0x00,0x0E,0x11,0x1F,0x10,0x0E], // 'e'
    [0x06,0x08,0x1E,0x08,0x08,0x08,0x08], // 'f'
    [0x00,0x00,0x0F,0x11,0x0F,0x01,0x0E], // 'g'
    [0x10,0x10,0x1E,0x11,0x11,0x11,0x11], // 'h'
    [0x04,0x00,0x0C,0x04,0x04,0x04,0x0E], // 'i'
    [0x02,0x00,0x06,0x02,0x02,0x12,0x0C], // 'j'
    [0x10,0x10,0x12,0x14,0x18,0x14,0x12], // 'k'
    [0x0C,0x04,0x04,0x04,0x04,0x04,0x0E], // 'l'
    [0x00,0x00,0x1A,0x15,0x15,0x15,0x15], // 'm'
    [0x00,0x00,0x1E,0x11,0x11,0x11,0x11], // 'n'
    [0x00,0x00,0x0E,0x11,0x11,0x11,0x0E], // 'o'
    [0x00,0x00,0x1E,0x11,0x1E,0x10,0x10], // 'p'
    [0x00,0x00,0x0F,0x11,0x0F,0x01,0x01], // 'q'
    [0x00,0x00,0x16,0x19,0x10,0x10,0x10], // 'r'
    [0x00,0x00,0x0F,0x10,0x0E,0x01,0x1E], // 's'
    [0x08,0x08,0x1E,0x08,0x08,0x09,0x06], // 't'
    [0x00,0x00,0x11,0x11,0x11,0x11,0x0F], // 'u'
    [0x00,0x00,0x11,0x11,0x11,0x0A,0x04], // 'v'
    [0x00,0x00,0x11,0x11,0x15,0x15,0x0A], // 'w'
    [0x00,0x00,0x11,0x0A,0x04,0x0A,0x11], // 'x'
    [0x00,0x00,0x11,0x11,0x0F,0x01,0x0E], // 'y'
    [0x00,0x00,0x1F,0x02,0x04,0x08,0x1F], // 'z'
    [0x02,0x04,0x04,0x08,0x04,0x04,0x02], // '{'
    [0x04,0x04,0x04,0x04,0x04,0x04,0x04], // '|'
    [0x08,0x04,0x04,0x02,0x04,0x04,0x08], // '}'
    [0x00,0x00,0x08,0x15,0x02,0x00,0x00], // '~'
];

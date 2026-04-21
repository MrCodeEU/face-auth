use face_auth_core::geometry::BBox;
use ndarray::Array4;
use ort::session::Session;
use ort::value::Tensor;
use std::path::Path;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum LivenessError {
    #[error("model file not found: {0}")]
    ModelNotFound(String),
    #[error("ort error: {0}")]
    Ort(String),
    #[error("inference error: {0}")]
    Inference(String),
}

#[derive(Debug, Clone)]
pub struct LivenessResult {
    /// Probability that the face is real (0.0–1.0).
    pub real_score: f32,
    /// Probability that the face is a spoof (0.0–1.0).
    pub spoof_score: f32,
}

impl LivenessResult {
    pub fn is_real(&self, threshold: f32) -> bool {
        self.real_score >= threshold
    }
}

const LIVENESS_INPUT_SIZE: usize = 128;

pub struct LivenessDetector {
    session: Session,
}

impl LivenessDetector {
    pub fn load(model_path: &Path) -> Result<Self, LivenessError> {
        if !model_path.exists() {
            return Err(LivenessError::ModelNotFound(
                model_path.display().to_string(),
            ));
        }

        let session = Session::builder()
            .and_then(|mut b| b.commit_from_file(model_path))
            .map_err(|e| LivenessError::Ort(e.to_string()))?;

        tracing::info!(path = %model_path.display(), "liveness model loaded");
        Ok(Self { session })
    }

    pub fn load_default() -> Result<Self, LivenessError> {
        for path in [
            "models/antispoof_q.onnx",
            "/usr/share/face-auth/models/antispoof_q.onnx",
            "/var/lib/face-auth/models/antispoof_q.onnx",
        ] {
            if Path::new(path).exists() {
                return Self::load(Path::new(path));
            }
        }
        Err(LivenessError::ModelNotFound(
            "antispoof_q.onnx not found in ./models/, /usr/share/face-auth/models/, or /var/lib/face-auth/models/".into(),
        ))
    }

    /// Check liveness of a face region in a grayscale frame.
    /// Crops the face bbox, resizes to 128×128, runs MiniFASNetV2-SE.
    pub fn check(
        &mut self,
        frame_data: &[u8],
        frame_width: u32,
        frame_height: u32,
        bbox: &BBox,
    ) -> Result<LivenessResult, LivenessError> {
        let input = preprocess(frame_data, frame_width, frame_height, bbox);

        let tensor =
            Tensor::from_array(input).map_err(|e| LivenessError::Inference(e.to_string()))?;

        let outputs = self
            .session
            .run(ort::inputs![tensor])
            .map_err(|e| LivenessError::Inference(e.to_string()))?;

        let output_view = outputs[0]
            .try_extract_array::<f32>()
            .map_err(|e| LivenessError::Inference(e.to_string()))?;

        let raw: Vec<f32> = output_view.iter().copied().collect();
        if raw.len() < 2 {
            return Err(LivenessError::Inference(format!(
                "expected 2 outputs, got {}",
                raw.len()
            )));
        }

        // Apply softmax to get probabilities
        let (spoof_score, real_score) = softmax2(raw[0], raw[1]);

        Ok(LivenessResult {
            real_score,
            spoof_score,
        })
    }
}

/// Crop face region from grayscale frame and resize to 128×128 for liveness model.
/// Expands bbox by 20% to include some context (forehead, chin).
fn preprocess(frame_data: &[u8], frame_width: u32, frame_height: u32, bbox: &BBox) -> Array4<f32> {
    // Expand bbox by 20% for context
    let bw = bbox.x2 - bbox.x1;
    let bh = bbox.y2 - bbox.y1;
    let cx = (bbox.x1 + bbox.x2) / 2.0;
    let cy = (bbox.y1 + bbox.y2) / 2.0;
    let side = bw.max(bh) * 1.2; // square crop with 20% expansion
    let half = side / 2.0;

    let x1 = (cx - half).max(0.0) as u32;
    let y1 = (cy - half).max(0.0) as u32;
    let x2 = (cx + half).min(frame_width as f32) as u32;
    let y2 = (cy + half).min(frame_height as f32) as u32;

    let crop_w = (x2 - x1) as usize;
    let crop_h = (y2 - y1) as usize;

    let mut input = Array4::<f32>::zeros((1, 3, LIVENESS_INPUT_SIZE, LIVENESS_INPUT_SIZE));

    if crop_w < 2 || crop_h < 2 {
        return input;
    }

    // Bilinear resize from crop to 128×128, replicate grayscale to 3 channels
    let scale_x = crop_w as f32 / LIVENESS_INPUT_SIZE as f32;
    let scale_y = crop_h as f32 / LIVENESS_INPUT_SIZE as f32;

    for out_y in 0..LIVENESS_INPUT_SIZE {
        for out_x in 0..LIVENESS_INPUT_SIZE {
            let src_x = x1 as f32 + out_x as f32 * scale_x;
            let src_y = y1 as f32 + out_y as f32 * scale_y;

            let pixel = bilinear_sample(frame_data, frame_width, frame_height, src_x, src_y);
            // Normalize: (pixel - 127.5) / 128.0
            let normalized = (pixel - 127.5) / 128.0;

            input[[0, 0, out_y, out_x]] = normalized;
            input[[0, 1, out_y, out_x]] = normalized;
            input[[0, 2, out_y, out_x]] = normalized;
        }
    }

    input
}

fn bilinear_sample(data: &[u8], width: u32, height: u32, x: f32, y: f32) -> f32 {
    let x0 = (x.floor() as i32).max(0).min(width as i32 - 1) as u32;
    let y0 = (y.floor() as i32).max(0).min(height as i32 - 1) as u32;
    let x1 = (x0 + 1).min(width - 1);
    let y1 = (y0 + 1).min(height - 1);

    let fx = x - x0 as f32;
    let fy = y - y0 as f32;

    let p00 = data[(y0 * width + x0) as usize] as f32;
    let p10 = data[(y0 * width + x1) as usize] as f32;
    let p01 = data[(y1 * width + x0) as usize] as f32;
    let p11 = data[(y1 * width + x1) as usize] as f32;

    p00 * (1.0 - fx) * (1.0 - fy) + p10 * fx * (1.0 - fy) + p01 * (1.0 - fx) * fy + p11 * fx * fy
}

fn softmax2(a: f32, b: f32) -> (f32, f32) {
    let max = a.max(b);
    let ea = (a - max).exp();
    let eb = (b - max).exp();
    let sum = ea + eb;
    (ea / sum, eb / sum)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn softmax2_balanced() {
        let (a, b) = softmax2(0.0, 0.0);
        assert!((a - 0.5).abs() < 1e-6);
        assert!((b - 0.5).abs() < 1e-6);
    }

    #[test]
    fn softmax2_skewed() {
        let (a, b) = softmax2(10.0, 0.0);
        assert!(a > 0.99);
        assert!(b < 0.01);
    }

    #[test]
    fn liveness_result_threshold() {
        let r = LivenessResult {
            real_score: 0.8,
            spoof_score: 0.2,
        };
        assert!(r.is_real(0.5));
        assert!(!r.is_real(0.9));
    }

    #[test]
    fn preprocess_produces_correct_shape() {
        let data = vec![128u8; 640 * 360];
        let bbox = BBox {
            x1: 200.0,
            y1: 100.0,
            x2: 300.0,
            y2: 200.0,
        };
        let input = preprocess(&data, 640, 360, &bbox);
        assert_eq!(input.shape(), &[1, 3, 128, 128]);
    }
}

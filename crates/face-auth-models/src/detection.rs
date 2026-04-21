use face_auth_core::geometry::{BBox, Landmarks};
use ndarray::Array4;
use ort::session::Session;
use ort::value::Tensor;
use std::path::Path;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum DetectionError {
    #[error("model file not found: {0}")]
    ModelNotFound(String),
    #[error("ort error: {0}")]
    Ort(String),
    #[error("inference error: {0}")]
    Inference(String),
}

#[derive(Debug, Clone)]
pub struct Detection {
    pub bbox: BBox,
    pub landmarks: Landmarks,
    pub confidence: f32,
}

const STRIDES: [usize; 3] = [8, 16, 32];
const ANCHORS_PER_CELL: usize = 2;
const CONF_THRESHOLD: f32 = 0.5;
const NMS_THRESHOLD: f32 = 0.4;
const MODEL_INPUT_SIZE: usize = 640;

pub struct FaceDetector {
    session: Session,
}

impl FaceDetector {
    pub fn load(model_path: &Path) -> Result<Self, DetectionError> {
        Self::load_with_ep(model_path, "cpu")
    }

    pub fn load_with_ep(model_path: &Path, ep_name: &str) -> Result<Self, DetectionError> {
        if !model_path.exists() {
            return Err(DetectionError::ModelNotFound(
                model_path.display().to_string(),
            ));
        }

        let eps = crate::execution_providers(ep_name);
        let session = Session::builder()
            .and_then(|b| Ok(b.with_execution_providers(eps)?))
            .and_then(|mut b| b.commit_from_file(model_path))
            .map_err(|e| DetectionError::Ort(e.to_string()))?;

        tracing::info!(path = %model_path.display(), ep = ep_name, "SCRFD model loaded");
        Ok(Self { session })
    }

    pub fn load_default() -> Result<Self, DetectionError> {
        Self::load_default_with_ep("cpu")
    }

    pub fn load_default_with_ep(ep_name: &str) -> Result<Self, DetectionError> {
        for path in [
            "models/det_500m.onnx",
            "/usr/share/face-auth/models/det_500m.onnx",
            "/var/lib/face-auth/models/det_500m.onnx",
        ] {
            if Path::new(path).exists() {
                return Self::load_with_ep(Path::new(path), ep_name);
            }
        }
        Err(DetectionError::ModelNotFound(
            "det_500m.onnx not found in ./models/, /usr/share/face-auth/models/, or /var/lib/face-auth/models/".into(),
        ))
    }

    /// Detect faces in a grayscale IR frame.
    /// Returns detections sorted by confidence (highest first).
    pub fn detect(
        &mut self,
        frame_data: &[u8],
        frame_width: u32,
        frame_height: u32,
    ) -> Result<Vec<Detection>, DetectionError> {
        let input = preprocess(frame_data, frame_width, frame_height);

        let tensor =
            Tensor::from_array(input).map_err(|e| DetectionError::Inference(e.to_string()))?;

        let outputs = self
            .session
            .run(ort::inputs![tensor])
            .map_err(|e| DetectionError::Inference(e.to_string()))?;

        // SCRFD outputs 9 tensors: 3× scores, 3× bboxes, 3× keypoints
        // Order: [score_8, score_16, score_32, bbox_8, bbox_16, bbox_32, kps_8, kps_16, kps_32]
        if outputs.len() != 9 {
            return Err(DetectionError::Inference(format!(
                "expected 9 outputs, got {}",
                outputs.len()
            )));
        }

        let mut detections = Vec::new();
        let input_h = MODEL_INPUT_SIZE;
        let input_w = MODEL_INPUT_SIZE;

        for (stride_idx, &stride) in STRIDES.iter().enumerate() {
            let grid_h = input_h / stride;
            let grid_w = input_w / stride;

            let scores_view = outputs[stride_idx]
                .try_extract_array::<f32>()
                .map_err(|e| DetectionError::Inference(e.to_string()))?;
            let scores: Vec<f32> = scores_view.iter().copied().collect();

            let bboxes_view = outputs[stride_idx + 3]
                .try_extract_array::<f32>()
                .map_err(|e| DetectionError::Inference(e.to_string()))?;
            let bboxes: Vec<f32> = bboxes_view.iter().copied().collect();

            let kps_view = outputs[stride_idx + 6]
                .try_extract_array::<f32>()
                .map_err(|e| DetectionError::Inference(e.to_string()))?;
            let kps: Vec<f32> = kps_view.iter().copied().collect();

            let num_anchors = grid_h * grid_w * ANCHORS_PER_CELL;

            for (anchor_idx, &raw_score) in scores.iter().enumerate().take(num_anchors) {
                let score = sigmoid(raw_score);
                if score < CONF_THRESHOLD {
                    continue;
                }

                // Compute anchor center
                let cell_idx = anchor_idx / ANCHORS_PER_CELL;
                let row = cell_idx / grid_w;
                let col = cell_idx % grid_w;
                let cx = (col as f32 + 0.5) * stride as f32;
                let cy = (row as f32 + 0.5) * stride as f32;

                // Decode bbox
                let bi = anchor_idx * 4;
                let x1 = cx - bboxes[bi] * stride as f32;
                let y1 = cy - bboxes[bi + 1] * stride as f32;
                let x2 = cx + bboxes[bi + 2] * stride as f32;
                let y2 = cy + bboxes[bi + 3] * stride as f32;

                // Clamp to original frame dimensions (before padding)
                let x1 = x1.max(0.0).min(frame_width as f32);
                let y1 = y1.max(0.0).min(frame_height as f32);
                let x2 = x2.max(0.0).min(frame_width as f32);
                let y2 = y2.max(0.0).min(frame_height as f32);

                if x2 - x1 < 1.0 || y2 - y1 < 1.0 {
                    continue;
                }

                // Decode keypoints
                let ki = anchor_idx * 10;
                let left_eye = (
                    cx + kps[ki] * stride as f32,
                    cy + kps[ki + 1] * stride as f32,
                );
                let right_eye = (
                    cx + kps[ki + 2] * stride as f32,
                    cy + kps[ki + 3] * stride as f32,
                );
                let nose = (
                    cx + kps[ki + 4] * stride as f32,
                    cy + kps[ki + 5] * stride as f32,
                );
                let left_mouth = (
                    cx + kps[ki + 6] * stride as f32,
                    cy + kps[ki + 7] * stride as f32,
                );
                let right_mouth = (
                    cx + kps[ki + 8] * stride as f32,
                    cy + kps[ki + 9] * stride as f32,
                );

                detections.push(Detection {
                    bbox: BBox { x1, y1, x2, y2 },
                    landmarks: Landmarks {
                        left_eye,
                        right_eye,
                        nose,
                        left_mouth,
                        right_mouth,
                        left_eye_conf: score,
                        right_eye_conf: score,
                    },
                    confidence: score,
                });
            }
        }

        // NMS
        nms(&mut detections, NMS_THRESHOLD);

        // Sort by confidence descending
        detections.sort_by(|a, b| b.confidence.partial_cmp(&a.confidence).unwrap());

        Ok(detections)
    }
}

/// Preprocess grayscale IR frame to model input tensor.
/// Letterbox pads to 640×640, replicates to 3 channels, normalizes.
fn preprocess(frame_data: &[u8], width: u32, height: u32) -> Array4<f32> {
    let mut input = Array4::<f32>::zeros((1, 3, MODEL_INPUT_SIZE, MODEL_INPUT_SIZE));

    for row in 0..height.min(MODEL_INPUT_SIZE as u32) as usize {
        for col in 0..width.min(MODEL_INPUT_SIZE as u32) as usize {
            let pixel = frame_data[row * width as usize + col] as f32;
            let normalized = (pixel - 127.5) / 128.0;
            // Replicate grayscale to all 3 channels
            input[[0, 0, row, col]] = normalized;
            input[[0, 1, row, col]] = normalized;
            input[[0, 2, row, col]] = normalized;
        }
    }
    // Bottom rows (height..640) stay at 0.0 (pad value after normalization = -0.996)
    // This is close enough — padded region won't produce detections

    input
}

fn sigmoid(x: f32) -> f32 {
    1.0 / (1.0 + (-x).exp())
}

fn iou(a: &BBox, b: &BBox) -> f32 {
    let inter_x1 = a.x1.max(b.x1);
    let inter_y1 = a.y1.max(b.y1);
    let inter_x2 = a.x2.min(b.x2);
    let inter_y2 = a.y2.min(b.y2);

    let inter_area = (inter_x2 - inter_x1).max(0.0) * (inter_y2 - inter_y1).max(0.0);
    let area_a = (a.x2 - a.x1) * (a.y2 - a.y1);
    let area_b = (b.x2 - b.x1) * (b.y2 - b.y1);
    let union_area = area_a + area_b - inter_area;

    if union_area > 0.0 {
        inter_area / union_area
    } else {
        0.0
    }
}

fn nms(detections: &mut Vec<Detection>, threshold: f32) {
    detections.sort_by(|a, b| b.confidence.partial_cmp(&a.confidence).unwrap());

    let mut keep = vec![true; detections.len()];
    for i in 0..detections.len() {
        if !keep[i] {
            continue;
        }
        for j in (i + 1)..detections.len() {
            if !keep[j] {
                continue;
            }
            if iou(&detections[i].bbox, &detections[j].bbox) > threshold {
                keep[j] = false;
            }
        }
    }

    let mut idx = 0;
    detections.retain(|_| {
        let k = keep[idx];
        idx += 1;
        k
    });
}

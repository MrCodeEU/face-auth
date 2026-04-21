use crate::alignment::AlignedFace;
use ndarray::Array4;
use ort::session::Session;
use ort::value::Tensor;
use std::path::Path;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum RecognitionError {
    #[error("model file not found: {0}")]
    ModelNotFound(String),
    #[error("ort error: {0}")]
    Ort(String),
    #[error("inference error: {0}")]
    Inference(String),
}

pub struct FaceRecognizer {
    session: Session,
}

impl FaceRecognizer {
    pub fn load(model_path: &Path) -> Result<Self, RecognitionError> {
        Self::load_with_ep(model_path, "cpu")
    }

    pub fn load_with_ep(model_path: &Path, ep_name: &str) -> Result<Self, RecognitionError> {
        if !model_path.exists() {
            return Err(RecognitionError::ModelNotFound(
                model_path.display().to_string(),
            ));
        }

        let eps = crate::execution_providers(ep_name);
        let session = Session::builder()
            .and_then(|b| Ok(b.with_execution_providers(eps)?))
            .and_then(|mut b| b.commit_from_file(model_path))
            .map_err(|e| RecognitionError::Ort(e.to_string()))?;

        tracing::info!(path = %model_path.display(), ep = ep_name, "ArcFace model loaded");
        Ok(Self { session })
    }

    pub fn load_default() -> Result<Self, RecognitionError> {
        Self::load_default_with_ep("cpu")
    }

    pub fn load_default_with_ep(ep_name: &str) -> Result<Self, RecognitionError> {
        for path in [
            "models/w600k_mbf.onnx",
            "/usr/share/face-auth/models/w600k_mbf.onnx",
            "/var/lib/face-auth/models/w600k_mbf.onnx",
        ] {
            if Path::new(path).exists() {
                return Self::load_with_ep(Path::new(path), ep_name);
            }
        }
        Err(RecognitionError::ModelNotFound(
            "w600k_mbf.onnx not found in ./models/, /usr/share/face-auth/models/, or /var/lib/face-auth/models/".into(),
        ))
    }

    /// Extract a 512-dimensional L2-normalized embedding from an aligned face.
    pub fn embed(&mut self, face: &AlignedFace) -> Result<[f32; 512], RecognitionError> {
        let input = preprocess(face);

        let tensor = Tensor::from_array(input)
            .map_err(|e| RecognitionError::Inference(e.to_string()))?;

        let outputs = self
            .session
            .run(ort::inputs![tensor])
            .map_err(|e| RecognitionError::Inference(e.to_string()))?;

        let embedding_view = outputs[0]
            .try_extract_array::<f32>()
            .map_err(|e| RecognitionError::Inference(e.to_string()))?;

        let raw: Vec<f32> = embedding_view.iter().copied().collect();
        if raw.len() != 512 {
            return Err(RecognitionError::Inference(format!(
                "expected 512-dim embedding, got {}",
                raw.len()
            )));
        }

        // L2 normalize
        let mut embedding = [0.0f32; 512];
        embedding.copy_from_slice(&raw);
        l2_normalize(&mut embedding);

        Ok(embedding)
    }
}

/// Preprocess aligned 112×112 grayscale face for ArcFace input.
/// ArcFace expects [1, 3, 112, 112] float32, RGB normalized.
/// Since IR is grayscale, replicate to 3 channels.
/// MobileFaceNet w600k normalization: (pixel - 127.5) / 127.5
///
/// Applies histogram equalization first to normalize brightness/contrast,
/// making embeddings robust across different lighting conditions.
fn preprocess(face: &AlignedFace) -> Array4<f32> {
    // CLAHE: 8×8 tiles, clip_limit=2.0 — stable across lighting/framing changes
    let equalized = clahe(&face.data, 112, 112, 14, 2.0);
    let mut input = Array4::<f32>::zeros((1, 3, 112, 112));

    for row in 0..112 {
        for col in 0..112 {
            let pixel = equalized[row * 112 + col] as f32;
            let normalized = (pixel - 127.5) / 127.5;
            input[[0, 0, row, col]] = normalized;
            input[[0, 1, row, col]] = normalized;
            input[[0, 2, row, col]] = normalized;
        }
    }

    input
}

/// CLAHE (Contrast Limited Adaptive Histogram Equalization) on grayscale image.
/// More stable than global HE for face recognition — operates on local tiles
/// with clipped contrast, so small crop shifts don't wildly change the output.
pub fn clahe(data: &[u8], width: usize, height: usize, tile_size: usize, clip_limit: f32) -> Vec<u8> {
    let tiles_x = (width + tile_size - 1) / tile_size;
    let tiles_y = (height + tile_size - 1) / tile_size;

    // Build clipped histograms for each tile
    let mut tile_maps: Vec<Vec<u8>> = Vec::with_capacity(tiles_x * tiles_y);

    for ty in 0..tiles_y {
        for tx in 0..tiles_x {
            let x0 = tx * tile_size;
            let y0 = ty * tile_size;
            let x1 = (x0 + tile_size).min(width);
            let y1 = (y0 + tile_size).min(height);

            let mut hist = [0u32; 256];
            let mut count = 0u32;
            for row in y0..y1 {
                for col in x0..x1 {
                    hist[data[row * width + col] as usize] += 1;
                    count += 1;
                }
            }

            // Clip histogram and redistribute
            let clip = (clip_limit * count as f32 / 256.0).max(1.0) as u32;
            let mut excess = 0u32;
            for h in hist.iter_mut() {
                if *h > clip {
                    excess += *h - clip;
                    *h = clip;
                }
            }
            let bonus = excess / 256;
            let remainder = (excess % 256) as usize;
            for (i, h) in hist.iter_mut().enumerate() {
                *h += bonus;
                if i < remainder {
                    *h += 1;
                }
            }

            // Build CDF lookup table
            let mut cdf = [0u32; 256];
            cdf[0] = hist[0];
            for i in 1..256 {
                cdf[i] = cdf[i - 1] + hist[i];
            }
            let cdf_min = cdf.iter().copied().find(|&v| v > 0).unwrap_or(0) as f32;
            let total = cdf[255] as f32;
            let scale = 255.0 / (total - cdf_min).max(1.0);

            let map: Vec<u8> = (0..256)
                .map(|i| ((cdf[i] as f32 - cdf_min) * scale).round().clamp(0.0, 255.0) as u8)
                .collect();
            tile_maps.push(map);
        }
    }

    // Bilinear interpolation between tile maps for smooth output
    let mut output = vec![0u8; width * height];
    let half = tile_size as f32 / 2.0;

    for row in 0..height {
        for col in 0..width {
            // Find position relative to tile centers
            let fx = (col as f32 - half) / tile_size as f32;
            let fy = (row as f32 - half) / tile_size as f32;

            let tx0 = (fx.floor() as isize).clamp(0, tiles_x as isize - 1) as usize;
            let ty0 = (fy.floor() as isize).clamp(0, tiles_y as isize - 1) as usize;
            let tx1 = (tx0 + 1).min(tiles_x - 1);
            let ty1 = (ty0 + 1).min(tiles_y - 1);

            let ax = (fx - tx0 as f32).clamp(0.0, 1.0);
            let ay = (fy - ty0 as f32).clamp(0.0, 1.0);

            let pixel = data[row * width + col] as usize;
            let v00 = tile_maps[ty0 * tiles_x + tx0][pixel] as f32;
            let v10 = tile_maps[ty0 * tiles_x + tx1][pixel] as f32;
            let v01 = tile_maps[ty1 * tiles_x + tx0][pixel] as f32;
            let v11 = tile_maps[ty1 * tiles_x + tx1][pixel] as f32;

            let val = v00 * (1.0 - ax) * (1.0 - ay)
                + v10 * ax * (1.0 - ay)
                + v01 * (1.0 - ax) * ay
                + v11 * ax * ay;

            output[row * width + col] = val.round().clamp(0.0, 255.0) as u8;
        }
    }

    output
}

fn l2_normalize(v: &mut [f32; 512]) {
    let norm: f32 = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm > 1e-10 {
        for x in v.iter_mut() {
            *x /= norm;
        }
    }
}

/// Cosine similarity between two L2-normalized embeddings (= dot product).
pub fn cosine_similarity(a: &[f32; 512], b: &[f32; 512]) -> f32 {
    a.iter().zip(b.iter()).map(|(x, y)| x * y).sum()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn l2_normalize_unit_vector() {
        let mut v = [0.0f32; 512];
        v[0] = 3.0;
        v[1] = 4.0;
        l2_normalize(&mut v);
        assert!((v[0] - 0.6).abs() < 1e-6);
        assert!((v[1] - 0.8).abs() < 1e-6);

        let norm: f32 = v.iter().map(|x| x * x).sum::<f32>().sqrt();
        assert!((norm - 1.0).abs() < 1e-5);
    }

    #[test]
    fn cosine_similarity_identical() {
        let mut a = [0.0f32; 512];
        a[0] = 1.0;
        assert!((cosine_similarity(&a, &a) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn cosine_similarity_orthogonal() {
        let mut a = [0.0f32; 512];
        let mut b = [0.0f32; 512];
        a[0] = 1.0;
        b[1] = 1.0;
        assert!(cosine_similarity(&a, &b).abs() < 1e-6);
    }

    #[test]
    fn clahe_spreads_dark_image() {
        // Dark image: all pixels in 10-30 range
        let data: Vec<u8> = (0..112 * 112).map(|i| 10 + (i % 21) as u8).collect();
        let eq = clahe(&data, 112, 112, 14, 2.0);
        let min = *eq.iter().min().unwrap();
        let max = *eq.iter().max().unwrap();
        // CLAHE with clip_limit=2.0 intentionally limits contrast amplification
        // but should still spread the range meaningfully beyond the original 20-pixel span
        assert!(max - min > 40, "expected spread >40, got {}-{}", min, max);
    }

    #[test]
    fn clahe_stable_under_small_shift() {
        // Same pattern, shifted by 1 pixel — CLAHE output should be similar
        let make_face = |offset: usize| -> Vec<u8> {
            (0..112 * 112)
                .map(|i| (((i + offset) * 7 + 50) % 200) as u8)
                .collect()
        };
        let eq1 = clahe(&make_face(0), 112, 112, 14, 2.0);
        let eq2 = clahe(&make_face(1), 112, 112, 14, 2.0);
        // Mean absolute difference should be small
        let mad: f32 = eq1
            .iter()
            .zip(eq2.iter())
            .map(|(&a, &b)| (a as f32 - b as f32).abs())
            .sum::<f32>()
            / (112 * 112) as f32;
        assert!(
            mad < 20.0,
            "CLAHE should be stable under small shifts, MAD={mad}"
        );
    }
}

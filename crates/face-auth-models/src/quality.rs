use face_auth_core::geometry::BBox;

/// Check if face ROI has IR saturation (>30% pixels above 250).
/// IR cameras naturally reflect off foreheads/noses — needs generous threshold.
pub fn ir_saturated(frame_data: &[u8], bbox: &BBox, frame_width: u32) -> bool {
    let (total, saturated) = count_roi_pixels(frame_data, bbox, frame_width, |p| p > 250);
    if total == 0 {
        return false;
    }
    (saturated as f32 / total as f32) > 0.30
}

/// Compute blur score via Laplacian variance of face ROI.
/// Higher = sharper. < 50.0 = too blurry.
pub fn blur_score(frame_data: &[u8], bbox: &BBox, frame_width: u32, frame_height: u32) -> f32 {
    // Crop face ROI and resize to ~64×64 for blur analysis
    let x1 = (bbox.x1 as u32).min(frame_width.saturating_sub(1));
    let y1 = (bbox.y1 as u32).min(frame_height.saturating_sub(1));
    let x2 = (bbox.x2 as u32).min(frame_width);
    let y2 = (bbox.y2 as u32).min(frame_height);

    let roi_w = (x2 - x1) as usize;
    let roi_h = (y2 - y1) as usize;

    if roi_w < 3 || roi_h < 3 {
        return 0.0;
    }

    // Extract ROI pixels
    let mut roi = Vec::with_capacity(roi_w * roi_h);
    for row in y1..y2 {
        for col in x1..x2 {
            roi.push(frame_data[(row * frame_width + col) as usize] as f32);
        }
    }

    // Apply Laplacian kernel: [[0,1,0],[1,-4,1],[0,1,0]]
    let mut laplacian_values = Vec::with_capacity((roi_w - 2) * (roi_h - 2));
    for row in 1..roi_h - 1 {
        for col in 1..roi_w - 1 {
            let center = roi[row * roi_w + col];
            let up = roi[(row - 1) * roi_w + col];
            let down = roi[(row + 1) * roi_w + col];
            let left = roi[row * roi_w + (col - 1)];
            let right = roi[row * roi_w + (col + 1)];
            let lap = up + down + left + right - 4.0 * center;
            laplacian_values.push(lap);
        }
    }

    if laplacian_values.is_empty() {
        return 0.0;
    }

    // Variance of Laplacian
    let mean = laplacian_values.iter().sum::<f32>() / laplacian_values.len() as f32;
    let variance = laplacian_values
        .iter()
        .map(|v| (v - mean) * (v - mean))
        .sum::<f32>()
        / laplacian_values.len() as f32;

    variance
}

/// IR-specific liveness scores based on texture analysis.
#[derive(Debug, Clone)]
pub struct IrLivenessScores {
    /// LBP (Local Binary Pattern) entropy of face ROI.
    /// Real skin: high entropy (diverse micro-texture, ~5.0–7.0).
    /// Screen/photo: low entropy (flat/uniform, ~2.0–4.0).
    pub lbp_entropy: f32,
    /// Coefficient of variation of local patch standard deviations.
    /// Real 3D face: high CV (eyes vs cheeks vs nose vary, ~0.3–0.8).
    /// Flat screen: low CV (uniform intensity distribution, ~0.05–0.25).
    pub local_contrast_cv: f32,
}

impl IrLivenessScores {
    pub fn is_live(
        &self,
        lbp_entropy_min: f32,
        local_contrast_cv_min: f32,
        local_contrast_cv_max: f32,
    ) -> bool {
        self.lbp_entropy >= lbp_entropy_min
            && self.local_contrast_cv >= local_contrast_cv_min
            && self.local_contrast_cv <= local_contrast_cv_max
    }
}

/// Compute IR texture liveness scores for a face ROI.
/// Uses LBP entropy and local contrast variance to distinguish real faces
/// from photos/screens under IR illumination.
pub fn ir_liveness_check(
    frame_data: &[u8],
    bbox: &BBox,
    frame_width: u32,
    frame_height: u32,
) -> IrLivenessScores {
    let roi = extract_roi(frame_data, bbox, frame_width, frame_height);
    let roi_w = roi.1;
    let roi_h = roi.2;
    let pixels = &roi.0;

    let lbp_entropy = compute_lbp_entropy(pixels, roi_w, roi_h);
    let local_contrast_cv = compute_local_contrast_cv(pixels, roi_w, roi_h);

    IrLivenessScores {
        lbp_entropy,
        local_contrast_cv,
    }
}

/// Extract face ROI pixels as (Vec<u8>, width, height).
fn extract_roi(
    frame_data: &[u8],
    bbox: &BBox,
    frame_width: u32,
    frame_height: u32,
) -> (Vec<u8>, usize, usize) {
    let x1 = (bbox.x1.max(0.0) as u32).min(frame_width.saturating_sub(1));
    let y1 = (bbox.y1.max(0.0) as u32).min(frame_height.saturating_sub(1));
    let x2 = (bbox.x2 as u32).min(frame_width);
    let y2 = (bbox.y2 as u32).min(frame_height);

    let roi_w = (x2 - x1) as usize;
    let roi_h = (y2 - y1) as usize;

    let mut pixels = Vec::with_capacity(roi_w * roi_h);
    for row in y1..y2 {
        for col in x1..x2 {
            pixels.push(frame_data[(row * frame_width + col) as usize]);
        }
    }
    (pixels, roi_w, roi_h)
}

/// Compute LBP entropy of an ROI.
/// LBP compares each pixel to its 8 neighbors, producing an 8-bit pattern.
/// Entropy of the pattern histogram measures texture diversity.
fn compute_lbp_entropy(pixels: &[u8], width: usize, height: usize) -> f32 {
    if width < 3 || height < 3 {
        return 0.0;
    }

    let mut histogram = [0u32; 256];
    let mut total = 0u32;

    for row in 1..height - 1 {
        for col in 1..width - 1 {
            let center = pixels[row * width + col];
            let mut pattern: u8 = 0;

            // 8 neighbors clockwise from right
            if pixels[row * width + col + 1] >= center {
                pattern |= 1;
            }
            if pixels[(row + 1) * width + col + 1] >= center {
                pattern |= 2;
            }
            if pixels[(row + 1) * width + col] >= center {
                pattern |= 4;
            }
            if pixels[(row + 1) * width + col - 1] >= center {
                pattern |= 8;
            }
            if pixels[row * width + col - 1] >= center {
                pattern |= 16;
            }
            if pixels[(row - 1) * width + col - 1] >= center {
                pattern |= 32;
            }
            if pixels[(row - 1) * width + col] >= center {
                pattern |= 64;
            }
            if pixels[(row - 1) * width + col + 1] >= center {
                pattern |= 128;
            }

            histogram[pattern as usize] += 1;
            total += 1;
        }
    }

    if total == 0 {
        return 0.0;
    }

    // Shannon entropy
    let mut entropy: f32 = 0.0;
    for &count in &histogram {
        if count > 0 {
            let p = count as f32 / total as f32;
            entropy -= p * p.log2();
        }
    }
    entropy
}

/// Compute coefficient of variation of local patch standard deviations.
/// Divides ROI into patches, computes std dev per patch, then CV of those.
fn compute_local_contrast_cv(pixels: &[u8], width: usize, height: usize) -> f32 {
    const PATCH_SIZE: usize = 16;

    if width < PATCH_SIZE || height < PATCH_SIZE {
        return 0.0;
    }

    let patches_x = width / PATCH_SIZE;
    let patches_y = height / PATCH_SIZE;

    if patches_x == 0 || patches_y == 0 {
        return 0.0;
    }

    let mut patch_stddevs: Vec<f32> = Vec::with_capacity(patches_x * patches_y);

    for py in 0..patches_y {
        for px in 0..patches_x {
            let start_x = px * PATCH_SIZE;
            let start_y = py * PATCH_SIZE;

            let mut sum: f32 = 0.0;
            let mut sum_sq: f32 = 0.0;
            let n = (PATCH_SIZE * PATCH_SIZE) as f32;

            for dy in 0..PATCH_SIZE {
                for dx in 0..PATCH_SIZE {
                    let val = pixels[(start_y + dy) * width + (start_x + dx)] as f32;
                    sum += val;
                    sum_sq += val * val;
                }
            }

            let mean = sum / n;
            let variance = (sum_sq / n) - (mean * mean);
            patch_stddevs.push(variance.max(0.0).sqrt());
        }
    }

    if patch_stddevs.len() < 2 {
        return 0.0;
    }

    // Coefficient of variation = stddev(patch_stddevs) / mean(patch_stddevs)
    let n = patch_stddevs.len() as f32;
    let mean: f32 = patch_stddevs.iter().sum::<f32>() / n;

    if mean < 1.0 {
        return 0.0; // Nearly uniform image
    }

    let variance: f32 = patch_stddevs
        .iter()
        .map(|s| (s - mean) * (s - mean))
        .sum::<f32>()
        / n;
    variance.sqrt() / mean
}

fn count_roi_pixels(
    frame_data: &[u8],
    bbox: &BBox,
    frame_width: u32,
    predicate: impl Fn(u8) -> bool,
) -> (usize, usize) {
    let x1 = bbox.x1.max(0.0) as u32;
    let y1 = bbox.y1.max(0.0) as u32;
    let x2 = (bbox.x2 as u32).min(frame_width);
    let y2 = bbox.y2.max(0.0) as u32;

    let frame_height = frame_data.len() as u32 / frame_width;
    let y2 = y2.min(frame_height);

    let mut total = 0usize;
    let mut count = 0usize;

    for row in y1..y2 {
        for col in x1..x2 {
            let pixel = frame_data[(row * frame_width + col) as usize];
            total += 1;
            if predicate(pixel) {
                count += 1;
            }
        }
    }

    (total, count)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn not_saturated() {
        let data = vec![128u8; 640 * 360];
        let bbox = BBox {
            x1: 100.0,
            y1: 100.0,
            x2: 200.0,
            y2: 200.0,
        };
        assert!(!ir_saturated(&data, &bbox, 640));
    }

    #[test]
    fn saturated_roi() {
        let mut data = vec![128u8; 640 * 360];
        // Fill ROI with 255
        for row in 100..200 {
            for col in 100..200 {
                data[row * 640 + col] = 255;
            }
        }
        let bbox = BBox {
            x1: 100.0,
            y1: 100.0,
            x2: 200.0,
            y2: 200.0,
        };
        assert!(ir_saturated(&data, &bbox, 640));
    }

    #[test]
    fn sharp_image_high_blur_score() {
        // Checkerboard pattern = high frequency = high blur score
        let mut data = vec![0u8; 640 * 360];
        for row in 100..200 {
            for col in 100..200 {
                data[row * 640 + col] = if (row + col) % 2 == 0 { 200 } else { 50 };
            }
        }
        let bbox = BBox {
            x1: 100.0,
            y1: 100.0,
            x2: 200.0,
            y2: 200.0,
        };
        let score = blur_score(&data, &bbox, 640, 360);
        assert!(score > 50.0, "expected sharp score, got {score}");
    }

    #[test]
    fn uniform_image_low_blur_score() {
        let data = vec![128u8; 640 * 360];
        let bbox = BBox {
            x1: 100.0,
            y1: 100.0,
            x2: 200.0,
            y2: 200.0,
        };
        let score = blur_score(&data, &bbox, 640, 360);
        assert!(score < 1.0, "expected blurry score, got {score}");
    }

    // --- IR liveness tests ---

    #[test]
    fn lbp_entropy_textured_image_high() {
        // Spatially varied texture → high LBP entropy
        let mut data = vec![0u8; 640 * 360];
        for row in 0..360 {
            for col in 0..640 {
                // XOR pattern creates diverse spatial neighborhoods
                let v = ((row * 7 + col * 13) ^ (row * col)) as u8;
                data[row * 640 + col] = v;
            }
        }
        let bbox = BBox {
            x1: 100.0,
            y1: 100.0,
            x2: 260.0,
            y2: 260.0,
        };
        let scores = ir_liveness_check(&data, &bbox, 640, 360);
        assert!(
            scores.lbp_entropy > 5.0,
            "textured image should have high LBP entropy, got {}",
            scores.lbp_entropy
        );
    }

    #[test]
    fn lbp_entropy_uniform_image_low() {
        // Uniform → low LBP entropy
        let data = vec![128u8; 640 * 360];
        let bbox = BBox {
            x1: 100.0,
            y1: 100.0,
            x2: 260.0,
            y2: 260.0,
        };
        let scores = ir_liveness_check(&data, &bbox, 640, 360);
        assert!(
            scores.lbp_entropy < 1.0,
            "uniform image should have low LBP entropy, got {}",
            scores.lbp_entropy
        );
    }

    #[test]
    fn local_contrast_cv_varied_patches() {
        // Some patches with high internal variance (textured), some uniform → high CV
        let mut data = vec![128u8; 640 * 360];
        // Add noisy texture to some patches (top-left quadrant of ROI)
        for row in 100..164 {
            for col in 100..164 {
                // Checkerboard within patch → high std dev
                data[row * 640 + col] = if (row + col) % 2 == 0 { 40 } else { 220 };
            }
        }
        // Bottom-right quadrant stays uniform (128) → low std dev
        // This creates patches with very different internal variances
        let bbox = BBox {
            x1: 100.0,
            y1: 100.0,
            x2: 260.0,
            y2: 260.0,
        };
        let scores = ir_liveness_check(&data, &bbox, 640, 360);
        assert!(
            scores.local_contrast_cv > 0.3,
            "varied image should have high contrast CV, got {}",
            scores.local_contrast_cv
        );
    }

    #[test]
    fn local_contrast_cv_uniform_low() {
        let data = vec![128u8; 640 * 360];
        let bbox = BBox {
            x1: 100.0,
            y1: 100.0,
            x2: 260.0,
            y2: 260.0,
        };
        let scores = ir_liveness_check(&data, &bbox, 640, 360);
        assert!(
            scores.local_contrast_cv < 0.1,
            "uniform image should have low contrast CV, got {}",
            scores.local_contrast_cv
        );
    }

    #[test]
    fn ir_liveness_scores_is_live() {
        let live = IrLivenessScores {
            lbp_entropy: 6.0,
            local_contrast_cv: 0.35,
        };
        assert!(live.is_live(5.5, 0.20, 0.50));
        assert!(!live.is_live(7.0, 0.20, 0.50)); // entropy too low
        assert!(!live.is_live(5.5, 0.40, 0.50)); // contrast too low

        // CV above max → spoof (screen edge artifacts)
        let screen = IrLivenessScores {
            lbp_entropy: 6.5,
            local_contrast_cv: 1.2,
        };
        assert!(!screen.is_live(5.5, 0.20, 0.50)); // cv exceeds max
    }
}

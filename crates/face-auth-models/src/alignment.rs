use face_auth_core::geometry::Landmarks;
use nalgebra::{Matrix2, Vector2};

/// A 112×112 face crop aligned for ArcFace input.
pub struct AlignedFace {
    /// 112×112 grayscale pixels (row-major).
    pub data: Vec<u8>,
    pub width: u32,
    pub height: u32,
}

/// ArcFace canonical target positions for 112×112 crop.
const ARCFACE_TARGETS: [(f32, f32); 5] = [
    (38.2946, 51.6963),  // left eye
    (73.5318, 51.5014),  // right eye
    (56.0252, 71.7366),  // nose tip
    (41.5493, 92.3655),  // left mouth corner
    (70.7299, 92.2041),  // right mouth corner
];

/// Align a grayscale frame using 5-point landmarks to produce a 112×112 crop
/// suitable for ArcFace recognition.
pub fn align_face(
    frame_data: &[u8],
    frame_width: u32,
    frame_height: u32,
    landmarks: &Landmarks,
) -> AlignedFace {
    let src = [
        landmarks.left_eye,
        landmarks.right_eye,
        landmarks.nose,
        landmarks.left_mouth,
        landmarks.right_mouth,
    ];

    // Compute similarity transform (scale + rotation + translation)
    let (rot, scale, tx, ty) = compute_similarity_transform(&src, &ARCFACE_TARGETS);

    // Inverse warp: for each output pixel, find source pixel
    let out_size = 112usize;
    let mut output = vec![0u8; out_size * out_size];

    // Inverse of similarity transform: M = s*R, t
    // M_inv = R^T / s, -R^T * t / s
    let inv_scale = 1.0 / scale;
    let inv_rot = Matrix2::new(rot[(0, 0)], rot[(1, 0)], rot[(0, 1)], rot[(1, 1)]); // transpose
    let inv_t = -(inv_rot * Vector2::new(tx, ty)) * inv_scale;

    for out_y in 0..out_size {
        for out_x in 0..out_size {
            let dst = Vector2::new(out_x as f32, out_y as f32);
            let src_pos = inv_rot * dst * inv_scale + inv_t;

            let sx = src_pos.x;
            let sy = src_pos.y;

            // Bilinear interpolation
            let pixel = bilinear_sample(frame_data, frame_width, frame_height, sx, sy);
            output[out_y * out_size + out_x] = pixel;
        }
    }

    AlignedFace {
        data: output,
        width: 112,
        height: 112,
    }
}

/// Compute similarity transform from src points to dst points.
/// Returns (rotation_matrix, scale, tx, ty).
fn compute_similarity_transform(
    src: &[(f32, f32); 5],
    dst: &[(f32, f32); 5],
) -> (Matrix2<f32>, f32, f32, f32) {
    // Compute centroids
    let src_cx: f32 = src.iter().map(|p| p.0).sum::<f32>() / 5.0;
    let src_cy: f32 = src.iter().map(|p| p.1).sum::<f32>() / 5.0;
    let dst_cx: f32 = dst.iter().map(|p| p.0).sum::<f32>() / 5.0;
    let dst_cy: f32 = dst.iter().map(|p| p.1).sum::<f32>() / 5.0;

    // Center points
    let src_centered: Vec<(f32, f32)> = src.iter().map(|p| (p.0 - src_cx, p.1 - src_cy)).collect();
    let dst_centered: Vec<(f32, f32)> = dst.iter().map(|p| (p.0 - dst_cx, p.1 - dst_cy)).collect();

    // Compute scale: ratio of RMS distances
    let src_rms = (src_centered.iter().map(|p| p.0 * p.0 + p.1 * p.1).sum::<f32>() / 5.0).sqrt();
    let dst_rms = (dst_centered.iter().map(|p| p.0 * p.0 + p.1 * p.1).sum::<f32>() / 5.0).sqrt();
    let scale = if src_rms > 1e-6 { dst_rms / src_rms } else { 1.0 };

    // Compute rotation angle using Procrustes (cross-covariance)
    let mut num = 0.0f32; // sin component
    let mut den = 0.0f32; // cos component
    for i in 0..5 {
        let (sx, sy) = src_centered[i];
        let (dx, dy) = dst_centered[i];
        den += sx * dx + sy * dy;
        num += sx * dy - sy * dx;
    }
    let angle = num.atan2(den);

    let cos_a = angle.cos();
    let sin_a = angle.sin();
    let rot = Matrix2::new(cos_a, -sin_a, sin_a, cos_a);

    // Translation: dst_center = scale * R * src_center + t
    let tx = dst_cx - scale * (cos_a * src_cx - sin_a * src_cy);
    let ty = dst_cy - scale * (sin_a * src_cx + cos_a * src_cy);

    (rot, scale, tx, ty)
}

fn bilinear_sample(data: &[u8], width: u32, height: u32, x: f32, y: f32) -> u8 {
    let x0 = x.floor() as i32;
    let y0 = y.floor() as i32;
    let x1 = x0 + 1;
    let y1 = y0 + 1;

    let fx = x - x0 as f32;
    let fy = y - y0 as f32;

    let get = |px: i32, py: i32| -> f32 {
        if px < 0 || py < 0 || px >= width as i32 || py >= height as i32 {
            return 0.0;
        }
        data[(py as u32 * width + px as u32) as usize] as f32
    };

    let val = get(x0, y0) * (1.0 - fx) * (1.0 - fy)
        + get(x1, y0) * fx * (1.0 - fy)
        + get(x0, y1) * (1.0 - fx) * fy
        + get(x1, y1) * fx * fy;

    val.round().clamp(0.0, 255.0) as u8
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identity_transform_preserves_points() {
        // When src == dst, transform should be ~identity
        let points: [(f32, f32); 5] = ARCFACE_TARGETS;
        let (rot, scale, tx, ty) = compute_similarity_transform(&points, &points);
        assert!((scale - 1.0).abs() < 0.01, "scale should be ~1.0, got {scale}");
        assert!(tx.abs() < 0.5, "tx should be ~0, got {tx}");
        assert!(ty.abs() < 0.5, "ty should be ~0, got {ty}");
        assert!((rot[(0, 0)] - 1.0).abs() < 0.01);
    }

    #[test]
    fn align_produces_correct_size() {
        let data = vec![128u8; 640 * 360];
        let landmarks = Landmarks {
            left_eye: (200.0, 150.0),
            right_eye: (280.0, 150.0),
            nose: (240.0, 190.0),
            left_mouth: (210.0, 220.0),
            right_mouth: (270.0, 220.0),
            left_eye_conf: 0.9,
            right_eye_conf: 0.9,
        };
        let aligned = align_face(&data, 640, 360, &landmarks);
        assert_eq!(aligned.width, 112);
        assert_eq!(aligned.height, 112);
        assert_eq!(aligned.data.len(), 112 * 112);
    }
}

use crate::config::GeometryConfig;
use crate::protocol::FeedbackState;
use std::time::{Duration, Instant};

/// 5-point facial landmarks from SCRFD (pixel coordinates).
#[derive(Debug, Clone)]
pub struct Landmarks {
    pub left_eye: (f32, f32),
    pub right_eye: (f32, f32),
    pub nose: (f32, f32),
    pub left_mouth: (f32, f32),
    pub right_mouth: (f32, f32),
    pub left_eye_conf: f32,
    pub right_eye_conf: f32,
}

/// Bounding box from SCRFD.
#[derive(Debug, Clone)]
pub struct BBox {
    pub x1: f32,
    pub y1: f32,
    pub x2: f32,
    pub y2: f32,
}

impl BBox {
    pub fn width(&self) -> f32 {
        self.x2 - self.x1
    }
}

/// Computed geometry metrics for a detected face.
#[derive(Debug, Clone)]
pub struct FaceMetrics {
    pub face_width_ratio: f32,
    pub yaw_deg: f32,
    pub pitch_deg: f32,
    pub roll_deg: f32,
    pub ir_saturated: bool,
    pub eyes_visible: bool,
    pub blur_score: f32,
}

/// Auth state machine states.
#[derive(Debug, Clone, PartialEq)]
pub enum AuthState {
    Idle,
    Guidance(FeedbackState),
    Authenticating,
    Done,
}

pub struct StateMachine {
    pub state: AuthState,
    last_guidance_change: Option<Instant>,
    debounce: Duration,
    geo_config: GeometryConfig,
}

impl StateMachine {
    pub fn new(geo_config: &GeometryConfig) -> Self {
        Self {
            state: AuthState::Idle,
            last_guidance_change: None,
            debounce: Duration::from_millis(geo_config.guidance_debounce_ms),
            geo_config: GeometryConfig {
                distance_min: geo_config.distance_min,
                distance_max: geo_config.distance_max,
                yaw_max_deg: geo_config.yaw_max_deg,
                pitch_max_deg: geo_config.pitch_max_deg,
                roll_max_deg: geo_config.roll_max_deg,
                guidance_debounce_ms: geo_config.guidance_debounce_ms,
            },
        }
    }

    pub fn transition(
        &mut self,
        metrics: Option<&FaceMetrics>,
        now: Instant,
    ) -> Option<FeedbackState> {
        // Done is terminal
        if self.state == AuthState::Done {
            return None;
        }

        let desired = match metrics {
            None => FeedbackState::Scanning,
            Some(m) => classify(m, &self.geo_config),
        };

        // Authenticating transitions bypass debounce
        if desired == FeedbackState::Authenticating {
            self.state = AuthState::Authenticating;
            return Some(FeedbackState::Authenticating);
        }

        // Debounce guidance changes
        let should_emit = match &self.state {
            AuthState::Guidance(current) if *current == desired => false,
            _ => {
                let elapsed = self
                    .last_guidance_change
                    .map(|t| now.duration_since(t))
                    .unwrap_or(Duration::MAX);
                elapsed >= self.debounce
            }
        };

        if should_emit {
            self.state = AuthState::Guidance(desired.clone());
            self.last_guidance_change = Some(now);
            Some(desired)
        } else {
            None
        }
    }

    pub fn finish(&mut self) {
        self.state = AuthState::Done;
    }
}

/// Compute FeedbackState from metrics using config thresholds.
fn classify(m: &FaceMetrics, cfg: &GeometryConfig) -> FeedbackState {
    if m.ir_saturated {
        return FeedbackState::IRSaturated;
    }
    if !m.eyes_visible {
        return FeedbackState::EyesNotVisible;
    }
    if m.face_width_ratio < cfg.distance_min {
        return FeedbackState::TooFar;
    }
    if m.face_width_ratio > cfg.distance_max {
        return FeedbackState::TooClose;
    }
    if m.roll_deg.abs() > cfg.roll_max_deg {
        return FeedbackState::LookAtCamera;
    }
    if m.yaw_deg > cfg.yaw_max_deg {
        return FeedbackState::TurnLeft; // turned right → tell user to turn left
    }
    if m.yaw_deg < -cfg.yaw_max_deg {
        return FeedbackState::TurnRight; // turned left → tell user to turn right
    }
    if m.pitch_deg > cfg.pitch_max_deg {
        return FeedbackState::TiltUp; // looking down → tell user to tilt up
    }
    if m.pitch_deg < -cfg.pitch_max_deg {
        return FeedbackState::TiltDown; // looking up → tell user to tilt down
    }
    FeedbackState::Authenticating
}

/// Compute FaceMetrics from SCRFD landmarks + bbox.
pub fn analyze_geometry(
    landmarks: &Landmarks,
    bbox: &BBox,
    frame_width: u32,
    _frame_height: u32,
) -> FaceMetrics {
    let face_width_ratio = bbox.width() / frame_width as f32;

    let left_offset = landmarks.nose.0 - landmarks.left_eye.0;
    let right_offset = landmarks.right_eye.0 - landmarks.nose.0;
    let yaw_deg = if left_offset + right_offset > 0.0 {
        ((left_offset - right_offset) / (left_offset + right_offset)) * 45.0
    } else {
        0.0
    };

    let eye_mid_y = (landmarks.left_eye.1 + landmarks.right_eye.1) / 2.0;
    let mouth_mid_y = (landmarks.left_mouth.1 + landmarks.right_mouth.1) / 2.0;
    let face_height = mouth_mid_y - eye_mid_y;
    let pitch_deg = if face_height > 0.0 {
        ((landmarks.nose.1 - eye_mid_y) / face_height - 0.40) * 100.0
    } else {
        0.0
    };

    let dx = landmarks.right_eye.0 - landmarks.left_eye.0;
    let dy = landmarks.right_eye.1 - landmarks.left_eye.1;
    let roll_deg = dy.atan2(dx).to_degrees();

    let eyes_visible = landmarks.left_eye_conf > 0.5 && landmarks.right_eye_conf > 0.5;

    FaceMetrics {
        face_width_ratio,
        yaw_deg,
        pitch_deg,
        roll_deg,
        ir_saturated: false, // computed from pixel histogram in Phase 4
        eyes_visible,
        blur_score: 100.0, // computed from Laplacian in Phase 4
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn default_cfg() -> GeometryConfig {
        GeometryConfig::default()
    }

    fn landmarks_frontal() -> (Landmarks, BBox) {
        (
            Landmarks {
                left_eye: (100.0, 150.0),
                right_eye: (180.0, 150.0),
                nose: (140.0, 180.0),
                left_mouth: (110.0, 220.0),
                right_mouth: (170.0, 220.0),
                left_eye_conf: 0.95,
                right_eye_conf: 0.95,
            },
            BBox {
                x1: 80.0,
                y1: 120.0,
                x2: 200.0,
                y2: 250.0,
            },
        )
    }

    // --- analyze_geometry tests ---

    #[test]
    fn frontal_face_authenticates() {
        let cfg = default_cfg();
        let (lm, bbox) = landmarks_frontal();
        let m = analyze_geometry(&lm, &bbox, 640, 480);
        assert!(m.face_width_ratio > cfg.distance_min && m.face_width_ratio < cfg.distance_max);
        assert!(m.yaw_deg.abs() < cfg.yaw_max_deg);
        assert!(m.pitch_deg.abs() < cfg.pitch_max_deg);
        assert!(m.roll_deg.abs() < cfg.roll_max_deg);
        assert!(m.eyes_visible);
        assert_eq!(classify(&m, &cfg), FeedbackState::Authenticating);
    }

    #[test]
    fn face_too_far() {
        let (lm, mut bbox) = landmarks_frontal();
        bbox.x2 = bbox.x1 + 30.0; // narrow bbox → small ratio
        let m = analyze_geometry(&lm, &bbox, 640, 480);
        assert_eq!(classify(&m, &default_cfg()), FeedbackState::TooFar);
    }

    #[test]
    fn face_too_close() {
        let (lm, mut bbox) = landmarks_frontal();
        bbox.x2 = bbox.x1 + 400.0; // wide bbox
        let m = analyze_geometry(&lm, &bbox, 640, 480);
        assert_eq!(classify(&m, &default_cfg()), FeedbackState::TooClose);
    }

    #[test]
    fn face_at_distance_boundary_min() {
        let (lm, mut bbox) = landmarks_frontal();
        // Exactly at min threshold → should be TooFar (< not <=)
        bbox.x2 = bbox.x1 + 640.0 * 0.06 - 1.0;
        let m = analyze_geometry(&lm, &bbox, 640, 480);
        assert_eq!(classify(&m, &default_cfg()), FeedbackState::TooFar);
    }

    #[test]
    fn face_at_distance_boundary_max() {
        let (lm, mut bbox) = landmarks_frontal();
        // Just above max threshold → TooClose
        bbox.x2 = bbox.x1 + 640.0 * 0.55 + 1.0;
        let m = analyze_geometry(&lm, &bbox, 640, 480);
        assert_eq!(classify(&m, &default_cfg()), FeedbackState::TooClose);
    }

    #[test]
    fn yaw_right() {
        let (mut lm, bbox) = landmarks_frontal();
        // Nose shifted far right → face turned right → tell user to turn left
        lm.nose.0 = 190.0;
        let m = analyze_geometry(&lm, &bbox, 640, 480);
        assert_eq!(classify(&m, &default_cfg()), FeedbackState::TurnLeft);
    }

    #[test]
    fn yaw_left() {
        let (mut lm, bbox) = landmarks_frontal();
        // Nose shifted far left → face turned left → tell user to turn right
        lm.nose.0 = 90.0;
        let m = analyze_geometry(&lm, &bbox, 640, 480);
        assert_eq!(classify(&m, &default_cfg()), FeedbackState::TurnRight);
    }

    #[test]
    fn pitch_down() {
        let (mut lm, bbox) = landmarks_frontal();
        // Nose extremely low → looking way down → tell user to tilt up
        lm.nose.1 = 250.0;
        let m = analyze_geometry(&lm, &bbox, 640, 480);
        assert!(
            m.pitch_deg > 45.0,
            "pitch_deg={} should be >45",
            m.pitch_deg
        );
        assert_eq!(classify(&m, &default_cfg()), FeedbackState::TiltUp);
    }

    #[test]
    fn pitch_up() {
        let (mut lm, bbox) = landmarks_frontal();
        // Nose above eyes → looking way up → tell user to tilt down
        lm.nose.1 = 120.0;
        let m = analyze_geometry(&lm, &bbox, 640, 480);
        assert!(
            m.pitch_deg < -45.0,
            "pitch_deg={} should be <-45",
            m.pitch_deg
        );
        assert_eq!(classify(&m, &default_cfg()), FeedbackState::TiltDown);
    }

    #[test]
    fn roll_triggers_look_at_camera() {
        let (mut lm, bbox) = landmarks_frontal();
        // Tilt head: right eye much lower than left
        lm.right_eye.1 = 220.0; // 70px lower
        let m = analyze_geometry(&lm, &bbox, 640, 480);
        assert!(
            m.roll_deg.abs() > 35.0,
            "roll_deg={} should be >35",
            m.roll_deg
        );
        assert_eq!(classify(&m, &default_cfg()), FeedbackState::LookAtCamera);
    }

    #[test]
    fn eyes_not_visible() {
        let (mut lm, bbox) = landmarks_frontal();
        lm.left_eye_conf = 0.2;
        let m = analyze_geometry(&lm, &bbox, 640, 480);
        assert_eq!(classify(&m, &default_cfg()), FeedbackState::EyesNotVisible);
    }

    #[test]
    fn ir_saturated_takes_priority() {
        let (lm, bbox) = landmarks_frontal();
        let mut m = analyze_geometry(&lm, &bbox, 640, 480);
        m.ir_saturated = true;
        assert_eq!(classify(&m, &default_cfg()), FeedbackState::IRSaturated);
    }

    // --- classify priority order tests ---

    #[test]
    fn ir_saturated_beats_eyes_not_visible() {
        let m = FaceMetrics {
            face_width_ratio: 0.3,
            yaw_deg: 0.0,
            pitch_deg: 0.0,
            roll_deg: 0.0,
            ir_saturated: true,
            eyes_visible: false,
            blur_score: 100.0,
        };
        assert_eq!(classify(&m, &default_cfg()), FeedbackState::IRSaturated);
    }

    #[test]
    fn eyes_not_visible_beats_distance() {
        let m = FaceMetrics {
            face_width_ratio: 0.05, // too far
            yaw_deg: 0.0,
            pitch_deg: 0.0,
            roll_deg: 0.0,
            ir_saturated: false,
            eyes_visible: false,
            blur_score: 100.0,
        };
        assert_eq!(classify(&m, &default_cfg()), FeedbackState::EyesNotVisible);
    }

    // --- StateMachine tests ---

    #[test]
    fn state_machine_debounce() {
        let cfg = default_cfg();
        let mut sm = StateMachine::new(&cfg);
        let t0 = Instant::now();

        // First transition always emits
        let result = sm.transition(None, t0);
        assert_eq!(result, Some(FeedbackState::Scanning));

        // Different state within debounce window → suppressed
        let (lm, bbox) = landmarks_frontal();
        let mut m = analyze_geometry(&lm, &bbox, 640, 480);
        m.face_width_ratio = 0.05; // TooFar
        let result = sm.transition(Some(&m), t0 + Duration::from_millis(50));
        assert!(result.is_none());

        // After debounce window → emits
        let result = sm.transition(Some(&m), t0 + Duration::from_millis(200));
        assert_eq!(result, Some(FeedbackState::TooFar));
    }

    #[test]
    fn state_machine_authenticating_bypasses_debounce() {
        let cfg = default_cfg();
        let mut sm = StateMachine::new(&cfg);
        let t0 = Instant::now();

        // Start scanning
        sm.transition(None, t0);
        assert_eq!(sm.state, AuthState::Guidance(FeedbackState::Scanning));

        // Immediately transition to Authenticating (no debounce wait)
        let (lm, bbox) = landmarks_frontal();
        let m = analyze_geometry(&lm, &bbox, 640, 480);
        let result = sm.transition(Some(&m), t0 + Duration::from_millis(1));
        assert_eq!(result, Some(FeedbackState::Authenticating));
        assert_eq!(sm.state, AuthState::Authenticating);
    }

    #[test]
    fn state_machine_can_leave_authenticating() {
        let cfg = default_cfg();
        let mut sm = StateMachine::new(&cfg);
        let t0 = Instant::now();

        // Get to Authenticating
        let (lm, bbox) = landmarks_frontal();
        let m = analyze_geometry(&lm, &bbox, 640, 480);
        sm.transition(Some(&m), t0);
        assert_eq!(sm.state, AuthState::Authenticating);

        // Face disappears → back to Scanning (after debounce)
        let result = sm.transition(None, t0 + Duration::from_millis(400));
        assert_eq!(result, Some(FeedbackState::Scanning));
    }

    #[test]
    fn state_machine_done_is_terminal() {
        let cfg = default_cfg();
        let mut sm = StateMachine::new(&cfg);
        let t0 = Instant::now();

        sm.finish();
        assert_eq!(sm.state, AuthState::Done);

        // No more transitions
        let result = sm.transition(None, t0);
        assert!(result.is_none());

        let (lm, bbox) = landmarks_frontal();
        let m = analyze_geometry(&lm, &bbox, 640, 480);
        let result = sm.transition(Some(&m), t0);
        assert!(result.is_none());
    }

    #[test]
    fn state_machine_no_face_scanning() {
        let cfg = default_cfg();
        let mut sm = StateMachine::new(&cfg);
        let t0 = Instant::now();

        let result = sm.transition(None, t0);
        assert_eq!(result, Some(FeedbackState::Scanning));
        assert_eq!(sm.state, AuthState::Guidance(FeedbackState::Scanning));
    }

    #[test]
    fn state_machine_repeated_same_state_no_emit() {
        let cfg = default_cfg();
        let mut sm = StateMachine::new(&cfg);
        let t0 = Instant::now();

        sm.transition(None, t0);

        // Same state (Scanning) — should not emit regardless of time
        let result = sm.transition(None, t0 + Duration::from_secs(10));
        assert!(result.is_none());
    }

    // --- Custom config threshold tests ---

    #[test]
    fn custom_thresholds_respected() {
        let cfg = GeometryConfig {
            distance_min: 0.30,
            distance_max: 0.50,
            yaw_max_deg: 10.0,
            pitch_max_deg: 10.0,
            roll_max_deg: 5.0,
            guidance_debounce_ms: 300,
        };

        // Face ratio 0.25 is fine with defaults but TooFar with strict config
        let m = FaceMetrics {
            face_width_ratio: 0.25,
            yaw_deg: 0.0,
            pitch_deg: 0.0,
            roll_deg: 0.0,
            ir_saturated: false,
            eyes_visible: true,
            blur_score: 100.0,
        };
        assert_eq!(classify(&m, &cfg), FeedbackState::TooFar);

        // Yaw 15° is fine with defaults but triggers with strict config
        // Positive yaw = turned right → instruction is TurnLeft
        let m = FaceMetrics {
            face_width_ratio: 0.35,
            yaw_deg: 15.0,
            pitch_deg: 0.0,
            roll_deg: 0.0,
            ir_saturated: false,
            eyes_visible: true,
            blur_score: 100.0,
        };
        assert_eq!(classify(&m, &cfg), FeedbackState::TurnLeft);
    }
}

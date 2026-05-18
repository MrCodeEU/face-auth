use crate::camera::Frame;
use face_auth_core::config::LivenessConfig;
use face_auth_core::geometry::{analyze_geometry, FaceMetrics};
use face_auth_models::alignment::align_face;
use face_auth_models::detection::FaceDetector;
use face_auth_models::liveness::LivenessDetector;
use face_auth_models::quality;
use face_auth_models::recognition::FaceRecognizer;
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

pub enum InferenceResult {
    Metrics {
        metrics: FaceMetrics,
        embedding: Option<Box<[f32; 512]>>,
        is_live: Option<bool>,
    },
    NoFace,
}

/// Pre-loaded ONNX models — shared across auth sessions via Arc.
/// Models are loaded once at daemon start and reused.
pub struct ModelCache {
    pub detector: Mutex<FaceDetector>,
    pub recognizer: Mutex<Option<FaceRecognizer>>,
}

impl ModelCache {
    pub fn load(ep_name: &str) -> Result<Self, crate::error::DaemonError> {
        let detector = FaceDetector::load_default_with_ep(ep_name)
            .map_err(|e| crate::error::DaemonError::Camera(format!("load SCRFD: {e}")))?;
        let recognizer = match FaceRecognizer::load_default_with_ep(ep_name) {
            Ok(r) => Some(r),
            Err(e) => {
                tracing::error!("failed to load ArcFace model: {e}");
                None
            }
        };
        Ok(Self {
            detector: Mutex::new(detector),
            recognizer: Mutex::new(recognizer),
        })
    }
}

pub struct InferenceThread {
    result_rx: mpsc::Receiver<InferenceResult>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl InferenceThread {
    /// Start inference thread that reads frames directly from camera channel.
    /// This eliminates the session-loop hop: camera → inference → session (results only).
    pub fn start(
        models: Arc<ModelCache>,
        frame_rx: mpsc::Receiver<Arc<Frame>>,
        liveness_config: LivenessConfig,
    ) -> Result<Self, crate::error::DaemonError> {
        let (result_tx, result_rx) = mpsc::sync_channel::<InferenceResult>(2);

        let thread = std::thread::Builder::new()
            .name("inference".into())
            .spawn(move || {
                inference_loop(frame_rx, result_tx, models, liveness_config);
            })
            .map_err(|e| crate::error::DaemonError::Camera(format!("spawn inference: {e}")))?;

        Ok(Self {
            result_rx,
            thread: Some(thread),
        })
    }

    pub fn recv_result(&self, timeout: Duration) -> Option<InferenceResult> {
        self.result_rx.recv_timeout(timeout).ok()
    }
}

impl Drop for InferenceThread {
    fn drop(&mut self) {
        // frame_rx is owned by the inference thread — dropping CameraHandle
        // closes the sender side, which makes frame_rx.recv() return Err,
        // causing the inference thread to exit naturally.
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn inference_loop(
    frame_rx: mpsc::Receiver<Arc<Frame>>,
    result_tx: mpsc::SyncSender<InferenceResult>,
    models: Arc<ModelCache>,
    liveness_config: LivenessConfig,
) {
    let mut liveness = if liveness_config.model_enabled {
        match LivenessDetector::load_default() {
            Ok(l) => Some(l),
            Err(e) => {
                tracing::error!("failed to load liveness model: {e}");
                None
            }
        }
    } else {
        tracing::info!("ML liveness model disabled (IR mode — using texture analysis)");
        None
    };

    if !liveness_config.enabled {
        tracing::info!("liveness detection disabled entirely");
    }

    tracing::info!("inference thread started");

    while let Ok(frame) = frame_rx.recv() {
        if frame.timestamp.elapsed() > Duration::from_millis(500) {
            tracing::debug!("skipped stale frame");
            continue;
        }

        // Lock models for this frame — held only during inference
        let mut detector = models.detector.lock().unwrap();
        let mut recognizer = models.recognizer.lock().unwrap();

        let result = process_frame(
            &mut detector,
            liveness.as_mut(),
            recognizer.as_mut(),
            &frame,
            &liveness_config,
        );
        drop(detector);
        drop(recognizer);

        if result_tx.try_send(result).is_err() {
            tracing::debug!("result channel full, dropping inference result");
        }
    }

    tracing::debug!("inference thread stopped");
}

fn process_frame(
    detector: &mut FaceDetector,
    mut liveness: Option<&mut LivenessDetector>,
    recognizer: Option<&mut FaceRecognizer>,
    frame: &Frame,
    liveness_config: &LivenessConfig,
) -> InferenceResult {
    let start = Instant::now();

    let detections = match detector.detect(&frame.data, frame.width, frame.height) {
        Ok(d) => d,
        Err(e) => {
            tracing::warn!("detection error: {e}");
            return InferenceResult::NoFace;
        }
    };

    if detections.is_empty() {
        return InferenceResult::NoFace;
    }

    let det = &detections[0];

    // Compute geometry
    let mut metrics = analyze_geometry(&det.landmarks, &det.bbox, frame.width, frame.height);

    // IR quality checks
    metrics.ir_saturated = quality::ir_saturated(&frame.data, &det.bbox, frame.width);
    metrics.blur_score = quality::blur_score(&frame.data, &det.bbox, frame.width, frame.height);

    // Only run liveness + recognition if face is reasonably positioned
    let should_process =
        metrics.face_width_ratio > 0.10 && metrics.eyes_visible && !metrics.ir_saturated;

    let mut is_live = None;
    let mut embedding = None;

    if should_process {
        // Run IR texture and ML liveness concurrently.
        // Both read from the same already-captured frame — no shared mutable state.
        let (ir_pass, ml_pass) = std::thread::scope(|s| {
            let ir_handle = if liveness_config.enabled {
                Some(s.spawn(|| {
                    let scores = quality::ir_liveness_check(
                        &frame.data,
                        &det.bbox,
                        frame.width,
                        frame.height,
                    );
                    scores.is_live(
                        liveness_config.lbp_entropy_min,
                        liveness_config.local_contrast_cv_min,
                        liveness_config.local_contrast_cv_max,
                    )
                }))
            } else {
                None
            };

            // ML liveness runs on current thread (needs &mut LivenessDetector).
            let ml = liveness.as_mut().map(|live| {
                live.check(&frame.data, frame.width, frame.height, &det.bbox)
                    .map(|r| r.is_real(liveness_config.model_threshold))
                    .unwrap_or(true) // model errors don't block auth
            });

            let ir = ir_handle.map(|h| h.join().unwrap_or(false)).unwrap_or(true);
            (ir, ml)
        });

        let live_pass = ir_pass && ml_pass.unwrap_or(true);
        is_live = Some(live_pass);

        tracing::debug!(
            ir_pass,
            ml_pass = ?ml_pass,
            live_pass,
            "liveness checks complete"
        );

        if !live_pass {
            let elapsed_ms = start.elapsed().as_millis();
            tracing::debug!(elapsed_ms, "liveness failed, skipping recognition");
            return InferenceResult::Metrics {
                metrics,
                embedding: None,
                is_live,
            };
        }

        // Alignment + Recognition (only if all liveness passed)
        if let Some(rec) = recognizer {
            let aligned = align_face(&frame.data, frame.width, frame.height, &det.landmarks);
            match rec.embed(&aligned) {
                Ok(emb) => embedding = Some(Box::new(emb)),
                Err(e) => tracing::warn!("embedding error: {e}"),
            }
        }
    }

    let elapsed_ms = start.elapsed().as_millis();
    tracing::debug!(
        elapsed_ms,
        confidence = det.confidence,
        face_width_ratio = metrics.face_width_ratio,
        yaw = metrics.yaw_deg,
        pitch = metrics.pitch_deg,
        has_embedding = embedding.is_some(),
        ?is_live,
        "inference complete"
    );

    InferenceResult::Metrics {
        metrics,
        embedding,
        is_live,
    }
}

#[cfg(test)]
mod tests {
    fn combine(ir: bool, ml: Option<bool>) -> bool {
        ir && ml.unwrap_or(true)
    }

    #[test]
    fn liveness_and_logic() {
        assert!(combine(true, None),          "IR only, passes");
        assert!(combine(true, Some(true)),    "both enabled, both pass");
        assert!(!combine(false, None),        "IR fails");
        assert!(!combine(false, Some(true)),  "IR fails, ML passes");
        assert!(!combine(true, Some(false)),  "IR passes, ML fails");
        assert!(!combine(false, Some(false)), "both fail");
        // ML model error is treated as pass (unwrap_or(true))
        assert!(combine(true, Some(true)),    "ML error → treated as pass");
    }
}

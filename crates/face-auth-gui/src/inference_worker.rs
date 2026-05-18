use face_auth_core::config::LivenessConfig;
use face_auth_core::geometry::{analyze_geometry, BBox, FaceMetrics, Landmarks};
use face_auth_models::alignment::align_face;
use face_auth_models::detection::FaceDetector;
use face_auth_models::quality;
use face_auth_models::recognition::FaceRecognizer;
use std::sync::{mpsc, Arc, Mutex};
use std::time::Duration;

pub struct FrameResult {
    pub bbox: BBox,
    pub landmarks: Landmarks,
    pub metrics: FaceMetrics,
    pub embedding: Option<[f32; 512]>,
    pub liveness_pass: bool,
    pub liveness_scores: quality::IrLivenessScores,
}

pub enum WorkerResult {
    Face(FrameResult),
    NoFace,
}

/// Shared model cache for the GUI enrollment inference.
/// Loaded once when enrollment starts, dropped when enrollment ends.
pub struct GuiModelCache {
    pub detector: Mutex<FaceDetector>,
    pub recognizer: Mutex<FaceRecognizer>,
}

impl GuiModelCache {
    pub fn load() -> Result<Self, String> {
        let detector = FaceDetector::load_default()
            .map_err(|e| format!("load detector: {e}"))?;
        let recognizer = FaceRecognizer::load_default()
            .map_err(|e| format!("load recognizer: {e}"))?;
        Ok(Self {
            detector: Mutex::new(detector),
            recognizer: Mutex::new(recognizer),
        })
    }
}

pub struct InferenceWorker {
    result_rx: mpsc::Receiver<WorkerResult>,
    // Keep thread alive — drops when InferenceWorker drops
    _thread: std::thread::JoinHandle<()>,
}

impl InferenceWorker {
    /// Start background inference thread consuming frames from camera channel.
    pub fn start(
        models: Arc<GuiModelCache>,
        frame_rx: mpsc::Receiver<Arc<face_auth_camera::Frame>>,
        liveness_config: LivenessConfig,
    ) -> Self {
        let (result_tx, result_rx) = mpsc::sync_channel::<WorkerResult>(4);

        let thread = std::thread::Builder::new()
            .name("gui-inference".into())
            .spawn(move || {
                inference_loop(models, frame_rx, result_tx, liveness_config);
            })
            .expect("spawn gui-inference thread");

        Self { result_rx, _thread: thread }
    }

    /// Non-blocking poll for latest result.
    pub fn try_recv(&self) -> Option<WorkerResult> {
        self.result_rx.try_recv().ok()
    }
}

fn inference_loop(
    models: Arc<GuiModelCache>,
    frame_rx: mpsc::Receiver<Arc<face_auth_camera::Frame>>,
    result_tx: mpsc::SyncSender<WorkerResult>,
    liveness_config: LivenessConfig,
) {
    while let Ok(frame) = frame_rx.recv() {
        // Skip stale frames (older than 200ms)
        if frame.timestamp.elapsed() > Duration::from_millis(200) {
            continue;
        }

        let mut detector = models.detector.lock().unwrap();
        let detections = match detector.detect(&frame.data, frame.width, frame.height) {
            Ok(d) => d,
            Err(_) => {
                let _ = result_tx.try_send(WorkerResult::NoFace);
                continue;
            }
        };
        drop(detector);

        if detections.is_empty() {
            let _ = result_tx.try_send(WorkerResult::NoFace);
            continue;
        }

        let det = &detections[0];
        let mut metrics = analyze_geometry(&det.landmarks, &det.bbox, frame.width, frame.height);
        metrics.ir_saturated = quality::ir_saturated(&frame.data, &det.bbox, frame.width);
        metrics.blur_score = quality::blur_score(&frame.data, &det.bbox, frame.width, frame.height);

        let liveness_scores =
            quality::ir_liveness_check(&frame.data, &det.bbox, frame.width, frame.height);
        let liveness_pass = liveness_scores.is_live(
            liveness_config.lbp_entropy_min,
            liveness_config.local_contrast_cv_min,
            liveness_config.local_contrast_cv_max,
        );

        // Only embed when face quality is acceptable
        let embedding = if metrics.face_width_ratio > 0.10
            && metrics.eyes_visible
            && !metrics.ir_saturated
            && metrics.blur_score >= 50.0
            && liveness_pass
        {
            let aligned = align_face(&frame.data, frame.width, frame.height, &det.landmarks);
            let mut recognizer = models.recognizer.lock().unwrap();
            recognizer.embed(&aligned).ok()
        } else {
            None
        };

        let result = WorkerResult::Face(FrameResult {
            bbox: det.bbox.clone(),
            landmarks: det.landmarks.clone(),
            metrics,
            embedding,
            liveness_pass,
            liveness_scores,
        });

        let _ = result_tx.try_send(result);
    }
}

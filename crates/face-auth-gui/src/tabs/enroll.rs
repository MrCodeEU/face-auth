use crate::camera_texture;
use crate::inference_worker::{GuiModelCache, InferenceWorker, WorkerResult};
use face_auth_core::config::Config;
use face_auth_core::enrollment;
use face_auth_core::geometry::{AuthState, StateMachine};
use face_auth_models::recognition::cosine_similarity;
use std::sync::mpsc;
use std::sync::Arc;
use std::time::{Duration, Instant};

const POSES: &[&str] = &[
    "straight ahead",
    "slightly LEFT",
    "slightly RIGHT",
    "slightly UP",
    "slightly DOWN",
];
const EMBEDDINGS_PER_POSE: usize = 3;
const MIN_CAPTURE_INTERVAL: Duration = Duration::from_millis(500);

pub enum EnrollState {
    Idle,
    LoadingModels,
    ModelError(String),
    CapturingPose {
        pose_idx: usize,
        pose_captured: usize,
        all_embeddings: Vec<[f32; 512]>,
        last_capture: Instant,
    },
    QualityReview {
        embeddings: Vec<[f32; 512]>,
        avg_sim: f32,
        min_sim: f32,
        suggested_threshold: f32,
    },
    Saving,
    Done { embed_count: usize },
    Error(String),
}

pub struct EnrollTab {
    append: bool,
    state: EnrollState,
    camera: Option<face_auth_camera::CameraHandle>,
    frame_tx: Option<mpsc::SyncSender<Arc<face_auth_camera::Frame>>>,
    worker: Option<InferenceWorker>,
    models: Option<Arc<GuiModelCache>>,
    latest_frame: Option<Arc<face_auth_camera::Frame>>,
    latest_result: Option<WorkerResult>,
    state_machine: Option<StateMachine>,
    existing_embeddings: Vec<[f32; 512]>,
    username: String,
}

impl EnrollTab {
    pub fn new(append: bool) -> Self {
        let username = std::env::var("SUDO_USER")
            .or_else(|_| std::env::var("USER"))
            .unwrap_or_else(|_| "unknown".into());
        Self {
            append,
            state: EnrollState::Idle,
            camera: None,
            frame_tx: None,
            worker: None,
            models: None,
            latest_frame: None,
            latest_result: None,
            state_machine: None,
            existing_embeddings: Vec::new(),
            username,
        }
    }

    pub fn deactivate(&mut self) {
        self.worker = None;
        self.frame_tx = None;
        self.camera = None;
        self.latest_frame = None;
        self.latest_result = None;
        self.state = EnrollState::Idle;
        self.existing_embeddings.clear();
    }

    fn start_enrollment(&mut self, config: &Config) {
        // Load existing embeddings if appending
        self.existing_embeddings = if self.append {
            enrollment::load_embeddings(&self.username).unwrap_or_default()
        } else {
            Vec::new()
        };

        // Open camera
        let camera = match face_auth_camera::open_camera(&config.camera) {
            Ok(c) => c,
            Err(e) => {
                self.state = EnrollState::Error(format!("Camera: {e}"));
                return;
            }
        };
        self.camera = Some(camera);
        self.state = EnrollState::LoadingModels;

        // Load models synchronously (~1-2s first run)
        match GuiModelCache::load() {
            Ok(cache) => {
                let models = Arc::new(cache);
                let (frame_tx, frame_rx) = mpsc::sync_channel::<Arc<face_auth_camera::Frame>>(4);
                self.frame_tx = Some(frame_tx);
                self.worker = Some(InferenceWorker::start(
                    models.clone(),
                    frame_rx,
                    config.liveness.clone(),
                ));
                self.models = Some(models);
                self.state_machine = Some(StateMachine::new(&config.geometry));
                self.state = EnrollState::CapturingPose {
                    pose_idx: 0,
                    pose_captured: 0,
                    all_embeddings: Vec::new(),
                    last_capture: Instant::now() - Duration::from_secs(10),
                };
            }
            Err(e) => {
                self.state = EnrollState::ModelError(e);
            }
        }
    }

    fn pump_frames(&mut self) {
        // Forward camera frames to inference worker
        if let (Some(cam), Some(tx)) = (&self.camera, &self.frame_tx) {
            while let Some(f) = cam.try_recv_frame() {
                self.latest_frame = Some(f.clone());
                let _ = tx.try_send(f);
            }
        }
        // Collect latest inference result
        if let Some(w) = &self.worker {
            while let Some(r) = w.try_recv() {
                self.latest_result = Some(r);
            }
        }
    }

    fn try_capture_embedding(&mut self) {
        let sm = match self.state_machine.as_mut() {
            Some(s) => s,
            None => return,
        };
        let now = Instant::now();

        // Advance state machine with current metrics
        let in_auth_state = {
            match &self.latest_result {
                Some(WorkerResult::Face(r)) => {
                    sm.transition(Some(&r.metrics), now);
                }
                _ => {
                    sm.transition(None, now);
                }
            }
            sm.state == AuthState::Authenticating
        };

        if !in_auth_state {
            return;
        }

        // Get embedding if available and quality gate passed
        let embedding = match &self.latest_result {
            Some(WorkerResult::Face(r)) => r.embedding,
            _ => return,
        };
        let Some(embedding) = embedding else { return };

        // Capture with interval throttle
        let EnrollState::CapturingPose {
            pose_idx,
            pose_captured,
            all_embeddings,
            last_capture,
        } = &mut self.state else { return };

        if now.duration_since(*last_capture) < MIN_CAPTURE_INTERVAL {
            return;
        }
        all_embeddings.push(embedding);
        *pose_captured += 1;
        *last_capture = now;

        if *pose_captured >= EMBEDDINGS_PER_POSE {
            *pose_idx += 1;
            *pose_captured = 0;

            if *pose_idx >= POSES.len() {
                let filtered = score_and_filter(all_embeddings.clone());
                let (avg_sim, min_sim) = embedding_stats(&filtered);
                let suggested = (min_sim - 0.10_f32).max(0.40);
                self.state = EnrollState::QualityReview {
                    embeddings: filtered,
                    avg_sim,
                    min_sim,
                    suggested_threshold: suggested,
                };
            }
        }
    }

    fn save_embeddings(&mut self, embeddings: Vec<[f32; 512]>) {
        let mut all = self.existing_embeddings.clone();
        all.extend_from_slice(&embeddings);
        let count = all.len();

        match enrollment::save_embeddings(&self.username, &all, count as u32) {
            Ok(()) => self.state = EnrollState::Done { embed_count: count },
            Err(e) => self.state = EnrollState::Error(format!("Save failed: {e}")),
        }
    }

    pub fn ui(&mut self, ui: &mut egui::Ui, ctx: &egui::Context, config: &Config) {
        self.pump_frames();

        ui.heading(if self.append { "Re-enroll (append)" } else { "Enroll" });
        ui.separator();

        // Snapshot state type to avoid borrow issues
        let state_tag = match &self.state {
            EnrollState::Idle => 0,
            EnrollState::LoadingModels => 1,
            EnrollState::ModelError(_) => 2,
            EnrollState::CapturingPose { .. } => 3,
            EnrollState::QualityReview { .. } => 4,
            EnrollState::Saving => 5,
            EnrollState::Done { .. } => 6,
            EnrollState::Error(_) => 7,
        };

        match state_tag {
            0 => {
                // Idle
                ui.label("Enroll your face for authentication.");
                ui.label(format!(
                    "Poses: {}  ×  {} frames each = {} total embeddings",
                    POSES.len(),
                    EMBEDDINGS_PER_POSE,
                    POSES.len() * EMBEDDINGS_PER_POSE
                ));
                ui.separator();
                if ui.button("Start Enrollment").clicked() {
                    let cfg = config.clone();
                    self.start_enrollment(&cfg);
                }
            }
            1 => {
                // LoadingModels
                ui.label("Loading models... (first run may take a few seconds)");
                ctx.request_repaint_after(Duration::from_millis(100));
            }
            2 => {
                // ModelError
                if let EnrollState::ModelError(ref e) = self.state {
                    let e = e.clone();
                    ui.colored_label(egui::Color32::RED, format!("Model load error: {e}"));
                }
                if ui.button("Retry").clicked() {
                    self.state = EnrollState::Idle;
                }
            }
            3 => {
                // CapturingPose
                let (pose_idx, pose_captured, total_captured) = match &self.state {
                    EnrollState::CapturingPose { pose_idx, pose_captured, all_embeddings, .. } => {
                        (*pose_idx, *pose_captured, all_embeddings.len())
                    }
                    _ => return,
                };

                let total_target = POSES.len() * EMBEDDINGS_PER_POSE;
                let progress = total_captured as f32 / total_target as f32;
                ui.add(
                    egui::ProgressBar::new(progress)
                        .text(format!("{}/{} embeddings", total_captured, total_target)),
                );
                ui.separator();
                ui.label(format!(
                    "Pose {}/{}: Look {}",
                    pose_idx + 1,
                    POSES.len(),
                    POSES[pose_idx.min(POSES.len() - 1)]
                ));
                ui.label(format!(
                    "Captured for this pose: {}/{}",
                    pose_captured, EMBEDDINGS_PER_POSE
                ));
                ui.separator();

                // Camera feed
                if let Some(ref frame) = self.latest_frame.clone() {
                    let texture = camera_texture::upload_frame(frame, ctx, "enroll_camera");
                    camera_texture::show_texture(ui, &texture, frame.width, frame.height);
                }

                // Guidance
                let guidance = match &self.latest_result {
                    Some(WorkerResult::Face(r)) => {
                        if !r.liveness_pass {
                            "Liveness check failed"
                        } else if r.embedding.is_some() {
                            "Good position — capturing..."
                        } else {
                            "Adjusting position..."
                        }
                    }
                    Some(WorkerResult::NoFace) | None => "No face detected",
                };
                ui.label(guidance);

                self.try_capture_embedding();
                ctx.request_repaint_after(Duration::from_millis(50));
            }
            4 => {
                // QualityReview
                let (embeddings, avg_sim, min_sim, suggested) = match &self.state {
                    EnrollState::QualityReview { embeddings, avg_sim, min_sim, suggested_threshold } => {
                        (embeddings.clone(), *avg_sim, *min_sim, *suggested_threshold)
                    }
                    _ => return,
                };
                let current_threshold = config.recognition.threshold;

                ui.heading("Quality Review");
                egui::Grid::new("quality_grid").num_columns(2).show(ui, |ui| {
                    ui.label("Embeddings captured:");
                    ui.label(embeddings.len().to_string());
                    ui.end_row();

                    ui.label("Avg inter-embedding similarity:");
                    let grade = if avg_sim >= 0.80 { "Excellent" }
                        else if avg_sim >= 0.70 { "Good" }
                        else if avg_sim >= 0.60 { "Fair" }
                        else { "Poor" };
                    ui.label(format!("{:.3} ({})", avg_sim, grade));
                    ui.end_row();

                    ui.label("Suggested threshold:");
                    ui.label(format!("{:.2}  (current: {:.2})", suggested, current_threshold));
                    ui.end_row();
                });

                ui.separator();
                ui.horizontal(|ui| {
                    if ui.button("Save").clicked() {
                        self.save_embeddings(embeddings);
                    }
                    if ui.button("Re-do enrollment").clicked() {
                        self.deactivate();
                    }
                });
            }
            5 => {
                // Saving
                ui.label("Saving...");
            }
            6 => {
                // Done
                let count = match &self.state {
                    EnrollState::Done { embed_count } => *embed_count,
                    _ => 0,
                };
                ui.colored_label(
                    egui::Color32::GREEN,
                    format!("Enrolled successfully — {} embeddings saved.", count),
                );
                if ui.button("Enroll again").clicked() {
                    self.state = EnrollState::Idle;
                }
            }
            7 => {
                // Error
                if let EnrollState::Error(ref e) = self.state {
                    let e = e.clone();
                    ui.colored_label(egui::Color32::RED, format!("Error: {e}"));
                }
                if ui.button("Back").clicked() {
                    self.state = EnrollState::Idle;
                }
            }
            _ => {}
        }
    }
}

fn score_and_filter(embeddings: Vec<[f32; 512]>) -> Vec<[f32; 512]> {
    if embeddings.len() < 3 {
        return embeddings;
    }
    let n = embeddings.len();
    let avg_sims: Vec<f32> = (0..n).map(|i| {
        let sum: f32 = (0..n).filter(|&j| j != i)
            .map(|j| cosine_similarity(&embeddings[i], &embeddings[j]))
            .sum();
        sum / (n - 1) as f32
    }).collect();

    let filtered: Vec<[f32; 512]> = embeddings.iter().copied().zip(avg_sims.iter())
        .filter(|(_, &sim)| sim >= 0.5)
        .map(|(e, _)| e)
        .collect();

    if filtered.is_empty() { embeddings } else { filtered }
}

fn embedding_stats(embeddings: &[[f32; 512]]) -> (f32, f32) {
    if embeddings.len() < 2 {
        return (0.0, 0.0);
    }
    let n = embeddings.len();
    let avg_sim: f32 = (0..n).map(|i| {
        let sum: f32 = (0..n).filter(|&j| j != i)
            .map(|j| cosine_similarity(&embeddings[i], &embeddings[j]))
            .sum();
        sum / (n - 1) as f32
    }).sum::<f32>() / n as f32;

    let mut min_sim = 1.0f32;
    for i in 0..n {
        for j in (i+1)..n {
            let s = cosine_similarity(&embeddings[i], &embeddings[j]);
            if s < min_sim { min_sim = s; }
        }
    }
    (avg_sim, min_sim)
}

use crate::camera_texture;
use crate::inference_worker::{GuiModelCache, InferenceWorker, WorkerResult};
use face_auth_core::config::Config;
use face_auth_core::enrollment;
use face_auth_core::geometry::{AuthState, BBox, Landmarks, StateMachine};
use face_auth_models::recognition::cosine_similarity;
use std::sync::{mpsc, Arc};
use std::time::{Duration, Instant};

#[derive(Clone, Debug)]
enum LocalAuthState {
    Idle,
    LoadingModels,
    Running {
        start: Instant,
        state_label: String,
        consecutive_matches: u32,
        best_sim: f32,
    },
    Done {
        outcome: &'static str,
        color: egui::Color32,
        elapsed: f32,
        best_sim: f32,
    },
    Error(String),
}

pub struct TestAuthTab {
    state: LocalAuthState,
    camera: Option<face_auth_camera::CameraHandle>,
    frame_tx: Option<mpsc::SyncSender<Arc<face_auth_camera::Frame>>>,
    worker: Option<InferenceWorker>,
    models: Option<Arc<GuiModelCache>>,
    latest_frame: Option<Arc<face_auth_camera::Frame>>,
    latest_result: Option<WorkerResult>,
    state_machine: Option<StateMachine>,
    enrolled: Vec<[f32; 512]>,
    // Overlay info from last frame
    last_bbox: Option<BBox>,
    last_landmarks: Option<Landmarks>,
    last_liveness_pass: bool,
    last_sim: Option<f32>,
    username: String,
}

impl TestAuthTab {
    pub fn new() -> Self {
        let username = std::env::var("SUDO_USER")
            .or_else(|_| std::env::var("USER"))
            .unwrap_or_else(|_| "unknown".into());
        Self {
            state: LocalAuthState::Idle,
            camera: None,
            frame_tx: None,
            worker: None,
            models: None,
            latest_frame: None,
            latest_result: None,
            state_machine: None,
            enrolled: Vec::new(),
            last_bbox: None,
            last_landmarks: None,
            last_liveness_pass: false,
            last_sim: None,
            username,
        }
    }

    pub fn deactivate(&mut self) {
        self.worker = None;
        self.frame_tx = None;
        self.camera = None;
        self.latest_frame = None;
        self.latest_result = None;
        self.state = LocalAuthState::Idle;
        self.state_machine = None;
        self.enrolled.clear();
        self.last_bbox = None;
        self.last_landmarks = None;
    }

    fn start_auth(&mut self, config: &Config) {
        self.state = LocalAuthState::LoadingModels;

        // Load enrolled embeddings
        self.enrolled = enrollment::load_embeddings(&self.username).unwrap_or_default();
        if self.enrolled.is_empty() {
            self.state = LocalAuthState::Error(format!(
                "No enrollment found for '{}'.\nRun enrollment first.",
                self.username
            ));
            return;
        }

        // Load models (reuse if already loaded)
        let models = if let Some(m) = self.models.take() {
            m
        } else {
            match GuiModelCache::load() {
                Ok(m) => Arc::new(m),
                Err(e) => {
                    self.state = LocalAuthState::Error(format!("Model load failed: {e}"));
                    return;
                }
            }
        };
        self.models = Some(models.clone());

        // Open camera
        let camera = match face_auth_camera::open_camera(&config.camera) {
            Ok(c) => c,
            Err(e) => {
                self.state = LocalAuthState::Error(format!("Camera: {e}"));
                return;
            }
        };

        // Wire camera → inference worker via channel
        let (frame_tx, frame_rx) = mpsc::sync_channel::<Arc<face_auth_camera::Frame>>(2);
        self.frame_tx = Some(frame_tx);
        self.worker = Some(InferenceWorker::start(
            models,
            frame_rx,
            config.liveness.clone(),
        ));
        self.camera = Some(camera);
        self.state_machine = Some(StateMachine::new(&config.geometry));
        self.state = LocalAuthState::Running {
            start: Instant::now(),
            state_label: "Scanning...".into(),
            consecutive_matches: 0,
            best_sim: 0.0,
        };
    }

    pub fn ui(&mut self, ui: &mut egui::Ui, ctx: &egui::Context, config: &Config) {
        ui.heading("Test Authentication");
        ui.separator();

        // Feed camera frames to worker
        if let Some(cam) = &self.camera {
            if let Some(tx) = &self.frame_tx {
                while let Some(f) = cam.try_recv_frame() {
                    self.latest_frame = Some(f.clone());
                    let _ = tx.try_send(f);
                }
            }
        }

        // Drain inference results
        if let Some(ref w) = self.worker {
            while let Some(r) = w.try_recv() {
                self.latest_result = Some(r);
            }
        }

        // Process latest result against auth logic
        self.process_result(config);

        match &self.state {
            LocalAuthState::Idle => {
                ui.label(format!("Local auth test — user: {}", self.username));
                ui.label("Opens camera locally, runs full pipeline, no daemon needed.");
                ui.separator();
                if ui.button("Start Auth Test").clicked() {
                    let cfg = config.clone();
                    self.start_auth(&cfg);
                }
            }

            LocalAuthState::LoadingModels => {
                ui.label("Loading models...");
                ctx.request_repaint_after(Duration::from_millis(100));
            }

            LocalAuthState::Running { start, state_label, .. } => {
                let elapsed = start.elapsed().as_secs_f32();
                let timeout = config.daemon.session_timeout_s as f32;
                let progress = (elapsed / timeout).min(1.0);

                ui.add(
                    egui::ProgressBar::new(progress)
                        .text(format!("{elapsed:.1}s / {timeout:.0}s")),
                );
                ui.separator();

                ui.heading(state_label.clone());
                if let Some(sim) = self.last_sim {
                    let color = sim_color(sim, config.recognition.threshold);
                    ui.colored_label(color, format!("similarity: {sim:.3}  threshold: {:.2}", config.recognition.threshold));
                }
                ui.separator();

                self.draw_camera(ui, ctx);
                ctx.request_repaint_after(Duration::from_millis(50));
            }

            LocalAuthState::Done { outcome, color, elapsed, best_sim } => {
                let (outcome, color, elapsed, best_sim) = (*outcome, *color, *elapsed, *best_sim);
                ui.colored_label(color, format!("{outcome}  ({elapsed:.1}s)"));
                if best_sim > 0.0 {
                    ui.label(format!("Best similarity: {best_sim:.3}"));
                }
                ui.separator();
                if ui.button("Try Again").clicked() {
                    self.deactivate();
                }
            }

            LocalAuthState::Error(e) => {
                let msg = e.clone();
                ui.colored_label(egui::Color32::RED, &msg);
                ui.separator();
                if ui.button("Retry").clicked() {
                    self.state = LocalAuthState::Idle;
                }
            }
        }
    }

    fn process_result(&mut self, config: &Config) {
        let result = match self.latest_result.take() {
            Some(r) => r,
            None => return,
        };

        let now = Instant::now();
        let sm = match self.state_machine.as_mut() {
            Some(s) => s,
            None => return,
        };

        let (consecutive, best_sim, start) = match &mut self.state {
            LocalAuthState::Running { consecutive_matches, best_sim, start, .. } => {
                (consecutive_matches, best_sim, *start)
            }
            _ => return,
        };

        // Check timeout
        let timeout = Duration::from_secs(config.daemon.session_timeout_s);
        if start.elapsed() >= timeout {
            let elapsed = start.elapsed().as_secs_f32();
            self.state = LocalAuthState::Done {
                outcome: "TIMEOUT",
                color: egui::Color32::YELLOW,
                elapsed,
                best_sim: *best_sim,
            };
            self.cleanup();
            return;
        }

        match result {
            WorkerResult::NoFace => {
                sm.transition(None, now);
                self.last_bbox = None;
                self.last_landmarks = None;
                self.last_sim = None;
                *consecutive = 0;

                let label = state_label(sm);
                if let LocalAuthState::Running { state_label, .. } = &mut self.state {
                    *state_label = label;
                }
            }

            WorkerResult::Face(fr) => {
                let feedback = sm.transition(Some(&fr.metrics), now);

                self.last_bbox = Some(fr.bbox.clone());
                self.last_landmarks = Some(fr.landmarks.clone());
                self.last_liveness_pass = fr.liveness_pass;

                let is_authenticating = matches!(sm.state, AuthState::Authenticating);

                if is_authenticating {
                    if let Some(emb) = fr.embedding {
                        let sim = self.enrolled.iter()
                            .map(|e| cosine_similarity(&emb, e))
                            .fold(0.0f32, f32::max);
                        self.last_sim = Some(sim);

                        if sim > *best_sim { *best_sim = sim; }

                        if sim >= config.recognition.threshold {
                            *consecutive += 1;
                        } else {
                            *consecutive = 0;
                        }

                        let effective_required = if *best_sim >= config.recognition.threshold + 0.10 {
                            1
                        } else {
                            config.recognition.frames_required
                        };

                        if *consecutive >= effective_required {
                            let elapsed = start.elapsed().as_secs_f32();
                            let bs = *best_sim;
                            self.state = LocalAuthState::Done {
                                outcome: "SUCCESS",
                                color: egui::Color32::GREEN,
                                elapsed,
                                best_sim: bs,
                            };
                            self.cleanup();
                            return;
                        }
                    } else {
                        self.last_sim = None;
                        *consecutive = 0;
                    }
                } else {
                    self.last_sim = None;
                    *consecutive = 0;
                }

                let label = feedback
                    .map(|f| feedback_str(&f).to_owned())
                    .unwrap_or_else(|| state_label(sm));
                if let LocalAuthState::Running { state_label, .. } = &mut self.state {
                    *state_label = label;
                }
            }
        }
    }

    fn cleanup(&mut self) {
        self.worker = None;
        self.frame_tx = None;
        self.camera = None;
        self.state_machine = None;
    }

    fn draw_camera(&self, ui: &mut egui::Ui, ctx: &egui::Context) {
        let frame = match &self.latest_frame {
            Some(f) => f,
            None => {
                ui.label("Waiting for camera...");
                return;
            }
        };

        let texture = camera_texture::upload_frame(frame, ctx, "test_auth_camera");
        let response = camera_texture::show_texture(ui, &texture, frame.width, frame.height);
        let rect = response.rect;

        // Scale factors: camera coords → screen coords
        let sx = rect.width() / frame.width as f32;
        let sy = rect.height() / frame.height as f32;
        let origin = rect.min;
        let painter = ui.painter();

        if let Some(ref bbox) = self.last_bbox {
            let bbox_color = if self.last_sim.is_some_and(|s| s >= 0.60) {
                egui::Color32::GREEN
            } else if self.last_liveness_pass {
                egui::Color32::from_rgb(68, 136, 255) // blue
            } else {
                egui::Color32::YELLOW
            };
            let r = egui::Rect::from_min_max(
                egui::pos2(origin.x + bbox.x1 * sx, origin.y + bbox.y1 * sy),
                egui::pos2(origin.x + bbox.x2 * sx, origin.y + bbox.y2 * sy),
            );
            painter.rect_stroke(r, 0.0, egui::Stroke::new(2.0, bbox_color), egui::StrokeKind::Outside);

            // Confidence label above bbox
            let label = if let Some(sim) = self.last_sim {
                format!("sim:{sim:.3}")
            } else {
                String::new()
            };
            if !label.is_empty() {
                painter.text(
                    egui::pos2(r.min.x, r.min.y - 14.0),
                    egui::Align2::LEFT_TOP,
                    &label,
                    egui::FontId::proportional(12.0),
                    bbox_color,
                );
            }
        }

        // Landmarks
        if let Some(ref lm) = self.last_landmarks {
            let lm_color = egui::Color32::from_rgb(255, 80, 80);
            let pts = [
                lm.left_eye,
                lm.right_eye,
                lm.nose,
                lm.left_mouth,
                lm.right_mouth,
            ];
            for (lx, ly) in pts {
                let pos = egui::pos2(origin.x + lx * sx, origin.y + ly * sy);
                painter.circle_filled(pos, 3.0, lm_color);
            }
        }
    }
}

fn sim_color(sim: f32, threshold: f32) -> egui::Color32 {
    if sim >= threshold {
        egui::Color32::GREEN
    } else if sim >= threshold - 0.15 {
        egui::Color32::YELLOW
    } else {
        egui::Color32::RED
    }
}

fn state_label(sm: &StateMachine) -> String {
    match &sm.state {
        AuthState::Idle | AuthState::Guidance(_) => "Scanning...".into(),
        AuthState::Authenticating => "Authenticating...".into(),
        AuthState::Done => "Done".into(),
    }
}

fn feedback_str(f: &face_auth_core::protocol::FeedbackState) -> &'static str {
    use face_auth_core::protocol::FeedbackState;
    match f {
        FeedbackState::Scanning => "Scanning...",
        FeedbackState::TooFar => "Move closer",
        FeedbackState::TooClose => "Move back",
        FeedbackState::TurnLeft => "Turn left",
        FeedbackState::TurnRight => "Turn right",
        FeedbackState::TiltUp => "Tilt up",
        FeedbackState::TiltDown => "Tilt down",
        FeedbackState::IRSaturated => "Too much IR glare — move back",
        FeedbackState::EyesNotVisible => "Eyes not visible",
        FeedbackState::LookAtCamera => "Look at camera",
        FeedbackState::Authenticating => "Authenticating...",
    }
}

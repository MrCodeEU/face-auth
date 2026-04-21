use crate::camera::CameraHandle;
use crate::error::DaemonError;
use crate::inference::{InferenceResult, InferenceThread};
use face_auth_core::config::Config;
use face_auth_core::enrollment;
use face_auth_core::framing::write_message;
use face_auth_core::geometry::StateMachine;
use face_auth_core::protocol::{AuthOutcome, DaemonMessage, FeedbackState};
use face_auth_models::recognition::cosine_similarity;
use std::collections::VecDeque;
use std::io::BufWriter;
#[cfg(unix)]
use std::os::unix::process::CommandExt;
use std::sync::Arc;
use std::time::{Duration, Instant};

pub struct SessionManager {
    active: Option<ActiveSession>,
}

struct ActiveSession {
    session_id: u64,
}

impl SessionManager {
    pub fn new() -> Self {
        Self { active: None }
    }

    pub fn try_start(&mut self, session_id: u64) -> Result<(), DaemonError> {
        if self.active.is_some() {
            return Err(DaemonError::SessionBusy);
        }
        self.active = Some(ActiveSession { session_id });
        Ok(())
    }

    pub fn end(&mut self, session_id: u64) {
        if let Some(ref active) = self.active {
            if active.session_id == session_id {
                self.active = None;
            }
        }
    }

    #[allow(dead_code)]
    pub fn is_active(&self) -> bool {
        self.active.is_some()
    }
}

pub async fn run_auth_session(
    session_id: u64,
    username: String,
    config: Arc<Config>,
    session_manager: Arc<tokio::sync::Mutex<SessionManager>>,
    models: Arc<crate::inference::ModelCache>,
    model_store: Arc<tokio::sync::Mutex<crate::model_store::ModelStore>>,
    pam_stream: std::os::unix::net::UnixStream,
) {
    let result = run_session_inner(session_id, &username, &config, models, &pam_stream).await;

    if let Err(e) = result {
        tracing::error!(session_id, "session error: {e}");
        let _ = send_result(&pam_stream, session_id, AuthOutcome::Failed);
    }

    // Always clear session
    {
        let mut sm = session_manager.lock().await;
        sm.end(session_id);
    }

    // Touch model store so idle timer resets from session end, not last auth frame
    model_store.lock().await.touch();

    tracing::info!(session_id, "auth session ended");
}

async fn run_session_inner(
    session_id: u64,
    username: &str,
    config: &Config,
    models: Arc<crate::inference::ModelCache>,
    pam_stream: &std::os::unix::net::UnixStream,
) -> Result<(), DaemonError> {
    // Load enrollment data
    let stored_embeddings = match enrollment::load_embeddings(username) {
        Ok(e) => {
            let ver = enrollment::enrollment_version(username).unwrap_or(1);
            if ver < enrollment::ENROLLMENT_VERSION {
                tracing::warn!(
                    session_id, %username,
                    version = ver, current = enrollment::ENROLLMENT_VERSION,
                    "stale enrollment format — re-enroll for best accuracy"
                );
            }
            tracing::info!(
                session_id,
                count = e.len(),
                version = ver,
                "enrollment loaded"
            );
            e
        }
        Err(e) => {
            tracing::warn!(session_id, %username, "no enrollment: {e}");
            send_result(pam_stream, session_id, AuthOutcome::Failed)?;
            return Ok(());
        }
    };

    // Send initial scanning feedback
    send_feedback(pam_stream, session_id, FeedbackState::Scanning)?;

    // Open camera
    let camera_config = config.camera.clone();
    let mut camera = tokio::task::spawn_blocking(move || CameraHandle::open(&camera_config))
        .await
        .map_err(|e| DaemonError::Join(e.to_string()))??;

    tracing::info!(session_id, "camera pipeline started");

    // Connect camera directly to inference (camera → inference → session results)
    let frame_rx = camera
        .take_frame_rx()
        .ok_or_else(|| DaemonError::Camera("frame channel already taken".into()))?;
    let liveness_config = config.liveness.clone();
    let inference = tokio::task::spawn_blocking(move || {
        InferenceThread::start(models, frame_rx, liveness_config)
    })
    .await
    .map_err(|e| DaemonError::Join(e.to_string()))??;

    tracing::info!(session_id, "inference thread started");

    // Run the detection + recognition loop
    let timeout = Duration::from_secs(config.daemon.session_timeout_s);
    let geo_cfg_for_sm = config.geometry.clone();
    let notify_cfg = config.notify.clone();
    let username_owned = username.to_string();
    let threshold = config.recognition.threshold;
    let frames_required = config.recognition.frames_required;
    let loop_stream = pam_stream.try_clone().map_err(DaemonError::Io)?;

    tokio::task::spawn_blocking(move || {
        detection_loop(
            camera,
            inference,
            &geo_cfg_for_sm,
            &notify_cfg,
            &username_owned,
            &loop_stream,
            session_id,
            timeout,
            &stored_embeddings,
            threshold,
            frames_required,
        )
    })
    .await
    .map_err(|e| DaemonError::Join(e.to_string()))?;

    // Result already sent inside detection_loop (before cleanup drops)
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn detection_loop(
    camera: CameraHandle, // kept alive so capture thread runs
    inference: InferenceThread,
    geo_config: &face_auth_core::config::GeometryConfig,
    notify_config: &face_auth_core::config::NotifyConfig,
    username: &str,
    pam_stream: &std::os::unix::net::UnixStream,
    session_id: u64,
    timeout: Duration,
    stored_embeddings: &[[f32; 512]],
    threshold: f32,
    frames_required: u32,
) {
    let outcome = detection_loop_inner(
        &inference,
        geo_config,
        pam_stream,
        session_id,
        timeout,
        stored_embeddings,
        threshold,
        frames_required,
    );
    // Send result IMMEDIATELY — before cleanup. Saves ~360ms of user-perceived latency.
    let _ = send_result(pam_stream, session_id, outcome.clone());

    // Desktop notification on success (opt-in, background)
    if outcome == AuthOutcome::Success && notify_config.enabled {
        send_desktop_notification(username, notify_config.timeout_ms);
    }
    // CRITICAL: drop camera BEFORE inference to avoid deadlock.
    // Camera drop → capture thread stops → tx dropped → frame_rx.recv() unblocks
    // → inference thread exits → inference join succeeds.
    drop(camera);
    drop(inference);
}

#[allow(clippy::too_many_arguments)]
fn detection_loop_inner(
    inference: &InferenceThread,
    geo_config: &face_auth_core::config::GeometryConfig,
    pam_stream: &std::os::unix::net::UnixStream,
    session_id: u64,
    timeout: Duration,
    stored_embeddings: &[[f32; 512]],
    threshold: f32,
    frames_required: u32,
) -> AuthOutcome {
    let start = Instant::now();
    let mut state_machine = StateMachine::new(geo_config);
    let mut last_feedback: Option<FeedbackState> = None;
    let mut consecutive_matches: u32 = 0;
    let mut liveness_history: VecDeque<bool> = VecDeque::with_capacity(10);
    let mut last_diag = Instant::now();
    let mut diag_frames: u32 = 0;
    let mut diag_no_face: u32 = 0;
    let mut diag_no_embed: u32 = 0;
    let mut diag_liveness_fail: u32 = 0;
    let mut diag_best_sim: f32 = f32::NEG_INFINITY;
    let mut diag_worst_sim: f32 = f32::INFINITY;

    while start.elapsed() < timeout {
        // Inference thread reads frames directly from camera — we just consume results
        let remaining = timeout.saturating_sub(start.elapsed());
        let wait = remaining.min(Duration::from_millis(200));
        let result = match inference.recv_result(wait) {
            Some(r) => r,
            None => continue,
        };

        diag_frames += 1;
        let now = Instant::now();
        let (metrics, embedding, is_live) = match result {
            InferenceResult::Metrics {
                metrics,
                embedding,
                is_live,
            } => (Some(metrics), embedding, is_live),
            InferenceResult::NoFace => {
                diag_no_face += 1;
                (None, None, None)
            }
        };

        // Run state machine
        if let Some(feedback) = state_machine.transition(metrics.as_ref(), now) {
            // Only send if different from last sent
            let should_send = last_feedback.as_ref() != Some(&feedback);
            if should_send {
                tracing::debug!(session_id, ?feedback, "state transition");
                let _ = send_feedback(pam_stream, session_id, feedback.clone());
                last_feedback = Some(feedback.clone());
            }

            // When in Authenticating state, try recognition
            if feedback == FeedbackState::Authenticating {
                // Track liveness history for temporal stability check
                if let Some(live) = is_live {
                    if liveness_history.len() >= 10 {
                        liveness_history.pop_front();
                    }
                    liveness_history.push_back(live);
                }

                // Spoof detected — silently reject
                if is_live == Some(false) {
                    tracing::debug!(session_id, "spoof detected, resetting match counter");
                    diag_liveness_fail += 1;
                    consecutive_matches = 0;
                    continue;
                }

                // Require temporal liveness stability: ≥80% of last 10 frames must pass
                let pass_count = liveness_history.iter().filter(|&&v| v).count();
                let liveness_stable =
                    !liveness_history.is_empty() && pass_count * 100 / liveness_history.len() >= 80;

                if !liveness_stable {
                    if !liveness_history.is_empty() {
                        tracing::debug!(
                            session_id,
                            pass_count,
                            total = liveness_history.len(),
                            "liveness unstable, resetting match counter"
                        );
                        diag_liveness_fail += 1;
                    }
                    consecutive_matches = 0;
                    continue;
                }

                if let Some(ref emb) = embedding {
                    let max_sim = stored_embeddings
                        .iter()
                        .map(|stored| cosine_similarity(emb, stored))
                        .fold(f32::NEG_INFINITY, f32::max);

                    if max_sim > diag_best_sim {
                        diag_best_sim = max_sim;
                    }
                    if max_sim < diag_worst_sim {
                        diag_worst_sim = max_sim;
                    }

                    if max_sim >= threshold {
                        consecutive_matches += 1;
                        tracing::debug!(
                            session_id,
                            max_sim,
                            consecutive_matches,
                            frames_required,
                            "face match"
                        );
                        // High-confidence shortcut: if similarity is very strong
                        // (>= threshold + 0.10), accept on first match
                        let effective_required = if max_sim >= threshold + 0.10 {
                            1
                        } else {
                            frames_required
                        };
                        if consecutive_matches >= effective_required {
                            tracing::info!(session_id, max_sim, "authentication successful");
                            return AuthOutcome::Success;
                        }
                    } else {
                        tracing::debug!(
                            session_id,
                            max_sim,
                            threshold,
                            "face below threshold, resetting"
                        );
                        consecutive_matches = 0;
                    }
                } else {
                    diag_no_embed += 1;
                }
            } else {
                // Left Authenticating state — face lost resets fully,
                // brief guidance flickers only decay (so near-matches survive)
                if feedback == FeedbackState::Scanning {
                    consecutive_matches = 0;
                    liveness_history.clear();
                } else {
                    consecutive_matches = consecutive_matches.saturating_sub(1);
                }
            }
        }

        // Periodic diagnostics every 1s at info level
        if last_diag.elapsed() >= Duration::from_secs(1) {
            let live_pass = liveness_history.iter().filter(|&&v| v).count();
            let live_total = liveness_history.len();
            tracing::info!(
                session_id,
                frames = diag_frames,
                no_face = diag_no_face,
                no_embed = diag_no_embed,
                liveness_fail = diag_liveness_fail,
                best_sim = %format!("{:.3}", if diag_best_sim == f32::NEG_INFINITY { 0.0 } else { diag_best_sim }),
                worst_sim = %format!("{:.3}", if diag_worst_sim == f32::INFINITY { 0.0 } else { diag_worst_sim }),
                matches = consecutive_matches,
                liveness = %format!("{}/{}", live_pass, live_total),
                ?last_feedback,
                "session diagnostic"
            );
            diag_frames = 0;
            diag_no_face = 0;
            diag_no_embed = 0;
            diag_liveness_fail = 0;
            diag_best_sim = f32::NEG_INFINITY;
            diag_worst_sim = f32::INFINITY;
            last_diag = now;
        }
    }

    tracing::info!(session_id, "session timed out");
    AuthOutcome::Timeout
}

fn send_feedback(
    stream: &std::os::unix::net::UnixStream,
    session_id: u64,
    state: FeedbackState,
) -> Result<(), DaemonError> {
    let msg = DaemonMessage::Feedback { session_id, state };
    let mut writer = BufWriter::new(stream);
    write_message(&mut writer, &msg)?;
    std::io::Write::flush(&mut writer)?;
    Ok(())
}

/// Send a desktop notification for successful auth via notify-send.
/// Looks up user's D-Bus socket at /run/user/<uid>/bus (systemd standard).
/// Fire-and-forget — errors logged but not propagated.
fn send_desktop_notification(username: &str, timeout_ms: i32) {
    let uid = match resolve_uid(username) {
        Some(u) => u,
        None => {
            tracing::debug!("notify: cannot resolve uid for {username}");
            return;
        }
    };

    let dbus_addr = format!("unix:path=/run/user/{uid}/bus");
    let timeout_arg = format!("{timeout_ms}");

    let result = std::process::Command::new("notify-send")
        .env("DBUS_SESSION_BUS_ADDRESS", &dbus_addr)
        .args([
            "--app-name=face-auth",
            "--icon=face-recognition",
            "--urgency=low",
            "--expire-time",
            &timeout_arg,
            "Face Authentication",
            "Authenticated successfully",
        ])
        .uid(uid)
        .status();

    match result {
        Ok(s) if s.success() => tracing::debug!("notify: sent to {username}"),
        Ok(s) => tracing::debug!("notify-send exit {}", s.code().unwrap_or(-1)),
        Err(e) => tracing::debug!("notify-send failed: {e}"),
    }
}

fn resolve_uid(username: &str) -> Option<u32> {
    let passwd = std::fs::read_to_string("/etc/passwd").ok()?;
    for line in passwd.lines() {
        let fields: Vec<&str> = line.splitn(7, ':').collect();
        if fields.len() >= 3 && fields[0] == username {
            return fields[2].parse().ok();
        }
    }
    None
}

fn send_result(
    stream: &std::os::unix::net::UnixStream,
    session_id: u64,
    outcome: AuthOutcome,
) -> Result<(), DaemonError> {
    tracing::info!(session_id, ?outcome, "sending auth result");
    let msg = DaemonMessage::AuthResult {
        session_id,
        outcome,
    };
    let mut writer = BufWriter::new(stream);
    write_message(&mut writer, &msg)?;
    std::io::Write::flush(&mut writer)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_manager_start_and_end() {
        let mut sm = SessionManager::new();
        assert!(!sm.is_active());

        sm.try_start(1).unwrap();
        assert!(sm.is_active());

        assert!(sm.try_start(2).is_err());

        sm.end(1);
        assert!(!sm.is_active());

        sm.try_start(3).unwrap();
        assert!(sm.is_active());
    }

    #[test]
    fn session_manager_end_wrong_id_no_op() {
        let mut sm = SessionManager::new();
        sm.try_start(1).unwrap();
        sm.end(999);
        assert!(sm.is_active());
    }
}

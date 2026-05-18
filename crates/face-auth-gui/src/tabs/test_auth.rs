use face_auth_core::config::Config;
use face_auth_core::framing::{read_message, write_message};
use face_auth_core::protocol::{AuthOutcome, DaemonMessage, FeedbackState, PamRequest, PROTOCOL_VERSION};
use std::io::BufWriter;
use std::os::unix::net::UnixStream;
use std::sync::mpsc;
use std::time::{Duration, Instant};

#[derive(Clone, Debug)]
enum AuthState {
    Idle,
    Connecting,
    Running { start: Instant, last_feedback: String },
    Done { outcome: AuthOutcome, elapsed: f32 },
    Error(String),
}

pub struct TestAuthTab {
    state: AuthState,
    result_rx: Option<mpsc::Receiver<AuthEvent>>,
}

enum AuthEvent {
    Feedback(String),
    Result(AuthOutcome, f32),
    Error(String),
}

impl TestAuthTab {
    pub fn new() -> Self {
        Self { state: AuthState::Idle, result_rx: None }
    }

    pub fn deactivate(&mut self) {
        self.state = AuthState::Idle;
        self.result_rx = None;
    }

    fn start_auth(&mut self, config: &Config) {
        let socket_path = config.daemon.socket_path.clone();
        let username = std::env::var("SUDO_USER")
            .or_else(|_| std::env::var("USER"))
            .unwrap_or_else(|_| "unknown".into());

        let (event_tx, event_rx) = mpsc::channel::<AuthEvent>();
        self.result_rx = Some(event_rx);
        self.state = AuthState::Connecting;

        std::thread::spawn(move || {
            let stream = match UnixStream::connect(&socket_path) {
                Ok(s) => s,
                Err(e) => {
                    let _ = event_tx.send(AuthEvent::Error(
                        format!("Cannot connect to {socket_path}: {e}\nIs face-authd running?")
                    ));
                    return;
                }
            };

            stream.set_read_timeout(Some(Duration::from_secs(35))).ok();
            let session_id: u64 = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis() as u64;

            let request = PamRequest::Auth {
                version: PROTOCOL_VERSION,
                username,
                session_id,
            };
            let mut writer = BufWriter::new(&stream);
            if write_message(&mut writer, &request).is_err()
                || std::io::Write::flush(&mut writer).is_err()
            {
                let _ = event_tx.send(AuthEvent::Error("Failed to send auth request".into()));
                return;
            }

            let start = Instant::now();
            let mut reader = &stream;
            loop {
                let msg: DaemonMessage = match read_message(&mut reader) {
                    Ok(m) => m,
                    Err(e) => {
                        let _ = event_tx.send(AuthEvent::Error(format!("Connection lost: {e}")));
                        return;
                    }
                };
                match msg {
                    DaemonMessage::Feedback { state, .. } => {
                        let label = feedback_label(state);
                        let _ = event_tx.send(AuthEvent::Feedback(label.to_string()));
                    }
                    DaemonMessage::AuthResult { outcome, .. } => {
                        let elapsed = start.elapsed().as_secs_f32();
                        let _ = event_tx.send(AuthEvent::Result(outcome, elapsed));
                        return;
                    }
                }
            }
        });
    }

    pub fn ui(&mut self, ui: &mut egui::Ui, ctx: &egui::Context, config: &Config) {
        // Drain events from auth thread — collect first to release borrow before mutating state.
        let events: Vec<AuthEvent> = self
            .result_rx
            .as_ref()
            .map(|rx| std::iter::from_fn(|| rx.try_recv().ok()).collect())
            .unwrap_or_default();

        for event in events {
            match event {
                AuthEvent::Feedback(label) => {
                    if let AuthState::Running { ref mut last_feedback, .. } = self.state {
                        *last_feedback = label;
                    }
                }
                AuthEvent::Result(outcome, elapsed) => {
                    self.state = AuthState::Done { outcome, elapsed };
                    self.result_rx = None;
                }
                AuthEvent::Error(e) => {
                    self.state = AuthState::Error(e);
                    self.result_rx = None;
                }
            }
        }

        // Transition Connecting → Running
        if matches!(self.state, AuthState::Connecting) {
            self.state = AuthState::Running {
                start: Instant::now(),
                last_feedback: "Connecting...".into(),
            };
        }

        ui.heading("Test Authentication");
        ui.separator();

        match &self.state {
            AuthState::Idle => {
                ui.label("Connect to face-authd and run a test authentication.");
                ui.label(format!("Socket: {}", config.daemon.socket_path));
                ui.separator();
                if ui.button("Start Auth Test").clicked() {
                    let cfg = config.clone();
                    self.start_auth(&cfg);
                }
            }

            AuthState::Running { start, last_feedback } => {
                let elapsed = start.elapsed().as_secs_f32();
                ui.label(format!("Elapsed: {elapsed:.1}s"));
                ui.separator();
                ui.label("Look at the camera...");
                ui.separator();
                ui.heading(last_feedback.as_str());
                ctx.request_repaint_after(Duration::from_millis(100));
            }

            AuthState::Done { outcome, elapsed } => {
                let elapsed = *elapsed;
                match outcome {
                    AuthOutcome::Success => {
                        ui.colored_label(egui::Color32::GREEN,
                            format!("SUCCESS ({elapsed:.1}s)"));
                    }
                    AuthOutcome::Failed => {
                        ui.colored_label(egui::Color32::RED, "FAILED");
                    }
                    AuthOutcome::Timeout => {
                        ui.colored_label(egui::Color32::YELLOW,
                            format!("TIMEOUT ({elapsed:.1}s)"));
                    }
                    _ => { ui.label(format!("{outcome:?}")); }
                }
                ui.separator();
                if ui.button("Try Again").clicked() {
                    self.state = AuthState::Idle;
                }
            }

            AuthState::Error(e) => {
                ui.colored_label(egui::Color32::RED, e.as_str());
                ui.separator();
                if ui.button("Retry").clicked() {
                    self.state = AuthState::Idle;
                }
            }

            AuthState::Connecting => {
                ui.label("Connecting...");
                ctx.request_repaint_after(Duration::from_millis(100));
            }
        }
    }
}

fn feedback_label(state: FeedbackState) -> &'static str {
    match state {
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

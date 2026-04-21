mod configure;
mod debug_ui;

use face_auth_core::config::Config;
use face_auth_core::enrollment;
use face_auth_core::geometry::StateMachine;
use face_auth_models::alignment::align_face;
use face_auth_models::detection::FaceDetector;
use face_auth_models::quality;
use face_auth_models::recognition::FaceRecognizer;
use face_auth_core::geometry::analyze_geometry;
use std::time::{Duration, Instant};

fn main() {
    // Init tracing
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info,ort=warn")),
        )
        .init();

    let args: Vec<String> = std::env::args().collect();
    let username = whoami();

    // Parse flags
    if args.iter().any(|a| a == "--help" || a == "-h") {
        print_usage();
        return;
    }

    if args.iter().any(|a| a == "--test-camera") {
        cmd_test_camera();
        return;
    }

    if args.iter().any(|a| a == "--delete") {
        cmd_delete(&username);
        return;
    }

    if args.iter().any(|a| a == "--status") {
        cmd_status(&username);
        return;
    }

    if args.iter().any(|a| a == "--configure") {
        configure::run_configure();
        return;
    }

    if args.iter().any(|a| a == "--check-config") {
        cmd_check_config(&username);
        return;
    }

    if args.iter().any(|a| a == "--test-auth") {
        if args.iter().any(|a| a == "--debug") {
            cmd_test_auth_debug(&username);
        } else {
            cmd_test_auth(&username);
        }
        return;
    }

    if args.iter().any(|a| a == "--install") {
        cmd_install();
        return;
    }

    if args.iter().any(|a| a == "--uninstall") {
        cmd_uninstall();
        return;
    }

    let debug = args.iter().any(|a| a == "--debug");
    if debug {
        cmd_enroll_debug(&username);
    } else {
        cmd_enroll(&username);
    }
}

fn print_usage() {
    eprintln!("face-enroll — Face authentication enrollment tool");
    eprintln!();
    eprintln!("Usage: face-enroll [OPTIONS]");
    eprintln!();
    eprintln!("Options:");
    eprintln!("  --test-camera   Verify camera access (capture 1 frame, print info, exit)");
    eprintln!("  --test-auth     Run a single auth attempt against the daemon");
    eprintln!("  --debug         Visual debug mode (combine with --test-auth or enrollment)");
    eprintln!("  --configure     Interactive TUI config editor (requires root to save)");
    eprintln!("  --check-config  Validate config, models, camera, enrollment");
    eprintln!("  --delete        Remove enrollment data for current user");
    eprintln!("  --status        Show enrollment status for current user");
    eprintln!("  --install       Configure PAM, video group, systemd, SELinux (requires root)");
    eprintln!("  --uninstall     Remove PAM config and restore backups (requires root)");
    eprintln!("  -h, --help      Show this help");
    eprintln!();
    eprintln!("Run without options to enroll your face.");
    eprintln!("Debug mode: face-enroll --test-auth --debug");
}

// --- Test camera command ---

fn cmd_test_camera() {
    println!("=== Camera Test ===");
    println!();

    let config = load_config();

    println!("Opening camera...");
    let camera = match face_auth_camera::open_camera(&config.camera) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("Failed to open camera: {e}");
            std::process::exit(1);
        }
    };

    println!("Waiting for frame...");
    match camera.recv_frame_timeout(Duration::from_secs(5)) {
        Some(frame) => {
            println!();
            println!("  Resolution: {}x{}", frame.width, frame.height);
            println!("  Frame size: {} bytes", frame.data.len());
            println!("  Format:     GREY (IR grayscale)");
            println!();
            println!("Camera is working.");
        }
        None => {
            eprintln!("No frame received within 5 seconds.");
            std::process::exit(1);
        }
    }
}

// --- Delete command ---

fn cmd_delete(username: &str) {
    let dir = enrollment::enrollment_dir(username);
    if !dir.exists() {
        println!("No enrollment data found for user '{username}'.");
        return;
    }

    eprint!("Delete enrollment for '{username}'? [y/N] ");
    let mut answer = String::new();
    std::io::stdin().read_line(&mut answer).unwrap_or(0);
    if !answer.trim().eq_ignore_ascii_case("y") {
        println!("Cancelled.");
        return;
    }

    match std::fs::remove_dir_all(&dir) {
        Ok(()) => println!("Enrollment data removed for '{username}'."),
        Err(e) => {
            eprintln!("Failed to remove {}: {e}", dir.display());
            std::process::exit(1);
        }
    }
}

// --- Status command ---

fn cmd_status(username: &str) {
    match enrollment::load_embeddings(username) {
        Ok(embeddings) => {
            let version = enrollment::enrollment_version(username).unwrap_or(0);
            println!("User: {username}");
            println!("Enrolled: yes");
            println!("Embeddings: {}", embeddings.len());
            println!("Format version: {version} (current: {})", enrollment::ENROLLMENT_VERSION);
            println!("Path: {}", enrollment::enrollment_dir(username).display());
            if version < enrollment::ENROLLMENT_VERSION {
                println!();
                println!("\x1b[33mWarning: enrollment uses old format (v{version}).\x1b[0m");
                println!("Re-enroll for best accuracy: sudo face-enroll");
            }
        }
        Err(_) => {
            println!("User: {username}");
            println!("Enrolled: no");
        }
    }
}

// --- Check config command ---

fn cmd_check_config(username: &str) {
    use std::path::Path;

    println!("=== Configuration Check ===");
    println!();

    let mut errors = 0u32;
    let mut warnings = 0u32;

    // 1. Config file
    let config_path = "/etc/face-auth/config.toml";
    if Path::new(config_path).exists() {
        match face_auth_core::config::Config::load(Path::new(config_path)) {
            Ok(_) => println!("\x1b[32m[OK]\x1b[0m Config: {config_path}"),
            Err(e) => {
                println!("\x1b[31m[FAIL]\x1b[0m Config parse error: {e}");
                errors += 1;
            }
        }
    } else {
        println!("\x1b[33m[WARN]\x1b[0m Config not found at {config_path} (using defaults)");
        warnings += 1;
    }
    let config = load_config();

    // 2. Model files
    let model_dirs = ["models", "/usr/share/face-auth/models", "/var/lib/face-auth/models"];
    let models = [
        ("det_500m.onnx", "SCRFD detection"),
        ("w600k_mbf.onnx", "ArcFace recognition"),
    ];
    for (file, desc) in &models {
        let found = model_dirs.iter().any(|dir| Path::new(dir).join(file).exists());
        if found {
            println!("\x1b[32m[OK]\x1b[0m Model: {desc} ({file})");
        } else {
            println!("\x1b[31m[FAIL]\x1b[0m Model not found: {file} ({desc})");
            errors += 1;
        }
    }

    // Optional liveness model
    if config.liveness.model_enabled {
        let found = model_dirs.iter().any(|dir| Path::new(dir).join("antispoof_q.onnx").exists());
        if found {
            println!("\x1b[32m[OK]\x1b[0m Model: liveness (antispoof_q.onnx)");
        } else {
            println!("\x1b[31m[FAIL]\x1b[0m Model not found: antispoof_q.onnx (liveness enabled but model missing)");
            errors += 1;
        }
    } else {
        println!("\x1b[90m[SKIP]\x1b[0m Model: liveness (model_enabled=false)");
    }

    // 3. Camera
    let cam_result = if config.camera.device_path.is_empty() {
        // Auto-detect
        let mut found = None;
        for i in 0..8 {
            let path = format!("/dev/video{i}");
            if Path::new(&path).exists() {
                if let Ok(dev) = v4l::Device::with_path(&path) {
                    if let Ok(fmts) = v4l::video::Capture::enum_formats(&dev) {
                        let ir_fourccs = [
                            v4l::FourCC::new(b"GREY"),
                            v4l::FourCC::new(b"Y800"),
                            v4l::FourCC::new(b"BA81"),
                        ];
                        if fmts.iter().any(|f| ir_fourccs.contains(&f.fourcc)) {
                            found = Some(path);
                            break;
                        }
                    }
                }
            }
        }
        found.ok_or_else(|| "no IR camera found".to_string())
    } else {
        if Path::new(&config.camera.device_path).exists() {
            Ok(config.camera.device_path.clone())
        } else {
            Err(format!("{} does not exist", config.camera.device_path))
        }
    };

    match cam_result {
        Ok(path) => {
            // Try opening
            match v4l::Device::with_path(&path) {
                Ok(dev) => {
                    match v4l::video::Capture::format(&dev) {
                        Ok(fmt) => println!(
                            "\x1b[32m[OK]\x1b[0m Camera: {path} ({}x{})",
                            fmt.width, fmt.height
                        ),
                        Err(e) => {
                            println!("\x1b[31m[FAIL]\x1b[0m Camera {path}: cannot get format: {e}");
                            errors += 1;
                        }
                    }
                }
                Err(e) => {
                    println!("\x1b[31m[FAIL]\x1b[0m Camera {path}: {e}");
                    errors += 1;
                }
            }
        }
        Err(e) => {
            println!("\x1b[31m[FAIL]\x1b[0m Camera: {e}");
            errors += 1;
        }
    }

    // 4. IR emitter config
    let ir_paths = ["ir-emitter.toml", "/etc/face-auth/ir-emitter.toml"];
    let ir_found = ir_paths.iter().find(|p| Path::new(p).exists());
    match ir_found {
        Some(path) => {
            match face_auth_platform::ir_emitter::IrEmitterConfig::load(path) {
                Ok(cfg) => println!(
                    "\x1b[32m[OK]\x1b[0m IR emitter: {path} (unit={}, selector={})",
                    cfg.unit, cfg.selector
                ),
                Err(e) => {
                    println!("\x1b[33m[WARN]\x1b[0m IR emitter config parse error: {e}");
                    warnings += 1;
                }
            }
        }
        None => {
            println!("\x1b[33m[WARN]\x1b[0m IR emitter config not found (may work without it)");
            warnings += 1;
        }
    }

    // 5. Enrollment
    match enrollment::load_embeddings(username) {
        Ok(e) => {
            let ver = enrollment::enrollment_version(username).unwrap_or(1);
            let current = enrollment::ENROLLMENT_VERSION;
            if ver < current {
                println!(
                    "\x1b[33m[WARN]\x1b[0m Enrollment: {username} ({} embeddings, format v{ver} — stale, re-enroll for best accuracy)",
                    e.len()
                );
                warnings += 1;
            } else {
                println!("\x1b[32m[OK]\x1b[0m Enrollment: {username} ({} embeddings, v{ver})", e.len());
            }
        }
        Err(_) => {
            println!("\x1b[33m[WARN]\x1b[0m Enrollment: no data for '{username}'");
            warnings += 1;
        }
    }

    // 6. Daemon socket
    if Path::new(&config.daemon.socket_path).exists() {
        println!("\x1b[32m[OK]\x1b[0m Daemon: socket exists ({})", config.daemon.socket_path);
    } else {
        println!("\x1b[33m[WARN]\x1b[0m Daemon: socket not found (is face-authd running?)");
        warnings += 1;
    }

    // 7. PAM module
    let pam_paths = [
        "/usr/lib64/security/pam_face.so",
        "/var/lib/face-auth/pam_face.so",
    ];
    let pam_found = pam_paths.iter().any(|p| Path::new(p).exists());
    if pam_found {
        println!("\x1b[32m[OK]\x1b[0m PAM module: installed");
    } else {
        println!("\x1b[33m[WARN]\x1b[0m PAM module: not found (run 'make install')");
        warnings += 1;
    }

    // 8. Config value sanity checks
    if config.recognition.threshold < 0.5 {
        println!("\x1b[33m[WARN]\x1b[0m Threshold {:.2} is low — may allow spoofs", config.recognition.threshold);
        warnings += 1;
    }
    if config.recognition.threshold > 0.95 {
        println!("\x1b[33m[WARN]\x1b[0m Threshold {:.2} is very high — may reject real faces", config.recognition.threshold);
        warnings += 1;
    }
    if config.daemon.session_timeout_s < 3 {
        println!("\x1b[33m[WARN]\x1b[0m Timeout {}s is very short", config.daemon.session_timeout_s);
        warnings += 1;
    }

    // Summary
    println!();
    if errors == 0 && warnings == 0 {
        println!("\x1b[32mAll checks passed.\x1b[0m");
    } else if errors == 0 {
        println!("\x1b[32mNo errors.\x1b[0m {warnings} warning(s).");
    } else {
        println!("\x1b[31m{errors} error(s)\x1b[0m, {warnings} warning(s).");
        std::process::exit(1);
    }
}

// --- Test auth command ---

fn cmd_test_auth(username: &str) {
    use face_auth_core::framing::{read_message, write_message};
    use face_auth_core::protocol::{
        AuthOutcome, DaemonMessage, FeedbackState, PamRequest, PROTOCOL_VERSION,
    };
    use std::io::BufWriter;
    use std::os::unix::net::UnixStream;

    let config = load_config();
    let socket_path = &config.daemon.socket_path;

    println!("=== Auth Test ===");
    println!("User: {username}");
    println!("Socket: {socket_path}");
    println!();

    // Check enrollment
    match enrollment::load_embeddings(username) {
        Ok(e) => {
            let ver = enrollment::enrollment_version(username).unwrap_or(1);
            if ver < enrollment::ENROLLMENT_VERSION {
                eprintln!("Warning: enrollment is format v{ver} (current: v{}). Re-enroll for best accuracy.", enrollment::ENROLLMENT_VERSION);
            }
            println!("Enrollment: {} embeddings loaded", e.len());
        }
        Err(e) => {
            eprintln!("No enrollment found: {e}");
            eprintln!("Run face-enroll first.");
            std::process::exit(1);
        }
    }

    // Connect to daemon (retry briefly — socket may not exist yet after restart)
    println!("Connecting to daemon...");
    let stream = {
        let mut last_err = None;
        let mut connected = None;
        for attempt in 0..5 {
            match UnixStream::connect(socket_path) {
                Ok(s) => {
                    connected = Some(s);
                    break;
                }
                Err(e) => {
                    last_err = Some(e);
                    if attempt < 4 {
                        std::thread::sleep(Duration::from_millis(500));
                    }
                }
            }
        }
        match connected {
            Some(s) => s,
            None => {
                eprintln!("Cannot connect to {socket_path}: {}", last_err.unwrap());
                eprintln!("Is face-authd running? Check: systemctl status face-authd");
                std::process::exit(1);
            }
        }
    };
    stream
        .set_read_timeout(Some(Duration::from_secs(config.daemon.session_timeout_s + 5)))
        .ok();

    let session_id: u64 = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64;

    // Send auth request
    let request = PamRequest::Auth {
        version: PROTOCOL_VERSION,
        username: username.to_string(),
        session_id,
    };
    let mut writer = BufWriter::new(&stream);
    if let Err(e) = write_message(&mut writer, &request) {
        eprintln!("Failed to send auth request: {e}");
        std::process::exit(1);
    }
    if let Err(e) = std::io::Write::flush(&mut writer) {
        eprintln!("Failed to flush: {e}");
        std::process::exit(1);
    }

    println!("Auth request sent. Look at the camera...");
    println!();

    // Read responses
    let mut reader = &stream;
    let start = Instant::now();

    loop {
        let msg: DaemonMessage = match read_message(&mut reader) {
            Ok(m) => m,
            Err(e) => {
                eprintln!("Connection lost: {e}");
                std::process::exit(1);
            }
        };

        let elapsed = start.elapsed().as_secs_f32();

        match msg {
            DaemonMessage::Feedback { state, .. } => {
                let label = match state {
                    FeedbackState::Scanning => "Scanning...",
                    FeedbackState::TooFar => "Move closer",
                    FeedbackState::TooClose => "Move back",
                    FeedbackState::TurnLeft => "Turn left",
                    FeedbackState::TurnRight => "Turn right",
                    FeedbackState::TiltUp => "Tilt up",
                    FeedbackState::TiltDown => "Tilt down",
                    FeedbackState::IRSaturated => "IR glare — move back",
                    FeedbackState::EyesNotVisible => "Eyes not visible",
                    FeedbackState::LookAtCamera => "Look at camera",
                    FeedbackState::Authenticating => "Authenticating...",
                };
                println!("  [{elapsed:5.1}s] {label}");
            }
            DaemonMessage::AuthResult { outcome, .. } => {
                println!();
                match outcome {
                    AuthOutcome::Success => {
                        println!("Result: SUCCESS ({elapsed:.1}s)");
                    }
                    AuthOutcome::Failed => {
                        println!("Result: FAILED");
                        std::process::exit(1);
                    }
                    AuthOutcome::Timeout => {
                        println!("Result: TIMEOUT ({elapsed:.1}s)");
                        std::process::exit(1);
                    }
                    other => {
                        println!("Result: {other:?}");
                        std::process::exit(1);
                    }
                }
                return;
            }
        }
    }
}

// --- Debug test-auth command (local pipeline with visualization) ---

fn cmd_test_auth_debug(username: &str) {
    use debug_ui::{DebugDetection, DebugFrame, DebugWindow};
    use face_auth_core::protocol::FeedbackState;
    use face_auth_models::recognition::clahe;

    println!("=== Debug Auth Test (local) ===");
    println!("User: {username}");
    println!();

    let config = load_config();

    // Load enrollment
    let enrolled = match enrollment::load_embeddings(username) {
        Ok(e) => {
            println!("Enrollment: {} embeddings", e.len());
            e
        }
        Err(e) => {
            eprintln!("No enrollment: {e}");
            eprintln!("Run face-enroll first.");
            std::process::exit(1);
        }
    };

    // Load models
    println!("Loading models...");
    let mut detector = FaceDetector::load_default().expect("detection model");
    let mut recognizer = FaceRecognizer::load_default().expect("recognition model");

    // Open camera
    println!("Opening camera...");
    let camera = face_auth_camera::open_camera(&config.camera).expect("camera");

    let mut window = DebugWindow::new("face-auth debug — test-auth (ESC to close)");
    let mut state_machine = StateMachine::new(&config.geometry);
    let mut liveness_history: Vec<bool> = Vec::new();
    let mut consecutive_matches: u32 = 0;
    let mut best_sim: f32 = 0.0;
    let mut last_frame_time = Instant::now();
    let mut fps: f32 = 0.0;
    let timeout = Duration::from_secs(config.daemon.session_timeout_s);
    let start = Instant::now();
    let threshold = config.recognition.threshold;
    let frames_required = config.recognition.frames_required;

    println!("Debug window opened. Look at the camera. ESC or timeout to quit.");
    println!();

    while window.is_open() && start.elapsed() < timeout {
        // FPS
        let now = Instant::now();
        let dt = now.duration_since(last_frame_time).as_secs_f32();
        if dt > 0.0 {
            fps = fps * 0.8 + (1.0 / dt) * 0.2; // smoothed
        }
        last_frame_time = now;

        let frame = match camera.recv_frame_timeout(Duration::from_millis(100)) {
            Some(f) => f,
            None => continue,
        };

        let detections = detector
            .detect(&frame.data, frame.width, frame.height)
            .unwrap_or_default();

        let detection_info;
        let border_mode;
        let state_label;

        if detections.is_empty() {
            let feedback = state_machine.transition(None, now);
            if matches!(
                state_machine.state,
                face_auth_core::geometry::AuthState::Idle
                    | face_auth_core::geometry::AuthState::Guidance(FeedbackState::Scanning)
            ) {
                liveness_history.clear();
                consecutive_matches = 0;
            }
            detection_info = None;
            border_mode = 1; // yellow scanning
            state_label = feedback
                .as_ref()
                .map(|f| feedback_to_string(f).to_string())
                .unwrap_or_else(|| "Scanning...".to_string());
        } else {
            let det = &detections[0];
            let mut metrics =
                analyze_geometry(&det.landmarks, &det.bbox, frame.width, frame.height);
            metrics.ir_saturated =
                quality::ir_saturated(&frame.data, &det.bbox, frame.width);
            metrics.blur_score = quality::blur_score(
                &frame.data,
                &det.bbox,
                frame.width,
                frame.height,
            );

            let feedback = state_machine.transition(Some(&metrics), now);

            // Liveness check
            let liveness_scores = quality::ir_liveness_check(
                &frame.data,
                &det.bbox,
                frame.width,
                frame.height,
            );
            let live_pass = liveness_scores.is_live(
                config.liveness.lbp_entropy_min,
                config.liveness.local_contrast_cv_min,
                config.liveness.local_contrast_cv_max,
            );

            liveness_history.push(live_pass);
            if liveness_history.len() > 10 {
                liveness_history.remove(0);
            }
            let pass_count = liveness_history.iter().filter(|&&p| p).count();
            let liveness_stable = !liveness_history.is_empty()
                && pass_count * 100 / liveness_history.len() >= 80;

            // Alignment + recognition if in Authenticating state
            let mut similarity = None;
            let mut aligned_crop = None;
            let mut clahe_crop = None;

            let is_authenticating = matches!(
                state_machine.state,
                face_auth_core::geometry::AuthState::Authenticating
            );

            if is_authenticating && liveness_stable {
                let aligned =
                    align_face(&frame.data, frame.width, frame.height, &det.landmarks);
                aligned_crop = Some(aligned.data.clone());
                clahe_crop = Some(clahe(&aligned.data, 112, 112, 14, 2.0));

                if let Ok(embedding) = recognizer.embed(&aligned) {
                    let max_sim = enrolled
                        .iter()
                        .map(|e| {
                            face_auth_models::recognition::cosine_similarity(&embedding, e)
                        })
                        .fold(0.0f32, |a, b| a.max(b));

                    similarity = Some(max_sim);
                    if max_sim > best_sim {
                        best_sim = max_sim;
                    }

                    if max_sim >= threshold {
                        consecutive_matches += 1;
                    } else {
                        consecutive_matches = 0;
                    }
                }
            } else if !is_authenticating {
                // In guidance state — still show crops if face detected
                let aligned =
                    align_face(&frame.data, frame.width, frame.height, &det.landmarks);
                aligned_crop = Some(aligned.data.clone());
                clahe_crop = Some(clahe(&aligned.data, 112, 112, 14, 2.0));
                consecutive_matches = 0;
            }

            // Check success
            let effective_required =
                if best_sim >= threshold + 0.10 { 1 } else { frames_required };
            if consecutive_matches >= effective_required {
                // Flash green briefly
                let debug_frame = DebugFrame {
                    frame_data: frame.data.to_vec(),
                    frame_w: frame.width,
                    frame_h: frame.height,
                    detection: None,
                    state: "SUCCESS".to_string(),
                    fps,
                    border_mode: 3,
                };
                window.render(&debug_frame);
                std::thread::sleep(Duration::from_millis(1500));
                println!("Result: SUCCESS ({:.1}s, best sim: {:.3})", start.elapsed().as_secs_f32(), best_sim);
                return;
            }

            border_mode = if is_authenticating { 2 } else { 1 };
            state_label = feedback
                .as_ref()
                .map(|f| feedback_to_string(f).to_string())
                .unwrap_or_else(|| {
                    if is_authenticating {
                        "Authenticating...".to_string()
                    } else {
                        "Detecting...".to_string()
                    }
                });

            detection_info = Some(DebugDetection {
                bbox: det.bbox.clone(),
                landmarks: det.landmarks.clone(),
                confidence: det.confidence,
                yaw: metrics.yaw_deg,
                pitch: metrics.pitch_deg,
                roll: metrics.roll_deg,
                face_ratio: metrics.face_width_ratio,
                blur_score: metrics.blur_score,
                ir_saturated: metrics.ir_saturated,
                lbp_entropy: liveness_scores.lbp_entropy,
                contrast_cv: liveness_scores.local_contrast_cv,
                liveness_pass: liveness_stable,
                similarity,
                aligned_crop,
                clahe_crop,
            });
        }

        let debug_frame = DebugFrame {
            frame_data: frame.data.to_vec(),
            frame_w: frame.width,
            frame_h: frame.height,
            detection: detection_info,
            state: state_label,
            fps,
            border_mode,
        };
        window.render(&debug_frame);
    }

    // Flash red for timeout
    let debug_frame = DebugFrame {
        frame_data: vec![0u8; 640 * 360],
        frame_w: 640,
        frame_h: 360,
        detection: None,
        state: "TIMEOUT".to_string(),
        fps: 0.0,
        border_mode: 4,
    };
    window.render(&debug_frame);
    std::thread::sleep(Duration::from_millis(1500));
    println!("Result: TIMEOUT ({:.1}s, best sim: {:.3})", start.elapsed().as_secs_f32(), best_sim);
    std::process::exit(1);
}

// --- Install command ---

fn cmd_install() {
    use std::path::Path;
    use std::process::Command;

    if unsafe { libc::geteuid() } != 0 {
        eprintln!("Must run as root (sudo face-enroll --install)");
        std::process::exit(1);
    }

    println!();
    println!("\x1b[1m=== face-auth installer ===\x1b[0m");
    println!();

    // Detect Atomic
    let is_atomic = !is_dir_writable("/usr/libexec");

    let (libexecdir, pamdir, datadir) = if is_atomic {
        let base = "/var/lib/face-auth";
        (format!("{base}/bin"), base.to_string(), base.to_string())
    } else {
        (
            "/usr/libexec".to_string(),
            "/usr/lib64/security".to_string(),
            "/usr/share/face-auth".to_string(),
        )
    };

    let pam_module_path = if is_atomic {
        format!("{pamdir}/pam_face.so")
    } else {
        "pam_face.so".to_string()
    };

    // Step 1: Verify binaries
    info_msg("Checking installed files...");
    let daemon = format!("{libexecdir}/face-authd");
    let enroll = format!("{libexecdir}/face-enroll");
    let pam_so = format!("{pamdir}/pam_face.so");
    let mut missing = false;
    for f in [&daemon, &enroll, &pam_so] {
        if !Path::new(f).exists() {
            error_msg(&format!("Missing: {f}"));
            missing = true;
        }
    }
    if missing {
        error_msg("Run 'make install' first.");
        std::process::exit(1);
    }
    info_msg("Binaries found.");

    // Step 2: Verify models
    let model_dir = format!("{datadir}/models");
    let model_count = std::fs::read_dir(&model_dir)
        .map(|entries| {
            entries
                .filter_map(|e| e.ok())
                .filter(|e| e.path().extension().is_some_and(|ext| ext == "onnx"))
                .count()
        })
        .unwrap_or(0);
    if model_count < 2 {
        warn_msg(&format!(
            "Found {model_count} ONNX models in {model_dir} (need at least 2: detection + recognition)"
        ));
    } else {
        info_msg(&format!("Found {model_count} ONNX models."));
    }

    // Step 3: Detect display manager
    let dm = detect_display_manager();
    info_msg(&format!("Detected display manager: {dm}"));

    // Step 4: Video group
    let dm_user = if dm == "unknown" {
        warn_msg("Could not detect DM, defaulting to user 'sddm'");
        "sddm"
    } else {
        &dm
    };
    setup_video_group(dm_user);

    // Step 5: PAM configuration
    let pam_line = format!("auth    sufficient  {pam_module_path}");
    let backup_dir = "/etc/face-auth/pam-backup";
    configure_pam_services(&pam_line, backup_dir);

    // Step 6: SELinux
    install_selinux_policy(&datadir);

    // Step 7: systemd
    if command_exists("systemctl") {
        let _ = Command::new("systemctl").arg("daemon-reload").status();
        let _ = Command::new("systemctl")
            .args(["enable", "face-authd.service"])
            .status();
        let _ = Command::new("systemctl")
            .args(["restart", "face-authd.service"])
            .status();
        if Command::new("systemctl")
            .args(["is-active", "--quiet", "face-authd.service"])
            .status()
            .is_ok_and(|s| s.success())
        {
            info_msg("face-authd service started.");
        } else {
            warn_msg("face-authd service failed to start. Check: journalctl -u face-authd");
        }
    }

    // Step 8: PATH setup (Atomic only)
    if is_atomic {
        setup_atomic_path(&libexecdir);
    }

    // Done
    println!();
    println!("\x1b[1m=== Installation complete ===\x1b[0m");
    println!();
    println!("Next steps:");
    println!("  1. Run 'sudo {enroll}' to register your face");
    println!("  2. Lock screen and test face authentication");
    println!();
}

fn cmd_uninstall() {
    use std::path::Path;
    use std::process::Command;

    if unsafe { libc::geteuid() } != 0 {
        eprintln!("Must run as root (sudo face-enroll --uninstall)");
        std::process::exit(1);
    }

    println!();
    println!("\x1b[1m=== face-auth uninstaller ===\x1b[0m");
    println!();

    // Step 1: Stop service
    if command_exists("systemctl") {
        let _ = Command::new("systemctl")
            .args(["stop", "face-authd.service"])
            .status();
        let _ = Command::new("systemctl")
            .args(["disable", "face-authd.service"])
            .status();
        info_msg("Service stopped and disabled.");
    }

    // Step 2: Restore PAM files
    let backup_dir = Path::new("/etc/face-auth/pam-backup");
    if backup_dir.is_dir() {
        if let Ok(entries) = std::fs::read_dir(backup_dir) {
            for entry in entries.filter_map(|e| e.ok()) {
                let path = entry.path();
                if path.extension().is_some_and(|e| e == "bak") {
                    let service = path.file_stem().unwrap_or_default().to_string_lossy();
                    let pam_file = format!("/etc/pam.d/{service}");
                    if Path::new(&pam_file).exists() {
                        if std::fs::copy(&path, &pam_file).is_ok() {
                            info_msg(&format!("Restored {pam_file} from backup"));
                        }
                    }
                }
            }
        }
        let _ = std::fs::remove_dir_all(backup_dir);
        info_msg("Removed PAM backups.");
    }

    // Scan all PAM files for remaining pam_face.so lines
    if let Ok(entries) = std::fs::read_dir("/etc/pam.d") {
        for entry in entries.filter_map(|e| e.ok()) {
            let path = entry.path();
            if let Ok(content) = std::fs::read_to_string(&path) {
                if content.contains("pam_face.so") {
                    let cleaned: String = content
                        .lines()
                        .filter(|l| !l.contains("pam_face.so"))
                        .collect::<Vec<_>>()
                        .join("\n");
                    let _ = std::fs::write(&path, cleaned + "\n");
                    info_msg(&format!("Removed pam_face.so from {}", path.display()));
                }
            }
        }
    }

    // Step 3: SELinux policy
    if command_exists("semodule") {
        let output = Command::new("semodule").args(["-l"]).output();
        if let Ok(out) = output {
            if String::from_utf8_lossy(&out.stdout).contains("face_auth") {
                let _ = Command::new("semodule").args(["-r", "face_auth"]).status();
                info_msg("SELinux policy removed.");
            }
        }
    }

    // Step 4: Reload systemd
    if command_exists("systemctl") {
        let _ = Command::new("systemctl").arg("daemon-reload").status();
    }

    println!();
    println!("\x1b[1m=== Uninstall complete ===\x1b[0m");
    println!();
    info_msg("Note: binaries and models not removed. Run 'make uninstall' or remove manually.");
    println!();
}

// --- Install helper functions ---

const RECOMMENDED_SERVICES: &[&str] = &[
    "sddm", "gdm", "lightdm", "login", "kde", "kde-fingerprint", "kscreensaver",
    "kscreenlocker", "xscreensaver", "sudo", "su", "polkit-1",
    "xfce4-screensaver", "cinnamon-screensaver", "mate-screensaver",
];

const SKIP_SERVICES: &[&str] = &[
    "sddm-greeter", "sddm-autologin", "other", "password-auth", "system-auth",
    "smartcard-auth", "fingerprint-auth", "postlogin", "config-util", "runuser",
    "runuser-l", "remote", "crond", "atd", "cups", "httpd", "cockpit", "vlock",
    "systemd-user",
];

/// Recommended PAM services that may not exist yet and can be created from a template.
/// Each entry: (service_name, fn(pam_line) -> file_content)
const CREATABLE_SERVICES: &[(&str, fn(&str) -> String)] = &[
    ("polkit-1", |pam_line| {
        format!(
            "#%PAM-1.0\n\
             # Created by face-auth installer\n\
             {pam_line}\n\
             auth       include      system-auth\n\
             account    include      system-auth\n\
             password   include      system-auth\n\
             session    include      system-auth\n"
        )
    }),
];

fn configure_pam_services(pam_line: &str, backup_dir: &str) {
    use std::io::{self, Write};
    use std::path::Path;

    println!();
    info_msg("Select which PAM services to enable face auth for:");
    info_msg("(Services with [Y/n] are recommended, [y/N] are optional)");
    println!();

    let mut selected: Vec<String> = Vec::new();
    let mut already_count = 0u32;
    let mut skipped_count = 0u32;

    let mut entries: Vec<_> = std::fs::read_dir("/etc/pam.d")
        .unwrap_or_else(|_| {
            error_msg("Cannot read /etc/pam.d");
            std::process::exit(1);
        })
        .filter_map(|e| e.ok())
        .collect();
    entries.sort_by_key(|e| e.file_name());

    for entry in &entries {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let service = entry.file_name().to_string_lossy().to_string();

        if SKIP_SERVICES.contains(&service.as_str()) {
            skipped_count += 1;
            continue;
        }

        // Check for auth section
        let content = match std::fs::read_to_string(&path) {
            Ok(c) => c,
            Err(_) => continue,
        };
        let has_auth = content
            .lines()
            .any(|l| l.starts_with("auth") || l.starts_with("-auth"));
        if !has_auth {
            continue;
        }

        // Already configured
        if content.contains("pam_face.so") {
            println!("  {service} — already configured");
            already_count += 1;
            continue;
        }

        let is_recommended = RECOMMENDED_SERVICES.contains(&service.as_str());
        let prompt = if is_recommended {
            format!("  Enable for \x1b[1m{service}\x1b[0m (/etc/pam.d/{service})? [Y/n] ")
        } else {
            format!("  Enable for {service} (/etc/pam.d/{service})? [y/N] ")
        };

        print!("{prompt}");
        let _ = io::stdout().flush();
        let mut answer = String::new();
        let _ = io::stdin().read_line(&mut answer);
        let answer = answer.trim();

        let accepted = if is_recommended {
            answer.is_empty() || answer.starts_with(['y', 'Y'])
        } else {
            answer.starts_with(['y', 'Y'])
        };

        if accepted {
            selected.push(path.to_string_lossy().to_string());
        }
    }

    if already_count > 0 {
        info_msg(&format!("{already_count} service(s) already configured."));
    }
    if skipped_count > 0 {
        info_msg(&format!(
            "{skipped_count} system service(s) skipped (no local camera access)."
        ));
    }

    // Offer to create recommended PAM files that don't exist yet (e.g. polkit-1)
    let mut created_count = 0u32;
    for (service, template_fn) in CREATABLE_SERVICES {
        let pam_path = format!("/etc/pam.d/{service}");
        if Path::new(&pam_path).exists() {
            continue; // already handled in the scan above
        }
        print!("  Create \x1b[1m{service}\x1b[0m (/etc/pam.d/{service}) — not present [Y/n] ");
        let _ = io::stdout().flush();
        let mut answer = String::new();
        let _ = io::stdin().read_line(&mut answer);
        if answer.trim().is_empty() || answer.trim().starts_with(['y', 'Y']) {
            let content = template_fn(pam_line);
            match std::fs::write(&pam_path, &content) {
                Ok(()) => {
                    info_msg(&format!("Created {pam_path}"));
                    created_count += 1;
                }
                Err(e) => error_msg(&format!("Failed to create {pam_path}: {e}")),
            }
        }
    }
    if created_count > 0 {
        info_msg(&format!("{created_count} PAM file(s) created."));
    }

    if selected.is_empty() && created_count == 0 {
        info_msg("No PAM files modified.");
        return;
    }
    if selected.is_empty() {
        return;
    }

    // Backup and inject
    let _ = std::fs::create_dir_all(backup_dir);
    for pam_file in &selected {
        let pam_path = Path::new(pam_file);
        let filename = pam_path.file_name().unwrap_or_default().to_string_lossy();
        let backup = format!("{backup_dir}/{filename}.bak");

        // Backup original
        if !Path::new(&backup).exists() {
            if std::fs::copy(pam_file, &backup).is_ok() {
                info_msg(&format!("Backed up {pam_file} → {backup}"));
            }
        }

        // Read content and insert before first auth line
        let content = match std::fs::read_to_string(pam_file) {
            Ok(c) => c,
            Err(_) => continue,
        };

        let lines: Vec<&str> = content.lines().collect();
        let first_auth = lines
            .iter()
            .position(|l| l.starts_with("auth") || l.starts_with("-auth"));

        let mut new_lines: Vec<String> = Vec::with_capacity(lines.len() + 1);
        match first_auth {
            Some(idx) => {
                for (i, line) in lines.iter().enumerate() {
                    if i == idx {
                        new_lines.push(pam_line.to_string());
                    }
                    new_lines.push(line.to_string());
                }
            }
            None => {
                new_lines.push(pam_line.to_string());
                for line in &lines {
                    new_lines.push(line.to_string());
                }
            }
        }

        let _ = std::fs::write(pam_file, new_lines.join("\n") + "\n");
        info_msg(&format!("Added face-auth to {pam_file}"));
    }
}

fn detect_display_manager() -> String {
    use std::process::Command;
    for dm in ["sddm", "gdm", "lightdm"] {
        if Command::new("systemctl")
            .args(["is-active", "--quiet", dm])
            .status()
            .is_ok_and(|s| s.success())
        {
            return dm.to_string();
        }
    }
    "unknown".to_string()
}

fn setup_video_group(dm_user: &str) {
    use std::process::Command;

    // Check if user exists
    let user_exists = Command::new("id").arg(dm_user).output().is_ok_and(|o| o.status.success());
    if !user_exists {
        warn_msg(&format!("User '{dm_user}' does not exist — skip video group setup."));
        return;
    }

    // Check if already in video group
    let in_video = Command::new("id")
        .args(["-nG", dm_user])
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).contains("video"))
        .unwrap_or(false);

    if in_video {
        info_msg(&format!("{dm_user} already in video group."));
    } else {
        let _ = Command::new("usermod").args(["-aG", "video", dm_user]).status();
        info_msg(&format!("Added {dm_user} to video group."));
    }
}

fn install_selinux_policy(datadir: &str) {
    use std::io::Write;
    use std::process::Command;

    if !command_exists("getenforce") {
        info_msg("SELinux not available, skipping policy install.");
        return;
    }

    let enforcing = Command::new("getenforce")
        .output()
        .map(|o| {
            let s = String::from_utf8_lossy(&o.stdout).trim().to_string();
            s != "Disabled"
        })
        .unwrap_or(false);

    if !enforcing {
        info_msg("SELinux not active, skipping policy install.");
        return;
    }

    let te_path = format!("{datadir}/selinux/face_auth.te");
    if !std::path::Path::new(&te_path).exists() {
        warn_msg(&format!("SELinux policy source not found at {te_path}"));
        return;
    }

    print!("\x1b[1m[?]\x1b[0m Install SELinux policy? [Y/n] ");
    let _ = std::io::stdout().flush();
    let mut answer = String::new();
    let _ = std::io::stdin().read_line(&mut answer);
    let answer = answer.trim();
    if !answer.is_empty() && !answer.starts_with(['y', 'Y']) {
        return;
    }

    let tmpdir = std::env::temp_dir().join("face-auth-selinux");
    let _ = std::fs::create_dir_all(&tmpdir);
    let _ = std::fs::copy(&te_path, tmpdir.join("face_auth.te"));

    let success = Command::new("sh")
        .arg("-c")
        .arg(format!(
            "cd {} && checkmodule -M -m -o face_auth.mod face_auth.te 2>/dev/null && \
             semodule_package -o face_auth.pp -m face_auth.mod 2>/dev/null && \
             semodule -i face_auth.pp 2>/dev/null",
            tmpdir.display()
        ))
        .status()
        .is_ok_and(|s| s.success());

    let _ = std::fs::remove_dir_all(&tmpdir);

    if success {
        info_msg("SELinux policy installed.");
    } else {
        warn_msg("SELinux policy installation failed.");
    }
}

fn setup_atomic_path(libexecdir: &str) {
    use std::io::Write;
    use std::process::Command;

    let profile_script = "/etc/profile.d/face-auth.sh";
    if !std::path::Path::new(profile_script).exists() {
        if let Ok(mut f) = std::fs::File::create(profile_script) {
            let _ = writeln!(f, "# Added by face-auth installer");
            let _ = writeln!(f, "export PATH=\"$PATH:{libexecdir}\"");
            info_msg(&format!("Added {libexecdir} to PATH via {profile_script}"));
            info_msg("Open a new terminal or run: source /etc/profile.d/face-auth.sh");
        }
    } else {
        info_msg("PATH profile already configured.");
    }

    let sudoers_drop = "/etc/sudoers.d/face-auth";
    if !std::path::Path::new(sudoers_drop).exists() {
        // Read current secure_path
        let current_secure = Command::new("sudo")
            .arg("-V")
            .output()
            .ok()
            .and_then(|o| {
                let text = String::from_utf8_lossy(&o.stdout).to_string();
                text.lines()
                    .find(|l| l.contains("Default value for") && l.contains("secure_path"))
                    .and_then(|l| {
                        let start = l.find('"')?;
                        let end = l.rfind('"')?;
                        if start < end {
                            Some(l[start + 1..end].to_string())
                        } else {
                            None
                        }
                    })
            })
            .unwrap_or_else(|| "/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin".to_string());

        let content = format!(
            "# Added by face-auth installer\nDefaults secure_path=\"{current_secure}:{libexecdir}\"\n"
        );

        if std::fs::write(sudoers_drop, &content).is_ok() {
            // chmod 440
            let _ = Command::new("chmod").args(["440", sudoers_drop]).status();
            // Validate
            if Command::new("visudo")
                .args(["-c", "-f", sudoers_drop])
                .output()
                .is_ok_and(|o| o.status.success())
            {
                info_msg(&format!("Added {libexecdir} to sudo PATH"));
            } else {
                let _ = std::fs::remove_file(sudoers_drop);
                warn_msg(&format!(
                    "Could not configure sudo PATH. Use full paths: sudo {libexecdir}/face-enroll"
                ));
            }
        }
    }
}

fn is_dir_writable(path: &str) -> bool {
    use std::fs;
    let test = format!("{path}/.face-auth-write-test");
    if fs::write(&test, "").is_ok() {
        let _ = fs::remove_file(&test);
        true
    } else {
        false
    }
}

fn command_exists(cmd: &str) -> bool {
    std::process::Command::new("which")
        .arg(cmd)
        .output()
        .is_ok_and(|o| o.status.success())
}

fn info_msg(msg: &str) {
    println!("\x1b[32m[+]\x1b[0m {msg}");
}

fn warn_msg(msg: &str) {
    println!("\x1b[33m[!]\x1b[0m {msg}");
}

fn error_msg(msg: &str) {
    eprintln!("\x1b[31m[x]\x1b[0m {msg}");
}

// --- Enrollment command ---

fn cmd_enroll(username: &str) {
    // Enrollment needs root to write to user home dirs reliably (chown after save)
    if unsafe { libc::geteuid() } != 0 {
        eprintln!("Warning: not running as root. Enrollment may fail with permission errors.");
        eprintln!("Recommended: sudo {}", std::env::args().next().unwrap_or_default());
        eprintln!();
    }

    println!("=== Face Enrollment ===");
    println!("User: {username}");
    println!();

    let config = load_config();

    // Load models
    println!("Loading detection model...");
    let mut detector = match FaceDetector::load_default() {
        Ok(d) => d,
        Err(e) => {
            eprintln!("Failed to load detection model: {e}");
            std::process::exit(1);
        }
    };

    println!("Loading recognition model...");
    let mut recognizer = match FaceRecognizer::load_default() {
        Ok(r) => r,
        Err(e) => {
            eprintln!("Failed to load recognition model: {e}");
            std::process::exit(1);
        }
    };

    // Open camera
    println!("Opening camera...");
    let camera = match face_auth_camera::open_camera(&config.camera) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("Failed to open camera: {e}");
            std::process::exit(1);
        }
    };

    println!();

    // Multi-angle enrollment
    let poses = [
        ("straight ahead", 0),
        ("slightly LEFT",  1),
        ("slightly RIGHT", 2),
        ("slightly UP",    3),
        ("slightly DOWN",  4),
    ];

    let embeddings_per_pose = 3;
    let total_target = poses.len() * embeddings_per_pose;
    let max_target = config.recognition.max_enrollment.min(total_target as u32) as usize;

    let mut all_embeddings: Vec<[f32; 512]> = Vec::new();
    let timeout_per_pose = Duration::from_secs(10);

    for (pose_name, pose_idx) in &poses {
        if all_embeddings.len() >= max_target {
            break;
        }

        println!("Look {pose_name} at the camera.");
        eprint!("Press Enter when ready...");
        let mut buf = String::new();
        let _ = std::io::stdin().read_line(&mut buf);

        let captured = capture_pose(
            &camera,
            &mut detector,
            &mut recognizer,
            &config,
            embeddings_per_pose,
            timeout_per_pose,
            *pose_idx,
        );

        if captured.is_empty() {
            println!("  No good frames captured for this pose. Skipping.");
        } else {
            println!("  Captured {}/{} for {pose_name}", captured.len(), embeddings_per_pose);
            all_embeddings.extend(captured);
        }
        println!();
    }

    if all_embeddings.is_empty() {
        eprintln!("No embeddings captured. Enrollment failed.");
        std::process::exit(1);
    }

    if all_embeddings.len() < 5 {
        eprintln!(
            "Warning: only {} embeddings captured (recommended: {}). Quality may be lower.",
            all_embeddings.len(),
            max_target
        );
    }

    // Quality scoring: reject outlier embeddings
    let all_embeddings = score_and_filter_embeddings(all_embeddings);

    // Save
    match enrollment::save_embeddings(username, &all_embeddings, config.recognition.max_enrollment) {
        Ok(()) => {
            println!(
                "Enrolled {} face models for user '{username}'.",
                all_embeddings.len(),
            );
            println!("Face authentication is ready.");
        }
        Err(e) => {
            eprintln!("Failed to save enrollment: {e}");
            std::process::exit(1);
        }
    }
}

fn capture_pose(
    camera: &face_auth_camera::CameraHandle,
    detector: &mut FaceDetector,
    recognizer: &mut FaceRecognizer,
    config: &Config,
    target: usize,
    timeout: Duration,
    _pose_idx: usize,
) -> Vec<[f32; 512]> {
    let mut embeddings = Vec::new();
    let mut state_machine = StateMachine::new(&config.geometry);
    let start = Instant::now();
    let mut last_accepted = Instant::now() - Duration::from_secs(10);
    let min_interval = Duration::from_millis(500);

    while embeddings.len() < target && start.elapsed() < timeout {
        let frame = match camera.recv_frame_timeout(Duration::from_millis(200)) {
            Some(f) => f,
            None => continue,
        };

        let detections = match detector.detect(&frame.data, frame.width, frame.height) {
            Ok(d) => d,
            Err(_) => continue,
        };

        if detections.is_empty() {
            continue;
        }

        let det = &detections[0];

        let mut metrics = analyze_geometry(&det.landmarks, &det.bbox, frame.width, frame.height);
        metrics.ir_saturated = quality::ir_saturated(&frame.data, &det.bbox, frame.width);
        metrics.blur_score =
            quality::blur_score(&frame.data, &det.bbox, frame.width, frame.height);

        let now = Instant::now();
        let feedback = state_machine.transition(Some(&metrics), now);

        if let Some(ref fb) = feedback {
            match fb {
                face_auth_core::protocol::FeedbackState::Authenticating => {
                    // Quality gates
                    if metrics.ir_saturated {
                        continue;
                    }
                    if metrics.blur_score < 50.0 {
                        continue;
                    }

                    if now.duration_since(last_accepted) >= min_interval {
                        let aligned =
                            align_face(&frame.data, frame.width, frame.height, &det.landmarks);
                        match recognizer.embed(&aligned) {
                            Ok(emb) => {
                                embeddings.push(emb);
                                last_accepted = now;
                                print!(".");
                                let _ = std::io::Write::flush(&mut std::io::stdout());
                            }
                            Err(e) => {
                                tracing::warn!("embedding error: {e}");
                            }
                        }
                    }
                }
                other => {
                    // Print guidance once per state
                    let msg = feedback_to_string(other);
                    if !msg.is_empty() {
                        print!("\r  {msg}                    \r");
                        let _ = std::io::Write::flush(&mut std::io::stdout());
                    }
                }
            }
        }
    }

    if !embeddings.is_empty() {
        println!();
    }
    embeddings
}

// --- Enrollment quality scoring ---

fn score_and_filter_embeddings(mut embeddings: Vec<[f32; 512]>) -> Vec<[f32; 512]> {
    use face_auth_models::recognition::cosine_similarity;

    if embeddings.len() < 3 {
        println!("Quality: too few embeddings to score (keeping all {}).", embeddings.len());
        return embeddings;
    }

    // Compute average similarity of each embedding to all others
    let n = embeddings.len();
    let mut avg_sims: Vec<f32> = Vec::with_capacity(n);
    for i in 0..n {
        let sum: f32 = (0..n)
            .filter(|&j| j != i)
            .map(|j| cosine_similarity(&embeddings[i], &embeddings[j]))
            .sum();
        avg_sims.push(sum / (n - 1) as f32);
    }

    // Overall quality stats
    let overall_avg = avg_sims.iter().sum::<f32>() / n as f32;
    let min_sim = avg_sims.iter().copied().fold(f32::MAX, f32::min);
    let max_sim = avg_sims.iter().copied().fold(f32::MIN, f32::max);

    println!();
    println!("Embedding quality:");
    println!("  Average inter-similarity: {overall_avg:.3}");
    println!("  Range: {min_sim:.3} — {max_sim:.3}");

    // Reject embeddings with avg similarity < 0.5 (clearly bad crops)
    let reject_threshold = 0.5;
    let mut rejected = 0;
    let mut keep_indices: Vec<usize> = Vec::new();

    for (i, &sim) in avg_sims.iter().enumerate() {
        if sim < reject_threshold {
            rejected += 1;
        } else {
            keep_indices.push(i);
        }
    }

    if rejected > 0 {
        println!("  Rejected {rejected} low-quality embedding(s) (avg sim < {reject_threshold:.2})");
        let filtered: Vec<[f32; 512]> = keep_indices.iter().map(|&i| embeddings[i]).collect();
        if filtered.is_empty() {
            println!("  Warning: all embeddings would be rejected — keeping originals.");
        } else {
            embeddings = filtered;
        }
    } else {
        println!("  All embeddings passed quality check.");
    }

    // Quality grade
    let grade = if overall_avg >= 0.80 {
        "Excellent"
    } else if overall_avg >= 0.70 {
        "Good"
    } else if overall_avg >= 0.60 {
        "Fair"
    } else {
        "Poor (consider re-enrolling in better conditions)"
    };
    println!("  Grade: {grade}");

    // Auto-threshold suggestion
    // Find the minimum pairwise similarity among kept embeddings
    if embeddings.len() >= 2 {
        let mut min_pair_sim = 1.0f32;
        for i in 0..embeddings.len() {
            for j in (i + 1)..embeddings.len() {
                let s = cosine_similarity(&embeddings[i], &embeddings[j]);
                if s < min_pair_sim {
                    min_pair_sim = s;
                }
            }
        }

        // Suggest threshold = min_pair_sim - margin (so real face always passes)
        let suggested = (min_pair_sim - 0.10).max(0.40);
        let current = face_auth_core::config::RecognitionConfig::default().threshold;

        println!();
        println!("Threshold suggestion:");
        println!("  Lowest intra-embedding similarity: {min_pair_sim:.3}");
        println!("  Suggested threshold: {suggested:.2} (current: {current:.2})");
        if (suggested - current).abs() > 0.05 {
            if suggested > current {
                println!("  Note: you could raise threshold to {suggested:.2} for better security.");
            } else {
                println!("  Warning: current threshold {current:.2} may be too high for these embeddings.");
                println!("  Consider lowering to {suggested:.2} in /etc/face-auth/config.toml");
            }
        } else {
            println!("  Current threshold looks appropriate.");
        }
    }

    embeddings
}

// --- Debug enrollment command ---

fn cmd_enroll_debug(username: &str) {
    use debug_ui::{DebugDetection, DebugFrame, DebugWindow};
    use face_auth_models::recognition::clahe;

    if unsafe { libc::geteuid() } != 0 {
        eprintln!("Warning: not running as root. Enrollment may fail with permission errors.");
        eprintln!("Recommended: sudo {}", std::env::args().next().unwrap_or_default());
        eprintln!();
    }

    println!("=== Debug Enrollment ===");
    println!("User: {username}");
    println!();

    let config = load_config();

    println!("Loading models...");
    let mut detector = FaceDetector::load_default().expect("detection model");
    let mut recognizer = FaceRecognizer::load_default().expect("recognition model");

    println!("Opening camera...");
    let camera = face_auth_camera::open_camera(&config.camera).expect("camera");

    let mut window = DebugWindow::new("face-auth debug — enrollment (ESC to close)");
    let mut state_machine = StateMachine::new(&config.geometry);

    let poses = [
        ("straight ahead", 0),
        ("slightly LEFT", 1),
        ("slightly RIGHT", 2),
        ("slightly UP", 3),
        ("slightly DOWN", 4),
    ];
    let embeddings_per_pose = 3;
    let max_target = config.recognition.max_enrollment.min((poses.len() * embeddings_per_pose) as u32) as usize;

    let mut all_embeddings: Vec<[f32; 512]> = Vec::new();
    let mut pose_idx = 0;
    let mut pose_captured = 0;
    let mut last_accepted = Instant::now() - Duration::from_secs(10);
    let min_interval = Duration::from_millis(500);
    let mut fps: f32 = 0.0;
    let mut last_frame_time = Instant::now();
    let mut flash_until: Option<(Instant, u32)> = None; // (deadline, color)

    println!("Debug window opened. Follow on-screen pose instructions. ESC to quit.");
    println!();

    while window.is_open() && pose_idx < poses.len() && all_embeddings.len() < max_target {
        let now = Instant::now();
        let dt = now.duration_since(last_frame_time).as_secs_f32();
        if dt > 0.0 {
            fps = fps * 0.8 + (1.0 / dt) * 0.2;
        }
        last_frame_time = now;

        let frame = match camera.recv_frame_timeout(Duration::from_millis(100)) {
            Some(f) => f,
            None => continue,
        };

        let detections = detector
            .detect(&frame.data, frame.width, frame.height)
            .unwrap_or_default();

        let detection_info;
        let border_mode;

        // Check for active flash
        let flash_active = flash_until
            .as_ref()
            .is_some_and(|(deadline, _)| now < *deadline);

        let state_label = format!(
            "Pose {}/{}: Look {}  [{}/{}]",
            pose_idx + 1,
            poses.len(),
            poses[pose_idx].0,
            pose_captured,
            embeddings_per_pose,
        );

        if detections.is_empty() {
            state_machine.transition(None, now);
            detection_info = None;
            border_mode = if flash_active { flash_until.unwrap().1 as u8 } else { 1 };
        } else {
            let det = &detections[0];
            let mut metrics =
                analyze_geometry(&det.landmarks, &det.bbox, frame.width, frame.height);
            metrics.ir_saturated = quality::ir_saturated(&frame.data, &det.bbox, frame.width);
            metrics.blur_score =
                quality::blur_score(&frame.data, &det.bbox, frame.width, frame.height);

            state_machine.transition(Some(&metrics), now);

            let liveness_scores = quality::ir_liveness_check(
                &frame.data,
                &det.bbox,
                frame.width,
                frame.height,
            );

            let is_authenticating = matches!(
                state_machine.state,
                face_auth_core::geometry::AuthState::Authenticating
            );

            let similarity = None;
            let mut aligned_crop = None;
            let mut clahe_crop = None;

            // Try to capture embedding if quality OK
            if is_authenticating
                && !metrics.ir_saturated
                && metrics.blur_score >= 50.0
                && now.duration_since(last_accepted) >= min_interval
            {
                let aligned =
                    align_face(&frame.data, frame.width, frame.height, &det.landmarks);
                aligned_crop = Some(aligned.data.clone());
                clahe_crop = Some(clahe(&aligned.data, 112, 112, 14, 2.0));

                match recognizer.embed(&aligned) {
                    Ok(emb) => {
                        all_embeddings.push(emb);
                        pose_captured += 1;
                        last_accepted = now;
                        flash_until = Some((now + Duration::from_millis(200), 3)); // green flash

                        if pose_captured >= embeddings_per_pose {
                            pose_idx += 1;
                            pose_captured = 0;
                        }
                    }
                    Err(_) => {
                        flash_until = Some((now + Duration::from_millis(200), 4)); // red flash
                    }
                }
            } else if !aligned_crop.is_some() {
                // Show crops even when not capturing
                let aligned =
                    align_face(&frame.data, frame.width, frame.height, &det.landmarks);
                aligned_crop = Some(aligned.data.clone());
                clahe_crop = Some(clahe(&aligned.data, 112, 112, 14, 2.0));
            }

            border_mode = if flash_active {
                flash_until.unwrap().1 as u8
            } else if is_authenticating {
                2
            } else {
                1
            };

            detection_info = Some(DebugDetection {
                bbox: det.bbox.clone(),
                landmarks: det.landmarks.clone(),
                confidence: det.confidence,
                yaw: metrics.yaw_deg,
                pitch: metrics.pitch_deg,
                roll: metrics.roll_deg,
                face_ratio: metrics.face_width_ratio,
                blur_score: metrics.blur_score,
                ir_saturated: metrics.ir_saturated,
                lbp_entropy: liveness_scores.lbp_entropy,
                contrast_cv: liveness_scores.local_contrast_cv,
                liveness_pass: true, // not relevant for enrollment
                similarity,
                aligned_crop,
                clahe_crop,
            });
        }

        let debug_frame = DebugFrame {
            frame_data: frame.data.to_vec(),
            frame_w: frame.width,
            frame_h: frame.height,
            detection: detection_info,
            state: state_label,
            fps,
            border_mode,
        };
        window.render(&debug_frame);
    }

    // Save
    drop(camera);
    if all_embeddings.is_empty() {
        eprintln!("No embeddings captured. Enrollment failed.");
        std::process::exit(1);
    }

    match enrollment::save_embeddings(username, &all_embeddings, config.recognition.max_enrollment) {
        Ok(()) => {
            println!(
                "Enrolled {} face models for user '{username}'.",
                all_embeddings.len(),
            );
        }
        Err(e) => {
            eprintln!("Failed to save: {e}");
            std::process::exit(1);
        }
    }
}

fn load_config() -> Config {
    Config::load_system().unwrap_or_else(|e| {
        tracing::warn!("config load warning: {e}, using defaults");
        Config::default()
    })
}

fn whoami() -> String {
    std::env::var("SUDO_USER")
        .or_else(|_| std::env::var("USER"))
        .or_else(|_| std::env::var("LOGNAME"))
        .unwrap_or_else(|_| "unknown".into())
}

fn feedback_to_string(fb: &face_auth_core::protocol::FeedbackState) -> &'static str {
    use face_auth_core::protocol::FeedbackState::*;
    match fb {
        Scanning => "Scanning...",
        TooFar => "Move closer",
        TooClose => "Move back",
        TurnLeft => "Turn left",
        TurnRight => "Turn right",
        TiltUp => "Tilt up",
        TiltDown => "Tilt down",
        IRSaturated => "Too much IR glare — move back",
        EyesNotVisible => "Eyes not visible",
        LookAtCamera => "Look at camera",
        Authenticating => "",
    }
}

/// Camera access for enrollment (reuses face-authd's camera module logic).
mod face_auth_camera {
    use face_auth_core::config::CameraConfig;
    use face_auth_platform::ir_emitter::IrEmitterConfig;
    use std::path::Path;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::mpsc;
    use std::sync::Arc;
    use std::time::{Duration, Instant};
    use v4l::buffer::Type;
    use v4l::io::mmap::Stream;
    use v4l::io::traits::CaptureStream;
    use v4l::video::Capture;
    use v4l::{Device, FourCC};

    #[allow(dead_code)]
    pub struct Frame {
        pub data: Vec<u8>,
        pub width: u32,
        pub height: u32,
        pub timestamp: Instant,
    }

    pub struct CameraHandle {
        frame_rx: mpsc::Receiver<Arc<Frame>>,
        stop: Arc<AtomicBool>,
        thread: Option<std::thread::JoinHandle<()>>,
    }

    impl CameraHandle {
        pub fn recv_frame_timeout(&self, timeout: Duration) -> Option<Arc<Frame>> {
            self.frame_rx.recv_timeout(timeout).ok()
        }
    }

    impl Drop for CameraHandle {
        fn drop(&mut self) {
            self.stop.store(true, Ordering::Relaxed);
            if let Some(t) = self.thread.take() {
                let _ = t.join();
            }
        }
    }

    pub fn open_camera(config: &CameraConfig) -> Result<CameraHandle, String> {
        let device_path = if config.device_path.is_empty() {
            detect_ir_camera()?
        } else {
            config.device_path.clone()
        };

        let dev = Device::with_path(&device_path)
            .map_err(|e| format!("open {device_path}: {e}"))?;

        let fmt = dev.format().map_err(|e| format!("get format: {e}"))?;
        let width = fmt.width;
        let height = fmt.height;

        tracing::info!(path = %device_path, width, height, "camera opened for enrollment");

        // Activate IR emitter
        let fd = dev.handle().fd();
        let ir_config = load_ir_config();
        if let Some(ref cfg) = ir_config {
            match cfg.activate(fd) {
                Ok(()) => tracing::info!("IR emitter activated"),
                Err(e) => tracing::warn!("IR emitter activation failed: {e}"),
            }
        }

        let (tx, rx) = mpsc::sync_channel::<Arc<Frame>>(3);
        let stop = Arc::new(AtomicBool::new(false));
        let stop_clone = stop.clone();
        let flush_frames = config.flush_frames;

        let thread = std::thread::Builder::new()
            .name("enroll-camera".into())
            .spawn(move || {
                let fd = dev.handle().fd();
                let mut stream =
                    match Stream::with_buffers(&dev, Type::VideoCapture, 4) {
                        Ok(s) => s,
                        Err(e) => {
                            tracing::error!("stream error: {e}");
                            deactivate_emitter(&ir_config, fd);
                            return;
                        }
                    };

                // Flush initial frames
                for _ in 0..flush_frames {
                    if stop_clone.load(Ordering::Relaxed) {
                        break;
                    }
                    let _ = stream.next();
                }

                while !stop_clone.load(Ordering::Relaxed) {
                    let (buf, _meta) = match stream.next() {
                        Ok(f) => f,
                        Err(_) => continue,
                    };

                    let frame = Arc::new(Frame {
                        data: buf[..(width * height) as usize].to_vec(),
                        width,
                        height,
                        timestamp: Instant::now(),
                    });

                    let _ = tx.try_send(frame);
                }

                drop(stream);
                deactivate_emitter(&ir_config, fd);
            })
            .map_err(|e| format!("spawn camera thread: {e}"))?;

        Ok(CameraHandle {
            frame_rx: rx,
            stop,
            thread: Some(thread),
        })
    }

    fn deactivate_emitter(
        ir_config: &Option<IrEmitterConfig>,
        fd: std::os::fd::RawFd,
    ) {
        if let Some(ref cfg) = ir_config {
            let _ = cfg.deactivate(fd);
        }
    }

    fn detect_ir_camera() -> Result<String, String> {
        let ir_fourccs = [
            FourCC::new(b"GREY"),
            FourCC::new(b"Y800"),
            FourCC::new(b"BA81"),
        ];

        for i in 0..8 {
            let path = format!("/dev/video{i}");
            if !Path::new(&path).exists() {
                continue;
            }
            let Ok(dev) = Device::with_path(&path) else { continue };
            let Ok(formats) = dev.enum_formats() else { continue };
            if formats.iter().any(|f| ir_fourccs.contains(&f.fourcc)) {
                return Ok(path);
            }
        }
        Err("no IR camera found".into())
    }

    fn load_ir_config() -> Option<IrEmitterConfig> {
        for path in ["ir-emitter.toml", "/etc/face-auth/ir-emitter.toml"] {
            if Path::new(path).exists() {
                match IrEmitterConfig::load(path) {
                    Ok(cfg) => return Some(cfg),
                    Err(e) => tracing::warn!(path, "IR config parse error: {e}"),
                }
            }
        }
        None
    }
}

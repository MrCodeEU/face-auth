// Daemon stub for testing PAM module (Phase 2).
//
// Usage:
//   daemon-stub success   — always returns AuthOutcome::Success
//   daemon-stub fail      — always returns AuthOutcome::Failed
//   daemon-stub feedback  — sends feedback messages then Success
//   daemon-stub timeout   — connects but never responds (test PAM timeout)
//
// Listens on /run/face-auth/pam.sock (requires root / socket dir to exist).

use face_auth_core::framing::{read_message, write_message};
use face_auth_core::protocol::{AuthOutcome, DaemonMessage, FeedbackState, PamRequest};
use std::io::BufWriter;
use std::os::unix::net::UnixListener;
use std::path::Path;

const SOCKET_PATH: &str = "/run/face-auth/pam.sock";

fn main() {
    let mode = std::env::args().nth(1).unwrap_or_else(|| "success".into());

    // Ensure socket dir exists
    let socket_dir = Path::new(SOCKET_PATH).parent().unwrap();
    if !socket_dir.exists() {
        std::fs::create_dir_all(socket_dir).expect("create /run/face-auth/");
    }

    // Remove stale socket
    let _ = std::fs::remove_file(SOCKET_PATH);

    let listener = UnixListener::bind(SOCKET_PATH).expect("bind socket");

    // Set socket permissions so PAM module can connect
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(SOCKET_PATH, std::fs::Permissions::from_mode(0o666))
            .expect("set socket permissions");
    }

    println!("=== Phase 2: Daemon Stub (mode={mode}) ===");
    println!("Listening on {SOCKET_PATH}");
    println!("Press Ctrl+C to stop\n");

    for stream in listener.incoming() {
        let stream = match stream {
            Ok(s) => s,
            Err(e) => {
                eprintln!("accept error: {e}");
                continue;
            }
        };

        println!("Client connected");

        let mut reader = &stream;
        let request: PamRequest = match read_message(&mut reader) {
            Ok(r) => r,
            Err(e) => {
                eprintln!("read request error: {e}");
                continue;
            }
        };

        match &request {
            PamRequest::Auth {
                username,
                session_id,
                ..
            } => {
                println!("  Auth request: user={username}, session_id={session_id}");
            }
            PamRequest::Cancel { session_id } => {
                println!("  Cancel request: session_id={session_id}");
                continue;
            }
        }

        let session_id = match &request {
            PamRequest::Auth { session_id, .. } => *session_id,
            PamRequest::Cancel { session_id } => *session_id,
        };

        let mut writer = BufWriter::new(&stream);

        match mode.as_str() {
            "success" => {
                let msg = DaemonMessage::AuthResult {
                    session_id,
                    outcome: AuthOutcome::Success,
                };
                write_message(&mut writer, &msg).expect("write response");
                std::io::Write::flush(&mut writer).unwrap();
                println!("  → Sent AuthResult::Success");
            }
            "fail" => {
                let msg = DaemonMessage::AuthResult {
                    session_id,
                    outcome: AuthOutcome::Failed,
                };
                write_message(&mut writer, &msg).expect("write response");
                std::io::Write::flush(&mut writer).unwrap();
                println!("  → Sent AuthResult::Failed");
            }
            "feedback" => {
                // Send some feedback, then success
                let states = [
                    FeedbackState::Scanning,
                    FeedbackState::TooFar,
                    FeedbackState::Authenticating,
                ];
                for state in &states {
                    let msg = DaemonMessage::Feedback {
                        session_id,
                        state: state.clone(),
                    };
                    write_message(&mut writer, &msg).expect("write feedback");
                    std::io::Write::flush(&mut writer).unwrap();
                    println!("  → Sent Feedback::{state:?}");
                    std::thread::sleep(std::time::Duration::from_millis(500));
                }
                let msg = DaemonMessage::AuthResult {
                    session_id,
                    outcome: AuthOutcome::Success,
                };
                write_message(&mut writer, &msg).expect("write response");
                std::io::Write::flush(&mut writer).unwrap();
                println!("  → Sent AuthResult::Success");
            }
            "timeout" => {
                println!("  → Hanging (testing PAM timeout)...");
                std::thread::sleep(std::time::Duration::from_secs(60));
            }
            other => {
                eprintln!("Unknown mode: {other}");
                eprintln!("Usage: daemon-stub [success|fail|feedback|timeout]");
                std::process::exit(1);
            }
        }

        println!("  Session complete\n");
    }
}

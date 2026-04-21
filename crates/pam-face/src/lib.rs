// PAM module for face authentication.
//
// Connects to face-authd via Unix socket, runs face auth session,
// returns PAM_SUCCESS on match, PAM_AUTH_ERR on mismatch,
// PAM_AUTHINFO_UNAVAIL on any infrastructure failure (falls through to password).
//
// PAM requires the .so name to be `pam_face.so` (no `lib` prefix).
// After `cargo build --release`: cp target/release/libpam_face.so /usr/lib64/security/pam_face.so
//
// SAFETY: This module uses raw C FFI for the PAM interface. All extern "C"
// functions must never panic — a panic in a PAM module crashes the caller
// (sudo, SDDM, etc.).

#![allow(non_camel_case_types)]

use face_auth_core::config::Config;
use face_auth_core::framing::{read_message, write_message};
use face_auth_core::protocol::{
    AuthOutcome, DaemonMessage, FeedbackState, PamRequest, PROTOCOL_VERSION,
};
use libc::{c_char, c_int, c_void};
use std::ffi::CStr;
use std::io::BufWriter;
use std::net::Shutdown;
use std::os::unix::net::UnixStream;
use std::time::Duration;

// --- PAM constants ---

const PAM_SUCCESS: c_int = 0;
const PAM_AUTH_ERR: c_int = 7;
const PAM_AUTHINFO_UNAVAIL: c_int = 9;
// PAM item types
const PAM_USER: c_int = 2;
const PAM_CONV: c_int = 5;

// PAM message styles
const PAM_TEXT_INFO: c_int = 4;

// syslog
const LOG_AUTH: c_int = 4 << 3; // LOG_AUTHPRIV on Linux
const LOG_INFO: c_int = 6;
const LOG_DEBUG: c_int = 7;
const LOG_ERR: c_int = 3;

const SESSION_TIMEOUT: Duration = Duration::from_secs(15);
const DEFAULT_SOCKET: &str = "/run/face-auth/pam.sock";

// --- PAM types ---

#[repr(C)]
pub struct pam_handle_t {
    _opaque: [u8; 0],
}

#[repr(C)]
struct pam_message {
    msg_style: c_int,
    msg: *const c_char,
}

#[repr(C)]
struct pam_response {
    resp: *mut c_char,
    resp_retcode: c_int,
}

#[repr(C)]
struct pam_conv {
    conv: Option<
        unsafe extern "C" fn(
            num_msg: c_int,
            msg: *mut *const pam_message,
            resp: *mut *mut pam_response,
            appdata_ptr: *mut c_void,
        ) -> c_int,
    >,
    appdata_ptr: *mut c_void,
}

extern "C" {
    fn pam_get_item(
        pamh: *const pam_handle_t,
        item_type: c_int,
        item: *mut *const c_void,
    ) -> c_int;

    fn openlog(ident: *const c_char, option: c_int, facility: c_int);
    fn syslog(priority: c_int, format: *const c_char, ...);
    fn closelog();
}

// --- Syslog helpers ---

fn log_open() {
    unsafe {
        openlog(b"pam_face\0".as_ptr() as *const c_char, 0, LOG_AUTH);
    }
}

fn log_close() {
    unsafe {
        closelog();
    }
}

fn log_info(msg: &str) {
    let c_msg = std::ffi::CString::new(msg).unwrap_or_default();
    unsafe {
        syslog(LOG_INFO, b"%s\0".as_ptr() as *const c_char, c_msg.as_ptr());
    }
}

fn log_debug(msg: &str) {
    let c_msg = std::ffi::CString::new(msg).unwrap_or_default();
    unsafe {
        syslog(LOG_DEBUG, b"%s\0".as_ptr() as *const c_char, c_msg.as_ptr());
    }
}

fn log_err(msg: &str) {
    let c_msg = std::ffi::CString::new(msg).unwrap_or_default();
    unsafe {
        syslog(LOG_ERR, b"%s\0".as_ptr() as *const c_char, c_msg.as_ptr());
    }
}

// --- PAM helpers ---

fn get_username(pamh: *mut pam_handle_t) -> Option<String> {
    let mut item: *const c_void = std::ptr::null();
    let ret = unsafe { pam_get_item(pamh as *const _, PAM_USER, &mut item) };
    if ret != PAM_SUCCESS || item.is_null() {
        return None;
    }
    let c_str = unsafe { CStr::from_ptr(item as *const c_char) };
    c_str.to_str().ok().map(|s| s.to_owned())
}

fn get_conv(pamh: *mut pam_handle_t) -> Option<*const pam_conv> {
    let mut item: *const c_void = std::ptr::null();
    let ret = unsafe { pam_get_item(pamh as *const _, PAM_CONV, &mut item) };
    if ret != PAM_SUCCESS || item.is_null() {
        return None;
    }
    Some(item as *const pam_conv)
}

fn send_text_info(conv: *const pam_conv, text: &str) {
    let c_text = match std::ffi::CString::new(text) {
        Ok(s) => s,
        Err(_) => return,
    };

    let msg = pam_message {
        msg_style: PAM_TEXT_INFO,
        msg: c_text.as_ptr(),
    };
    let msg_ptr: *const pam_message = &msg;
    let mut resp: *mut pam_response = std::ptr::null_mut();

    unsafe {
        let conv_ref = &*conv;
        if let Some(conv_fn) = conv_ref.conv {
            // Best-effort: ignore errors from conv callback
            let _ = conv_fn(1, &msg_ptr as *const _ as *mut _, &mut resp, conv_ref.appdata_ptr);
            // Free response if allocated
            if !resp.is_null() {
                if !(*resp).resp.is_null() {
                    libc::free((*resp).resp as *mut c_void);
                }
                libc::free(resp as *mut c_void);
            }
        }
    }
}

fn feedback_to_text(state: &FeedbackState) -> &'static str {
    match state {
        FeedbackState::Scanning => "face-auth: scanning face...",
        FeedbackState::TooFar => "face-auth: move closer",
        FeedbackState::TooClose => "face-auth: move back",
        FeedbackState::TurnLeft => "face-auth: turn left",
        FeedbackState::TurnRight => "face-auth: turn right",
        FeedbackState::TiltUp => "face-auth: tilt up",
        FeedbackState::TiltDown => "face-auth: tilt down",
        FeedbackState::IRSaturated => "face-auth: move back slightly",
        FeedbackState::EyesNotVisible => "face-auth: look at camera",
        FeedbackState::LookAtCamera => "face-auth: look at camera",
        FeedbackState::Authenticating => "face-auth: verifying...",
    }
}

// --- Socket path from config ---

fn get_socket_path() -> String {
    Config::load_system()
        .ok()
        .map(|c| c.daemon.socket_path)
        .unwrap_or_else(|| DEFAULT_SOCKET.to_owned())
}

// --- Core auth logic (never panics — returns PAM code) ---

fn do_authenticate(pamh: *mut pam_handle_t, _flags: c_int) -> c_int {
    let username = match get_username(pamh) {
        Some(u) => u,
        None => {
            log_err("could not get username");
            return PAM_AUTHINFO_UNAVAIL;
        }
    };
    log_debug(&format!("auth request for user"));

    let conv = get_conv(pamh);

    let socket_path = get_socket_path();

    // Connect to daemon socket
    let stream = match UnixStream::connect(&socket_path) {
        Ok(s) => s,
        Err(e) => {
            log_info(&format!("daemon unavailable: {e}"));
            return PAM_AUTHINFO_UNAVAIL;
        }
    };

    if let Err(e) = stream.set_read_timeout(Some(SESSION_TIMEOUT)) {
        log_err(&format!("set timeout failed: {e}"));
        return PAM_AUTHINFO_UNAVAIL;
    }

    let session_id: u64 = {
        let mut buf = [0u8; 8];
        if getrandom(&mut buf) {
            u64::from_le_bytes(buf)
        } else {
            // Fallback: use time-based ID
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos() as u64
        }
    };

    // Send auth request
    let request = PamRequest::Auth {
        version: PROTOCOL_VERSION,
        username,
        session_id,
    };

    let mut writer = BufWriter::new(&stream);
    if let Err(e) = write_message(&mut writer, &request) {
        log_err(&format!("send auth request failed: {e}"));
        return PAM_AUTHINFO_UNAVAIL;
    }
    // Flush the buffered writer
    if let Err(e) = std::io::Write::flush(&mut writer) {
        log_err(&format!("flush failed: {e}"));
        return PAM_AUTHINFO_UNAVAIL;
    }
    drop(writer);

    // Read message loop
    let mut reader = &stream;
    loop {
        let msg: DaemonMessage = match read_message(&mut reader) {
            Ok(m) => m,
            Err(e) => {
                // Timeout or connection dropped
                log_info(&format!("read failed (timeout or disconnect): {e}"));
                // Send cancel best-effort
                let cancel = PamRequest::Cancel { session_id };
                let mut w = BufWriter::new(&stream);
                let _ = write_message(&mut w, &cancel);
                let _ = std::io::Write::flush(&mut w);
                let _ = stream.shutdown(Shutdown::Both);
                return PAM_AUTHINFO_UNAVAIL;
            }
        };

        match msg {
            DaemonMessage::Feedback { state, .. } => {
                log_debug(&format!("feedback: {:?}", state));
                if let Some(c) = conv {
                    send_text_info(c, feedback_to_text(&state));
                }
            }
            DaemonMessage::AuthResult { outcome, .. } => {
                let _ = stream.shutdown(Shutdown::Both);
                return match outcome {
                    AuthOutcome::Success => {
                        log_info("auth success");
                        PAM_SUCCESS
                    }
                    AuthOutcome::Failed => {
                        log_info("auth failed");
                        PAM_AUTH_ERR
                    }
                    AuthOutcome::Timeout
                    | AuthOutcome::DaemonUnavailable
                    | AuthOutcome::Cancelled => {
                        log_info(&format!("auth unavailable: {:?}", outcome));
                        PAM_AUTHINFO_UNAVAIL
                    }
                };
            }
        }
    }
}

/// Read random bytes via getrandom(2). Returns false on failure.
fn getrandom(buf: &mut [u8]) -> bool {
    let ret = unsafe {
        libc::syscall(libc::SYS_getrandom, buf.as_mut_ptr(), buf.len(), 0u32)
    };
    ret == buf.len() as i64
}

// --- PAM entry points ---

/// PAM module entry point: authenticate.
#[no_mangle]
pub extern "C" fn pam_sm_authenticate(
    pamh: *mut pam_handle_t,
    flags: c_int,
    _argc: c_int,
    _argv: *const *const c_char,
) -> c_int {
    // Catch any panic — a panic in PAM crashes the calling process
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        log_open();
        let ret = do_authenticate(pamh, flags);
        log_close();
        ret
    }));

    match result {
        Ok(code) => code,
        Err(_) => {
            // Panic occurred — fall through to password
            PAM_AUTHINFO_UNAVAIL
        }
    }
}

/// Required PAM module symbol — not used for auth.
#[no_mangle]
pub extern "C" fn pam_sm_setcred(
    _pamh: *mut pam_handle_t,
    _flags: c_int,
    _argc: c_int,
    _argv: *const *const c_char,
) -> c_int {
    PAM_SUCCESS
}

/// Required PAM module symbol — not used.
#[no_mangle]
pub extern "C" fn pam_sm_acct_mgmt(
    _pamh: *mut pam_handle_t,
    _flags: c_int,
    _argc: c_int,
    _argv: *const *const c_char,
) -> c_int {
    PAM_SUCCESS
}

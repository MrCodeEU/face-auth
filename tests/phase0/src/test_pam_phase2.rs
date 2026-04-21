// Phase 2: PAM module integration test
//
// Tests the real pam_face.so module against the daemon stub.
//
// Setup:
//   1. Build: cargo build -p pam-face -p phase0 --bin daemon-stub --bin test-pam-phase2
//   2. Install module:
//      sudo cp target/debug/libpam_face.so /usr/lib64/security/pam_face.so
//   3. Install PAM config:
//      sudo cp platform/pam.d/face-auth-test-phase2 /etc/pam.d/face-auth-test-phase2
//
// Test scenarios:
//   A) Daemon stub running (success mode):
//      Terminal 1: sudo cargo run -p phase0 --bin daemon-stub -- success
//      Terminal 2: cargo run -p phase0 --bin test-pam-phase2
//      Expected: pam_authenticate returns PAM_SUCCESS (0)
//
//   B) Daemon not running:
//      cargo run -p phase0 --bin test-pam-phase2
//      Expected: pam_authenticate returns PAM_AUTHINFO_UNAVAIL (9), falls through
//
//   C) Daemon stub running (fail mode):
//      Terminal 1: sudo cargo run -p phase0 --bin daemon-stub -- fail
//      Terminal 2: cargo run -p phase0 --bin test-pam-phase2
//      Expected: pam_authenticate returns PAM_AUTH_ERR (7)
//
//   D) Daemon stub running (feedback mode):
//      Terminal 1: sudo cargo run -p phase0 --bin daemon-stub -- feedback
//      Terminal 2: cargo run -p phase0 --bin test-pam-phase2
//      Expected: TEXT_INFO messages received, then PAM_SUCCESS (0)

use std::ffi::{CStr, CString};
use std::os::raw::{c_char, c_int, c_void};
use std::ptr;

const PAM_SUCCESS: c_int = 0;
const PAM_AUTH_ERR: c_int = 7;
const PAM_AUTHINFO_UNAVAIL: c_int = 9;
const PAM_PROMPT_ECHO_OFF: c_int = 1;
const PAM_TEXT_INFO: c_int = 4;

#[repr(C)]
struct PamMessage {
    msg_style: c_int,
    msg: *const c_char,
}

#[repr(C)]
struct PamResponse {
    resp: *mut c_char,
    resp_retcode: c_int,
}

#[repr(C)]
struct PamConv {
    conv: Option<
        unsafe extern "C" fn(
            num_msg: c_int,
            msg: *mut *const PamMessage,
            resp: *mut *mut PamResponse,
            appdata_ptr: *mut c_void,
        ) -> c_int,
    >,
    appdata_ptr: *mut c_void,
}

type PamHandle = c_void;

#[link(name = "pam")]
extern "C" {
    fn pam_start(
        service: *const c_char,
        user: *const c_char,
        conv: *const PamConv,
        handle: *mut *mut PamHandle,
    ) -> c_int;
    fn pam_authenticate(handle: *mut PamHandle, flags: c_int) -> c_int;
    fn pam_end(handle: *mut PamHandle, status: c_int) -> c_int;
    fn pam_strerror(handle: *mut PamHandle, errnum: c_int) -> *const c_char;
}

struct ConvData {
    text_info: Vec<String>,
}

unsafe extern "C" fn test_conv(
    num_msg: c_int,
    msg: *mut *const PamMessage,
    resp: *mut *mut PamResponse,
    appdata_ptr: *mut c_void,
) -> c_int {
    let data = &mut *(appdata_ptr as *mut ConvData);

    let responses =
        libc::calloc(num_msg as usize, std::mem::size_of::<PamResponse>()) as *mut PamResponse;
    if responses.is_null() {
        return 1;
    }

    for i in 0..num_msg as isize {
        let m = &**msg.offset(i);
        let msg_text = if m.msg.is_null() {
            "(null)".to_string()
        } else {
            CStr::from_ptr(m.msg).to_string_lossy().into_owned()
        };

        match m.msg_style {
            PAM_TEXT_INFO => {
                println!("  [TEXT_INFO] \"{msg_text}\"");
                data.text_info.push(msg_text);
            }
            PAM_PROMPT_ECHO_OFF => {
                println!("  [PROMPT] \"{msg_text}\"");
                let empty = libc::strdup(b"\0".as_ptr() as *const c_char);
                (*responses.offset(i)).resp = empty;
            }
            other => {
                println!("  [style={other}] \"{msg_text}\"");
            }
        }
    }

    *resp = responses;
    PAM_SUCCESS
}

fn main() {
    println!("=== Phase 2: PAM Module Integration Test ===\n");

    // Check prerequisites
    let module_path = std::path::Path::new("target/debug/libpam_face.so");
    if !module_path.exists() {
        println!("ERROR: pam_face.so not built. Run:");
        println!("  cargo build -p pam-face");
        std::process::exit(1);
    }

    let service_path = std::path::Path::new("/etc/pam.d/face-auth-test-phase2");
    if !service_path.exists() {
        println!("ERROR: PAM service not installed. Run:");
        println!("  sudo cp platform/pam.d/face-auth-test-phase2 /etc/pam.d/face-auth-test-phase2");
        std::process::exit(1);
    }

    let service = CString::new("face-auth-test-phase2").unwrap();
    let user = CString::new(std::env::var("USER").unwrap_or_else(|_| "root".to_string())).unwrap();

    let mut conv_data = ConvData {
        text_info: Vec::new(),
    };
    let conv = PamConv {
        conv: Some(test_conv),
        appdata_ptr: &mut conv_data as *mut ConvData as *mut c_void,
    };

    let mut handle: *mut PamHandle = ptr::null_mut();

    println!("Service: face-auth-test-phase2");
    println!("User: {}", user.to_str().unwrap());
    println!(
        "Module: {} (full path in PAM config)\n",
        module_path.canonicalize().unwrap_or_default().display()
    );

    println!("Calling pam_authenticate...\n");

    let rc = unsafe {
        let rc = pam_start(service.as_ptr(), user.as_ptr(), &conv, &mut handle);
        if rc != PAM_SUCCESS {
            let err = CStr::from_ptr(pam_strerror(handle, rc));
            println!("FAIL: pam_start failed: {} ({})", err.to_string_lossy(), rc);
            std::process::exit(1);
        }

        let rc = pam_authenticate(handle, 0);
        pam_end(handle, rc);
        rc
    };

    println!("\n--- Results ---");
    println!("Return code: {rc}");
    match rc {
        PAM_SUCCESS => println!("Status: PAM_SUCCESS — face auth accepted"),
        PAM_AUTH_ERR => println!("Status: PAM_AUTH_ERR — face auth rejected (wrong face)"),
        PAM_AUTHINFO_UNAVAIL => {
            println!("Status: PAM_AUTHINFO_UNAVAIL — daemon unavailable (fall through to password)")
        }
        other => println!("Status: unknown ({other})"),
    }

    if !conv_data.text_info.is_empty() {
        println!(
            "\nFeedback messages received ({}):",
            conv_data.text_info.len()
        );
        for msg in &conv_data.text_info {
            println!("  \"{msg}\"");
        }
    } else {
        println!("\nNo feedback messages received (expected if daemon sent immediate result)");
    }

    println!("\n--- Expected outcomes ---");
    println!("  daemon-stub success  → PAM_SUCCESS (0)");
    println!("  daemon-stub fail     → PAM_AUTH_ERR (7)");
    println!("  daemon-stub feedback → TEXT_INFO msgs + PAM_SUCCESS (0)");
    println!("  daemon not running   → PAM_AUTHINFO_UNAVAIL (9)");
}

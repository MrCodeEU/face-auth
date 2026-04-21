// Phase 0.4: SDDM PAM conversation test
//
// Tests whether PAM_TEXT_INFO messages from a PAM module reach the conversation
// function. This simulates what face-auth will do: send status messages like
// "Detecting face..." via PAM conv during authentication.
//
// Setup (run once, requires root):
//   sudo cp platform/pam.d/face-auth-test /etc/pam.d/face-auth-test
//
// Usage:
//   cargo run -p phase0 --bin test-pam-stub
//
// The test service uses pam_echo.so to send TEXT_INFO, then pam_permit.so to succeed.
// If the conversation function receives the message, PAM conv works.
//
// To test on SDDM lock screen: add `auth optional pam_echo.so "face-auth: testing"`
// to /etc/pam.d/sddm and lock screen. If message appears → SDDM forwards TEXT_INFO.
// If not → need UI socket (Phase 8) for login screen feedback.

use std::ffi::{CStr, CString};
use std::os::raw::{c_char, c_int, c_void};
use std::ptr;

// PAM constants
const PAM_SUCCESS: c_int = 0;
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

/// Track what messages we received
struct ConvData {
    text_info_received: Vec<String>,
    prompts_received: Vec<String>,
}

unsafe extern "C" fn test_conv(
    num_msg: c_int,
    msg: *mut *const PamMessage,
    resp: *mut *mut PamResponse,
    appdata_ptr: *mut c_void,
) -> c_int {
    let data = &mut *(appdata_ptr as *mut ConvData);

    // Allocate responses
    let responses =
        libc::calloc(num_msg as usize, std::mem::size_of::<PamResponse>()) as *mut PamResponse;
    if responses.is_null() {
        return 1; // PAM_BUF_ERR
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
                data.text_info_received.push(msg_text);
            }
            PAM_PROMPT_ECHO_OFF => {
                println!("  [PROMPT_ECHO_OFF] \"{msg_text}\"");
                data.prompts_received.push(msg_text);
                // Respond with empty string (no password needed for pam_permit)
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
    println!("=== Phase 0.4: PAM Conversation Test ===\n");

    // Check if test PAM service exists
    let service_file = std::path::Path::new("/etc/pam.d/face-auth-test");
    if !service_file.exists() {
        println!("PAM service not installed. Run:");
        println!("  sudo cp platform/pam.d/face-auth-test /etc/pam.d/face-auth-test");
        println!("\nCreating platform/pam.d/face-auth-test for you...");

        // Create the PAM service file in repo
        let pam_dir = std::path::Path::new("platform/pam.d");
        std::fs::create_dir_all(pam_dir).expect("create platform/pam.d");
        std::fs::write(
            pam_dir.join("face-auth-test"),
            "# face-auth Phase 0.4 test service\n\
             # Sends TEXT_INFO then permits auth (no password)\n\
             auth optional pam_echo.so \"face-auth: scanning face...\"\n\
             auth optional pam_echo.so \"face-auth: match found\"\n\
             auth required pam_permit.so\n\
             account required pam_permit.so\n",
        )
        .expect("write PAM service file");
        println!("Created. Now run:");
        println!("  sudo cp platform/pam.d/face-auth-test /etc/pam.d/face-auth-test");
        println!("Then re-run this test.");
        std::process::exit(1);
    }

    let service = CString::new("face-auth-test").unwrap();
    let user = CString::new(std::env::var("USER").unwrap_or_else(|_| "root".to_string())).unwrap();

    let mut conv_data = ConvData {
        text_info_received: Vec::new(),
        prompts_received: Vec::new(),
    };

    let conv = PamConv {
        conv: Some(test_conv),
        appdata_ptr: &mut conv_data as *mut ConvData as *mut c_void,
    };

    let mut handle: *mut PamHandle = ptr::null_mut();

    println!(
        "Starting PAM session (service=face-auth-test, user={})...\n",
        user.to_str().unwrap()
    );

    unsafe {
        let rc = pam_start(service.as_ptr(), user.as_ptr(), &conv, &mut handle);
        if rc != PAM_SUCCESS {
            let err = CStr::from_ptr(pam_strerror(handle, rc));
            println!("✗ pam_start failed: {} ({})", err.to_string_lossy(), rc);
            std::process::exit(1);
        }

        println!("Calling pam_authenticate...");
        let rc = pam_authenticate(handle, 0);
        println!();

        if rc == PAM_SUCCESS {
            println!("✓ pam_authenticate succeeded");
        } else {
            let err = CStr::from_ptr(pam_strerror(handle, rc));
            println!(
                "✗ pam_authenticate failed: {} ({})",
                err.to_string_lossy(),
                rc
            );
        }

        pam_end(handle, rc);
    }

    // Report
    println!("\n--- Results ---");
    if conv_data.text_info_received.is_empty() {
        println!("✗ No TEXT_INFO messages received");
        println!("  PAM conv function was not called with info messages.");
    } else {
        println!(
            "✓ Received {} TEXT_INFO message(s):",
            conv_data.text_info_received.len()
        );
        for msg in &conv_data.text_info_received {
            println!("    \"{msg}\"");
        }
    }

    println!("\nConclusion:");
    if !conv_data.text_info_received.is_empty() {
        println!("  PAM conv works for TEXT_INFO. face-auth can send status messages.");
        println!("  Next: test from SDDM lock screen to check if SDDM forwards them.");
        println!("  Add to /etc/pam.d/sddm:");
        println!("    auth optional pam_echo.so \"face-auth: testing SDDM\"");
        println!("  Then lock screen and attempt login.");
    }
}

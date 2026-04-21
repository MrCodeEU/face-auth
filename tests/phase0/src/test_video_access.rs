// Phase 0.5: Test /dev/videoN access as sddm user
//
// Verifies whether the sddm user can open the IR camera device.
// Must run as root (uses setuid/setgid to drop to sddm).
//
// Usage:
//   cargo build -p phase0 --bin test-video-access
//   sudo target/debug/test-video-access [/dev/video3]
//
// Tests:
//   1. Open device as current user (root) — should work
//   2. Drop to sddm uid/gid, try opening — likely fails (sddm not in video group)
//   3. Drop to sddm uid + video gid, try opening — should work
//
// If test 2 fails and test 3 passes → `sudo usermod -aG video sddm` is the fix.

use std::ffi::CString;
use std::os::raw::c_int;
use std::path::Path;

fn try_open(device: &str) -> Result<(), String> {
    let path = CString::new(device).unwrap();
    let fd = unsafe { libc::open(path.as_ptr(), libc::O_RDWR | libc::O_NONBLOCK) };
    if fd < 0 {
        let err = std::io::Error::last_os_error();
        Err(format!("{err}"))
    } else {
        unsafe { libc::close(fd) };
        Ok(())
    }
}

fn get_uid_gid(username: &str) -> Option<(u32, u32)> {
    let name = CString::new(username).ok()?;
    let pw = unsafe { libc::getpwnam(name.as_ptr()) };
    if pw.is_null() {
        None
    } else {
        unsafe { Some(((*pw).pw_uid, (*pw).pw_gid)) }
    }
}

fn get_group_gid(groupname: &str) -> Option<u32> {
    let name = CString::new(groupname).ok()?;
    let gr = unsafe { libc::getgrnam(name.as_ptr()) };
    if gr.is_null() {
        None
    } else {
        unsafe { Some((*gr).gr_gid) }
    }
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let device = args.get(1).map(|s| s.as_str()).unwrap_or("/dev/video3");

    println!("=== Phase 0.5: Video Device Access Test ===\n");

    // Check we're root
    let euid = unsafe { libc::geteuid() };
    if euid != 0 {
        eprintln!("Must run as root: sudo target/debug/test-video-access");
        std::process::exit(1);
    }

    // Check device exists
    if !Path::new(device).exists() {
        eprintln!("Device not found: {device}");
        std::process::exit(1);
    }

    // Show device permissions
    println!("Device: {device}");
    let meta = std::fs::metadata(device).expect("stat device");
    use std::os::unix::fs::MetadataExt;
    println!(
        "  mode: {:o}, uid: {}, gid: {}",
        meta.mode(),
        meta.uid(),
        meta.gid()
    );

    // Resolve sddm user
    let (sddm_uid, sddm_gid) = match get_uid_gid("sddm") {
        Some(ids) => ids,
        None => {
            eprintln!("User 'sddm' not found");
            std::process::exit(1);
        }
    };
    println!("  sddm: uid={sddm_uid}, gid={sddm_gid}");

    // Resolve video group
    let video_gid = match get_group_gid("video") {
        Some(gid) => gid,
        None => {
            eprintln!("Group 'video' not found");
            std::process::exit(1);
        }
    };
    println!("  video group: gid={video_gid}");

    // Check if sddm is already in video group
    let sddm_in_video = is_user_in_group(sddm_uid, video_gid);
    println!("  sddm in video group: {sddm_in_video}");

    // Test 1: Open as root
    println!("\n[Test 1] Open as root (uid={euid})...");
    match try_open(device) {
        Ok(()) => println!("  ✓ OK"),
        Err(e) => println!("  ✗ Failed: {e}"),
    }

    // Test 2: Fork + drop to sddm uid/gid, try open
    println!("\n[Test 2] Open as sddm (uid={sddm_uid}, gid={sddm_gid})...");
    let result = fork_and_test(device, sddm_uid, sddm_gid, &[]);
    match result {
        Ok(()) => println!("  ✓ OK — sddm can already access {device}"),
        Err(e) => println!("  ✗ Failed: {e}"),
    }

    // Test 3: Fork + drop to sddm uid but with video supplementary group
    println!("\n[Test 3] Open as sddm (uid={sddm_uid}) + video group ({video_gid})...");
    let result = fork_and_test(device, sddm_uid, sddm_gid, &[video_gid]);
    match result {
        Ok(()) => println!("  ✓ OK — adding sddm to video group grants access"),
        Err(e) => println!("  ✗ Failed: {e}"),
    }

    // Summary
    println!("\n--- Summary ---");
    if sddm_in_video {
        println!("sddm is already in video group. Device access should work.");
    } else {
        println!("sddm is NOT in video group.");
        println!("Fix: sudo usermod -aG video sddm");
        println!("Then restart SDDM: sudo systemctl restart sddm");
    }
}

fn is_user_in_group(_uid: u32, target_gid: u32) -> bool {
    let mut ngroups: c_int = 64;
    let mut groups = vec![0u32; ngroups as usize];
    let username = CString::new("sddm").unwrap();
    let rc = unsafe {
        libc::getgrouplist(
            username.as_ptr(),
            target_gid as libc::gid_t,
            groups.as_mut_ptr() as *mut libc::gid_t,
            &mut ngroups,
        )
    };
    if rc < 0 {
        return false;
    }
    groups.truncate(ngroups as usize);
    // getgrouplist always includes the basegid we passed, so check if it was
    // actually in the group by checking /etc/group instead
    // Actually let's just check supplementary groups from getgrouplist
    // But we passed target_gid as basegid so it's always included. Use getgrnam instead.
    let name = CString::new("video").unwrap();
    let gr = unsafe { libc::getgrnam(name.as_ptr()) };
    if gr.is_null() {
        return false;
    }
    unsafe {
        let mut mem = (*gr).gr_mem;
        while !(*mem).is_null() {
            let member = std::ffi::CStr::from_ptr(*mem).to_string_lossy();
            if member == "sddm" {
                return true;
            }
            mem = mem.add(1);
        }
    }
    false
}

/// Fork a child, drop privileges, try opening device, report via exit code.
fn fork_and_test(
    device: &str,
    uid: u32,
    gid: u32,
    supplementary: &[u32],
) -> Result<(), String> {
    let pid = unsafe { libc::fork() };
    if pid < 0 {
        return Err("fork failed".to_string());
    }

    if pid == 0 {
        // Child: drop privileges
        unsafe {
            if !supplementary.is_empty() {
                libc::setgroups(
                    supplementary.len(),
                    supplementary.as_ptr() as *const libc::gid_t,
                );
            } else {
                libc::setgroups(0, std::ptr::null());
            }
            libc::setgid(gid as libc::gid_t);
            libc::setuid(uid as libc::uid_t);
        }

        match try_open(device) {
            Ok(()) => std::process::exit(0),
            Err(_) => std::process::exit(1),
        }
    }

    // Parent: wait for child
    let mut status: c_int = 0;
    unsafe { libc::waitpid(pid, &mut status, 0) };

    if libc::WIFEXITED(status) && libc::WEXITSTATUS(status) == 0 {
        Ok(())
    } else {
        Err("Permission denied (child exited non-zero)".to_string())
    }
}

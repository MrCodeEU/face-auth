// IR emitter discovery for UVC cameras.
//
// Brute-forces UVC Extension Unit (XU) controls to find the one that activates
// the IR emitter, then saves the working config to ir-emitter.toml (or --save path).
//
// Mechanism identical to linux-enable-ir-emitter (credit: EmixamPP et al.):
//   https://github.com/EmixamPP/linux-enable-ir-emitter
// Re-implemented in Rust to remove the external tool dependency.
//
// Usage (must run as root):
//   sudo cargo run -p phase0 --bin ir-discover -- /dev/video3
//   sudo cargo run -p phase0 --bin ir-discover -- /dev/video3 --save /etc/face-auth/ir-emitter.toml
//   sudo cargo run -p phase0 --bin ir-discover -- /dev/video3 --exhaustive  # scan all 255×255

use std::fs;
use std::io::Write as IoWrite;
use std::time::Duration;
use v4l::buffer::Type;
use v4l::io::mmap::Stream;
use v4l::io::traits::CaptureStream;
use v4l::video::Capture;
use v4l::Device;

const FRAME_TIMEOUT: Duration = Duration::from_secs(1);

// ──────────────────────────────────────────────
// UVC XU ioctl definitions
// UVCIOC_CTRL_QUERY = _IOWR('u', 0x21, struct uvc_xu_control_query)
// struct size on x86_64 = 16 bytes (3×u8 + pad + u16 + pad + ptr)
// _IOWR = (3<<30)|(16<<16)|('u'<<8)|0x21 = 0xC010_7521
// ──────────────────────────────────────────────

const UVCIOC_CTRL_QUERY: u64 = 0xC010_7521;
const UVC_SET_CUR: u8 = 0x01;
const UVC_GET_CUR: u8 = 0x81;
const UVC_GET_LEN: u8 = 0x85;
const UVC_GET_INFO: u8 = 0x86;

#[repr(C)]
struct UvcXuControlQuery {
    unit: u8,
    selector: u8,
    query: u8,
    _pad1: u8,
    size: u16,
    _pad2: u16,
    data: *mut u8,
}

fn uvc_query(fd: i32, unit: u8, selector: u8, query: u8, data: &mut [u8]) -> std::io::Result<()> {
    let mut req = UvcXuControlQuery {
        unit,
        selector,
        query,
        _pad1: 0,
        size: data.len() as u16,
        _pad2: 0,
        data: data.as_mut_ptr(),
    };
    let ret = unsafe { libc::ioctl(fd, UVCIOC_CTRL_QUERY, &mut req as *mut _) };
    if ret < 0 {
        Err(std::io::Error::last_os_error())
    } else {
        Ok(())
    }
}

fn get_info(fd: i32, unit: u8, sel: u8) -> Option<u8> {
    let mut b = [0u8; 1];
    uvc_query(fd, unit, sel, UVC_GET_INFO, &mut b).ok()?;
    Some(b[0])
}

fn get_len(fd: i32, unit: u8, sel: u8) -> Option<u16> {
    let mut b = [0u8; 2];
    uvc_query(fd, unit, sel, UVC_GET_LEN, &mut b).ok()?;
    Some(u16::from_le_bytes(b))
}

fn get_cur(fd: i32, unit: u8, sel: u8, len: u16) -> Option<Vec<u8>> {
    let mut data = vec![0u8; len as usize];
    uvc_query(fd, unit, sel, UVC_GET_CUR, &mut data).ok()?;
    Some(data)
}

fn set_cur(fd: i32, unit: u8, sel: u8, data: &[u8]) -> std::io::Result<()> {
    let mut buf = data.to_vec();
    uvc_query(fd, unit, sel, UVC_SET_CUR, &mut buf)
}

// ──────────────────────────────────────────────
// Brightness measurement
// ──────────────────────────────────────────────

/// Poll the video fd for data ready, then call stream.next().
/// Returns None on timeout (camera stalled by bad XU command).
fn next_frame_timeout(fd: i32, stream: &mut Stream) -> Option<Vec<u8>> {
    let mut pfd = libc::pollfd {
        fd,
        events: libc::POLLIN,
        revents: 0,
    };
    let ret = unsafe { libc::poll(&mut pfd, 1, FRAME_TIMEOUT.as_millis() as i32) };
    if ret <= 0 {
        return None;
    }
    match stream.next() {
        Ok((buf, _)) => Some(buf.to_vec()),
        Err(_) => None,
    }
}

fn mean_brightness(fd: i32, stream: &mut Stream) -> Option<u8> {
    // discard one frame, then average next two
    next_frame_timeout(fd, stream)?;
    let mut total = 0u64;
    let mut count = 0u64;
    for _ in 0..2 {
        if let Some(buf) = next_frame_timeout(fd, stream) {
            total += buf.iter().map(|&p| p as u64).sum::<u64>();
            count += buf.len() as u64;
        }
    }
    total.checked_div(count).map(|v| v as u8)
}

// ──────────────────────────────────────────────
// Test patterns
// ──────────────────────────────────────────────

/// Known-good IR emitter values from linux-enable-ir-emitter issue #283.
/// Lenovo Luxvisions Innotech 8SSC21 cameras (IdeaPad, ThinkPad).
fn known_lenovo_patterns(len: usize) -> Vec<Vec<u8>> {
    if len == 9 {
        vec![
            vec![0x01, 0x03, 0x02, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00],
            vec![0x01, 0x03, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00],
            vec![0x01, 0x01, 0x02, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00],
            vec![0x01, 0x01, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00],
            vec![0x01, 0x02, 0x02, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00],
        ]
    } else {
        vec![]
    }
}

fn patterns_for(len: usize, current: &[u8]) -> Vec<Vec<u8>> {
    let mut out: Vec<Vec<u8>> = Vec::new();

    // Always try known Lenovo values first
    out.extend(known_lenovo_patterns(len));

    if len == 1 {
        for v in 0u8..=255 {
            out.push(vec![v]);
        }
        return out;
    }

    if len == 2 {
        out.push(vec![0x00, 0x00]);
        out.push(vec![0xFF, 0xFF]);
        let b1 = current.get(1).copied().unwrap_or(0);
        let b0 = current.first().copied().unwrap_or(0);
        for v in 0u8..=255 {
            out.push(vec![v, b1]);
        }
        for v in 0u8..=255 {
            out.push(vec![b0, v]);
        }
        return out;
    }

    // Longer controls: targeted patterns
    out.push(vec![0u8; len]);
    out.push(vec![0xFFu8; len]);
    for byte_pos in 0..len.min(4) {
        for v in [0x01u8, 0x02, 0x03, 0x10, 0x11, 0x80, 0xFF] {
            let mut p = current.to_vec();
            p[byte_pos] = v;
            out.push(p);
        }
    }
    out
}

// ──────────────────────────────────────────────
// Discovery
// ──────────────────────────────────────────────

#[derive(Debug)]
pub struct IrEmitterConfig {
    pub unit: u8,
    pub selector: u8,
    pub enable_data: Vec<u8>,
    pub disable_data: Option<Vec<u8>>,
}

fn discover(device_path: &str, exhaustive: bool) -> Option<IrEmitterConfig> {
    println!("Opening {device_path}...");
    let dev = match Device::with_path(device_path) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("Cannot open {device_path}: {e}");
            return None;
        }
    };
    let fd = dev.handle().fd();

    let mut stream = match Stream::with_buffers(&dev, Type::VideoCapture, 4) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("Cannot open capture stream: {e}");
            return None;
        }
    };

    // Warm up — flush 10 frames for AE to settle
    for _ in 0..10 {
        let _ = next_frame_timeout(fd, &mut stream);
    }
    let baseline1 = mean_brightness(fd, &mut stream).unwrap_or(0);
    println!("Baseline brightness (no face): {baseline1}/255");
    println!();
    println!(">>> Sit in front of the camera now (30cm away). Press Enter when ready.");
    let mut dummy = String::new();
    std::io::stdin().read_line(&mut dummy).ok();
    // Re-measure baseline with face present — flush extra frames for AE settle
    for _ in 0..10 {
        let _ = next_frame_timeout(fd, &mut stream);
    }
    let baseline2 = mean_brightness(fd, &mut stream).unwrap_or(0);
    // Use the MAX of both baselines — prevents false positives from lighting drift
    let baseline = baseline1.max(baseline2);
    // IR emitter should cause a significant brightness jump (typically 30-80+).
    // +20 minimum delta avoids noise from face movement or AE drift.
    let threshold = baseline.saturating_add(20).max(30);
    println!("Baseline with face: {baseline2}/255  using max baseline: {baseline}/255  threshold: >{threshold}/255");
    println!();

    // Scan units 1–20 / selectors 1–32. This covers all real UVC XU controls.
    // Exhaustive 255×255 removed — rapid-fire ioctls on unrecognised controls
    // trigger a uvcvideo kernel bug that removes /dev/videoN until module reload.
    let unit_max: u8 = 20;
    let sel_max: u8 = 32;
    let _ = exhaustive; // flag kept for CLI compat but ignored

    for unit in 1..=unit_max {
        for sel in 1..=sel_max {
            let Some(info) = get_info(fd, unit, sel) else {
                continue;
            };
            if info & 0x02 == 0 {
                continue;
            } // no SET supported

            let Some(len) = get_len(fd, unit, sel) else {
                continue;
            };
            if len == 0 || len > 64 {
                continue;
            }

            let original = get_cur(fd, unit, sel, len);
            let cur = original.clone().unwrap_or_else(|| vec![0u8; len as usize]);

            let patterns = patterns_for(len as usize, &cur);
            let total = patterns.len();
            print!("  unit={unit:3} sel={sel:3} len={len:2} ({total} patterns)  ");
            std::io::stdout().flush().ok();

            let mut found = None;
            let mut timeouts = 0u32;
            'patterns: for (i, pattern) in patterns.iter().enumerate() {
                if set_cur(fd, unit, sel, pattern).is_err() {
                    continue;
                }
                std::thread::sleep(Duration::from_millis(80));
                match mean_brightness(fd, &mut stream) {
                    Some(brightness) if brightness > threshold => {
                        println!("\n    ✓ FOUND at pattern {i}/{total}  brightness={brightness}/255  data={pattern:02x?}");
                        found = Some((pattern.clone(), brightness));
                        break 'patterns;
                    }
                    None => {
                        timeouts += 1;
                        // Camera stalled — restore original and skip remaining patterns
                        if timeouts >= 3 {
                            print!("T");
                            std::io::stdout().flush().ok();
                            if let Some(ref orig) = original {
                                let _ = set_cur(fd, unit, sel, orig);
                            }
                            // Try to unstall by dropping and recreating stream
                            drop(stream);
                            stream = match Stream::with_buffers(&dev, Type::VideoCapture, 4) {
                                Ok(s) => s,
                                Err(_) => {
                                    println!(" (stream lost, aborting)");
                                    return None;
                                }
                            };
                            for _ in 0..3 {
                                let _ = next_frame_timeout(fd, &mut stream);
                            }
                            break 'patterns;
                        }
                    }
                    _ => {}
                }
                // Progress dot every 50 patterns
                if (i + 1) % 50 == 0 {
                    print!(".");
                    std::io::stdout().flush().ok();
                }
            }

            // Always restore
            if let Some(ref orig) = original {
                let _ = set_cur(fd, unit, sel, orig);
                std::thread::sleep(Duration::from_millis(50));
            }

            if let Some((pattern, _)) = found {
                // Confirmation: restore original, measure OFF, re-apply, measure ON.
                // Must see a clear delta to rule out AE drift / lighting change.
                if let Some(ref orig) = original {
                    let _ = set_cur(fd, unit, sel, orig);
                }
                std::thread::sleep(Duration::from_millis(200));
                for _ in 0..5 {
                    let _ = next_frame_timeout(fd, &mut stream);
                }
                let off_brightness = mean_brightness(fd, &mut stream).unwrap_or(0);

                let _ = set_cur(fd, unit, sel, &pattern);
                std::thread::sleep(Duration::from_millis(200));
                for _ in 0..5 {
                    let _ = next_frame_timeout(fd, &mut stream);
                }
                let on_brightness = mean_brightness(fd, &mut stream).unwrap_or(0);

                let delta = on_brightness.saturating_sub(off_brightness);
                println!(
                    "    Confirm: OFF={off_brightness}/255  ON={on_brightness}/255  delta={delta}"
                );

                if delta >= 15 {
                    println!("    ✓ Confirmed! IR emitter control verified.");
                    // Restore before returning
                    if let Some(ref orig) = original {
                        let _ = set_cur(fd, unit, sel, orig);
                    }
                    return Some(IrEmitterConfig {
                        unit,
                        selector: sel,
                        enable_data: pattern,
                        disable_data: original,
                    });
                } else {
                    println!("    ✗ False positive (delta too small). Continuing scan...");
                }
            }

            if timeouts >= 3 {
                println!(" (stalled, skipped)");
            } else {
                println!(" (no activation)");
            }
        }
    }

    println!("\nNo IR emitter control found.");
    None
}

// ──────────────────────────────────────────────
// Config save
// ──────────────────────────────────────────────

fn save_config(cfg: &IrEmitterConfig, path: &str) {
    let enable_hex: Vec<String> = cfg
        .enable_data
        .iter()
        .map(|b| format!("0x{b:02x}"))
        .collect();
    let disable_line = cfg
        .disable_data
        .as_ref()
        .map(|d| {
            let h: Vec<String> = d.iter().map(|b| format!("0x{b:02x}")).collect();
            format!("disable_data = [{}]\n", h.join(", "))
        })
        .unwrap_or_default();

    let toml = format!(
        "# IR emitter config — face-auth ir-discover\n\
         # Camera: Luxvisions Innotech 30c9:00ec\n\
         # Mechanism: UVC XU ioctl UVCIOC_CTRL_QUERY\n\
         # Credit: linux-enable-ir-emitter (EmixamPP et al.)\n\
         #   https://github.com/EmixamPP/linux-enable-ir-emitter\n\
         \n\
         unit        = {}\n\
         selector    = {}\n\
         enable_data = [{}]\n\
         {}",
        cfg.unit,
        cfg.selector,
        enable_hex.join(", "),
        disable_line,
    );

    // Ensure parent dir exists
    if let Some(parent) = std::path::Path::new(path).parent() {
        let _ = fs::create_dir_all(parent);
    }

    match fs::write(path, &toml) {
        Ok(_) => println!("Saved to {path}"),
        Err(e) => {
            eprintln!("Could not write {path}: {e}");
            println!("\nPaste into /etc/face-auth/ir-emitter.toml:\n\n{toml}");
        }
    }
}

// ──────────────────────────────────────────────
// Manual test of known Lenovo IR emitter values
// ──────────────────────────────────────────────

struct KnownConfig {
    unit: u8,
    selector: u8,
    data: Vec<u8>,
    label: &'static str,
}

fn try_known(device_path: &str, save_path: &str) {
    println!("=== Testing known Lenovo IR emitter values ===");
    println!("Point a phone camera at the IR emitter area to see IR light.");
    println!("(Phone cameras can see near-IR that's invisible to eyes.)\n");

    let dev = Device::with_path(device_path).expect("open device");
    let fd = dev.handle().fd();

    let configs = [
        KnownConfig {
            unit: 7,
            selector: 6,
            data: vec![0x01, 0x03, 0x02, 0, 0, 0, 0, 0, 0],
            label: "unit=7  sel=6 (issue #283 variant)",
        },
        KnownConfig {
            unit: 14,
            selector: 6,
            data: vec![0x01, 0x03, 0x02, 0, 0, 0, 0, 0, 0],
            label: "unit=14 sel=6 (IdeaPad/ThinkPad)",
        },
        KnownConfig {
            unit: 7,
            selector: 6,
            data: vec![0x01, 0x01, 0x01, 0, 0, 0, 0, 0, 0],
            label: "unit=7  sel=6 alt1",
        },
        KnownConfig {
            unit: 14,
            selector: 6,
            data: vec![0x01, 0x01, 0x01, 0, 0, 0, 0, 0, 0],
            label: "unit=14 sel=6 alt1",
        },
    ];

    for (i, cfg) in configs.iter().enumerate() {
        // Read current value to restore later
        let original = get_cur(fd, cfg.unit, cfg.selector, cfg.data.len() as u16);

        print!(
            "\n[{}/{}] {}\n  Sending {:02x?} ... ",
            i + 1,
            configs.len(),
            cfg.label,
            cfg.data
        );
        std::io::stdout().flush().ok();

        match set_cur(fd, cfg.unit, cfg.selector, &cfg.data) {
            Ok(()) => println!("OK"),
            Err(e) => {
                println!("FAILED ({e})");
                continue;
            }
        }

        println!("  >>> Look at phone camera NOW. Do you see IR light? [y/n/q] ");
        let mut answer = String::new();
        std::io::stdin().read_line(&mut answer).ok();
        let answer = answer.trim().to_lowercase();

        // Restore original
        if let Some(ref orig) = original {
            let _ = set_cur(fd, cfg.unit, cfg.selector, orig);
            std::thread::sleep(Duration::from_millis(100));
        }

        if answer == "y" || answer == "yes" {
            println!("\n  ✓ Found working config!");
            let result = IrEmitterConfig {
                unit: cfg.unit,
                selector: cfg.selector,
                enable_data: cfg.data.clone(),
                disable_data: original,
            };
            save_config(&result, save_path);
            return;
        } else if answer == "q" || answer == "quit" {
            println!("Aborted.");
            return;
        }
    }

    println!("\nNone of the known configs worked.");
    println!("Try the full scan: sudo cargo run -p phase0 --bin ir-discover");
}

fn main() {
    let args: Vec<String> = std::env::args().collect();

    let exhaustive = args.iter().any(|a| a == "--exhaustive")
        || std::env::var("EXHAUSTIVE").as_deref() == Ok("1");
    let try_known_mode = args.iter().any(|a| a == "--try-known");

    let device_path = args
        .iter()
        .skip(1)
        .find(|a| a.starts_with("/dev/"))
        .cloned()
        .unwrap_or_else(|| {
            // auto-detect IR camera (GREY format)
            for i in 0..8 {
                let p = format!("/dev/video{i}");
                if !std::path::Path::new(&p).exists() {
                    continue;
                }
                let Ok(dev) = Device::with_path(&p) else {
                    continue;
                };
                let Ok(fmts) = dev.enum_formats() else {
                    continue;
                };
                let grey = v4l::FourCC::new(b"GREY");
                let y800 = v4l::FourCC::new(b"Y800");
                if fmts
                    .iter()
                    .any(|f: &v4l::format::Description| f.fourcc == grey || f.fourcc == y800)
                {
                    println!("[auto-detect] IR camera: {p}");
                    return p;
                }
            }
            eprintln!("No IR camera found. Pass /dev/videoN as argument.");
            std::process::exit(1);
        });

    let save_path = args
        .windows(2)
        .find(|w| w[0] == "--save")
        .map(|w| w[1].clone())
        .unwrap_or_else(|| "ir-emitter.toml".to_string());

    if try_known_mode {
        // Also try on video2 (RGB interface) — some cameras route XU through there
        let ir_path = device_path.clone();
        let rgb_path = ir_path
            .replace("video3", "video2")
            .replace("video4", "video2");
        println!("Testing on IR device: {ir_path}");
        try_known(&ir_path, &save_path);
        if ir_path != rgb_path && std::path::Path::new(&rgb_path).exists() {
            println!("\n--- Also testing on RGB device: {rgb_path} ---");
            try_known(&rgb_path, &save_path);
        }
        return;
    }

    match discover(&device_path, exhaustive) {
        Some(cfg) => {
            println!("\n=== Found IR emitter control ===");
            println!("  unit:     {}", cfg.unit);
            println!("  selector: {}", cfg.selector);
            println!("  data:     {:02x?}", cfg.enable_data);
            save_config(&cfg, &save_path);
        }
        None => {
            eprintln!("Not found. Try:");
            eprintln!("  sudo cargo run -p phase0 --bin ir-discover -- /dev/video3 --exhaustive");
            eprintln!("  (sit in front of the camera in a dim room)");
            std::process::exit(1);
        }
    }
}

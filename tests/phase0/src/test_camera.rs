// Phase 0.3: Detect IR camera, enumerate controls, capture one frame, save as PNG.
//
// Usage:
//   cargo run -p phase0 --bin test-camera                   # auto-detect + capture
//   cargo run -p phase0 --bin test-camera /dev/video3       # explicit device
//   cargo run -p phase0 --bin test-camera --list-controls   # enumerate all V4L2 controls on IR device
//   cargo run -p phase0 --bin test-camera --all-devices     # dump controls for every /dev/video*

use std::path::Path;
use v4l::buffer::Type;
use v4l::io::mmap::Stream;
use v4l::io::traits::CaptureStream;
use v4l::video::Capture;
use v4l::{Device, FourCC};

// UVC XU ioctl for IR emitter control
const UVCIOC_CTRL_QUERY: u64 = 0xC010_7521;
const UVC_SET_CUR: u8 = 0x01;

#[repr(C)]
struct UvcXuControlQuery {
    unit: u8, selector: u8, query: u8, _pad1: u8,
    size: u16, _pad2: u16, data: *mut u8,
}

fn uvc_set_cur(fd: i32, unit: u8, selector: u8, data: &[u8]) -> std::io::Result<()> {
    let mut buf = data.to_vec();
    let mut req = UvcXuControlQuery {
        unit, selector, query: UVC_SET_CUR, _pad1: 0,
        size: buf.len() as u16, _pad2: 0, data: buf.as_mut_ptr(),
    };
    let ret = unsafe { libc::ioctl(fd, UVCIOC_CTRL_QUERY, &mut req as *mut _) };
    if ret < 0 { Err(std::io::Error::last_os_error()) } else { Ok(()) }
}

fn activate_ir_emitter(fd: i32, config_path: &str) -> bool {
    let content = match std::fs::read_to_string(config_path) {
        Ok(c) => c,
        Err(_) => { println!("  No IR config at {config_path}, skipping emitter activation"); return false; }
    };
    // Minimal parse — just grab unit, selector, enable_data
    let mut unit = None;
    let mut selector = None;
    let mut enable_data = None;
    for line in content.lines() {
        let line = line.trim();
        if line.starts_with('#') || line.is_empty() { continue; }
        if let Some(rest) = line.strip_prefix("unit") {
            unit = rest.trim().trim_start_matches('=').trim().parse::<u8>().ok();
        } else if let Some(rest) = line.strip_prefix("selector") {
            selector = rest.trim().trim_start_matches('=').trim().parse::<u8>().ok();
        } else if let Some(rest) = line.strip_prefix("enable_data") {
            let s = rest.trim().trim_start_matches('=').trim()
                .trim_start_matches('[').trim_end_matches(']');
            let bytes: Option<Vec<u8>> = s.split(',').map(|t| {
                let t = t.trim();
                if let Some(h) = t.strip_prefix("0x").or_else(|| t.strip_prefix("0X")) {
                    u8::from_str_radix(h, 16).ok()
                } else {
                    t.parse().ok()
                }
            }).collect();
            enable_data = bytes;
        }
    }
    match (unit, selector, enable_data) {
        (Some(u), Some(s), Some(d)) => {
            match uvc_set_cur(fd, u, s, &d) {
                Ok(()) => { println!("  ✓ IR emitter activated (unit={u}, sel={s})"); true }
                Err(e) => { println!("  ✗ IR emitter ioctl failed: {e}"); false }
            }
        }
        _ => { println!("  ✗ Could not parse IR config"); false }
    }
}

fn is_ir_fourcc(fourcc: &FourCC) -> bool {
    *fourcc == FourCC::new(b"GREY")
        || *fourcc == FourCC::new(b"Y800")
        || *fourcc == FourCC::new(b"BA81")
}

fn detect_ir_device() -> Option<String> {
    for i in 0..8 {
        let path = format!("/dev/video{}", i);
        if !Path::new(&path).exists() {
            continue;
        }
        let Ok(dev) = Device::with_path(&path) else { continue };
        let Ok(formats) = dev.enum_formats() else { continue };
        let is_ir = formats.iter().any(|f| is_ir_fourcc(&f.fourcc));
        if is_ir {
            println!("[detect] IR camera: {path}");
            for f in &formats {
                println!("  format: {} ({})", f.fourcc, f.description);
            }
            return Some(path);
        } else {
            let fmt_list: Vec<_> = formats.iter().map(|f| f.fourcc.to_string()).collect();
            let fmt_str = if fmt_list.is_empty() { "(none)".into() } else { fmt_list.join(", ") };
            println!("[detect] {path}: not IR ({fmt_str})");
        }
    }
    None
}

fn print_controls(path: &str) {
    println!("\n=== Controls for {path} ===");
    // v4l 0.14 panics on unknown control types — isolate in thread
    let path_owned = path.to_string();
    let result = std::thread::spawn(move || {
        let dev = Device::with_path(&path_owned)?;
        dev.query_controls()
    }).join();

    match result {
        Err(_) => println!("  (panicked on unknown control type — camera has no standard V4L2 controls)"),
        Ok(Err(e)) => println!("  (error: {e})"),
        Ok(Ok(controls)) => {
            if controls.is_empty() {
                println!("  (no controls)");
                return;
            }
            for ctrl in &controls {
                println!(
                    "  [0x{:08x}] {:40} type={:?} range=[{}, {}] step={} default={}",
                    ctrl.id, ctrl.name, ctrl.typ,
                    ctrl.minimum, ctrl.maximum, ctrl.step, ctrl.default,
                );
            }
        }
    }
}

fn capture_frame(device_path: &str, ir_config: &str) {
    println!("\nOpening {device_path} for capture...");
    let dev = Device::with_path(device_path).expect("open device");
    let fd = dev.handle().fd();

    let fmt = dev.format().expect("get format");
    println!("Format: {}×{} {}", fmt.width, fmt.height, fmt.fourcc);

    // Activate IR emitter before capture
    activate_ir_emitter(fd, ir_config);

    // Print controls so we can spot IR emitter control
    print_controls(device_path);

    let mut stream = Stream::with_buffers(&dev, Type::VideoCapture, 4)
        .expect("create stream");

    // Flush more frames to let IR emitter + AE settle
    println!("\nFlushing 10 frames (IR emitter + AE settle)...");
    for i in 0..10 {
        stream.next().expect("flush frame");
        print!("  {}", i + 1);
        std::io::Write::flush(&mut std::io::stdout()).ok();
    }
    println!();

    let (buf, _meta) = stream.next().expect("capture frame");
    println!("Captured: {} bytes", buf.len());

    let out_path = "ir_frame.png";
    let width = fmt.width;
    let height = fmt.height;

    let img = if is_ir_fourcc(&fmt.fourcc) {
        let data = buf[..(width * height) as usize].to_vec();
        image::GrayImage::from_raw(width, height, data).expect("build image")
    } else {
        eprintln!("Unexpected format {}, saving first W×H bytes as gray", fmt.fourcc);
        let data = buf[..(width * height) as usize].to_vec();
        image::GrayImage::from_raw(width, height, data).expect("build image")
    };

    img.save(out_path).expect("save PNG");

    // Report brightness stats to diagnose IR emitter
    let pixels = img.pixels().map(|p| p.0[0] as u64).collect::<Vec<_>>();
    let mean = pixels.iter().sum::<u64>() / pixels.len() as u64;
    let max = pixels.iter().max().copied().unwrap_or(0);
    println!("Saved {out_path} — mean brightness: {mean}/255, max: {max}/255");
    if mean < 10 {
        println!("  ⚠ Very dark frame — IR emitter likely NOT active.");
        println!("  Try: point a phone camera at the IR camera area to see if LED is on.");
    } else {
        println!("  ✓ Frame has usable brightness — IR emitter appears active.");
    }
}


fn main() {
    let args: Vec<String> = std::env::args().collect();

    if args.iter().any(|a| a == "--all-devices") {
        for i in 0..8 {
            let path = format!("/dev/video{}", i);
            if !Path::new(&path).exists() { continue; }
            print_controls(&path);
        }
        return;
    }

    let device_path = args.iter()
        .skip(1)
        .find(|a| a.starts_with("/dev/"))
        .cloned()
        .or_else(detect_ir_device);

    let device_path = match device_path {
        Some(p) => p,
        None => {
            eprintln!("No IR camera found. Pass device path as argument.");
            std::process::exit(1);
        }
    };

    if args.iter().any(|a| a == "--list-controls") {
        print_controls(&device_path);
        return;
    }

    // IR config: --ir-config path or default locations
    let ir_config = args.windows(2)
        .find(|w| w[0] == "--ir-config")
        .map(|w| w[1].clone())
        .unwrap_or_else(|| {
            // Check common locations
            for p in ["/etc/face-auth/ir-emitter.toml", "ir-emitter.toml"] {
                if Path::new(p).exists() { return p.to_string(); }
            }
            "ir-emitter.toml".to_string()
        });

    capture_frame(&device_path, &ir_config);
}

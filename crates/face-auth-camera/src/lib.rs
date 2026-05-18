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
    /// Blocking receive with timeout (used by CLI enrollment).
    pub fn recv_frame_timeout(&self, timeout: Duration) -> Option<Arc<Frame>> {
        self.frame_rx.recv_timeout(timeout).ok()
    }

    /// Non-blocking receive (used by GUI render loop).
    pub fn try_recv_frame(&self) -> Option<Arc<Frame>> {
        self.frame_rx.try_recv().ok()
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

    let dev = Device::with_path(&device_path).map_err(|e| format!("open {device_path}: {e}"))?;

    let fmt = dev.format().map_err(|e| format!("get format: {e}"))?;
    let width = fmt.width;
    let height = fmt.height;

    tracing::info!(path = %device_path, width, height, "camera opened");

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
        .name("camera".into())
        .spawn(move || {
            let fd = dev.handle().fd();
            let mut stream = match Stream::with_buffers(&dev, Type::VideoCapture, 4) {
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

fn deactivate_emitter(ir_config: &Option<IrEmitterConfig>, fd: std::os::fd::RawFd) {
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
        let Ok(dev) = Device::with_path(&path) else {
            continue;
        };
        let Ok(formats) = dev.enum_formats() else {
            continue;
        };
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

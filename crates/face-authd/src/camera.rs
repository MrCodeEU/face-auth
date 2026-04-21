use crate::error::DaemonError;
use face_auth_core::config::CameraConfig;
use face_auth_platform::ir_emitter::IrEmitterConfig;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Instant;
use v4l::buffer::Type;
use v4l::io::mmap::Stream;
use v4l::io::traits::CaptureStream;
use v4l::video::Capture;
use v4l::{Device, FourCC};

pub struct Frame {
    pub data: Vec<u8>,
    pub width: u32,
    pub height: u32,
    pub timestamp: Instant,
}

pub struct CameraHandle {
    capture_thread: Option<std::thread::JoinHandle<()>>,
    stop_flag: Arc<AtomicBool>,
    frame_rx: Option<std::sync::mpsc::Receiver<Arc<Frame>>>,
}

impl CameraHandle {
    /// Take ownership of the frame receiver (for passing to inference thread).
    /// Can only be called once.
    pub fn take_frame_rx(&mut self) -> Option<std::sync::mpsc::Receiver<Arc<Frame>>> {
        self.frame_rx.take()
    }

    pub fn open(config: &CameraConfig) -> Result<Self, DaemonError> {
        let device_path = if config.device_path.is_empty() {
            detect_ir_camera()?
        } else {
            config.device_path.clone()
        };

        let dev = Device::with_path(&device_path)
            .map_err(|e| DaemonError::Camera(format!("open {device_path}: {e}")))?;

        let fmt = dev
            .format()
            .map_err(|e| DaemonError::Camera(format!("query format: {e}")))?;

        tracing::info!(
            path = %device_path,
            width = fmt.width,
            height = fmt.height,
            fourcc = %fmt.fourcc,
            "camera opened"
        );

        let width = fmt.width;
        let height = fmt.height;

        // Activate IR emitter
        let fd = dev.handle().fd();
        let ir_config = load_ir_config();
        if let Some(ref cfg) = ir_config {
            match cfg.activate(fd) {
                Ok(()) => tracing::info!("IR emitter activated"),
                Err(e) => tracing::warn!("IR emitter activation failed: {e}"),
            }
        }

        let flush_count = config.flush_frames;
        let stop_flag = Arc::new(AtomicBool::new(false));
        let stop = stop_flag.clone();
        let (tx, rx) = std::sync::mpsc::sync_channel::<Arc<Frame>>(3);

        let capture_thread = std::thread::Builder::new()
            .name("camera-capture".into())
            .spawn(move || {
                capture_loop(dev, width, height, flush_count, ir_config, stop, tx);
            })
            .map_err(|e| DaemonError::Camera(format!("spawn capture thread: {e}")))?;

        Ok(Self {
            capture_thread: Some(capture_thread),
            stop_flag,
            frame_rx: Some(rx),
        })
    }
}

impl Drop for CameraHandle {
    fn drop(&mut self) {
        self.stop_flag.store(true, Ordering::Relaxed);
        if let Some(thread) = self.capture_thread.take() {
            let _ = thread.join();
        }
    }
}

fn capture_loop(
    dev: Device,
    width: u32,
    height: u32,
    flush_count: u32,
    ir_config: Option<IrEmitterConfig>,
    stop: Arc<AtomicBool>,
    tx: std::sync::mpsc::SyncSender<Arc<Frame>>,
) {
    let fd = dev.handle().fd();

    let mut stream = match Stream::with_buffers(&dev, Type::VideoCapture, 4) {
        Ok(s) => s,
        Err(e) => {
            tracing::error!("create capture stream: {e}");
            deactivate_emitter(&ir_config, fd);
            return;
        }
    };

    // Flush initial frames (AE settle)
    for _ in 0..flush_count {
        if stop.load(Ordering::Relaxed) {
            deactivate_emitter(&ir_config, fd);
            return;
        }
        if stream.next().is_err() {
            break;
        }
    }
    tracing::debug!(flush_count, "flushed initial frames");

    // Capture loop
    while !stop.load(Ordering::Relaxed) {
        let (buf, _meta) = match stream.next() {
            Ok(frame) => frame,
            Err(e) => {
                tracing::warn!("capture error: {e}");
                break;
            }
        };

        let frame = Arc::new(Frame {
            data: buf[..(width * height) as usize].to_vec(),
            width,
            height,
            timestamp: Instant::now(),
        });

        // try_send: if channel full, drop frame (receiver always gets latest)
        let _ = tx.try_send(frame);
    }

    // Cleanup
    drop(stream);
    deactivate_emitter(&ir_config, fd);
    tracing::debug!("capture thread stopped");
}

fn deactivate_emitter(ir_config: &Option<IrEmitterConfig>, fd: std::os::fd::RawFd) {
    if let Some(ref cfg) = ir_config {
        match cfg.deactivate(fd) {
            Ok(()) => tracing::debug!("IR emitter deactivated"),
            Err(e) => tracing::warn!("IR emitter deactivation failed: {e}"),
        }
    }
}

fn detect_ir_camera() -> Result<String, DaemonError> {
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
            tracing::info!(path = %path, "IR camera detected");
            return Ok(path);
        }
    }

    Err(DaemonError::Camera("no IR camera found".into()))
}

fn load_ir_config() -> Option<IrEmitterConfig> {
    for path in ["/etc/face-auth/ir-emitter.toml", "ir-emitter.toml"] {
        if Path::new(path).exists() {
            match IrEmitterConfig::load(path) {
                Ok(cfg) => {
                    tracing::debug!(path, "loaded IR emitter config");
                    return Some(cfg);
                }
                Err(e) => {
                    tracing::warn!(path, "IR emitter config parse error: {e}");
                }
            }
        }
    }
    tracing::info!("no IR emitter config found, skipping emitter activation");
    None
}

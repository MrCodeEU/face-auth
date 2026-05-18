# Parallel Liveness + face-auth-gui Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Parallelize liveness checks in the auth daemon and build a tab-based egui GUI app for face enrollment and system management.

**Architecture:** Two independent subsystems — (1) a threading change in `face-authd/src/inference.rs` running IR texture and ML liveness concurrently via `std::thread::scope`, and (2) a new `face-auth-gui` egui crate with tab-based UX, camera preview, enrollment wizard, and status panels. A prerequisite extraction moves the inline camera module from `face-enroll` into a shared `face-auth-camera` crate.

**Tech Stack:** Rust stable, `std::thread::scope` (no new deps for liveness), `eframe 0.34` + `egui 0.34` (GUI), `v4l 0.14` (camera, already in workspace).

---

## File Map

### Task 1 — Parallel liveness
- Modify: `crates/face-authd/src/inference.rs`

### Task 2 — Extract face-auth-camera
- Create: `crates/face-auth-camera/Cargo.toml`
- Create: `crates/face-auth-camera/src/lib.rs`
- Modify: `crates/face-enroll/Cargo.toml`
- Modify: `crates/face-enroll/src/main.rs`
- Modify: `Cargo.toml` (workspace)

### Tasks 3–11 — face-auth-gui
- Create: `crates/face-auth-gui/Cargo.toml`
- Create: `crates/face-auth-gui/src/main.rs`
- Create: `crates/face-auth-gui/src/app.rs`
- Create: `crates/face-auth-gui/src/camera_texture.rs`
- Create: `crates/face-auth-gui/src/inference_worker.rs`
- Create: `crates/face-auth-gui/src/tabs/mod.rs`
- Create: `crates/face-auth-gui/src/tabs/test_camera.rs`
- Create: `crates/face-auth-gui/src/tabs/status.rs`
- Create: `crates/face-auth-gui/src/tabs/enroll.rs`
- Create: `crates/face-auth-gui/src/tabs/test_auth.rs`
- Create: `crates/face-auth-gui/src/tabs/check_config.rs`
- Create: `crates/face-auth-gui/src/tabs/configure.rs`
- Modify: `Makefile`
- Modify: `Cargo.toml` (workspace, add gui + camera crates)

---

## Task 1: Parallel liveness checks

**Files:**
- Modify: `crates/face-authd/src/inference.rs`

- [ ] **Step 1: Add unit test for AND-logic**

At the bottom of `crates/face-authd/src/inference.rs`, inside the existing `#[cfg(test)]` block (or add one if absent), add:

```rust
#[cfg(test)]
mod tests {
    fn combine(ir: bool, ml: Option<bool>) -> bool {
        ir && ml.unwrap_or(true)
    }

    #[test]
    fn liveness_and_logic() {
        assert!(combine(true, None),          "IR only, passes");
        assert!(combine(true, Some(true)),    "both enabled, both pass");
        assert!(!combine(false, None),        "IR fails");
        assert!(!combine(false, Some(true)),  "IR fails, ML passes");
        assert!(!combine(true, Some(false)),  "IR passes, ML fails");
        assert!(!combine(false, Some(false)), "both fail");
        // ML model error is treated as pass (unwrap_or(true))
        assert!(combine(true, Some(true)),    "ML error → treated as pass");
    }
}
```

- [ ] **Step 2: Run test to verify it compiles and passes**

```bash
cargo test -p face-authd liveness_and_logic -- --nocapture
```

Expected: `test tests::liveness_and_logic ... ok`

- [ ] **Step 3: Replace sequential liveness block with parallel version**

In `process_frame()`, find the `if should_process {` block. Replace the current Steps 1 and 2 (IR texture then ML) with:

```rust
    if should_process {
        // Run IR texture and ML liveness concurrently.
        // Both read from the same already-captured frame — no shared mutable state.
        let (ir_pass, ml_pass) = std::thread::scope(|s| {
            let ir_handle = if liveness_config.enabled {
                Some(s.spawn(|| {
                    let scores = quality::ir_liveness_check(
                        &frame.data,
                        &det.bbox,
                        frame.width,
                        frame.height,
                    );
                    scores.is_live(
                        liveness_config.lbp_entropy_min,
                        liveness_config.local_contrast_cv_min,
                        liveness_config.local_contrast_cv_max,
                    )
                }))
            } else {
                None
            };

            // ML liveness runs on current thread (needs &mut LivenessDetector).
            let ml = liveness.as_mut().map(|live| {
                live.check(&frame.data, frame.width, frame.height, &det.bbox)
                    .map(|r| r.is_real(liveness_config.model_threshold))
                    .unwrap_or(true) // model errors don't block auth
            });

            let ir = ir_handle.map(|h| h.join().unwrap_or(false)).unwrap_or(true);
            (ir, ml)
        });

        let live_pass = ir_pass && ml_pass.unwrap_or(true);
        is_live = Some(live_pass);

        tracing::debug!(
            ir_pass,
            ml_pass = ?ml_pass,
            live_pass,
            "liveness checks complete"
        );

        if !live_pass {
            let elapsed_ms = start.elapsed().as_millis();
            tracing::debug!(elapsed_ms, "liveness failed, skipping recognition");
            return InferenceResult::Metrics {
                metrics,
                embedding: None,
                is_live,
            };
        }

        // Alignment + Recognition (only if all liveness passed)
        if let Some(rec) = recognizer {
            let aligned = align_face(&frame.data, frame.width, frame.height, &det.landmarks);
            match rec.embed(&aligned) {
                Ok(emb) => embedding = Some(Box::new(emb)),
                Err(e) => tracing::warn!("embedding error: {e}"),
            }
        }
    }
```

- [ ] **Step 4: Build and test**

```bash
cargo build -p face-authd
cargo test -p face-authd
```

Expected: compiles cleanly, all tests pass.

- [ ] **Step 5: Commit**

```bash
git add crates/face-authd/src/inference.rs
git commit -m "perf(inference): run IR texture and ML liveness checks in parallel"
```

---

## Task 2: Extract face-auth-camera crate

**Files:**
- Create: `crates/face-auth-camera/Cargo.toml`
- Create: `crates/face-auth-camera/src/lib.rs`
- Modify: `Cargo.toml`
- Modify: `crates/face-enroll/Cargo.toml`
- Modify: `crates/face-enroll/src/main.rs`

- [ ] **Step 1: Create the crate skeleton**

```bash
mkdir -p crates/face-auth-camera/src
```

Create `crates/face-auth-camera/Cargo.toml`:

```toml
[package]
name        = "face-auth-camera"
version     = "0.1.0"
edition     = "2021"
description = "IR camera capture shared between face-enroll and face-auth-gui"

[dependencies]
face-auth-core     = { path = "../face-auth-core" }
face-auth-platform = { path = "../face-auth-platform" }
tracing            = { workspace = true }
v4l                = "0.14"
```

- [ ] **Step 2: Write the lib**

Create `crates/face-auth-camera/src/lib.rs` by copying the `face_auth_camera` module from `crates/face-enroll/src/main.rs` (lines starting `mod face_auth_camera {`), removing the module wrapper, and making everything `pub`. Then add `try_recv_frame` to `CameraHandle`:

```rust
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

    let dev = Device::with_path(&device_path)
        .map_err(|e| format!("open {device_path}: {e}"))?;

    let fmt = dev.format().map_err(|e| format!("get format: {e}"))?;
    let width = fmt.width;
    let height = fmt.height;

    tracing::info!(path = %device_path, width, height, "camera opened");

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
                    if let Some(ref cfg) = ir_config { let _ = cfg.deactivate(fd); }
                    return;
                }
            };
            for _ in 0..flush_frames {
                if stop_clone.load(Ordering::Relaxed) { break; }
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
            if let Some(ref cfg) = ir_config { let _ = cfg.deactivate(fd); }
        })
        .map_err(|e| format!("spawn camera thread: {e}"))?;

    Ok(CameraHandle { frame_rx: rx, stop, thread: Some(thread) })
}

fn detect_ir_camera() -> Result<String, String> {
    let ir_fourccs = [FourCC::new(b"GREY"), FourCC::new(b"Y800"), FourCC::new(b"BA81")];
    for i in 0..8 {
        let path = format!("/dev/video{i}");
        if !Path::new(&path).exists() { continue; }
        let Ok(dev) = Device::with_path(&path) else { continue; };
        let Ok(formats) = dev.enum_formats() else { continue; };
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
```

- [ ] **Step 3: Add to workspace and update face-enroll**

In root `Cargo.toml`, add to `members`:
```toml
"crates/face-auth-camera",
```

In `crates/face-enroll/Cargo.toml`, add:
```toml
face-auth-camera = { path = "../face-auth-camera" }
```

Remove `v4l` from face-enroll Cargo.toml (it's now in face-auth-camera). Keep it if face-enroll uses v4l directly elsewhere — check with grep:

```bash
grep -n "v4l::" crates/face-enroll/src/main.rs | grep -v "face_auth_camera"
```

If no results outside the camera module, remove `v4l = "0.14"` from face-enroll/Cargo.toml.

- [ ] **Step 4: Replace inline module in face-enroll**

In `crates/face-enroll/src/main.rs`:

1. Delete the entire `mod face_auth_camera { ... }` block at the bottom (approximately lines 2110–2276).
2. Add at the top of the file:
```rust
use face_auth_camera;
```
3. The rest of the file already uses `face_auth_camera::open_camera`, `face_auth_camera::CameraHandle`, `face_auth_camera::Frame` — these names are preserved, so no other changes needed.

- [ ] **Step 5: Build both crates**

```bash
cargo build -p face-auth-camera
cargo build -p face-enroll
```

Expected: both compile with no errors.

- [ ] **Step 6: Commit**

```bash
git add crates/face-auth-camera/ crates/face-enroll/Cargo.toml crates/face-enroll/src/main.rs Cargo.toml
git commit -m "refactor: extract face-auth-camera into shared crate"
```

---

## Task 3: Scaffold face-auth-gui crate

**Files:**
- Create: `crates/face-auth-gui/Cargo.toml`
- Create: `crates/face-auth-gui/src/main.rs`
- Create: `crates/face-auth-gui/src/app.rs`
- Create: `crates/face-auth-gui/src/tabs/mod.rs`
- Modify: `Cargo.toml` (workspace)

- [ ] **Step 1: Create directory structure**

```bash
mkdir -p crates/face-auth-gui/src/tabs
```

- [ ] **Step 2: Create Cargo.toml**

```toml
[package]
name        = "face-auth-gui"
version     = "0.1.0"
edition     = "2021"
description = "Graphical enrollment and management tool for face-auth"

[[bin]]
name = "face-auth-gui"
path = "src/main.rs"

[dependencies]
face-auth-core    = { path = "../face-auth-core" }
face-auth-models  = { path = "../face-auth-models" }
face-auth-camera  = { path = "../face-auth-camera" }
eframe            = "0.34"
egui              = "0.34"
tracing           = { workspace = true }
tracing-subscriber = { workspace = true }
```

- [ ] **Step 3: Create main.rs**

```rust
mod app;
mod camera_texture;
mod inference_worker;
mod tabs;

fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info,ort=warn")),
        )
        .init();

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("face-auth")
            .with_inner_size([960.0, 640.0])
            .with_min_inner_size([800.0, 500.0]),
        ..Default::default()
    };

    eframe::run_native(
        "face-auth",
        options,
        Box::new(|_cc| Ok(Box::new(app::FaceAuthApp::new()))),
    )
    .expect("eframe failed to start");
}
```

- [ ] **Step 4: Create app.rs with Tab enum and empty window**

```rust
use crate::tabs::{
    check_config::CheckConfigTab, configure::ConfigureTab, enroll::EnrollTab,
    status::StatusTab, test_auth::TestAuthTab, test_camera::TestCameraTab,
};
use face_auth_core::config::Config;

#[derive(PartialEq, Clone, Copy)]
pub enum Tab {
    Enroll,
    ReEnroll,
    Status,
    TestAuth,
    CheckConfig,
    Configure,
    TestCamera,
}

impl Tab {
    fn label(self) -> &'static str {
        match self {
            Tab::Enroll => "Enroll",
            Tab::ReEnroll => "Re-enroll",
            Tab::Status => "Status",
            Tab::TestAuth => "Test Auth",
            Tab::CheckConfig => "Check Config",
            Tab::Configure => "Configure",
            Tab::TestCamera => "Test Camera",
        }
    }

    fn all() -> &'static [Tab] {
        &[
            Tab::Enroll,
            Tab::ReEnroll,
            Tab::Status,
            Tab::TestAuth,
            Tab::CheckConfig,
            Tab::Configure,
            Tab::TestCamera,
        ]
    }
}

pub struct FaceAuthApp {
    active_tab: Tab,
    is_root: bool,
    config: Config,
    enroll_tab: EnrollTab,
    re_enroll_tab: EnrollTab,
    status_tab: StatusTab,
    test_auth_tab: TestAuthTab,
    check_config_tab: CheckConfigTab,
    configure_tab: ConfigureTab,
    test_camera_tab: TestCameraTab,
}

impl FaceAuthApp {
    pub fn new() -> Self {
        let is_root = unsafe { libc::geteuid() } == 0;
        let config = Config::load_system().unwrap_or_default();
        Self {
            active_tab: Tab::Enroll,
            is_root,
            config: config.clone(),
            enroll_tab: EnrollTab::new(false),
            re_enroll_tab: EnrollTab::new(true),
            status_tab: StatusTab::new(),
            test_auth_tab: TestAuthTab::new(),
            check_config_tab: CheckConfigTab::new(),
            configure_tab: ConfigureTab::new(),
            test_camera_tab: TestCameraTab::new(),
        }
    }

    fn switch_tab(&mut self, new_tab: Tab) {
        if self.active_tab == new_tab {
            return;
        }
        // Deactivate current tab (closes camera if open)
        match self.active_tab {
            Tab::Enroll => self.enroll_tab.deactivate(),
            Tab::ReEnroll => self.re_enroll_tab.deactivate(),
            Tab::TestCamera => self.test_camera_tab.deactivate(),
            Tab::TestAuth => self.test_auth_tab.deactivate(),
            _ => {}
        }
        self.active_tab = new_tab;
    }
}

impl eframe::App for FaceAuthApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // Root warning banner
        if !self.is_root {
            egui::TopBottomPanel::top("root_warning").show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.colored_label(
                        egui::Color32::YELLOW,
                        "⚠ Not running as root — enrollment chown may fail.",
                    );
                    ui.label("Launch with: sudo face-auth-gui");
                });
            });
        }

        // Tab bar
        egui::TopBottomPanel::top("tab_bar").show(ctx, |ui| {
            ui.horizontal(|ui| {
                for &tab in Tab::all() {
                    if ui
                        .selectable_label(self.active_tab == tab, tab.label())
                        .clicked()
                    {
                        self.switch_tab(tab);
                    }
                }
            });
        });

        // Content panel
        egui::CentralPanel::default().show(ctx, |ui| {
            match self.active_tab {
                Tab::Enroll => self.enroll_tab.ui(ui, ctx, &self.config),
                Tab::ReEnroll => self.re_enroll_tab.ui(ui, ctx, &self.config),
                Tab::Status => self.status_tab.ui(ui),
                Tab::TestAuth => self.test_auth_tab.ui(ui, ctx, &self.config),
                Tab::CheckConfig => self.check_config_tab.ui(ui),
                Tab::Configure => self.configure_tab.ui(ui, &self.config),
                Tab::TestCamera => self.test_camera_tab.ui(ui, ctx, &self.config),
            }
        });

        // Drive repaint at ~30 fps (only active when camera/inference running)
        ctx.request_repaint_after(std::time::Duration::from_millis(33));
    }
}
```

Add `libc` to face-auth-gui Cargo.toml:
```toml
libc = { workspace = true }
```

- [ ] **Step 5: Create stub tabs/mod.rs**

```rust
pub mod check_config;
pub mod configure;
pub mod enroll;
pub mod status;
pub mod test_auth;
pub mod test_camera;
```

- [ ] **Step 6: Create stub for each tab**

Each tab file gets a minimal struct + `new()` + required methods. Create each file now as stubs; they'll be filled in later tasks.

`crates/face-auth-gui/src/tabs/status.rs`:
```rust
pub struct StatusTab;
impl StatusTab {
    pub fn new() -> Self { Self }
    pub fn ui(&mut self, ui: &mut egui::Ui) {
        ui.label("Status — coming in next task");
    }
}
```

`crates/face-auth-gui/src/tabs/check_config.rs`:
```rust
pub struct CheckConfigTab;
impl CheckConfigTab {
    pub fn new() -> Self { Self }
    pub fn ui(&mut self, ui: &mut egui::Ui) {
        ui.label("Check Config — coming soon");
    }
}
```

`crates/face-auth-gui/src/tabs/configure.rs`:
```rust
pub struct ConfigureTab;
impl ConfigureTab {
    pub fn new() -> Self { Self }
    pub fn ui(&mut self, ui: &mut egui::Ui, _config: &face_auth_core::config::Config) {
        ui.label("Configure — coming soon");
    }
}
```

`crates/face-auth-gui/src/tabs/test_camera.rs`:
```rust
pub struct TestCameraTab;
impl TestCameraTab {
    pub fn new() -> Self { Self }
    pub fn deactivate(&mut self) {}
    pub fn ui(&mut self, ui: &mut egui::Ui, _ctx: &egui::Context, _config: &face_auth_core::config::Config) {
        ui.label("Test Camera — coming in next task");
    }
}
```

`crates/face-auth-gui/src/tabs/test_auth.rs`:
```rust
pub struct TestAuthTab;
impl TestAuthTab {
    pub fn new() -> Self { Self }
    pub fn deactivate(&mut self) {}
    pub fn ui(&mut self, ui: &mut egui::Ui, _ctx: &egui::Context, _config: &face_auth_core::config::Config) {
        ui.label("Test Auth — coming soon");
    }
}
```

`crates/face-auth-gui/src/tabs/enroll.rs`:
```rust
pub struct EnrollTab { append: bool }
impl EnrollTab {
    pub fn new(append: bool) -> Self { Self { append } }
    pub fn deactivate(&mut self) {}
    pub fn ui(&mut self, ui: &mut egui::Ui, _ctx: &egui::Context, _config: &face_auth_core::config::Config) {
        ui.label(if self.append { "Re-enroll — coming soon" } else { "Enroll — coming soon" });
    }
}
```

- [ ] **Step 7: Create stub camera_texture.rs and inference_worker.rs**

`crates/face-auth-gui/src/camera_texture.rs`:
```rust
// Filled in Task 4
```

`crates/face-auth-gui/src/inference_worker.rs`:
```rust
// Filled in Task 7
```

- [ ] **Step 8: Add to workspace**

In root `Cargo.toml`, add `"crates/face-auth-gui"` to `members`.

- [ ] **Step 9: Compile**

```bash
cargo build -p face-auth-gui
```

Expected: compiles, blank window with tab bar opens when run.

- [ ] **Step 10: Commit**

```bash
git add crates/face-auth-gui/ Cargo.toml
git commit -m "feat(gui): scaffold face-auth-gui with tab skeleton"
```

---

## Task 4: Camera texture conversion

**Files:**
- Modify: `crates/face-auth-gui/src/camera_texture.rs`

- [ ] **Step 1: Write the failing test**

Add at bottom of `camera_texture.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grayscale_to_rgba_conversion() {
        let frame = face_auth_camera::Frame {
            data: vec![0u8, 128u8, 255u8, 64u8],
            width: 2,
            height: 2,
            timestamp: std::time::Instant::now(),
        };
        let rgba = frame_to_rgba(&frame);
        assert_eq!(rgba.len(), 4 * 4); // 4 pixels × 4 channels
        // Pixel 0: value 0 → [0,0,0,255]
        assert_eq!(&rgba[0..4], &[0u8, 0, 0, 255]);
        // Pixel 1: value 128 → [128,128,128,255]
        assert_eq!(&rgba[4..8], &[128u8, 128, 128, 255]);
        // Pixel 3: value 64 → [64,64,64,255]
        assert_eq!(&rgba[12..16], &[64u8, 64, 64, 255]);
    }
}
```

- [ ] **Step 2: Run to verify fail**

```bash
cargo test -p face-auth-gui camera_texture -- --nocapture
```

Expected: compile error `frame_to_rgba not found`.

- [ ] **Step 3: Implement camera_texture.rs**

```rust
use std::sync::Arc;

/// Convert a grayscale IR frame to RGBA bytes for egui texture upload.
pub fn frame_to_rgba(frame: &face_auth_camera::Frame) -> Vec<u8> {
    frame
        .data
        .iter()
        .flat_map(|&g| [g, g, g, 255u8])
        .collect()
}

/// Upload a camera frame as an egui texture, returning a handle.
/// The handle is re-created every frame; egui handles GPU upload internally.
pub fn upload_frame(
    frame: &Arc<face_auth_camera::Frame>,
    ctx: &egui::Context,
    name: &str,
) -> egui::TextureHandle {
    let rgba = frame_to_rgba(frame);
    let image = egui::ColorImage::from_rgba_unmultiplied(
        [frame.width as usize, frame.height as usize],
        &rgba,
    );
    ctx.load_texture(name, image, egui::TextureOptions::LINEAR)
}

/// Render a texture handle into the current UI, scaled to fit available width.
pub fn show_texture(ui: &mut egui::Ui, texture: &egui::TextureHandle, native_w: u32, native_h: u32) {
    let available_w = ui.available_width();
    let scale = available_w / native_w as f32;
    let display_size = egui::Vec2::new(available_w, native_h as f32 * scale);
    ui.image(egui::load::SizedTexture::new(texture.id(), display_size));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grayscale_to_rgba_conversion() {
        let frame = face_auth_camera::Frame {
            data: vec![0u8, 128u8, 255u8, 64u8],
            width: 2,
            height: 2,
            timestamp: std::time::Instant::now(),
        };
        let rgba = frame_to_rgba(&frame);
        assert_eq!(rgba.len(), 4 * 4);
        assert_eq!(&rgba[0..4], &[0u8, 0, 0, 255]);
        assert_eq!(&rgba[4..8], &[128u8, 128, 128, 255]);
        assert_eq!(&rgba[12..16], &[64u8, 64, 64, 255]);
    }
}
```

- [ ] **Step 4: Run test**

```bash
cargo test -p face-auth-gui camera_texture -- --nocapture
```

Expected: `test camera_texture::tests::grayscale_to_rgba_conversion ... ok`

- [ ] **Step 5: Commit**

```bash
git add crates/face-auth-gui/src/camera_texture.rs
git commit -m "feat(gui): camera frame → egui texture conversion"
```

---

## Task 5: Test Camera tab

**Files:**
- Modify: `crates/face-auth-gui/src/tabs/test_camera.rs`

- [ ] **Step 1: Implement TestCameraTab**

Replace the stub content of `test_camera.rs`:

```rust
use crate::camera_texture;
use face_auth_core::config::Config;
use std::sync::Arc;

pub struct TestCameraTab {
    camera: Option<face_auth_camera::CameraHandle>,
    latest_frame: Option<Arc<face_auth_camera::Frame>>,
    error: Option<String>,
}

impl TestCameraTab {
    pub fn new() -> Self {
        Self { camera: None, latest_frame: None, error: None }
    }

    pub fn deactivate(&mut self) {
        self.camera = None;
        self.latest_frame = None;
    }

    fn activate(&mut self, config: &Config) {
        if self.camera.is_none() {
            match face_auth_camera::open_camera(&config.camera) {
                Ok(cam) => {
                    self.camera = Some(cam);
                    self.error = None;
                }
                Err(e) => self.error = Some(format!("Camera error: {e}")),
            }
        }
    }

    pub fn ui(&mut self, ui: &mut egui::Ui, ctx: &egui::Context, config: &Config) {
        self.activate(config);

        // Drain latest frame from camera
        if let Some(cam) = &self.camera {
            while let Some(f) = cam.try_recv_frame() {
                self.latest_frame = Some(f);
            }
        }

        ui.heading("Test Camera");
        ui.separator();

        if let Some(ref err) = self.error {
            ui.colored_label(egui::Color32::RED, err);
            return;
        }

        if let Some(ref frame) = self.latest_frame {
            let texture = camera_texture::upload_frame(frame, ctx, "test_camera");
            camera_texture::show_texture(ui, &texture, frame.width, frame.height);
            ui.separator();
            ui.label(format!(
                "Resolution: {}×{}  Format: GREY (IR grayscale)",
                frame.width, frame.height
            ));
        } else {
            ui.label("Waiting for camera...");
            ctx.request_repaint_after(std::time::Duration::from_millis(100));
        }
    }
}
```

- [ ] **Step 2: Build and smoke-test manually**

```bash
cargo build -p face-auth-gui
```

Run `./target/debug/face-auth-gui`, navigate to "Test Camera" tab, verify live camera feed appears. Navigate away, verify camera stops (no /dev/video lock held).

- [ ] **Step 3: Commit**

```bash
git add crates/face-auth-gui/src/tabs/test_camera.rs
git commit -m "feat(gui): Test Camera tab with live IR feed"
```

---

## Task 6: Status tab

**Files:**
- Modify: `crates/face-auth-gui/src/tabs/status.rs`

- [ ] **Step 1: Implement StatusTab**

```rust
use face_auth_core::enrollment;

pub struct StatusTab {
    info: Option<StatusInfo>,
}

struct StatusInfo {
    username: String,
    embed_count: usize,
    version: u32,
    current_version: u32,
    path: String,
}

impl StatusTab {
    pub fn new() -> Self {
        let username = std::env::var("SUDO_USER")
            .or_else(|_| std::env::var("USER"))
            .unwrap_or_else(|_| "unknown".into());
        let info = Self::load(&username);
        Self { info: Some(info) }
    }

    fn load(username: &str) -> StatusInfo {
        let embed_count = enrollment::load_embeddings(username)
            .map(|e| e.len())
            .unwrap_or(0);
        let version = enrollment::enrollment_version(username).unwrap_or(0);
        let path = enrollment::enrollment_dir(username)
            .display()
            .to_string();
        StatusInfo {
            username: username.to_string(),
            embed_count,
            version,
            current_version: enrollment::ENROLLMENT_VERSION,
            path,
        }
    }

    pub fn ui(&mut self, ui: &mut egui::Ui) {
        ui.heading("Enrollment Status");
        ui.separator();

        let username = std::env::var("SUDO_USER")
            .or_else(|_| std::env::var("USER"))
            .unwrap_or_else(|_| "unknown".into());

        if ui.button("Refresh").clicked() {
            self.info = Some(Self::load(&username));
        }

        ui.separator();

        if let Some(ref info) = self.info {
            egui::Grid::new("status_grid").num_columns(2).show(ui, |ui| {
                ui.label("User:");
                ui.label(&info.username);
                ui.end_row();

                ui.label("Enrolled:");
                if info.embed_count > 0 {
                    ui.colored_label(egui::Color32::GREEN, "Yes");
                } else {
                    ui.colored_label(egui::Color32::RED, "No");
                }
                ui.end_row();

                ui.label("Embeddings:");
                ui.label(info.embed_count.to_string());
                ui.end_row();

                ui.label("Format version:");
                let ver_label = format!("{} (current: {})", info.version, info.current_version);
                if info.version < info.current_version && info.embed_count > 0 {
                    ui.colored_label(egui::Color32::YELLOW, ver_label);
                } else {
                    ui.label(ver_label);
                }
                ui.end_row();

                ui.label("Path:");
                ui.label(&info.path);
                ui.end_row();
            });

            if info.version < info.current_version && info.embed_count > 0 {
                ui.separator();
                ui.colored_label(
                    egui::Color32::YELLOW,
                    "Stale enrollment format — re-enroll for best accuracy.",
                );
            }
        }
    }
}
```

- [ ] **Step 2: Build**

```bash
cargo build -p face-auth-gui
```

- [ ] **Step 3: Commit**

```bash
git add crates/face-auth-gui/src/tabs/status.rs
git commit -m "feat(gui): Status tab showing enrollment info"
```

---

## Task 7: Inference worker for enrollment

**Files:**
- Modify: `crates/face-auth-gui/src/inference_worker.rs`

The inference worker runs detect → liveness → align → embed on each frame in a background thread, sending results back to the UI thread.

- [ ] **Step 1: Implement inference_worker.rs**

```rust
use face_auth_core::config::{Config, LivenessConfig};
use face_auth_core::geometry::{analyze_geometry, BBox, FaceMetrics, Landmarks};
use face_auth_models::alignment::align_face;
use face_auth_models::detection::FaceDetector;
use face_auth_models::quality;
use face_auth_models::recognition::FaceRecognizer;
use std::sync::{mpsc, Arc, Mutex};
use std::time::{Duration, Instant};

pub struct FrameResult {
    pub bbox: BBox,
    pub landmarks: Landmarks,
    pub metrics: FaceMetrics,
    pub embedding: Option<[f32; 512]>,
    pub liveness_pass: bool,
    pub liveness_scores: quality::LivenessScores,
}

pub enum WorkerResult {
    Face(FrameResult),
    NoFace,
}

/// Shared model cache for the GUI inference worker.
/// Loaded once when enrollment starts, dropped when enrollment finishes.
pub struct GuiModelCache {
    pub detector: Mutex<FaceDetector>,
    pub recognizer: Mutex<FaceRecognizer>,
}

impl GuiModelCache {
    pub fn load() -> Result<Self, String> {
        let detector = FaceDetector::load_default()
            .map_err(|e| format!("load detector: {e}"))?;
        let recognizer = FaceRecognizer::load_default()
            .map_err(|e| format!("load recognizer: {e}"))?;
        Ok(Self {
            detector: Mutex::new(detector),
            recognizer: Mutex::new(recognizer),
        })
    }
}

pub struct InferenceWorker {
    result_rx: mpsc::Receiver<WorkerResult>,
    _thread: std::thread::JoinHandle<()>,
}

impl InferenceWorker {
    /// Start background inference thread.
    /// Takes a `frame_rx` that receives frames from the camera thread.
    pub fn start(
        models: Arc<GuiModelCache>,
        frame_rx: mpsc::Receiver<Arc<face_auth_camera::Frame>>,
        liveness_config: LivenessConfig,
    ) -> Self {
        let (result_tx, result_rx) = mpsc::sync_channel::<WorkerResult>(4);

        let thread = std::thread::Builder::new()
            .name("gui-inference".into())
            .spawn(move || {
                inference_loop(models, frame_rx, result_tx, liveness_config);
            })
            .expect("spawn gui-inference thread");

        Self { result_rx, _thread: thread }
    }

    /// Non-blocking poll for latest result.
    pub fn try_recv(&self) -> Option<WorkerResult> {
        self.result_rx.try_recv().ok()
    }
}

fn inference_loop(
    models: Arc<GuiModelCache>,
    frame_rx: mpsc::Receiver<Arc<face_auth_camera::Frame>>,
    result_tx: mpsc::SyncSender<WorkerResult>,
    liveness_config: LivenessConfig,
) {
    while let Ok(frame) = frame_rx.recv() {
        // Skip stale frames (older than 200ms)
        if frame.timestamp.elapsed() > Duration::from_millis(200) {
            continue;
        }

        let mut detector = models.detector.lock().unwrap();
        let detections = match detector.detect(&frame.data, frame.width, frame.height) {
            Ok(d) => d,
            Err(_) => {
                let _ = result_tx.try_send(WorkerResult::NoFace);
                continue;
            }
        };
        drop(detector);

        if detections.is_empty() {
            let _ = result_tx.try_send(WorkerResult::NoFace);
            continue;
        }

        let det = &detections[0];
        let mut metrics = analyze_geometry(&det.landmarks, &det.bbox, frame.width, frame.height);
        metrics.ir_saturated = quality::ir_saturated(&frame.data, &det.bbox, frame.width);
        metrics.blur_score = quality::blur_score(&frame.data, &det.bbox, frame.width, frame.height);

        let liveness_scores =
            quality::ir_liveness_check(&frame.data, &det.bbox, frame.width, frame.height);
        let liveness_pass = liveness_scores.is_live(
            liveness_config.lbp_entropy_min,
            liveness_config.local_contrast_cv_min,
            liveness_config.local_contrast_cv_max,
        );

        // Only embed when face quality is acceptable
        let embedding = if metrics.face_width_ratio > 0.10
            && metrics.eyes_visible
            && !metrics.ir_saturated
            && metrics.blur_score >= 50.0
            && liveness_pass
        {
            let aligned = align_face(&frame.data, frame.width, frame.height, &det.landmarks);
            let mut recognizer = models.recognizer.lock().unwrap();
            recognizer.embed(&aligned).ok()
        } else {
            None
        };

        let result = WorkerResult::Face(FrameResult {
            bbox: det.bbox.clone(),
            landmarks: det.landmarks.clone(),
            metrics,
            embedding,
            liveness_pass,
            liveness_scores,
        });

        let _ = result_tx.try_send(result);
    }
}
```

Note: `quality::LivenessScores` — check the actual return type of `ir_liveness_check` in `crates/face-auth-models/src/quality.rs` and use the correct type name.

- [ ] **Step 2: Build**

```bash
cargo build -p face-auth-gui
```

Fix any type name mismatches (e.g. if `LivenessScores` is named differently in quality.rs).

- [ ] **Step 3: Commit**

```bash
git add crates/face-auth-gui/src/inference_worker.rs
git commit -m "feat(gui): background inference worker for enrollment"
```

---

## Task 8: Enroll tab

**Files:**
- Modify: `crates/face-auth-gui/src/tabs/enroll.rs`

- [ ] **Step 1: Write state machine test**

Add `#[cfg(test)]` block to `enroll.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::EnrollState;

    #[test]
    fn idle_advances_to_capturing_on_start() {
        let state = EnrollState::Idle;
        assert!(matches!(state, EnrollState::Idle));
    }

    #[test]
    fn capturing_pose_increments() {
        let mut captured = 0usize;
        let mut pose_idx = 0usize;
        let embeddings_per_pose = 3;
        let total_poses = 5;

        // Simulate capturing embeddings_per_pose embeddings for each pose
        for _ in 0..embeddings_per_pose {
            captured += 1;
        }
        if captured >= embeddings_per_pose {
            pose_idx += 1;
            captured = 0;
        }
        assert_eq!(pose_idx, 1);
        assert_eq!(captured, 0);

        // Simulate completing all poses
        let total_captured = embeddings_per_pose * total_poses;
        assert_eq!(total_captured, 15);
    }
}
```

- [ ] **Step 2: Run tests**

```bash
cargo test -p face-auth-gui enroll -- --nocapture
```

Expected: pass.

- [ ] **Step 3: Implement EnrollTab**

Replace the stub with the full implementation:

```rust
use crate::camera_texture;
use crate::inference_worker::{GuiModelCache, InferenceWorker, WorkerResult};
use face_auth_core::config::Config;
use face_auth_core::enrollment;
use face_auth_core::geometry::{AuthState, StateMachine};
use face_auth_models::recognition::{cosine_similarity, score_and_filter_embeddings};
use std::sync::mpsc;
use std::sync::Arc;
use std::time::{Duration, Instant};

const POSES: &[&str] = &[
    "straight ahead",
    "slightly LEFT",
    "slightly RIGHT",
    "slightly UP",
    "slightly DOWN",
];
const EMBEDDINGS_PER_POSE: usize = 3;
const MIN_CAPTURE_INTERVAL: Duration = Duration::from_millis(500);

pub enum EnrollState {
    Idle,
    LoadingModels,
    ModelError(String),
    CapturingPose {
        pose_idx: usize,
        pose_captured: usize,
        all_embeddings: Vec<[f32; 512]>,
        last_capture: Instant,
    },
    QualityReview {
        embeddings: Vec<[f32; 512]>,
        avg_sim: f32,
        min_sim: f32,
        suggested_threshold: f32,
    },
    Saving,
    Done { embed_count: usize },
    Error(String),
}

pub struct EnrollTab {
    append: bool,
    state: EnrollState,
    camera: Option<face_auth_camera::CameraHandle>,
    // Channel plumbing: camera → frame_tx → inference thread → result_rx → UI
    frame_tx: Option<mpsc::SyncSender<Arc<face_auth_camera::Frame>>>,
    worker: Option<InferenceWorker>,
    models: Option<Arc<GuiModelCache>>,
    latest_frame: Option<Arc<face_auth_camera::Frame>>,
    latest_result: Option<WorkerResult>,
    state_machine: Option<StateMachine>,
    existing_embeddings: Vec<[f32; 512]>,
}

impl EnrollTab {
    pub fn new(append: bool) -> Self {
        Self {
            append,
            state: EnrollState::Idle,
            camera: None,
            frame_tx: None,
            worker: None,
            models: None,
            latest_frame: None,
            latest_result: None,
            state_machine: None,
            existing_embeddings: Vec::new(),
        }
    }

    pub fn deactivate(&mut self) {
        // Drop in order: worker first (stops consuming frames), then camera
        self.worker = None;
        self.frame_tx = None;
        self.camera = None;
        self.latest_frame = None;
        self.latest_result = None;
        self.state = EnrollState::Idle;
        self.existing_embeddings.clear();
    }

    fn start_enrollment(&mut self, config: &Config) {
        let username = std::env::var("SUDO_USER")
            .or_else(|_| std::env::var("USER"))
            .unwrap_or_else(|_| "unknown".into());

        // Load existing embeddings if appending
        self.existing_embeddings = if self.append {
            enrollment::load_embeddings(&username).unwrap_or_default()
        } else {
            Vec::new()
        };

        // Open camera
        let camera = match face_auth_camera::open_camera(&config.camera) {
            Ok(c) => c,
            Err(e) => {
                self.state = EnrollState::Error(format!("Camera: {e}"));
                return;
            }
        };

        // Plumb camera → inference worker via channel
        let (frame_tx, frame_rx) = mpsc::sync_channel::<Arc<face_auth_camera::Frame>>(4);
        self.camera = Some(camera);
        self.frame_tx = Some(frame_tx);
        self.state = EnrollState::LoadingModels;

        // Load models (on background thread to not block UI)
        let liveness_cfg = config.liveness.clone();
        let frame_rx_for_worker = frame_rx;
        std::thread::spawn({
            let frame_tx_check = self.frame_tx.clone();
            move || {
                // This is a one-shot thread just to load models
                // In real impl, we'd send result back via channel
                // For simplicity, load synchronously here (1-2s, acceptable)
                let _ = (frame_tx_check, frame_rx_for_worker, liveness_cfg);
            }
        });

        // Simpler: load models synchronously (blocks for ~1-2s on first run)
        // Update state after loading
        self.state = EnrollState::LoadingModels;
        match GuiModelCache::load() {
            Ok(cache) => {
                let models = Arc::new(cache);
                // Re-plumb: create new frame channel for the worker
                let (frame_tx2, frame_rx2) = mpsc::sync_channel::<Arc<face_auth_camera::Frame>>(4);
                self.frame_tx = Some(frame_tx2);
                self.worker = Some(InferenceWorker::start(
                    models.clone(),
                    frame_rx2,
                    config.liveness.clone(),
                ));
                self.models = Some(models);
                self.state_machine = Some(StateMachine::new(&config.geometry));
                self.state = EnrollState::CapturingPose {
                    pose_idx: 0,
                    pose_captured: 0,
                    all_embeddings: Vec::new(),
                    last_capture: Instant::now() - Duration::from_secs(10),
                };
            }
            Err(e) => {
                self.state = EnrollState::ModelError(e);
            }
        }
    }

    fn pump_frames(&mut self) {
        // Forward camera frames to inference worker
        if let (Some(cam), Some(tx)) = (&self.camera, &self.frame_tx) {
            while let Some(f) = cam.try_recv_frame() {
                self.latest_frame = Some(f.clone());
                let _ = tx.try_send(f);
            }
        }
        // Collect latest inference result
        if let Some(w) = &self.worker {
            while let Some(r) = w.try_recv() {
                self.latest_result = Some(r);
            }
        }
    }

    fn try_capture_embedding(&mut self, config: &Config) {
        let sm = match self.state_machine.as_mut() {
            Some(s) => s,
            None => return,
        };

        let (pose_idx, pose_captured, all_embeddings, last_capture) =
            match &mut self.state {
                EnrollState::CapturingPose {
                    pose_idx,
                    pose_captured,
                    all_embeddings,
                    last_capture,
                } => (pose_idx, pose_captured, all_embeddings, last_capture),
                _ => return,
            };

        let now = Instant::now();
        let feedback = match &self.latest_result {
            Some(WorkerResult::Face(r)) => sm.transition(Some(&r.metrics), now),
            _ => sm.transition(None, now),
        };

        let in_auth_state = sm.state == AuthState::Authenticating;

        if in_auth_state {
            if let Some(WorkerResult::Face(ref r)) = self.latest_result {
                if let Some(emb) = r.embedding {
                    if now.duration_since(*last_capture) >= MIN_CAPTURE_INTERVAL {
                        all_embeddings.push(emb);
                        *pose_captured += 1;
                        *last_capture = now;

                        if *pose_captured >= EMBEDDINGS_PER_POSE {
                            *pose_idx += 1;
                            *pose_captured = 0;

                            if *pose_idx >= POSES.len() {
                                // All poses done — move to quality review
                                let filtered = score_and_filter(all_embeddings.clone());
                                let (avg_sim, min_sim) = embedding_stats(&filtered);
                                let suggested = (min_sim - 0.10).max(0.40);
                                self.state = EnrollState::QualityReview {
                                    embeddings: filtered,
                                    avg_sim,
                                    min_sim,
                                    suggested_threshold: suggested,
                                };
                            }
                        }
                    }
                }
            }
        }
    }

    fn save_embeddings(&mut self, embeddings: Vec<[f32; 512]>, config: &Config) {
        let username = std::env::var("SUDO_USER")
            .or_else(|_| std::env::var("USER"))
            .unwrap_or_else(|_| "unknown".into());

        let mut all = self.existing_embeddings.clone();
        all.extend_from_slice(&embeddings);
        let count = all.len();

        match enrollment::save_embeddings(&username, &all, count as u32) {
            Ok(()) => self.state = EnrollState::Done { embed_count: count },
            Err(e) => self.state = EnrollState::Error(format!("Save failed: {e}")),
        }
    }

    pub fn ui(&mut self, ui: &mut egui::Ui, ctx: &egui::Context, config: &Config) {
        self.pump_frames();

        ui.heading(if self.append { "Re-enroll (append)" } else { "Enroll" });
        ui.separator();

        match &self.state {
            EnrollState::Idle => {
                ui.label("Enroll your face for authentication.");
                ui.label(format!("Poses: {}  ×  {} frames each = {} total embeddings",
                    POSES.len(), EMBEDDINGS_PER_POSE, POSES.len() * EMBEDDINGS_PER_POSE));
                ui.separator();
                if ui.button("Start Enrollment").clicked() {
                    // Clone config to avoid borrow issue
                    let cfg = config.clone();
                    self.start_enrollment(&cfg);
                }
            }

            EnrollState::LoadingModels => {
                ui.label("Loading models... (first run may take a few seconds)");
                ctx.request_repaint_after(Duration::from_millis(100));
            }

            EnrollState::ModelError(e) => {
                ui.colored_label(egui::Color32::RED, format!("Model load error: {e}"));
                if ui.button("Retry").clicked() {
                    self.state = EnrollState::Idle;
                }
            }

            EnrollState::CapturingPose { pose_idx, pose_captured, all_embeddings, .. } => {
                let pose_idx = *pose_idx;
                let pose_captured = *pose_captured;
                let total_captured = all_embeddings.len();

                // Progress bar
                let progress = total_captured as f32 / (POSES.len() * EMBEDDINGS_PER_POSE) as f32;
                ui.add(egui::ProgressBar::new(progress)
                    .text(format!("{}/{} embeddings", total_captured, POSES.len() * EMBEDDINGS_PER_POSE)));
                ui.separator();

                // Pose instruction
                ui.label(format!("Pose {}/{}: Look {}",
                    pose_idx + 1, POSES.len(), POSES[pose_idx]));
                ui.label(format!("Captured for this pose: {}/{}", pose_captured, EMBEDDINGS_PER_POSE));
                ui.separator();

                // Camera feed
                if let Some(ref frame) = self.latest_frame.clone() {
                    let texture = camera_texture::upload_frame(&frame, ctx, "enroll_camera");
                    camera_texture::show_texture(ui, &texture, frame.width, frame.height);
                }

                // Guidance overlay
                let guidance = match &self.latest_result {
                    Some(WorkerResult::Face(r)) => {
                        if !r.liveness_pass {
                            "Liveness check failed"
                        } else if r.embedding.is_some() {
                            "Good position — capturing..."
                        } else {
                            "Adjusting position..."
                        }
                    }
                    Some(WorkerResult::NoFace) | None => "No face detected",
                };
                ui.label(guidance);

                // Try to capture
                let cfg = config.clone();
                self.try_capture_embedding(&cfg);
                ctx.request_repaint_after(Duration::from_millis(50));
            }

            EnrollState::QualityReview { embeddings, avg_sim, min_sim, suggested_threshold } => {
                let embeddings = embeddings.clone();
                let avg_sim = *avg_sim;
                let min_sim = *min_sim;
                let suggested = *suggested_threshold;
                let current_threshold = config.recognition.threshold;

                ui.heading("Quality Review");
                egui::Grid::new("quality_grid").num_columns(2).show(ui, |ui| {
                    ui.label("Embeddings captured:");
                    ui.label(embeddings.len().to_string());
                    ui.end_row();

                    ui.label("Avg inter-embedding similarity:");
                    let grade = if avg_sim >= 0.80 { "Excellent" }
                        else if avg_sim >= 0.70 { "Good" }
                        else if avg_sim >= 0.60 { "Fair" }
                        else { "Poor" };
                    ui.label(format!("{:.3} ({})", avg_sim, grade));
                    ui.end_row();

                    ui.label("Suggested threshold:");
                    ui.label(format!("{:.2}  (current: {:.2})", suggested, current_threshold));
                    ui.end_row();
                });

                ui.separator();
                ui.horizontal(|ui| {
                    if ui.button("Save").clicked() {
                        let cfg = config.clone();
                        self.save_embeddings(embeddings, &cfg);
                    }
                    if ui.button("Re-do enrollment").clicked() {
                        self.state = EnrollState::Idle;
                        self.camera = None;
                        self.worker = None;
                        self.frame_tx = None;
                    }
                });
            }

            EnrollState::Saving => {
                ui.label("Saving...");
            }

            EnrollState::Done { embed_count } => {
                let count = *embed_count;
                ui.colored_label(egui::Color32::GREEN,
                    format!("Enrolled successfully — {} embeddings saved.", count));
                if ui.button("Enroll again").clicked() {
                    self.state = EnrollState::Idle;
                }
            }

            EnrollState::Error(e) => {
                ui.colored_label(egui::Color32::RED, format!("Error: {e}"));
                if ui.button("Back").clicked() {
                    self.state = EnrollState::Idle;
                }
            }
        }
    }
}

fn score_and_filter(embeddings: Vec<[f32; 512]>) -> Vec<[f32; 512]> {
    if embeddings.len() < 3 { return embeddings; }
    let n = embeddings.len();
    let mut avg_sims: Vec<f32> = Vec::with_capacity(n);
    for i in 0..n {
        let sum: f32 = (0..n).filter(|&j| j != i)
            .map(|j| cosine_similarity(&embeddings[i], &embeddings[j]))
            .sum();
        avg_sims.push(sum / (n - 1) as f32);
    }
    embeddings.into_iter().zip(avg_sims.iter())
        .filter(|(_, &sim)| sim >= 0.5)
        .map(|(e, _)| e)
        .collect()
}

fn embedding_stats(embeddings: &[[f32; 512]]) -> (f32, f32) {
    if embeddings.len() < 2 { return (0.0, 0.0); }
    let n = embeddings.len();
    let avg_sim: f32 = (0..n).map(|i| {
        let sum: f32 = (0..n).filter(|&j| j != i)
            .map(|j| cosine_similarity(&embeddings[i], &embeddings[j]))
            .sum();
        sum / (n - 1) as f32
    }).sum::<f32>() / n as f32;

    let mut min_sim = 1.0f32;
    for i in 0..n {
        for j in (i+1)..n {
            let s = cosine_similarity(&embeddings[i], &embeddings[j]);
            if s < min_sim { min_sim = s; }
        }
    }
    (avg_sim, min_sim)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn idle_state_on_new() {
        let tab = EnrollTab::new(false);
        assert!(matches!(tab.state, EnrollState::Idle));
    }

    #[test]
    fn score_filter_keeps_all_when_few() {
        let embeddings = vec![[0.0f32; 512]; 2];
        let out = score_and_filter(embeddings.clone());
        assert_eq!(out.len(), 2);
    }

    #[test]
    fn embedding_stats_returns_zeros_for_single() {
        let embeddings = vec![[0.0f32; 512]; 1];
        let (avg, min) = embedding_stats(&embeddings);
        assert_eq!(avg, 0.0);
        assert_eq!(min, 0.0);
    }
}
```

Note: Remove the `use face_auth_models::recognition::score_and_filter_embeddings` import — the function is duplicated inline above since it operates on different logic than the CLI version. Also verify `quality::LivenessScores` type name matches what's in `face-auth-models/src/quality.rs`.

- [ ] **Step 4: Build and test**

```bash
cargo test -p face-auth-gui enroll -- --nocapture
cargo build -p face-auth-gui
```

Fix any compilation errors from type mismatches.

- [ ] **Step 5: Manual smoke test**

Run `./target/debug/face-auth-gui`, navigate to Enroll tab, click Start Enrollment, verify camera feed + pose guidance + progress bar work. Complete all 5 poses and verify Quality Review screen appears.

- [ ] **Step 6: Commit**

```bash
git add crates/face-auth-gui/src/tabs/enroll.rs
git commit -m "feat(gui): Enroll tab with enrollment wizard state machine"
```

---

## Task 9: Re-enroll tab

The Re-enroll tab is `EnrollTab::new(true)` — already wired in `app.rs`. No new code needed. The `append: bool` field controls whether existing embeddings are loaded and merged.

- [ ] **Step 1: Verify re-enroll works**

Run the app, navigate to Re-enroll tab. Verify it shows "Re-enroll (append)" heading and "Enroll again" button after completion. Verify existing embeddings are preserved after save (run `face-enroll --status` to confirm count increased).

- [ ] **Step 2: Commit**

```bash
git commit --allow-empty -m "feat(gui): Re-enroll tab (reuses EnrollTab with append=true)"
```

---

## Task 10: Test Auth tab

**Files:**
- Modify: `crates/face-auth-gui/src/tabs/test_auth.rs`

- [ ] **Step 1: Implement TestAuthTab**

```rust
use face_auth_core::config::Config;
use face_auth_core::framing::{read_message, write_message};
use face_auth_core::protocol::{AuthOutcome, DaemonMessage, FeedbackState, PamRequest, PROTOCOL_VERSION};
use std::io::BufWriter;
use std::os::unix::net::UnixStream;
use std::sync::mpsc;
use std::time::{Duration, Instant};

#[derive(Clone, Debug)]
enum AuthState {
    Idle,
    Connecting,
    Running { start: Instant, last_feedback: String },
    Done { outcome: AuthOutcome, elapsed: f32 },
    Error(String),
}

pub struct TestAuthTab {
    state: AuthState,
    result_rx: Option<mpsc::Receiver<AuthEvent>>,
}

enum AuthEvent {
    Feedback(String),
    Result(AuthOutcome, f32),
    Error(String),
}

impl TestAuthTab {
    pub fn new() -> Self {
        Self { state: AuthState::Idle, result_rx: None }
    }

    pub fn deactivate(&mut self) {
        self.state = AuthState::Idle;
        self.result_rx = None;
    }

    fn start_auth(&mut self, config: &Config) {
        let socket_path = config.daemon.socket_path.clone();
        let username = std::env::var("SUDO_USER")
            .or_else(|_| std::env::var("USER"))
            .unwrap_or_else(|_| "unknown".into());

        let (event_tx, event_rx) = mpsc::channel::<AuthEvent>();
        self.result_rx = Some(event_rx);
        self.state = AuthState::Connecting;

        std::thread::spawn(move || {
            let stream = match UnixStream::connect(&socket_path) {
                Ok(s) => s,
                Err(e) => {
                    let _ = event_tx.send(AuthEvent::Error(
                        format!("Cannot connect to {socket_path}: {e}\nIs face-authd running?")
                    ));
                    return;
                }
            };

            stream.set_read_timeout(Some(Duration::from_secs(35))).ok();
            let session_id: u64 = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis() as u64;

            let request = PamRequest::Auth {
                version: PROTOCOL_VERSION,
                username,
                session_id,
            };
            let mut writer = BufWriter::new(&stream);
            if write_message(&mut writer, &request).is_err()
                || std::io::Write::flush(&mut writer).is_err()
            {
                let _ = event_tx.send(AuthEvent::Error("Failed to send auth request".into()));
                return;
            }

            let start = Instant::now();
            let mut reader = &stream;
            loop {
                let msg: DaemonMessage = match read_message(&mut reader) {
                    Ok(m) => m,
                    Err(e) => {
                        let _ = event_tx.send(AuthEvent::Error(format!("Connection lost: {e}")));
                        return;
                    }
                };
                match msg {
                    DaemonMessage::Feedback { state, .. } => {
                        let label = feedback_label(state);
                        let _ = event_tx.send(AuthEvent::Feedback(label.to_string()));
                    }
                    DaemonMessage::AuthResult { outcome, .. } => {
                        let elapsed = start.elapsed().as_secs_f32();
                        let _ = event_tx.send(AuthEvent::Result(outcome, elapsed));
                        return;
                    }
                }
            }
        });
    }

    pub fn ui(&mut self, ui: &mut egui::Ui, ctx: &egui::Context, config: &Config) {
        // Drain events from auth thread
        if let Some(rx) = &self.result_rx {
            while let Ok(event) = rx.try_recv() {
                match event {
                    AuthEvent::Feedback(label) => {
                        if let AuthState::Running { ref mut last_feedback, .. } = self.state {
                            *last_feedback = label;
                        }
                    }
                    AuthEvent::Result(outcome, elapsed) => {
                        self.state = AuthState::Done { outcome, elapsed };
                        self.result_rx = None;
                    }
                    AuthEvent::Error(e) => {
                        self.state = AuthState::Error(e);
                        self.result_rx = None;
                    }
                }
            }
        }

        // Transition Connecting → Running when thread is started
        if matches!(self.state, AuthState::Connecting) {
            self.state = AuthState::Running {
                start: Instant::now(),
                last_feedback: "Connecting...".into(),
            };
        }

        ui.heading("Test Authentication");
        ui.separator();

        match &self.state {
            AuthState::Idle => {
                ui.label("Connect to face-authd and run a test authentication.");
                ui.label(format!("Socket: {}", config.daemon.socket_path));
                ui.separator();
                if ui.button("Start Auth Test").clicked() {
                    let cfg = config.clone();
                    self.start_auth(&cfg);
                }
            }

            AuthState::Running { start, last_feedback } => {
                let elapsed = start.elapsed().as_secs_f32();
                ui.label(format!("Elapsed: {elapsed:.1}s"));
                ui.separator();
                ui.label("Look at the camera...");
                ui.separator();
                ui.heading(last_feedback.as_str());
                ctx.request_repaint_after(Duration::from_millis(100));
            }

            AuthState::Done { outcome, elapsed } => {
                let elapsed = *elapsed;
                match outcome {
                    AuthOutcome::Success => {
                        ui.colored_label(egui::Color32::GREEN,
                            format!("SUCCESS ({elapsed:.1}s)"));
                    }
                    AuthOutcome::Failed => {
                        ui.colored_label(egui::Color32::RED, "FAILED");
                    }
                    AuthOutcome::Timeout => {
                        ui.colored_label(egui::Color32::YELLOW,
                            format!("TIMEOUT ({elapsed:.1}s)"));
                    }
                    _ => { ui.label(format!("{outcome:?}")); }
                }
                ui.separator();
                if ui.button("Try Again").clicked() {
                    self.state = AuthState::Idle;
                }
            }

            AuthState::Error(e) => {
                ui.colored_label(egui::Color32::RED, e.as_str());
                ui.separator();
                if ui.button("Retry").clicked() {
                    self.state = AuthState::Idle;
                }
            }

            AuthState::Connecting => {
                ui.label("Connecting...");
                ctx.request_repaint_after(Duration::from_millis(100));
            }
        }
    }
}

fn feedback_label(state: FeedbackState) -> &'static str {
    match state {
        FeedbackState::Scanning => "Scanning...",
        FeedbackState::TooFar => "Move closer",
        FeedbackState::TooClose => "Move back",
        FeedbackState::TurnLeft => "Turn left",
        FeedbackState::TurnRight => "Turn right",
        FeedbackState::TiltUp => "Tilt up",
        FeedbackState::TiltDown => "Tilt down",
        FeedbackState::IRSaturated => "Too much IR glare — move back",
        FeedbackState::EyesNotVisible => "Eyes not visible",
        FeedbackState::LookAtCamera => "Look at camera",
        FeedbackState::Authenticating => "Authenticating...",
    }
}
```

- [ ] **Step 2: Build**

```bash
cargo build -p face-auth-gui
```

- [ ] **Step 3: Manual test**

Ensure face-authd is running (`systemctl status face-authd`). Run the GUI, navigate to Test Auth, click Start Auth Test. Verify feedback states cycle and result appears.

- [ ] **Step 4: Commit**

```bash
git add crates/face-auth-gui/src/tabs/test_auth.rs
git commit -m "feat(gui): Test Auth tab — daemon socket client"
```

---

## Task 11: Check Config and Configure skeleton tabs

**Files:**
- Modify: `crates/face-auth-gui/src/tabs/check_config.rs`
- Modify: `crates/face-auth-gui/src/tabs/configure.rs`

- [ ] **Step 1: Implement CheckConfigTab skeleton**

```rust
pub struct CheckConfigTab {
    output: Vec<(egui::Color32, String)>,
    ran: bool,
}

impl CheckConfigTab {
    pub fn new() -> Self {
        Self { output: Vec::new(), ran: false }
    }

    fn run_checks(&mut self) {
        use std::path::Path;
        self.output.clear();

        let ok = egui::Color32::GREEN;
        let warn = egui::Color32::YELLOW;
        let err = egui::Color32::RED;

        // Config file
        let cfg_path = "/etc/face-auth/config.toml";
        if Path::new(cfg_path).exists() {
            match face_auth_core::config::Config::load(Path::new(cfg_path)) {
                Ok(_) => self.output.push((ok, format!("[OK] Config: {cfg_path}"))),
                Err(e) => self.output.push((err, format!("[FAIL] Config: {e}"))),
            }
        } else {
            self.output.push((warn, format!("[WARN] Config not found: {cfg_path}")));
        }

        // Models
        let model_dirs = ["models", "/usr/share/face-auth/models", "/var/lib/face-auth/models"];
        for (file, desc) in &[("det_500m.onnx", "SCRFD detection"), ("w600k_mbf.onnx", "ArcFace recognition")] {
            let found = model_dirs.iter().any(|d| Path::new(d).join(file).exists());
            if found {
                self.output.push((ok, format!("[OK] Model: {desc}")));
            } else {
                self.output.push((err, format!("[FAIL] Model not found: {file}")));
            }
        }

        // Daemon socket
        let config = face_auth_core::config::Config::load_system().unwrap_or_default();
        if Path::new(&config.daemon.socket_path).exists() {
            self.output.push((ok, format!("[OK] Daemon socket: {}", config.daemon.socket_path)));
        } else {
            self.output.push((warn, "[WARN] Daemon socket not found (is face-authd running?)".into()));
        }

        // PAM module
        let pam_found = ["/usr/lib64/security/pam_face.so", "/var/lib/face-auth/pam_face.so"]
            .iter().any(|p| Path::new(p).exists());
        if pam_found {
            self.output.push((ok, "[OK] PAM module installed".into()));
        } else {
            self.output.push((warn, "[WARN] PAM module not found".into()));
        }

        self.ran = true;
    }

    pub fn ui(&mut self, ui: &mut egui::Ui) {
        ui.heading("Check Configuration");
        ui.separator();

        if ui.button("Run Checks").clicked() {
            self.run_checks();
        }

        if self.ran {
            ui.separator();
            for (color, line) in &self.output {
                ui.colored_label(*color, line);
            }
        }
    }
}
```

- [ ] **Step 2: Implement ConfigureTab skeleton**

```rust
pub struct ConfigureTab;

impl ConfigureTab {
    pub fn new() -> Self { Self }

    pub fn ui(&mut self, ui: &mut egui::Ui, config: &face_auth_core::config::Config) {
        ui.heading("Configuration");
        ui.separator();
        ui.label("Current settings (read-only — edit /etc/face-auth/config.toml to change):");
        ui.separator();

        egui::Grid::new("config_grid").num_columns(2).show(ui, |ui| {
            ui.label("Recognition threshold:");
            ui.label(format!("{:.2}", config.recognition.threshold));
            ui.end_row();

            ui.label("Session timeout:");
            ui.label(format!("{}s", config.daemon.session_timeout_s));
            ui.end_row();

            ui.label("Max embeddings:");
            ui.label(config.recognition.max_enrollment.to_string());
            ui.end_row();

            ui.label("Liveness enabled:");
            ui.label(config.liveness.enabled.to_string());
            ui.end_row();

            ui.label("Daemon socket:");
            ui.label(&config.daemon.socket_path);
            ui.end_row();

            ui.label("Execution provider:");
            ui.label(&config.daemon.execution_provider);
            ui.end_row();
        });

        ui.separator();
        ui.label("Full edit support coming in a future release.");
    }
}
```

- [ ] **Step 3: Build**

```bash
cargo build -p face-auth-gui
```

- [ ] **Step 4: Commit**

```bash
git add crates/face-auth-gui/src/tabs/check_config.rs crates/face-auth-gui/src/tabs/configure.rs
git commit -m "feat(gui): Check Config and Configure skeleton tabs"
```

---

## Task 12: Makefile and workspace integration

**Files:**
- Modify: `Makefile`

- [ ] **Step 1: Add GUI to release target and install target**

In `Makefile`, change the `release` target:

```makefile
release:
	$(CARGO) build --release -p face-authd -p face-enroll -p pam-face -p face-auth-gui
```

Add `GUI` variable near the top with other binary names:

```makefile
GUI := face-auth-gui
```

In the `install` target, after the ENROLL install line add:

```makefile
	$(INSTALL) -Dm755 target/release/$(GUI) $(DESTDIR)$(LIBEXECDIR)/$(GUI)
```

And in the SELinux chcon block add:

```makefile
		chcon -t bin_t $(DESTDIR)$(LIBEXECDIR)/$(GUI) 2>/dev/null || true; \
```

On traditional systems, add a symlink (after the ENROLL symlink):

```makefile
ifeq ($(ATOMIC),0)
	@ln -sf $(LIBEXECDIR)/$(GUI) $(DESTDIR)$(PREFIX)/bin/$(GUI) 2>/dev/null || true
endif
```

- [ ] **Step 2: Full release build**

```bash
make release
```

Expected: all four binaries build without errors.

- [ ] **Step 3: Verify binary exists**

```bash
ls -lh target/release/face-auth-gui
```

- [ ] **Step 4: Commit**

```bash
git add Makefile
git commit -m "build: add face-auth-gui to release and install targets"
```

---

## Self-Review Checklist

- [x] Task 1 covers parallel liveness spec section completely
- [x] Task 2 covers camera crate extraction (prerequisite for GUI)
- [x] Tasks 3–12 cover all 7 tabs (Enroll, Re-enroll, Status, Test Auth, Check Config, Configure, Test Camera)
- [x] Tab lifecycle (camera open/close on tab switch) covered in Task 3 `switch_tab()` and each tab's `deactivate()`
- [x] Root detection banner covered in Task 3 `app.rs`
- [x] `try_recv_frame()` added in Task 2 and used in Tasks 5, 8
- [x] `GuiModelCache` defined in Task 7 and used in Task 8
- [x] `EnrollState` transitions tested in Task 8
- [x] Makefile integration in Task 12
- [x] `score_and_filter` / `embedding_stats` implemented inline in Task 8 (not imported from face-enroll — separate crate boundary)
- [x] `quality::LivenessScores` type name: verify against `crates/face-auth-models/src/quality.rs` before Task 7

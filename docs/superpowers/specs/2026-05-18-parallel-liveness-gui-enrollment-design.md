# Design: Parallel Liveness Checks + GUI Enrollment App

**Date:** 2026-05-18  
**Status:** Approved

---

## Overview

Two features borrowed from analysis of the biopass project:

1. **Parallel liveness checks** — run IR texture and ML model anti-spoof concurrently instead of sequentially, improving architecture and slightly reducing latency when both are enabled.
2. **face-auth-gui** — tab-based egui desktop application replacing the debug enrollment flow with a full graphical control panel.

---

## Feature 1: Parallel Liveness Checks

### Problem

`inference.rs::process_frame()` runs liveness checks sequentially with early exit:
- IR texture (~1ms) → if fail, skip ML → if pass, run ML (~15ms) → if pass, recognition

Semantics are correct (both must pass) but the structure is serial. When both checks are enabled, they are independent — both read from the same already-captured frame with no shared mutable state between them. They can run concurrently.

### Change

**File:** `crates/face-authd/src/inference.rs` → `process_frame()`

Replace sequential IR → ML flow with `std::thread::scope` parallel execution:

```rust
std::thread::scope(|s| {
    let ir_handle = s.spawn(|| {
        let scores = quality::ir_liveness_check(
            &frame.data, &det.bbox, frame.width, frame.height,
        );
        scores.is_live(
            liveness_config.lbp_entropy_min,
            liveness_config.local_contrast_cv_min,
            liveness_config.local_contrast_cv_max,
        )
    });

    let ml_pass = liveness.as_mut().map(|live| {
        live.check(&frame.data, frame.width, frame.height, &det.bbox)
            .map(|r| r.is_real(liveness_config.model_threshold))
            .unwrap_or(true) // model error → don't block auth
    });

    let ir_pass = ir_handle.join().unwrap_or(false);
    (ir_pass, ml_pass)
});

let live_pass = ir_pass && ml_pass.unwrap_or(true);
is_live = Some(live_pass);
if !live_pass {
    return InferenceResult::Metrics { metrics, embedding: None, is_live };
}
```

### Behavioral Changes

- Both checks always run when enabled (no early exit between them).
- Latency when both enabled and both pass: `max(IR, ML)` instead of `IR + ML`. Saves ~1ms in practice.
- Latency when IR fails: both still complete, then early exit before recognition. Negligible difference.
- Semantics unchanged: all enabled checks must pass.
- ML model error → `unwrap_or(true)` preserves current behaviour (model errors don't block auth).

### No Config Changes

No new fields. `liveness.enabled` and `liveness.model_enabled` control the same behaviour as before.

---

## Feature 2: face-auth-gui

### New Crate

`crates/face-auth-gui/`  
Binary: `face-auth-gui`  
Installed alongside `face-enroll` and `face-authd`.

### Dependencies

```toml
eframe = "0.34"        # egui native window backend
egui = "0.34"          # immediate-mode GUI
```

All other deps reuse existing workspace crates:
`face-auth-core`, `face-auth-models`, `face-auth-platform`, `face-auth-camera` (extracted from face-enroll — see Architecture note below).

### Architecture Note: Camera Module Extraction

The `face_auth_camera` module currently lives inline in `crates/face-enroll/src/main.rs`. Before building the GUI crate, extract it to a shared crate `crates/face-auth-camera/` so both `face-enroll` and `face-auth-gui` can use it without duplication.

### Tab Layout

```
┌──────────────────────────────────────────────────────────────────┐
│  Enroll  │  Re-enroll  │  Status  │  Test Auth  │  Check Config  │  Configure  │  Test Camera  │
├──────────────────────────────────────────────────────────────────┤
│                         [active tab content]                      │
└──────────────────────────────────────────────────────────────────┘
```

### Camera Lifecycle

Tabs requiring camera: Enroll, Re-enroll, Test Auth, Test Camera.  
Tabs without camera: Status, Check Config, Configure.

Each camera-using tab holds `Option<CameraHandle>` in the app state. On tab activate: open camera, start frame thread. On tab leave: drop `CameraHandle` (existing drop impl stops capture thread cleanly). No persistent stream between tabs.

### Threading Model

```
Camera thread ──[Arc<Frame>]──► mpsc ──► app.update() polls latest frame
                                              │
                                    std::thread::spawn (per inference request)
                                              │
                                    detect → liveness → align → embed
                                              │
                                    result_tx ──► app.update() polls result
```

`eframe::update()` never blocks. Latest frame stored in `app.latest_frame: Option<Arc<Frame>>`. Latest inference result stored in `app.latest_result`. Both updated from channels each frame.

### Frame Rendering

```rust
// Grayscale → RGBA for egui
let rgba: Vec<u8> = frame.data.iter()
    .flat_map(|&g| [g, g, g, 255u8])
    .collect();
let img = egui::ColorImage::from_rgba_unmultiplied(
    [frame.width as usize, frame.height as usize], &rgba
);
let texture = ctx.load_texture("camera_feed", img, egui::TextureOptions::LINEAR);
ui.image(&texture);
```

Texture re-uploaded each frame. egui handles GPU upload internally.

### Enrollment Sub-State Machine

State stored in `EnrollState` enum on the app struct:

```
Idle
  │ [Start button]
  ▼
CapturingPose { pose_idx: usize, captured: usize }
  │ [3 good frames per pose × 5 poses]
  ▼
QualityReview { embeddings: Vec<[f32; 512]>, stats: QualityStats }
  │ [Save button]
  ▼
Saving
  │
  ▼
Done { embed_count: usize }
  │ [Re-enroll button → back to Idle]
```

During `CapturingPose`:
- Camera feed displayed with detection overlay (bbox + landmarks)
- State machine feedback shown as guidance text (too far, turn left, etc.)
- Liveness scores shown as small indicators
- Green flash on successful frame capture
- Progress bar: `pose_idx * 3 + captured / 15`

During `QualityReview`:
- Inter-embedding similarity stats displayed
- Option to accept or re-do enrollment
- Threshold suggestion shown if current config threshold looks mismatched

### Tab Inventory

| Tab | Phase 1 | Description |
|---|---|---|
| Enroll | Full | Enrollment wizard — fresh enrollment |
| Re-enroll | Full | Same wizard with `append=true`, loads existing count |
| Status | Full | Shows embedding count, format version, path, staleness warning |
| Test Auth | Full | Connects to face-authd daemon, shows live feedback state, result |
| Check Config | Skeleton | "Run Check" button → displays check results as colored list |
| Configure | Skeleton | Displays current config.toml values read-only |
| Test Camera | Full | Raw camera feed + resolution/format info, no inference |

Skeleton = tab exists, UI shows read-only info or "Run" button with output. Architecture allows filling in later without structural changes.

### Root Detection

On startup: check `geteuid() != 0`. If not root, show persistent yellow banner at top of window:

> "Not running as root — enrollment chown may fail. Launch with: sudo face-auth-gui"

Does not block any functionality. Same behaviour as existing CLI warning.

### Test Auth Tab

Mirrors `cmd_test_auth()` logic:
1. Check enrollment exists → show count or "Not enrolled" error.
2. "Start Auth" button → connect to daemon Unix socket.
3. Show live feedback state as it arrives (Scanning → TooFar → Authenticating…).
4. Display final result (Success / Failed / Timeout) with elapsed time.
5. "Try Again" button to re-run.

Does not open camera directly — camera is on the daemon side.

### Cargo.toml Changes

Root `Cargo.toml` workspace members: add `crates/face-auth-camera` and `crates/face-auth-gui`.

Makefile: add `face-auth-gui` to build and install targets.

### File Layout

```
crates/
  face-auth-camera/          ← extracted from face-enroll
    Cargo.toml
    src/
      lib.rs                 ← CameraHandle, open_camera(), Frame
  face-auth-gui/
    Cargo.toml
    src/
      main.rs                ← eframe::run_native entry point
      app.rs                 ← FaceAuthApp struct, update() loop
      tabs/
        mod.rs
        enroll.rs            ← EnrollTab, EnrollState machine
        status.rs
        test_auth.rs
        check_config.rs
        configure.rs
        test_camera.rs
      camera_texture.rs      ← frame → egui ColorImage conversion
      inference_worker.rs    ← background detect/embed thread
```

---

## Out of Scope

- Install/uninstall GUI (stays CLI — runs once, needs root, drives shell commands)
- Multi-user enrollment management
- Daemon control (start/stop/restart) from GUI
- Packaging/desktop file (`.desktop` entry) — deferred

---

## Implementation Order

1. Extract `face-auth-camera` crate
2. Implement parallel liveness in `inference.rs`
3. Scaffold `face-auth-gui` crate with eframe skeleton
4. Implement camera texture rendering + Test Camera tab
5. Implement Status tab
6. Implement Enroll tab (wizard state machine)
7. Implement Re-enroll tab (reuse Enroll with append flag)
8. Implement Test Auth tab (daemon socket client)
9. Stub Check Config and Configure tabs
10. Wire into Makefile, test end-to-end

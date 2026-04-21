# Design Decisions

## Phase 0 Findings

### 0.1 — PAM crate choice

**Decision: use `pam` crate v0.8.0 with `module` feature + `pam-sys` v1.0.0-alpha5 as fallback.**

Evaluation:
- `pam` crate (github.com/1wilkens/pam): last release 0.8.0, active repo, MIT/Apache-2.0.
  Has a `module` feature for writing PAM modules (not just PAM clients).
  Requires `pam-devel` system package.
- `pam-sys` v1.0.0-alpha5: raw FFI, same author. Alpha quality — avoid as primary.
- `libpam-sys` v0.2.0: alternative low-level binding, newer, but less ecosystem.

**Primary choice: `pam` crate with `module` feature.**
If build fails on Fedora 43, fall back to raw `libc` FFI with manual PAM type definitions
(the PAM ABI is stable — manually defining `pam_handle_t`, `pam_message`, `pam_response`
from `security/pam_modules.h` is viable and avoids crate dependency entirely).

**Requires:** `sudo dnf install pam-devel`

### 0.2 — ONNX model sources

**Decision: download from insightface v0.7 GitHub releases and minivision-ai repo.**

| Model | Source | Size | SHA256 | Notes |
|-------|--------|------|--------|-------|
| SCRFD-500M | insightface buffalo_s | 2.4 MB | 5e4447f50245bbd7966bd6c0fa52938c61474a04ec7def48753668a9d8b4ea3a | 640×640 dynamic, 9 outputs (score/bbox/kps × 3 strides) |
| MobileFaceNet w600k | insightface buffalo_s | 13.0 MB | 9cc6e4a75f0e2bf0b1aed94578f144d15175f357bdc05e815e5c4a02b319eb4f | 112×112, 512-dim embedding |
| MiniFASNetV2-SE | Silent-Face-Anti-Spoofing | 0.6 MB | fde20585635cae62ed1d41796f76b6f8bc4b92cd91ec1cf0f1bc6485d2d587a9 | 128×128, [batch, 2] softmax output |

**Verified 2026-04-19:** all three models load, shapes match, dummy inference passes (see test-onnx).

**IMPORTANT color format note:**
- SCRFD-500M: BGR input
- MobileFaceNet w600k: RGB input ← easy source of bugs, document prominently
- MiniFASNetV2-SE: BGR input (assumed, verify during pipeline integration)

### 0.3 — V4L2 camera detection

IR camera identified by grayscale pixel format (GREY / Y800 / BA81).
RGB camera identified by MJPEG / YUYV.
Detection heuristic: iterate /dev/video*, query formats, return first grayscale device.

**Results on IdeaPad Pro 5 (Fedora Atomic):**

| Device | Formats | Name |
|--------|---------|------|
| /dev/video0 | (none) | OBS Virtual Camera |
| /dev/video1 | MJPG, YUYV | Integrated RGB Camera |
| /dev/video2 | (none) | Integrated RGB Camera (metadata/ctrl?) |
| /dev/video3 | GREY | **IR camera** ← target device |
| /dev/video4 | (none) | Integrated RGB Camera (metadata/ctrl?) |

IR camera: `/dev/video3`, GREY, 640×360.
Frame capture: confirmed working (230400 bytes = 640×360×1).

**IR emitter status:** Not confirmed lit during capture. IR LED on IdeaPad Pro 5
may require V4L2 control or UVC extension unit (XU) to activate.
Investigation needed — see test-camera `--list-controls` output.

### 0.4 — SDDM PAM conv messages

PAM conversation callback works for TEXT_INFO — test-pam-stub received 2 messages cleanly.
SDDM lock screen forwarding not tested (manual visual test, low priority).
If SDDM drops PAM_TEXT_INFO: UI socket (Phase 8) is mandatory for login screen feedback.
Terminal sudo always shows PAM_TEXT_INFO messages correctly.

### 0.5 — sddm video group access

**Confirmed:** sddm (uid=969) NOT in video group by default on Fedora 43.
- /dev/video3 owned root:video (gid=39), mode 20660
- Open as sddm without video group: Permission denied
- Open as sddm WITH video group: OK

**Fix:** `sudo usermod -aG video sddm && sudo systemctl restart sddm`
**Distribution requirement:** install process must include this step.

## Phase 1–3 Decisions

### PAM module — raw FFI over pam crate

**Decision: raw C FFI with libc instead of `pam` crate.**

The `pam` crate's `module` feature didn't compile cleanly on Fedora 43. Switched to manual
`pam_handle_t` / `pam_message` / `pam_response` definitions from `security/pam_modules.h`.
PAM ABI is stable — no risk. Eliminates crate dependency entirely.

### IPC — spawn_blocking with sync framing

Session uses `tokio::task::spawn_blocking` for all sync I/O (Unix sockets, V4L2, ONNX).
Simpler than async framing, adequate for low-throughput PAM IPC.
Protocol: 4-byte LE length prefix + bincode serialization.

### Camera pipeline — dedicated std::thread

V4L2 capture runs in a `std::thread` (not tokio task). Frames sent via `mpsc::sync_channel(3)`.
`try_send` drops frames if consumer is slow. IR emitter lifecycle managed in capture thread
(activated on start, deactivated on thread exit via Drop).

## Phase 4 Decisions

### SCRFD-500M postprocessing

**Input:** Grayscale 640×360 → letterbox pad bottom to 640×640, replicate to 3 channels,
normalize `(pixel - 127.5) / 128.0`.

**Output:** 9 tensors — 3× scores, 3× bboxes, 3× keypoints for strides [8, 16, 32].
Anchor-free: 2 anchors per grid cell, anchor center at `(col+0.5)*stride, (row+0.5)*stride`.
Bbox decode: `cx ± distance * stride`. Keypoint decode: `cx + offset * stride`.
Confidence threshold: 0.5. NMS IoU threshold: 0.4.

**Verified 2026-04-19:** Real-time detection working on IR frames. Face detected and
geometry feedback (Scanning → TooFar → Authenticating) sent to PAM within ~5s.

### InferenceThread architecture

ONNX session lives in a dedicated `std::thread` (not tokio). Receives `Arc<Frame>` via
`mpsc::sync_channel(2)`, sends `InferenceResult` back. Skips stale frames (>500ms old).
Session loop drives `StateMachine` from inference results, sends `FeedbackState` to PAM.

### ort crate log filtering

`ort` crate emits ~100 INFO lines per model load (graph optimizers, BFCArena reservations).
Default filter: `info,ort=warn`. Override with `RUST_LOG=ort=info` if needed.

### IR quality checks

- **Saturation:** >30% of face ROI pixels above 250 → `ir_saturated = true` (raised from 15% — IR emitter proximity causes moderate saturation on normal faces)
- **Blur:** Laplacian variance of face ROI. Kernel `[[0,1,0],[1,-4,1],[0,1,0]]`.
  Score < 50.0 = too blurry. Both computed per-frame in inference thread.

## Phase 5 Decisions

### Face alignment — 5-point similarity transform

Procrustes analysis via nalgebra: centroid alignment, RMS scaling, cross-covariance rotation.
Maps 5 detected landmarks to canonical ArcFace 112×112 positions. Bilinear interpolation
for sub-pixel accuracy.

### ArcFace MobileFaceNet w600k

112×112 input, 512-dim L2-normalized embedding. Preprocessing pipeline:
1. CLAHE (14×14 tiles, clip_limit=2.0) — lighting normalization
2. `(pixel - 127.5) / 127.5` — ArcFace normalization
3. Grayscale replicated to 3 channels

CLAHE replaced global histogram equalization (unstable — small crop shifts caused wildly
different equalized images and embeddings). CLAHE operates locally with clipped contrast,
producing stable output across frame-to-frame crop variation.

### Enrollment storage

Bincode serialization in `~/.local/share/face-auth/<username>/embeddings.bin`.
Uses `Vec<Vec<f32>>` wrapper (serde doesn't support `[f32; 512]`). Atomic write via
temp file + rename. User home resolved via `/etc/passwd` (avoids $HOME mismatch under sudo).

### Recognition thresholds

Multi-frame matching: 2 consecutive frames above threshold required (reduced from 3).
High-confidence shortcut: if sim ≥ threshold + 0.10 (i.e. ≥0.80), accept on 1 frame.
- Default threshold: 0.70 (raised from 0.28 → 0.55 → 0.70)
- Real face: ~0.87 cosine similarity (with CLAHE preprocessing)
- Phone photo (IR): ~0.42 cosine similarity (with CLAHE)
- Phone photo was able to fool system at threshold 0.55

### Feedback directions — corrective instructions

Feedback messages tell user what to DO, not what they ARE doing.
- Looking down (positive pitch) → TiltUp
- Looking up (negative pitch) → TiltDown
- Turned right (positive yaw) → TurnLeft
- Turned left (negative yaw) → TurnRight

## Phase 6 Decisions

### ML liveness model (MiniFASNetV2-SE) — INEFFECTIVE on IR

MiniFASNetV2-SE trained on RGB visible-light images. In IR domain, the discriminating cues
it relies on (moiré patterns, color distortion, specular highlights) don't exist.
All grayscale → model has no signal. Disabled by default (`model_enabled: false`), kept
in codebase for potential future RGB camera support.

### IR texture liveness — LBP entropy + local contrast CV

Replaced ML model with IR-specific texture analysis:
1. **LBP (Local Binary Pattern) entropy:** 8-neighbor comparison → 256-bin histogram → Shannon
   entropy. Real skin has diverse micro-texture (~5.0–7.0 bits). Flat screens have uniform
   texture (~2.0–4.0 bits). Default min: 4.5.
2. **Local contrast CV:** Divide face ROI into 16×16 patches, compute std dev per patch,
   then coefficient of variation. Real 3D faces have varied contrast (~0.3–0.8 CV). Flat
   screens are more uniform (~0.05–0.25 CV). Default min: 0.25.

**Measured 2026-04-19:**
- Real face: LBP 6.00–6.18 (stable), CV 0.28–0.36 (stable, one outlier 1.24)
- Phone spoof: LBP 0.4–6.9 (wildly unstable), CV 0.0–2.59 (wildly unstable)
- Key insight: individual phone frames at good angle match real face scores exactly.
  Single-frame thresholds alone cannot distinguish them. Temporal stability is the signal.

**Tuned thresholds:** lbp_min=5.5, cv_min=0.20, cv_max=0.80. Catches ~60% of phone frames.
Remaining ~40% require temporal stability check (below). cv_max initially set to 0.50 but
real face CV goes to 0.67–0.72 during normal head movement — widened to 0.80.

### Temporal liveness stability — rolling window

Single-frame texture metrics can't distinguish real face from well-positioned phone.
Added rolling window: track last 10 liveness results, require ≥80% pass rate before
recognition counts. Real face passes 100% of frames (always stable). Phone passes ~40-50%
(score jumps between 0.4 and 6.5 LBP entropy between consecutive frames).

### Multi-layer anti-spoofing defense

Pipeline: IR texture check → temporal stability → (optional ML model) → recognition threshold
- Layer 1: IR texture per-frame (LBP ≥5.5, CV 0.20–0.80) — fast, no model
- Layer 2: Temporal stability (≥80% of last 10 frames pass) — catches phone instability
- Layer 3: ML model (opt-in, `model_enabled: true`) — for RGB cameras only
- Layer 4: High recognition threshold (0.70) — catches degraded spoof embeddings
- Layer 5: Multi-frame consensus (3 consecutive matches) — further smoothing
- Spoof rejection is silent — indistinguishable from recognition failure (security)

### Liveness history clearing — only on face disappearance

Initially cleared liveness history on every state machine transition away from Authenticating
(including TiltUp, MoveCloser guidance). This was too aggressive — real face bounces between
Authenticating↔Guidance frequently, resetting the rolling window each time. Changed to only
clear on `Scanning` state (face disappeared entirely). Guidance transitions preserve history
so temporal stability signal accumulates across the session.

## Phase 7 Decisions

### Collapse Phases 7-10 into single Phase 7 "System Integration & Distribution"

Old plan had Phases 7 (SystemD/PAM/SELinux), 8 (SDDM UI Overlay), 9 (Packaging), 10 (Porting Guide).
UI overlay deferred — PAM TEXT_INFO feedback works for terminal, and SDDM overlay is a polish
item. Packaging/install/systemd are all interdependent and should ship together.

New Phase 7 covers: Makefile, install.sh, uninstall.sh, PAM injection, systemd service,
enrollment CLI polish, default config, Fedora Atomic support.

### Build system — Makefile over cargo-make

Simple GNU Makefile. `make release` builds all binaries, `make install` copies to system paths.
No additional build tool dependency. Makefile calls cargo internally.

### PAM injection — interactive install.sh, NOT rpm %post

PAM configuration is security-sensitive. Never auto-modify in package post-install scripts.
`install.sh` prompts user to select which PAM files to modify (sddm, kscreenlocker, sudo,
polkit). Backups created before modification.

### No separate watchdog — systemd Restart=on-failure

systemd already provides process supervision. `Restart=on-failure` with `RestartSec=2s` handles
daemon crashes. No custom watchdog needed. Type=notify for readiness signaling.

### Enrollment CLI — multi-angle capture with quality gates

`face-enroll` captures multiple frames from different angles, filters by quality (blur, saturation,
face size), and stores top-N embeddings. Better enrollment = better recognition consistency.

### Model pre-loading at daemon startup

Models loaded once in `ModelCache` with `Mutex<FaceDetector>` / `Mutex<Option<FaceRecognizer>>`.
Reused across all auth sessions via `Arc`. Saves ~150ms per session vs loading per-session.

### Camera → Inference direct pipeline

Camera sends `Arc<Frame>` directly to inference thread via `mpsc::Receiver`. Session loop only
consumes `InferenceResult` — no frame hop through session. Reduces latency by one channel step.

### Send auth result before cleanup

Auth result sent to PAM immediately on decision, BEFORE `drop(camera)` + `drop(inference)`.
Cleanup (capture thread stop, inference thread join) takes ~360ms. Moving it after result send
means user sees SUCCESS 360ms sooner. Cleanup happens in background.

### Performance tuning (Phase 7)

| Setting | Before | After | Impact |
|---------|--------|-------|--------|
| guidance_debounce_ms | 300 | 100 | Faster state transitions |
| flush_frames | 5 → 2 | 0 | Save ~66-165ms, bad frames rejected by pipeline |
| liveness warmup | 2 frames | 1 frame | Save ~130ms |
| frames_required | 3 | 2 (1 if high-confidence) | Save 1-2 inference cycles |
| Result timing | After cleanup | Before cleanup | Save ~360ms perceived |
| Geometry thresholds | Strict | Relaxed (45/45/35°) | Less bouncing |

Result: 0.4–1.3s auth (down from 3–10s in early Phase 7).

### GPU/NPU execution provider support

`execution_providers()` helper maps config string to `ort::ExecutionProviderDispatch`:
- "cpu" → CPU (default, always works)
- "rocm" → ROCm + CPU fallback (AMD GPU)
- "cuda" → CUDA + CPU fallback (NVIDIA GPU)
- "openvino" → OpenVINO + CPU fallback (Intel)
- "vitis"/"xdna" → Vitis AI + CPU fallback (AMD XDNA NPU, Linux 7.0+)

Always includes CPU as fallback. EP selected in config.toml, applied at daemon startup.

### PAM service discovery — scan all /etc/pam.d/*

Install script scans every file in `/etc/pam.d/` instead of hardcoded list. Services
classified as:
- **Recommended (default Y):** sddm, gdm, lightdm, kde, kde-fingerprint, kscreenlocker,
  sudo, su, polkit-1, login, xscreensaver, xfce4-screensaver, cinnamon-screensaver,
  mate-screensaver
- **Skip silently:** sddm-greeter, sddm-autologin, password-auth, system-auth, config-util,
  fingerprint-auth, smartcard-auth, postlogin, etc.
- **Optional (default N):** everything else

KDE lock screen uses `/etc/pam.d/kde`, NOT `/etc/pam.d/sddm`. Discovered by checking
actual PAM service called during lock screen auth.

## Phase 8 Decisions

### Debug UI — minifb + embedded bitmap font

`minifb` (0.28) chosen for debug window: no GPU, no X11 beyond basics, simple pixel
buffer API. Window fixed at 840×360 (640 camera + 200 sidebar panel). Text rendered
via embedded 5×7 bitmap font (ASCII 32–126, no freetype/fontconfig dependency).
Bbox colour: green=match, blue=liveness pass, red=spoof/below threshold, yellow=detecting.

### TUI config editor — ratatui + crossterm

`ratatui` (0.29) + `crossterm` (0.28) for `face-enroll --configure`. All 20 tunable
config fields exposed in a scrollable table grouped by section. Bool fields toggle on
Enter; string/number fields open inline edit bar. Modified values shown in yellow.
`toml` (0.8) crate for serialization — TOML written from full Config struct so unexposed
fields (platform.*) are preserved from the loaded base config.

After save: `systemctl kill --signal=SIGHUP face-authd` triggers live daemon reload.
Status bar shows "Saved — daemon reloaded" or actionable message if daemon is down.

### Config hot-reload — Arc<RwLock<Arc<Config>>>

`Arc<Config>` → `Arc<std::sync::RwLock<Arc<Config>>>`. Each incoming PAM connection
snapshots the inner Arc with a read-lock (cheap clone, no deep copy). In-flight sessions
hold their snapshot unaffected by reloads. SIGHUP handler: write-lock, swap inner Arc,
unlock. If `execution_provider` changed, also calls `ModelStore::reload_with_ep()`.
`std::sync::RwLock` used (not tokio) — critical section is a pointer swap, trivially short.

### ModelStore — idle unload with Arc::try_unwrap safety

`ModelStore` wraps `Option<Arc<ModelCache>>` with `last_used: Instant`. Idle background
task (30s interval) calls `maybe_unload()`. Unload uses `Arc::try_unwrap`: if an auth
session still holds a live Arc clone, unwrap fails → model stays loaded → retried next
interval. Safe by construction — no force-drop while session is running.
Default `idle_unload_s = 0` (disabled): keep models loaded for fast sudo/lockscreen auth.
Users on RAM-constrained hardware can enable (e.g. 300s).

### Desktop notifications — notify-send via user D-Bus

`send_desktop_notification()` in session.rs:
1. Resolves user's UID from `/etc/passwd` (correct under root daemon)
2. Sets `DBUS_SESSION_BUS_ADDRESS=unix:path=/run/user/<uid>/bus`
3. Spawns `notify-send` as that user via `CommandExt::uid()`

Opt-in via `[notify] enabled = false` default. Avoids systemd user session complexity.
Fire-and-forget — errors logged at debug, never propagated.

### Enrollment versioning — ENROLLMENT_VERSION field

`EnrollmentData` gained a `version: u32` field with `#[serde(default = "default_version")]`
(returns 1 for pre-existing files). `ENROLLMENT_VERSION = 2` stamps new saves.
On auth load: if stored version < current → log `warn` suggesting re-enrollment.
On `--status`: show version comparison explicitly.
Version bumped when preprocessing pipeline changes (e.g. CLAHE addition).

### polkit-1 — created by installer when absent

`/etc/pam.d/polkit-1` not present by default on this Fedora system. Installer now
has a `CREATABLE_SERVICES` list for recommended services whose PAM files don't exist yet.
After scanning existing files, installer offers to create them from templates (default Y).
polkit-1 template: standard system-auth delegate with pam_face.so prepended.
On uninstall: phase 2 (strip pam_face.so lines from all PAM files) cleans it correctly;
polkit-1 remains as a valid system-auth delegate — no deletion needed.

### KDE lockscreen — no PAM_TEXT_INFO feedback

kscreenlocker silently drops PAM `PAM_TEXT_INFO` conversation messages. Auth still works
(face is recognized), but user gets no "scanning" / "move closer" feedback on the lock screen.
Fix requires a custom kscreenlocker QML/DBus overlay (deferred to future backlog).
KWallet face-auth is not possible — kwallet requires the actual password for key decryption.

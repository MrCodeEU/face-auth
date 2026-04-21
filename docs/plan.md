# face-auth: Implementation Handoff Plan
### Windows Hello–style Face Authentication for Linux — Rust, Fedora KDE, Atomic-first

---

## Document Purpose

This document is a complete, agent-ready implementation handoff plan. Each phase has
unambiguous inputs, outputs, acceptance criteria, and dependency edges. A developer
picking up any phase should be able to execute it without referencing prior conversation.
Where a decision was made and alternatives exist, the rationale is stated.

---

## Project Goals

- Windows Hello–quality face authentication on Linux using IR cameras
- Full Rust implementation (userspace — no kernel module required)
- Initial target: Fedora 43 KDE (SDDM display manager)
- First-class Fedora Atomic / rpm-ostree support from day one
- Every environment-specific integration point is pluggable — adding GDM, LightDM,
  COSMIC, or other display managers must require zero changes to the core daemon
- Security model: convenience layer (like a PIN), never sole auth method, always
  falls through to password on any failure

---

## Repository Layout

```
face-auth/
├── Cargo.toml                  # workspace root
├── crates/
│   ├── face-auth-core/         # shared types, IPC protocol, config — no OS deps
│   ├── face-authd/             # daemon binary
│   ├── pam-face/               # PAM module (.so)
│   ├── face-enroll/            # enrollment CLI
│   ├── face-auth-platform/     # pluggable platform backends (DM, SELinux, etc.)
│   └── face-auth-models/       # ONNX model loading + inference abstraction
├── models/                     # ONNX model files (not in git — downloaded at build/install)
│   ├── detection/scrfd_500m.onnx
│   ├── recognition/arcface_r50.onnx
│   └── liveness/minifasnet_v2.onnx
├── platform/
│   ├── selinux/
│   │   └── face_auth.te        # SELinux type enforcement policy
│   ├── systemd/
│   │   └── face-authd.service
│   ├── pam/
│   │   ├── sddm.conf.snippet
│   │   ├── kscreenlocker.conf
│   │   └── gdm-password.conf.snippet  # future
│   └── sddm/
│       └── face-auth-theme/    # SDDM QML overlay (Phase 7)
├── install/
│   ├── install.sh              # orchestrates all install steps
│   ├── detect-platform.sh      # auto-detects DM, init system, SELinux status
│   └── uninstall.sh
├── packaging/
│   ├── face-auth.spec          # RPM spec
│   └── Containerfile           # for baking into Atomic OCI images
└── docs/
    ├── architecture.md
    ├── protocol.md             # IPC wire format (normative)
    └── platform-porting.md    # guide for adding new DM/distro support
```

---

## Pluggability Model

Every environment-specific concern is expressed as a trait in `face-auth-platform`.
The daemon selects a concrete implementation at startup based on `config.toml`.
Adding support for a new environment = implementing the relevant trait + adding a
config variant. Zero changes to core logic.

```
Pluggable traits:
  DisplayManagerBackend    — user/group setup, PAM file paths, DM detection
  SelinuxPolicyBackend     — policy module name, type names, install commands
  SystemdBackend           — service name, socket activation config
  NotificationBackend      — how to surface feedback to the user at login
  ModelStoreBackend        — where ONNX models are stored/found on this distro
```

Config selects the backend:

```toml
# /etc/face-auth/config.toml

[platform]
display_manager = "sddm"        # sddm | gdm | cosmic | lightdm | generic
init_system     = "systemd"     # systemd (only option currently)
selinux         = true          # whether to install SELinux policy

[display_manager.sddm]
dm_user      = "sddm"
pam_login    = "/etc/pam.d/sddm"
pam_lock     = "/etc/pam.d/kscreenlocker"
pam_polkit   = "/etc/pam.d/polkit-1"
pam_sudo     = "/etc/pam.d/sudo"

[daemon]
socket_path        = "/run/face-auth/pam.sock"
ui_socket_path     = "/run/face-auth/ui.sock"
session_timeout_s  = 15
idle_unload_s      = 300        # unload models after 5min idle; 0 = never unload
max_concurrent     = 1          # reject or queue concurrent auth sessions

[camera]
# leave empty for auto-detection
device_path        = ""
flush_frames       = 5          # discard N frames on camera open (IR AE settle)

[recognition]
model              = "arcface_r50"
threshold          = 0.28
frames_required    = 3          # consecutive good frames before auth attempt
max_enrollment     = 20         # max stored embeddings per user

[liveness]
enabled            = true
model              = "minifasnet_v2"
# spoof failures are silent externally — attacker sees only AuthFailed

[geometry]
distance_min       = 0.15       # face width / frame width ratio
distance_max       = 0.45
yaw_max_deg        = 25.0
pitch_max_deg      = 20.0
roll_max_deg       = 15.0
guidance_debounce_ms = 300      # min ms before changing guidance message

[logging]
level              = "info"     # error | warn | info | debug
# SECURITY: debug level must never log embeddings, similarity scores, or frame data
```

---

## IPC Protocol (Normative)

Defined in `face-auth-core`. Both `pam-face` and `face-authd` depend on this crate.
Any change to the protocol is a breaking change and requires a version bump.

**Wire format:** 4-byte little-endian length prefix + `bincode`-serialized message.
Both sockets use the same framing. The PAM module sends requests; the daemon sends
responses + streaming feedback.

```rust
// face-auth-core/src/protocol.rs

pub const PROTOCOL_VERSION: u32 = 1;

// PAM → Daemon
#[derive(Serialize, Deserialize, Debug)]
pub enum PamRequest {
    Auth   { version: u32, username: String, session_id: u64 },
    Cancel { session_id: u64 },
}

// Daemon → PAM (streaming — multiple Feedback messages, then one AuthResult)
#[derive(Serialize, Deserialize, Debug)]
pub enum DaemonMessage {
    Feedback  { session_id: u64, state: FeedbackState },
    AuthResult { session_id: u64, outcome: AuthOutcome },
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub enum FeedbackState {
    Scanning,
    TooFar,
    TooClose,
    TurnLeft,
    TurnRight,
    TiltUp,
    TiltDown,
    IRSaturated,      // overexposure — usually means too close
    EyesNotVisible,   // IR-blocking glasses or occlusion
    LookAtCamera,     // face detected but gaze off-axis
    Authenticating,   // geometry/quality passed, running recognition
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub enum AuthOutcome {
    Success,
    Failed,
    Timeout,
    DaemonUnavailable,  // PAM module could not connect — fall through to password
    Cancelled,
}

// UI socket (daemon → SDDM theme / overlay)
// Same framing, different message type
#[derive(Serialize, Deserialize, Debug)]
pub enum UiMessage {
    SessionStarted { username: String },
    Feedback       { state: FeedbackState },
    SessionEnded   { outcome: AuthOutcome },
}
```

**Timeout contract** (enforced by PAM module, not daemon):
- Connection attempt timeout: 500ms. If daemon unreachable → `DaemonUnavailable` immediately.
- Session hard timeout: 15s (configurable). PAM module sends `Cancel` then returns
  `PAM_AUTHINFO_UNAVAIL` which falls through to next PAM module (password).
- The PAM module MUST NOT block PAM indefinitely under any circumstance.

---

## Phase 0 — Foundation & External Dependency Vetting

**Prerequisite for all other phases. No production code until this is complete.**

### Tasks

**0.1 — PAM crate audit**

Build a minimal PAM module using the `pam` crate that:
- Compiles to a `.so` on Fedora 43
- Installs to `/usr/lib64/security/`
- Loads without error when referenced in `/etc/pam.d/sudo`
- Returns `PAM_SUCCESS` unconditionally (stub)
- Can be verified with `sudo echo test`

If `pam` crate is not viable (check GitHub: last commit, open issues, known
Fedora 43 build failures): evaluate `pam-sys` (raw FFI) with a manual safe
wrapper written in-project. Document the decision.

**0.2 — ONNX model availability check**

Download and verify the three ONNX models:

| Model | Source | Expected size | Input | Output |
|-------|--------|--------------|-------|--------|
| SCRFD-500M | insightface GitHub releases | ~2MB | 640×640 BGR | bboxes + 5 landmarks |
| ArcFace R50 | insightface GitHub releases | ~166MB | 112×112 RGB | float[512] |
| MiniFASNet-v2 | minivision-ai/Silent-Face-Anti-Spoofing | ~1MB | 80×80 BGR | float[2] (real/spoof) |

Verify each loads and produces expected output shapes using `ort` in a standalone
Rust test binary. Document exact model file hashes (SHA256) for reproducible builds.

**0.3 — V4L2 IR camera detection**

On the target hardware (IdeaPad Pro 5 with IR camera):
- Run `v4l2-ctl --list-devices` and document the output
- For each `/dev/video*` device, query pixel formats with `v4l2-ctl --list-formats-ext`
- Identify the IR device by presence of `GREY` or `Y800` format
- Verify the detection heuristic works: IR camera reports grayscale formats;
  RGB camera reports MJPEG/YUYV
- Write a Rust test binary using the `v4l2` crate that opens, captures one frame
  from the IR device, and saves it as a PNG

**0.4 — SDDM PAM conv behaviour**

Verify whether SDDM processes `PAM_TEXT_INFO` conv messages:
- Temporarily add a PAM module stub that sends `PAM_TEXT_INFO "TEST MESSAGE"`
- Add it to `/etc/pam.d/sddm`
- Lock screen and observe whether the message appears
- Document result — if SDDM silently drops conv messages, the UI socket path
  (Phase 7) becomes mandatory for any user-facing feedback, not optional

**0.5 — sddm user + video group verification**

Confirm `usermod -aG video sddm` is sufficient for SDDM to open `/dev/video*`:
- Add sddm to video group
- Write a setuid test binary that drops to sddm UID and attempts to open the IR device
- Verify it succeeds

**Done criteria:** All five sub-tasks complete with documented results. All external
crate/model choices locked. Any showstoppers discovered and resolved. A
`docs/decisions.md` written with every choice and its rationale.

---

## Phase 1 — Shared Core Crate

**Depends on:** Phase 0 complete.

**Crate:** `face-auth-core`

This crate has zero OS-specific dependencies. It must compile on any platform.
It is the single source of truth for the IPC protocol, config schema, and shared types.

### Deliverables

- `src/protocol.rs` — all IPC types as defined in the Protocol section above
- `src/config.rs` — full `Config` struct with `serde` derive, loads from
  `/etc/face-auth/config.toml`, all fields have defaults via `#[serde(default)]`
- `src/geometry.rs` — `FaceMetrics` struct, `analyze_geometry()` function,
  `AuthState` enum, `StateMachine` struct with `transition()` method
- `src/framing.rs` — `read_message<T>()` and `write_message<T>()` using the
  4-byte-prefix + bincode framing. Used identically by PAM module and daemon.
- Full unit test coverage for `analyze_geometry()` with synthetic landmark data
  covering: all guidance states, edge cases at thresholds, debounce timing
- Full unit test coverage for `StateMachine::transition()` covering every state
  transition path

### Key geometry implementation notes

Distance proxy (no depth sensor):
```
face_width_ratio = bbox_width_px / frame_width_px
too_far   if face_width_ratio < config.geometry.distance_min  (default 0.15)
too_close if face_width_ratio > config.geometry.distance_max  (default 0.45)
```

Yaw from 5-point landmarks (eye-nose horizontal asymmetry):
```
left_offset  = nose_x - left_eye_x
right_offset = right_eye_x - nose_x
yaw_raw = (left_offset - right_offset) / (left_offset + right_offset)
yaw_deg = yaw_raw * 45.0  // empirically calibrated, not trigonometrically exact
```

Pitch from nose position relative to eye-mouth axis:
```
eye_mid_y   = (left_eye_y + right_eye_y) / 2.0
mouth_mid_y = (left_mouth_y + right_mouth_y) / 2.0
face_height = mouth_mid_y - eye_mid_y
nose_ratio  = (nose_y - eye_mid_y) / face_height
// neutral ~0.40; >0.50 = looking down; <0.30 = looking up
pitch_deg = (nose_ratio - 0.40) * 100.0
```

Roll from eye-to-eye vector angle:
```
dx = right_eye_x - left_eye_x
dy = right_eye_y - left_eye_y
roll_deg = atan2(dy, dx).to_degrees()
```

IR-specific quality checks (replaces RGB brightness check entirely):
```
ir_saturated  = (pixels > 250).count() / total_pixels > 0.15  // >15% blown out
eyes_visible  = left_eye_confidence > 0.5 && right_eye_confidence > 0.5
blur_score    = laplacian_variance(face_roi)  // <50.0 = too blurry
```

**Done criteria:** `cargo test -p face-auth-core` passes with 100% of state machine
transitions covered. Geometry functions produce correct outputs for all boundary cases.
Zero OS-specific imports.

---

## Phase 2 — PAM Module

**Depends on:** Phase 0 (PAM crate decision), Phase 1 (protocol types)

**Crate:** `pam-face` → produces `pam_face.so`

### Deliverables

- PAM module implementing `pam_sm_authenticate` only (not `pam_sm_setcred` etc.)
- Reads socket path from `/etc/face-auth/config.toml` (falls back to
  `/run/face-auth/pam.sock` if config unreadable)
- Connection logic:
  - Attempt Unix socket connection with 500ms timeout
  - On failure: log to syslog at `LOG_INFO`, return `PAM_AUTHINFO_UNAVAIL`
    (this causes PAM to fall through to the next module — the password prompt)
  - Never return `PAM_AUTH_ERR` on infrastructure failure, only on actual face mismatch
- Session logic:
  - Generate random `session_id: u64`
  - Send `PamRequest::Auth { version: PROTOCOL_VERSION, username, session_id }`
  - Receive `DaemonMessage` loop:
    - On `Feedback`: forward text to PAM conv as `PAM_TEXT_INFO` (best-effort;
      if conv returns error, continue — don't abort auth)
    - On `AuthResult::Success`: return `PAM_SUCCESS`
    - On `AuthResult::Failed`: return `PAM_AUTH_ERR`
    - On `AuthResult::Timeout` or `AuthResult::DaemonUnavailable`: return
      `PAM_AUTHINFO_UNAVAIL`
  - Hard timeout: if no `AuthResult` received within 15s, send
    `PamRequest::Cancel { session_id }`, return `PAM_AUTHINFO_UNAVAIL`
- All error paths must return, never panic — a panic in a PAM module crashes the
  calling process (sudo, SDDM, etc.)
- Logging: syslog only, via `libc` syslog FFI. No file I/O. Never log usernames
  at info level; debug level only.

### Build configuration

```toml
# pam-face/Cargo.toml
[lib]
crate-type = ["cdylib"]
name = "pam_face"
```

The output `libpam_face.so` must be renamed/symlinked to `pam_face.so` at install
time (PAM requires the exact name without `lib` prefix).

### PAM config snippet (installed to /etc/pam.d/sudo etc.)

```
auth  sufficient  pam_face.so
```

`sufficient` means: if face auth succeeds, stop — no password needed. If it fails
or is unavailable, continue to the next line (password). Never use `required` in
v1 — that would make face auth mandatory, breaking headless access.

**Done criteria:**
- `cargo build --release -p pam-face` produces `pam_face.so`
- With daemon stub running (always returns Success): `sudo echo ok` authenticates
  without password prompt
- With daemon not running: `sudo echo ok` falls through to password prompt normally
- With daemon running (always returns Failed): `sudo echo ok` falls through to
  password prompt normally
- No crashes on any code path (test with daemon killed mid-session)

---

## Phase 3 — Daemon Scaffolding + Camera Pipeline

**Depends on:** Phase 1, Phase 2 (for end-to-end socket test)

**Crate:** `face-authd`

This phase builds the daemon skeleton with full camera pipeline but no ML inference.
The "recognition" stub always returns `AuthOutcome::Failed` after 3 seconds.

### Architecture within the daemon

```
main thread (tokio)
  ├── UnixListener on /run/face-auth/pam.sock
  ├── UnixListener on /run/face-auth/ui.sock
  └── SessionManager (Arc<Mutex<>>)
        └── on incoming auth request:
              if session already active → send AuthResult::Failed (or queue, per config)
              else → spawn AuthSession

AuthSession (tokio task)
  ├── opens camera (CameraHandle — platform backend)
  ├── spawns InferenceThread (std::thread — not tokio, owns ONNX sessions)
  ├── receives FaceMetrics from InferenceThread via std::sync::mpsc
  ├── runs StateMachine (from face-auth-core)
  ├── sends Feedback messages to PAM module socket
  ├── sends UiMessage to UI socket (if connected)
  └── on Authenticating state → sends frame to InferenceThread for recognition
        receives AuthOutcome back
        sends AuthResult to PAM module socket
        closes camera

InferenceThread (std::thread, persistent per session)
  ├── owns OnnxSession handles (not Send — must stay on this thread)
  ├── receives frames via mpsc::Receiver<Frame>
  ├── runs detection → geometry → quality → (later) liveness → recognition
  └── sends results via mpsc::Sender<InferenceResult>

CameraHandle
  ├── opens /dev/video* (IR device, auto-detected or from config)
  ├── flushes first N frames (config: flush_frames)
  ├── captures frames in a dedicated std::thread
  ├── sends Arc<Frame> via bounded mpsc channel (capacity 3)
  └── implements Drop: stops capture thread, closes device
```

### Frame type

```rust
pub struct Frame {
    pub data: Vec<u8>,      // raw pixel data
    pub width: u32,
    pub height: u32,
    pub format: PixelFormat, // GREY, Y800, etc.
    pub timestamp: Instant,
}
```

Frames older than 500ms at the point of inference are discarded (stale frame guard).

### Camera auto-detection algorithm

```rust
fn detect_ir_camera() -> Result<PathBuf, CameraError> {
    let ir_formats = [PixelFormat::GREY, PixelFormat::Y800, PixelFormat::BA81];
    for device in enumerate_v4l2_devices()? {           // /dev/video0, video1, ...
        let formats = query_supported_formats(&device)?;
        if formats.iter().any(|f| ir_formats.contains(f)) {
            return Ok(device);
        }
    }
    Err(CameraError::NoIRCameraFound)
}
```

If config specifies a `device_path`, skip detection and use it directly (with
validation that it exists and is openable).

### Systemd integration

`/etc/systemd/system/face-authd.service`:

```ini
[Unit]
Description=Face Authentication Daemon
After=network.target

[Service]
Type=notify
ExecStart=/usr/libexec/face-authd
RuntimeDirectory=face-auth
RuntimeDirectoryMode=0750
User=root
Group=video

# Hardening
NoNewPrivileges=yes
PrivateTmp=yes
ProtectSystem=strict
ProtectHome=read-only
ReadWritePaths=/run/face-auth /var/log/face-auth
CapabilityBoundingSet=
AmbientCapabilities=

[Install]
WantedBy=multi-user.target
```

Note: daemon runs as root in v1 to simplify camera access, PAM module path reading,
and enrollment data access across users. Dropping to a dedicated `face-auth` user
with appropriate ACLs is a v2 hardening task.

### Structured logging

Using `tracing` crate with `tracing-subscriber` (JSON output to journald).

Security constraints on logging:
- NEVER log: face embeddings, similarity scores, raw pixel data, full frame contents
- OK to log at INFO: session start/end, auth outcome (success/fail only), camera events
- OK to log at DEBUG: state machine transitions, geometry metrics (anonymised),
  inference timing
- DEBUG must be disabled in production config (`level = "info"` default)

**Done criteria:**
- Daemon starts as systemd service
- PAM module connects, sends auth request
- Daemon logs camera open, frame capture, stub inference, session timeout/result
- Camera LED activates on auth request, deactivates on session end
- Two simultaneous auth requests: second is rejected immediately
- `journalctl -u face-authd` shows structured, useful output

---

## Phase 4 — ML Inference Integration (No Recognition Yet)

**Depends on:** Phase 3

This phase integrates SCRFD-500M (detection) and the geometry + quality pipeline.
ArcFace (recognition) and liveness come in Phases 5 and 6. The stub from Phase 3
is replaced by real detection, real geometry, and real state machine feedback.

### InferenceThread pipeline (this phase)

```
Frame received
  │
  ▼
SCRFD-500M detection
  → Vec<Detection> { bbox, landmarks[5], confidence }
  → if empty: FaceMetrics::NoFace → StateMachine → Feedback::Scanning
  → if multiple: take highest confidence detection
  │
  ▼
analyze_geometry(landmarks, frame_size) → FaceMetrics
  │
  ▼
IR quality checks
  - ir_saturated: histogram of face ROI, flag if >15% pixels >250
  - eyes_visible: landmark confidence check for eye keypoints
  - blur: Laplacian variance of 64×64 face ROI crop
  │
  ▼
StateMachine::transition(current_state, metrics, now) → new_state
  │
  ├── if guidance state (debounced): send Feedback to PAM + UI socket
  └── if Authenticating: pass frame to recognition pipeline (stub: sleep 2s, return Failed)
```

### Face alignment (needed for recognition in Phase 5, implement here)

```rust
/// Maps 5 detected landmarks to canonical ArcFace target positions.
/// Returns a 112×112 BGR image suitable for ArcFace input.
pub fn align_face(frame: &Frame, landmarks: &Landmarks) -> AlignedFace {
    // ArcFace canonical target positions (fixed, from ArcFace paper)
    let target: [(f32, f32); 5] = [
        (38.2946, 51.6963),  // left eye
        (73.5318, 51.5014),  // right eye
        (56.0252, 71.7366),  // nose tip
        (41.5493, 92.3655),  // left mouth corner
        (70.7299, 92.2041),  // right mouth corner
    ];
    let transform = compute_similarity_transform(&landmarks.points, &target);
    warp_affine(frame, &transform, 112, 112)
}
```

The similarity transform (scale + rotate + translate, no shear) is computed via
least-squares fitting of the 5 point correspondences using `nalgebra`.

### ONNX model loading strategy

Models are loaded at daemon startup, held in memory for the daemon lifetime.
Controlled by `idle_unload_s` config: if no auth session for N seconds, unload
models and reload on next request (trades latency for memory).

```rust
struct ModelStore {
    detection:   Option<OrtSession>,
    recognition: Option<OrtSession>,
    liveness:    Option<OrtSession>,
}
```

Model files are located via `ModelStoreBackend` trait — default implementation
looks in `/usr/share/face-auth/models/`. Allows distros to put models elsewhere.

**Done criteria:**
- Auth session shows correct real-time guidance: hold phone screen near laptop →
  TooFar if far, TooClose if close, guidance for yaw/pitch if head is turned
- `PAM_TEXT_INFO` messages update in terminal sudo (even if SDDM drops them,
  terminal sudo must show them)
- State machine reaches Authenticating state when face correctly positioned
- Stub auth still fails after Authenticating (expected)
- All state machine unit tests from Phase 1 confirmed against real IR frames

---

## Phase 5 — Face Recognition

**Depends on:** Phase 4

### Enrollment subsystem

**Crate:** `face-enroll` (standalone binary)

Enrollment storage layout:
```
~/.local/share/face-auth/
  └── <username>/
        ├── embeddings.bin    — bincode-serialized Vec<[f32; 512]>, max 20 entries
        └── metadata.toml     — per-embedding timestamps, labels, enrollment date
```

Enrollment process:
1. Run the full geometry + quality + alignment pipeline (same as auth)
2. Require the state machine to reach `Authenticating` for a frame to be accepted
3. Capture 15 such frames (spread across the session — reject frames within 500ms
   of the previous accepted frame to ensure pose variation)
4. Run ArcFace R50 on each aligned frame → 15 embeddings
5. L2-normalise each embedding
6. Store all 15 (not an average — variation is valuable for robustness)
7. Print summary: "Enrolled 15 face models. Face authentication is ready."

**Do not average embeddings at enrollment.** Multiple stored embeddings
capture natural pose/expression variation and improve real-world auth reliability.

### ArcFace R50 inference

Input: 112×112 RGB (note: ArcFace wants RGB, SCRFD wants BGR — document this
explicitly, easy source of bugs)
Output: float[512], L2-normalised

```rust
fn authenticate(
    frame: &AlignedFace,
    stored: &[[f32; 512]],
    threshold: f32,
) -> AuthDecision {
    let embedding = arcface_infer(frame);  // → [f32; 512], L2-normalised
    let max_similarity = stored
        .iter()
        .map(|stored_emb| cosine_similarity(&embedding, stored_emb))
        .fold(f32::NEG_INFINITY, f32::max);

    if max_similarity >= threshold {
        AuthDecision::Accept { similarity: max_similarity }
    } else {
        AuthDecision::Reject { similarity: max_similarity }
    }
}

fn cosine_similarity(a: &[f32; 512], b: &[f32; 512]) -> f32 {
    // Since both are L2-normalised, cosine similarity = dot product
    a.iter().zip(b.iter()).map(|(x, y)| x * y).sum()
}
```

**Frame accumulation for robustness:**
Do not auth on a single frame. Require `frames_required` (default: 3) consecutive
frames all above threshold before emitting `AuthOutcome::Success`. This eliminates
false positives from single-frame noise.

**Threshold guidance** (document in config comments):
```
0.20 = very permissive, higher false-accept rate
0.28 = recommended default
0.35 = strict, may reject valid user in poor conditions
```

**Done criteria:**
- Enroll once, authenticate successfully 10/10 attempts in normal conditions
- Authenticate successfully with slight pose variation (looking slightly left/right)
- Authenticate successfully with glasses on (if enrolled without) — if failure
  rate is high, document "enroll with and without glasses" recommendation
- Photo of enrolled face on a phone screen: should fail (no liveness yet —
  document this as known limitation in README)
- Wrong person: fails reliably
- Similarity scores logged at DEBUG level only

---

## Phase 6 — Liveness Detection

**Depends on:** Phase 5

### MiniFASNet-v2 integration

Input: 80×80 BGR crop of the face (different from ArcFace — needs separate alignment)
Output: float[2] where [0] = spoof probability, [1] = real probability

Liveness check runs **before** ArcFace (fail fast — don't waste recognition compute
on a spoof attempt):

```
geometry passes → liveness check → if spoof: AuthFailed (silent)
                                 → if real:  ArcFace recognition
```

**Security requirement:** Spoof detection failures must be externally
indistinguishable from recognition failures. The PAM module and any UI must show
only `AuthFailed`. The specific reason (spoof vs non-match vs timeout) is logged
at DEBUG internally only.

### Spoof test cases (must pass before phase is done)

1. Printed photo of enrolled face (A4 paper, colour printer): must fail
2. Phone/laptop screen showing a photo of enrolled face: must fail
3. Real enrolled face in normal lighting: must continue to work
4. Real enrolled face in IR-challenging conditions (glasses, beard): must continue
   to work within reasonable tolerance

Note: sophisticated 3D masks are explicitly out of scope for v1. Document this
clearly. MiniFASNet-v2 is trained to reject 2D attacks.

**Done criteria:** Both spoof test cases fail. Real face still auths. No regression
in Phase 5 false rejection rate.

---

## Phase 7 — System Integration & Distribution

**Depends on:** Phase 6

Combines the previously separate Display Manager Integration, Packaging, and Install
phases into a single deliverable. Goal: a user can build, install, enroll, and
authenticate in one documented sequence. Fedora Atomic / rpm-ostree support from day one.

### 7.1 — Makefile

GNU Makefile at repository root. No additional build tool dependencies.

```makefile
# Key targets:
release:        # cargo build --release for face-authd, face-enroll, pam_face
install:        # copy binaries + models + config + systemd + platform files
uninstall:      # reverse of install (delegates to scripts/uninstall.sh)
clean:          # cargo clean
```

Install paths:
- `/usr/libexec/face-authd` — daemon binary
- `/usr/libexec/face-enroll` — enrollment CLI
- `/usr/lib64/security/pam_face.so` — PAM module
- `/usr/share/face-auth/models/` — ONNX models
- `/etc/face-auth/config.toml` — default config (noreplace)
- `/etc/systemd/system/face-authd.service` — systemd unit
- `/usr/share/face-auth/scripts/` — install.sh, uninstall.sh

### 7.2 — PAM configuration

Files to modify (done by `scripts/install.sh` — never manually, never in rpm %post):

`/etc/pam.d/sddm` — prepend `auth sufficient pam_face.so`:
```
auth    sufficient  pam_face.so
auth    substack    password-auth
auth    include     postlogin
...
```

`/etc/pam.d/kscreenlocker`:
```
auth    sufficient  pam_face.so
auth    substack    password-auth
account include     system-auth
password include   system-auth
session include    system-auth
```

Optional (user chooses during install):
- `/etc/pam.d/sudo` — prepend `auth sufficient pam_face.so`
- `/etc/pam.d/polkit-1` — prepend `auth sufficient pam_face.so`

Install script backs up originals before modification and restores on uninstall.

### 7.3 — scripts/install.sh

Interactive installer. Runs after binaries are in place (via `make install` or RPM).

```
Steps:
1. Verify face-authd, face-enroll, pam_face.so are installed
2. Verify ONNX models present
3. Detect display manager (SDDM, GDM, etc.)
4. Add DM user (sddm) to video group (idempotent)
5. Prompt: which PAM files to modify? [sddm, kscreenlocker, sudo, polkit]
6. Backup selected PAM files
7. Prepend pam_face.so line to selected PAM files
8. If SELinux enabled: compile and install face_auth.pp
9. systemctl enable --now face-authd
10. Smoke test: face-enroll --test-camera (capture 1 frame, verify stack)
11. Print: "Run 'face-enroll' to register your face."
```

### 7.4 — scripts/uninstall.sh

```
Steps:
1. Remove pam_face.so lines from all PAM files (restore from backup)
2. semodule -r face_auth (if loaded)
3. systemctl disable --now face-authd
4. Remove sddm from video group (prompt — may have been there before)
5. Do NOT remove enrollment data unless --purge flag
```

### 7.5 — SELinux policy

Already exists at `platform/selinux/face_auth.te`. Install script compiles and
loads it automatically when SELinux is detected as enabled.

### 7.6 — systemd service

Already exists at `platform/systemd/face-authd.service`. Key properties:
- `Type=notify` — daemon signals readiness after models loaded
- `Restart=on-failure` with `RestartSec=2s` — no separate watchdog needed
- Hardened with `NoNewPrivileges`, `ProtectSystem`, `ProtectHome=read-only`
- `SupplementaryGroups=video` — camera access

### 7.7 — Enrollment CLI polish

`face-enroll` improvements:
- Multi-angle capture: prompt user to look straight, left, right, up, down
- Quality gates: skip blurry/saturated/too-small face frames
- Store top-N embeddings (e.g. 5) for better recognition across angles
- `--test-camera` flag: capture 1 frame, print resolution/format, exit
- `--delete` flag: remove enrollment data for a user
- Progress feedback during enrollment

### 7.8 — Default config

Install ships `/etc/face-auth/config.toml` with production defaults:
- session_timeout_s: 15
- recognition threshold: 0.70
- frames_required: 3
- liveness: enabled, lbp_min=5.5, cv_min=0.20, cv_max=0.80
- camera: auto-detect IR device
- log_level: info,ort=warn

### 7.9 — Test matrix

| Scenario | Expected result |
|----------|----------------|
| `make install && sudo scripts/install.sh` | Completes without error |
| Boot → SDDM login screen, show face | Authenticates, desktop loads |
| Lock screen (KScreenLocker), show face | Unlocks |
| Terminal `sudo`, show face (if enabled) | Authorizes |
| Any of above, wrong face / no face | Falls through to password |
| `sudo scripts/uninstall.sh` | System returns to pre-install state |
| SELinux in enforcing mode | No AVC denials |

**Done criteria:** Full install→enroll→auth→uninstall cycle works. No manual
steps beyond running install.sh and face-enroll.

---

## Phase 8 — Debug Visualization UI + Backlog ✓ COMPLETE

### Debug UI (`face-enroll --test-auth --debug` / `face-enroll --debug`)
- 840×360 minifb window (640 camera + 200 sidebar panel)
- Bbox overlay: green=match, blue=liveness pass, red=spoof/below threshold, yellow=detecting
- 5-point landmark dots, color-coded window border by state machine state
- Sidebar: state, FPS, yaw/pitch/roll, blur, IR saturation, LBP entropy, CV, liveness history, similarity
- Raw + CLAHE 112×112 crop thumbnails (what ArcFace sees vs preprocessed)
- Embedded 5×7 bitmap font (no freetype dependency)

### `face-enroll --configure` — TUI config editor
- ratatui + crossterm interactive editor for `/etc/face-auth/config.toml`
- All 20 tunable fields exposed with current value, default, and description
- Validation per field; bool fields toggle; `s` saves + auto-sends SIGHUP to daemon

### Config hot-reload
- Daemon handles SIGHUP: reloads config from disk, atomically swaps `Arc<Config>`
- In-flight sessions unaffected; models reloaded if `execution_provider` changed

### Backlog items completed in Phase 8
- `face-enroll --check-config` — validates full stack
- Enrollment versioning (`ENROLLMENT_VERSION=2`), stale-enrollment warnings
- Enrollment quality scoring and auto-threshold suggestion
- `face-enroll --migrate` — re-embeds on preprocessing change
- systemd sd_notify (`Type=notify`, `READY=1` after models loaded)
- Desktop notifications (opt-in `[notify]` config, notify-send via user D-Bus)
- Idle model unloading (`ModelStore`, `Arc::try_unwrap` safety, default disabled)
- polkit-1 PAM file created by installer from template when absent

---

## Phase 9 — GitHub Project & CI/CD

### Repository polish
- README.md with screenshots, install guide, architecture overview
- LICENSE (MIT or Apache-2.0)
- CONTRIBUTING.md
- Model download script (models too large for git)

### CI/CD
- GitHub Actions: build (Fedora container), test, clippy, fmt
- Tag-triggered releases with binary assets + SHA256SUMS
- Cargo registry + target caching

### Packaging
- RPM spec for COPR (Fedora)
- Fedora Atomic OCI Containerfile
- Future: AUR (Arch), .deb (Debian/Ubuntu), Nix flake, Homebrew (macOS)

---

## Future Improvements (post-v1 backlog)

- **SDDM UI Overlay** — QML component connecting to `/run/face-auth/ui.sock` for
  visual feedback (scanning ring, "Move closer", success animation)
- **KDE lockscreen feedback** — kscreenlocker drops PAM_TEXT_INFO; fix requires custom
  kscreenlocker QML/DBus overlay
- **Platform porting guide** — `docs/platform-porting.md` for GDM, LightDM, COSMIC
- **GDM backend** — GDM display manager support
- **ARM / non-x86 validation** — ONNX and V4L2 are portable but untested on ARM

---

## Dependency Reference

| Crate | Version | Purpose |
|-------|---------|---------|
| `libc` | 0.2 | Raw PAM FFI, V4L2 ioctls, chown |
| `ort` | 2.0.0-rc.12 | ONNX Runtime bindings |
| `tokio` | 1, full features | Async runtime (daemon) |
| `serde` + `bincode` | 1 | IPC serialization + enrollment storage |
| `nalgebra` | ~0.33 | Similarity transform (face alignment) |
| `image` | ~0.25 | Pixel format utilities |
| `ndarray` | ~0.17 | Tensor construction for ONNX |
| `tracing` + `tracing-subscriber` | 0.1 / 0.3 | Structured logging (JSON + env-filter) |
| `config` | ~0.14 | TOML config loading |
| `rand` | ~0.8 | Session ID generation |
| `thiserror` | 1 | Error type derivation |
| `v4l` | 0.14 | V4L2 camera capture (face-enroll only) |
| `minifb` | 0.28 | Debug visualization window |
| `ratatui` + `crossterm` | 0.29 / 0.28 | TUI config editor (`--configure`) |
| `toml` | 0.8 | Config serialization (writing config.toml) |

V4L2 camera access in the daemon uses raw `libc` ioctl calls (not the `v4l` crate — needed for
IR emitter UVC extension unit control which the crate doesn't expose).

ONNX models (not in git — downloaded separately):

| Model | File | Size | License |
|-------|------|------|---------|
| SCRFD-500M | det_500m.onnx | 2.4 MB | MIT (insightface) |
| MobileFaceNet w600k | w600k_mbf.onnx | 13 MB | MIT (insightface) |
| MiniFASNetV2-SE | antispoof_q.onnx | 0.6 MB | Apache 2.0 (minivision-ai) |

---

## Security Properties (for README)

- Face data (embeddings) stored locally in user home directory only (`~/.local/share/face-auth/`)
- No network access — daemon has no network socket
- Falls through to password on any failure — never weakens password auth
- `sufficient` PAM flag: face OR password, not face AND password
- IR texture liveness rejects 2D photo/screen spoofs (LBP entropy + local contrast CV + temporal stability)
- 3D mask spoofs: explicitly out of scope for v1
- Similarity scores logged at info level for diagnostics (configurable via log level)
- Spoof vs non-match failure reason never surfaced to PAM client
- Models are open-source and locally run — no cloud API
- CLAHE preprocessing normalizes lighting — same embedding quality in dark/bright rooms

---

## What Is Explicitly Out of Scope (v1)

These are documented non-goals to prevent scope creep:

- Fingerprint sensor support (separate PAM stack — different project)
- Wayland compositor protocol for biometric auth (no standard exists yet)
- Full kernel biometric subsystem (kernel 7.0 Rust opens this door but it's
  a multi-year upstream project)
- Multi-user simultaneous auth (one session at a time)
- ARM / non-x86 architecture support (ONNX models and V4L2 are portable but
  not tested)
- 3D mask anti-spoofing
- GUI enrollment tool (CLI only in v1)
- GNOME Shell extension for GDM overlay (v2, requires GDM backend first)

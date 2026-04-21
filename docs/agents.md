# face-auth — Project Context

## Hardware: IdeaPad Pro 5 (Fedora Atomic, AMD Krackan)

- OS: Fedora 43 Atomic (rpm-ostree / ostree-based)
- Kernel: 6.19.7-200.fc43.x86_64
- Display manager: SDDM (KDE)
- Shell: zsh

## Camera Hardware

| Device | Format | Name | Role |
|--------|--------|------|------|
| /dev/video0 | virtual | OBS Virtual Camera | not relevant |
| /dev/video1 | MJPG, YUYV | Integrated RGB Camera | RGB webcam |
| /dev/video2 | (metadata) | Integrated RGB Camera | ctrl node |
| /dev/video3 | **GREY** | **IR Camera** | **target device** |
| /dev/video4 | (metadata) | Integrated RGB Camera | ctrl node |

- IR camera: `/dev/video3`, GREY format, 640×360, Luxvisions 30c9:00ec
- IR emitter: controlled via UVC extension unit (unit=7, selector=6)
- Config: `/etc/face-auth/ir-emitter.toml`
- Camera power ACPI: `/sys/bus/platform/devices/VPC2004:00/camera_power`
- Recovery: `echo 0 > camera_power && echo 1 > camera_power`, then reload uvcvideo

## Architecture Overview

```
PAM module (pam_face.so)
  ↕ Unix socket (/run/face-auth/pam.sock)
face-authd (daemon, systemd Type=notify service)
  ├── LiveConfig: Arc<RwLock<Arc<Config>>> — hot-swappable on SIGHUP
  ├── ModelStore: Option<Arc<ModelCache>> — idle-unloadable, reloads on demand
  ├── Camera thread (V4L2 capture, IR emitter control via UVC XU)
  ├── Inference thread (SCRFD → geometry → IR liveness → alignment → ArcFace)
  └── Session manager (one auth at a time, touch ModelStore on session end)

face-enroll (CLI tool)
  ├── Enrollment (multi-angle, quality-gated, CLAHE, auto-threshold suggestion)
  ├── --test-auth [--debug]  (test against daemon, optional minifb debug window)
  ├── --configure            (ratatui TUI editor, saves + SIGHUP daemon)
  ├── --check-config         (validates full stack)
  ├── --status               (enrollment quality grade + version)
  ├── --migrate              (re-embeds on ENROLLMENT_VERSION change)
  ├── --install / --uninstall (PAM + systemd + SELinux + polkit-1)
  └── --test-camera, --delete
```

## ML Pipeline

1. **SCRFD-500M** (2.4MB) — face detection, 5-point landmarks
2. **Geometry analysis** — distance, yaw, pitch, roll from landmarks
3. **IR quality** — saturation check, blur score (Laplacian variance)
4. **IR texture liveness** — LBP entropy + local contrast CV (rejects phone screens)
5. **Temporal liveness stability** — ≥80% pass rate in rolling window
6. **Face alignment** — 5-point similarity transform to 112×112 canonical crop
7. **CLAHE preprocessing** — contrast-limited adaptive histogram equalization
8. **ArcFace MobileFaceNet w600k** (13MB) — 512-dim L2-normalized embedding
9. **Cosine similarity matching** — against stored enrollment embeddings

## Performance (as of Phase 7)

| Metric | Value |
|--------|-------|
| Auth time (best) | 0.4s |
| Auth time (typical) | 1.3s |
| Similarity (real face) | ~0.87 |
| Phone screen | rejected (liveness + low similarity ~0.42) |
| Session timeout | 7s |
| Models pre-loaded | yes (daemon startup) |

## Key Design Decisions

- PAM module: raw C FFI (not `pam` crate — didn't compile on Fedora 43)
- IPC: 4-byte LE length prefix + bincode, sync via spawn_blocking
- Camera: std::thread, not tokio. Frames via mpsc::sync_channel(3)
- Inference: std::thread, models behind Mutex for cross-session reuse
- Drop ordering: camera drops BEFORE inference (avoids deadlock)
- Result sent before cleanup (saves ~360ms user-perceived latency)
- IR liveness: texture analysis, not ML (RGB-trained models don't work on IR)
- CLAHE on aligned face before ArcFace (lighting invariance)
- High-confidence shortcut: sim ≥ threshold+0.10 → accept on 1 frame

## PAM Configuration

- KDE lock screen: `/etc/pam.d/kde` (NOT `/etc/pam.d/sddm`)
- sddm must be in `video` group: `sudo usermod -aG video sddm`
- PAM line: `auth sufficient pam_face.so` (prepended)
- install.sh scans ALL /etc/pam.d/* services with smart defaults

## CLI Commands

| Command | Description |
|---------|-------------|
| `face-enroll` | Enroll face (multi-angle, quality-gated, with auto-threshold suggestion) |
| `face-enroll --test-auth` | Run one auth attempt against live daemon |
| `face-enroll --test-auth --debug` | Auth with live debug window (bbox, landmarks, similarity, crops) |
| `face-enroll --debug` | Enrollment with debug window |
| `face-enroll --configure` | Interactive TUI config editor (requires root to save) |
| `face-enroll --check-config` | Validate config, models, camera, enrollment, daemon |
| `face-enroll --status` | Show enrollment status + quality grade |
| `face-enroll --migrate` | Re-embed stored faces after preprocessing change |
| `face-enroll --delete` | Remove enrollment data for current user |
| `face-enroll --install` | Configure PAM, video group, systemd, SELinux (requires root) |
| `face-enroll --uninstall` | Remove PAM config and restore backups (requires root) |
| `face-enroll --test-camera` | Capture one frame and print camera info |

## Project Phases

| Phase | Status | Description |
|-------|--------|-------------|
| 0 | Done | Prerequisites (PAM, ONNX, V4L2, SDDM) |
| 1 | Done | face-auth-core (protocol, config, geometry) |
| 2 | Done | pam-face (PAM module) |
| 3 | Done | face-authd (daemon, camera pipeline) |
| 4 | Done | ML inference (SCRFD detection, geometry feedback) |
| 5 | Done | Face recognition (ArcFace, enrollment) |
| 6 | Done | Anti-spoofing (IR liveness, temporal stability) |
| 7 | Done | System integration (install, performance, CLAHE) |
| 8 | Done | Debug UI, TUI config editor, config hot-reload, backlog items |
| 9 | Next | GitHub project + CI/CD + packaging |

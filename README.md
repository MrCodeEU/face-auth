# face-auth

IR-based face authentication for Linux. Authenticates via PAM using your laptop's
built-in IR camera — the same hardware used by Windows Hello.

Works with KDE Plasma lock screen, `sudo`, `polkit`, and any PAM-integrated service.

## Features

- **IR camera only** — uses the dedicated IR sensor, not the RGB webcam
- **Anti-spoofing** — rejects phone screens via IR texture liveness analysis
- **Fast** — 0.4–1.3s typical auth time; models stay loaded for instant sudo
- **PAM integration** — drop-in for any `/etc/pam.d/` service
- **Live feedback** — tells you to move closer, tilt less, look at camera
- **CLAHE preprocessing** — robust to varying IR illumination
- **Hot-reload config** — `SIGHUP` the daemon to apply config changes live

## Hardware Requirements

- IR camera (tested: Luxvisions 30c9:00ec on IdeaPad Pro 5)
- Linux with V4L2 support
- The IR camera must report `GREY` pixel format

Check your IR camera:
```bash
v4l2-ctl --list-formats-ext -d /dev/video3
```
Look for a device with `GREY` format at 640×360 or similar.

## Install

### From release binaries

```bash
# Download the latest release
curl -Lo face-auth.tar.gz https://github.com/MrCodeEU/face-auth/releases/latest/download/face-auth-x86_64.tar.gz
tar xf face-auth.tar.gz
cd face-auth

# Install models
bash scripts/download-models.sh

# Install system-wide (PAM, systemd, SELinux, polkit)
sudo ./face-enroll --install
```

### From source

```bash
git clone https://github.com/MrCodeEU/face-auth
cd face-auth
bash scripts/download-models.sh
make build
sudo make install
sudo ./target/release/face-enroll --install
```

### Enroll your face

```bash
face-enroll
```

Follow the on-screen prompts. Hold still, look straight at the camera. The tool
captures multiple angles and shows a quality grade (A/B/C/F) when done.

## Usage

```
face-enroll                    Enroll face (multi-angle, quality-gated)
face-enroll --test-auth        Test auth against live daemon
face-enroll --test-auth --debug  Auth with debug overlay window
face-enroll --configure        Interactive TUI config editor
face-enroll --check-config     Validate full system stack
face-enroll --status           Show enrollment quality grade
face-enroll --migrate          Re-embed faces after pipeline change
face-enroll --delete           Remove enrollment for current user
face-enroll --install          Set up PAM, systemd, SELinux (root)
face-enroll --uninstall        Remove PAM config and restore backups
face-enroll --test-camera      Capture one frame and print camera info
```

## Architecture

```
PAM module (pam_face.so)
  ↕ Unix socket (/run/face-auth/pam.sock)
face-authd (daemon, systemd Type=notify)
  ├── LiveConfig: Arc<RwLock<Arc<Config>>>  hot-swappable on SIGHUP
  ├── ModelStore: Option<Arc<ModelCache>>   idle-unloadable, reloads on demand
  ├── Camera thread    V4L2 capture + IR emitter control (UVC XU)
  ├── Inference thread SCRFD → geometry → IR liveness → alignment → ArcFace
  └── Session manager  one auth at a time
```

### ML Pipeline

1. **SCRFD-500M** (2.4 MB) — face detection, 5-point landmarks
2. **Geometry** — distance, yaw, pitch, roll from landmarks → live feedback
3. **IR quality** — saturation check, blur score (Laplacian variance)
4. **IR liveness** — LBP entropy + local contrast CV (rejects phone screens)
5. **Temporal stability** — ≥80% pass rate in rolling window
6. **Face alignment** — 5-point similarity transform to 112×112 canonical crop
7. **CLAHE** — contrast-limited adaptive histogram equalization
8. **ArcFace MobileFaceNet w600k** (13 MB) — 512-dim L2-normalized embedding
9. **Cosine similarity** — matched against stored enrollment embeddings

## Configuration

Config lives at `/etc/face-auth/config.toml`. Edit interactively:

```bash
sudo face-enroll --configure
```

Or edit directly and reload:

```bash
sudo systemctl kill --signal=SIGHUP face-authd
```

Key settings:

| Key | Default | Description |
|-----|---------|-------------|
| `recognition.threshold` | 0.72 | Cosine similarity accept threshold |
| `daemon.session_timeout_s` | 7 | Max seconds per auth attempt |
| `daemon.idle_unload_s` | 0 | Seconds idle before unloading models (0 = never) |
| `notify.enabled` | false | Desktop notification on successful auth |

## PAM Setup

The installer (`--install`) configures PAM automatically. For manual setup:

```
# /etc/pam.d/kde  (KDE lock screen)
auth sufficient pam_face.so
```

```bash
# sddm needs camera access
sudo usermod -aG video sddm
```

## Troubleshooting

**Camera broken after suspend:**
```bash
echo 0 | sudo tee /sys/bus/platform/devices/VPC2004:00/camera_power
echo 1 | sudo tee /sys/bus/platform/devices/VPC2004:00/camera_power
sudo modprobe -r uvcvideo && sudo modprobe uvcvideo
```

**Validate full stack:**
```bash
sudo face-enroll --check-config
```

**Daemon logs:**
```bash
journalctl -u face-authd -f
```

## License

Apache-2.0 — see [LICENSE](LICENSE).

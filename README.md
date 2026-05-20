# face-auth

IR-based face authentication for Linux via PAM — uses your laptop's built-in IR camera (the same hardware as Windows Hello).

Works with KDE Plasma lock screen, `sudo`, `polkit`, and any PAM-integrated service.

---

## ⚠️ Disclaimer

> **This project is heavily AI-assisted.** The code, architecture, and documentation were developed with significant AI assistance (Claude). Review it critically before deploying on any security-sensitive system.
>
> **Tested hardware:** Lenovo IdeaPad Pro 5 16AKP10 (IR camera: Luxvisions `30c9:00ec`)
>
> **Tested OS:** [Aurora-DX](https://getaurora.dev/) (Fedora Atomic / ostree-based). Installation is primarily designed and tested on **Fedora Atomic** (immutable `/usr`) but should work on traditional Fedora and other RPM-based distros with minor path differences.
>
> **Use at your own risk.** This is not a hardened security product. Do not rely on it as a sole authentication factor for sensitive systems.

---

## Features

- **IR camera only** — dedicated IR sensor, not the RGB webcam
- **Anti-spoofing** — rejects phone/screen attacks via IR texture liveness analysis
- **Fast** — 0.4–1.3 s typical auth time; models stay loaded for instant `sudo`
- **PAM integration** — drop-in for any `/etc/pam.d/` service
- **Live feedback** — tells you to move closer, tilt less, look at camera
- **Multi-condition enrollment** — `--append` to add dark/glasses/etc. variants
- **Hot-reload config** — `SIGHUP` daemon to apply config changes live
- **GUI + CLI** — egui graphical tool (`face-auth-gui`) and terminal tool (`face-enroll`)

---

## Hardware Requirements

- Laptop with a dedicated IR camera (V4L2, `GREY` pixel format)
- Linux kernel with V4L2 and UVC support
- Tested on: Lenovo IdeaPad Pro 5 16AKP10

### Find your IR camera

IR cameras typically appear as a separate `/dev/videoN` device alongside the RGB webcam. To find it:

```bash
for d in /dev/video*; do
    echo -n "$d: "
    v4l2-ctl --list-formats-ext -d "$d" 2>/dev/null | grep -o "GREY\|YUYV\|MJPEG" | head -1
done
```

Look for a device reporting `GREY` format. Then verify:

```bash
v4l2-ctl --list-formats-ext -d /dev/video3
# Should show: GREY 640x360 or similar
```

Also check the USB ID to confirm it's the IR sensor (not the RGB camera):

```bash
v4l2-ctl --info -d /dev/video3 | grep -i "bus\|card"
# Or: lsusb | grep -i "camera\|luxvision\|chicony"
```

---

## Installation

### 1. Prerequisites

**Install Rust:**
```bash
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
source ~/.cargo/env
```

**Install system dependencies (Fedora/Aurora):**
```bash
sudo rpm-ostree install \
    gcc clang cmake pkg-config \
    pam-devel libv4l-devel openssl-devel \
    fontconfig-devel dbus-devel systemd-devel \
    selinux-policy-devel checkpolicy \
    policycoreutils policycoreutils-python-utils
# Reboot after rpm-ostree install on Atomic systems
```

### 2. Clone and build

```bash
git clone https://github.com/MrCodeEU/face-auth
cd face-auth
```

**Download ML models** (~15 MB total):
```bash
bash scripts/download-models.sh
```

**Build release binaries** (no sudo):
```bash
make release
```

### 3. Install system-wide

```bash
sudo make install
```

This installs:
- Binaries → `/var/lib/face-auth/bin/` (Atomic) or `/usr/libexec/` (traditional)
- PAM module → `/var/lib/face-auth/pam_face.so` (Atomic) or `/usr/lib64/security/`
- Config → `/etc/face-auth/config.toml`
- Systemd service → `/etc/systemd/system/face-authd.service`
- Desktop entry + icon (app launcher)
- Models → `/var/lib/face-auth/models/`

### 4. Run the post-install script

This configures PAM, SELinux, systemd, and adds `sddm` to the `video` group:

```bash
sudo /var/lib/face-auth/scripts/install.sh
# Traditional systems: sudo /usr/share/face-auth/scripts/install.sh
```

Follow the prompts. It will ask which PAM services to enable (lock screen, sudo, etc.).

### 5. Start the daemon

```bash
sudo systemctl enable --now face-authd
systemctl status face-authd
```

### 6. Verify the install

```bash
sudo face-enroll --check-config
# Or via GUI: face-auth-gui → "Check Config" tab
```

---

## IR Camera Configuration

The most important step is pointing face-auth at the correct camera device and handling any hardware quirks.

### Set the camera device path

Edit `/etc/face-auth/config.toml`:

```toml
[camera]
device_path = "/dev/video3"   # Change to your IR camera device
```

Or use the GUI: **face-auth-gui → Configure → Camera → Device path**

After changing, reload:
```bash
sudo systemctl kill --signal=SIGHUP face-authd
```

### Sensor crop (hardware artifact fix)

Some IR cameras (including the Luxvisions on IdeaPad Pro 5) produce a bright artifact column on the right side of the frame that causes false face detections. Fix with:

```toml
[camera]
crop_right_fraction = 0.65   # Keep only the left 65% of the frame
```

Adjust until the artifact disappears — use **face-auth-gui → Test Camera** to preview.

### IR emitter (active illumination)

If your camera has a separate IR emitter (UVC extension unit), create `/etc/face-auth/ir-emitter.toml`:

```toml
unit = 7        # UVC extension unit ID
selector = 6    # Control selector for emitter power
# On IdeaPad Pro 5 16AKP10: unit=7, selector=6
```

To find the right values for your hardware:
```bash
# List UVC extension units
v4l2-ctl -d /dev/video3 --list-ctrls-menu
# Or check dmesg after plugging camera
dmesg | grep -i "uvc\|extension"
```

### Camera stays on after suspend

If the IR camera stops working after suspend/resume:

```bash
# Reset via ACPI camera power toggle
echo 0 | sudo tee /sys/bus/platform/devices/VPC2004:00/camera_power
echo 1 | sudo tee /sys/bus/platform/devices/VPC2004:00/camera_power
sudo modprobe -r uvcvideo && sudo modprobe uvcvideo
```

---

## Getting Started: Enroll and Test

### Option A — GUI (recommended)

Launch the GUI:
```bash
sudo face-auth-gui
# Or from your app launcher: search "Face Auth"
# Note: sudo needed for enrollment (writes to /var/lib/face-auth/enrollments/)
```

**Step-by-step:**

1. **Check Config tab** — verify camera, daemon, models, PAM are all green
2. **Test Camera tab** — confirm live IR feed appears (your face should be visible)
3. **Enroll tab** — follow the wizard:
   - Look straight ahead, then slightly left/right/up/down
   - 5 poses × 3 embeddings = 15 captures total
   - Quality review shown at end (aim for grade A/B)
   - Click **Save**
4. **Test Auth tab** — click **Start Auth Test**, look at the camera
   - Shows live camera feed with bounding box and landmark overlays
   - Green box = liveness pass, blue = detecting, yellow = adjusting
   - Similarity score shown in real time
5. Lock screen / `sudo` should now use face auth

### Option B — Terminal (CLI)

**Step 1: Verify everything is working**
```bash
sudo face-enroll --check-config
```

**Step 2: Test the camera**
```bash
face-enroll --test-camera
```

**Step 3: Enroll your face**
```bash
sudo face-enroll
```
Follow the on-screen prompts. Multi-angle wizard captures 5 poses.

**Step 4: Test authentication**
```bash
# Basic test (connects to live daemon)
face-enroll --test-auth

# Visual debug mode (recommended — shows camera feed + overlays in a window)
face-enroll --test-auth --debug
```

The debug overlay shows:
- Bounding box (yellow=scanning, blue=liveness OK, green=match)
- Landmark dots (eyes, nose, mouth corners)
- Yaw/pitch/roll angles, blur score, IR saturation
- LBP entropy + contrast CV (liveness scores)
- Cosine similarity against enrolled embeddings

---

## Multi-Condition Enrollment

If face auth fails under certain conditions (low light, glasses, beard), add more embeddings:

```bash
# Add dark/night condition embeddings (keep existing, add new)
sudo face-enroll --append

# GUI: use the "Re-enroll" tab — same wizard but appends
```

Auth uses max cosine similarity across all stored embeddings, so adding more conditions only improves coverage.

---

## Configuration Reference

Config at `/etc/face-auth/config.toml`. Edit interactively:

```bash
sudo face-enroll --configure     # TUI editor
# Or: face-auth-gui → Configure tab (GUI editor with Save button)
```

Reload without restart:
```bash
sudo systemctl kill --signal=SIGHUP face-authd
```

### Key settings

| Section | Key | Default | Description |
|---------|-----|---------|-------------|
| `recognition` | `threshold` | `0.70` | Cosine similarity accept threshold. Lower = more permissive, higher = stricter. Suggested value shown after enrollment. |
| `recognition` | `frames_required` | `2` | Consecutive matching frames before auth succeeds |
| `recognition` | `max_enrollment` | `20` | Max stored embeddings per user |
| `daemon` | `session_timeout_s` | `7` | Max seconds per auth attempt |
| `daemon` | `idle_unload_s` | `0` | Unload models after N seconds idle (0 = keep loaded) |
| `daemon` | `execution_provider` | `"cpu"` | ONNX Runtime backend: `cpu`, `rocm`, `cuda`, `xdna` |
| `camera` | `device_path` | `""` | V4L2 device, e.g. `/dev/video3` |
| `camera` | `crop_right_fraction` | `1.0` | Fraction of frame width to use from left (0.65 = crop right 35%) |
| `liveness` | `enabled` | `true` | Enable IR texture liveness check |
| `liveness` | `lbp_entropy_min` | `5.5` | Min LBP entropy (real skin ~6.0–6.2, screens 0.4–5.5) |
| `liveness` | `local_contrast_cv_min` | `0.20` | Min local contrast CV |
| `liveness` | `local_contrast_cv_max` | `0.80` | Max local contrast CV |
| `notify` | `enabled` | `false` | Desktop notification on successful auth |
| `geometry` | `distance_min/max` | `0.06/0.55` | Face size ratio bounds (face width / frame width) |
| `geometry` | `yaw_max_deg` | `45` | Max horizontal turn before "turn left/right" feedback |

---

## PAM Setup

The installer (`--install`) handles this automatically. For manual setup:

```bash
# /etc/pam.d/kde  (KDE lock screen)
auth sufficient pam_face.so

# /etc/pam.d/sudo  (sudo)
auth sufficient pam_face.so
auth sufficient pam_unix.so try_first_pass
```

On Atomic systems the PAM module is not in the standard path, so the line needs the full path:
```
auth sufficient /var/lib/face-auth/pam_face.so
```

SDDM needs camera access:
```bash
sudo usermod -aG video sddm
# Then reboot (group change requires new session)
```

---

## Enrollment Management

```bash
face-enroll --status           # Show enrollment quality grade + embedding count
face-enroll --migrate          # Re-embed stored faces after pipeline/model change
face-enroll --delete           # Remove enrollment for current user
sudo face-enroll --delete --user alice   # Remove another user's enrollment (root)
```

---

## Debugging

### Debug auth with visual overlay

```bash
face-enroll --test-auth --debug
```

Opens a 840×360 window with live IR feed, bbox, landmarks, and all metrics.

### Check full system stack

```bash
sudo face-enroll --check-config
```

Checks: camera open, models load, daemon socket, PAM config, SELinux labels, enrollment exists.

### Daemon logs

```bash
journalctl -u face-authd -f
journalctl -u face-authd --since "5 min ago"
```

### Increase log verbosity

```toml
# /etc/face-auth/config.toml
[logging]
level = "debug"   # error / warn / info / debug / trace
```
Then: `sudo systemctl kill --signal=SIGHUP face-authd`

### Daemon not starting

```bash
systemctl status face-authd
journalctl -u face-authd -n 50

# Common causes:
# - Camera device_path not set or wrong device
# - Models missing (run scripts/download-models.sh)
# - SELinux blocking (check: ausearch -m avc -ts recent)
```

### Test PAM directly

```bash
# Trigger a sudo — watch daemon logs in another terminal
sudo -k && sudo whoami
```

---

## Architecture

```
face-auth-gui (optional egui GUI)
  └── local inference pipeline (enrollment, test auth, config)

PAM module (pam_face.so)
  ↕ Unix socket (/run/face-auth/pam.sock)
face-authd (daemon, systemd Type=notify)
  ├── LiveConfig: Arc<RwLock<Arc<Config>>>   hot-swappable on SIGHUP
  ├── ModelStore: Option<Arc<ModelCache>>    idle-unloadable, reloads on demand
  ├── Camera thread    V4L2 capture + IR emitter control (UVC XU)
  ├── Inference thread SCRFD → geometry → IR liveness → alignment → ArcFace
  └── Session manager  one auth at a time, 7s timeout

face-enroll (CLI enrollment + utilities)
```

### ML Pipeline

1. **SCRFD-500M** — face detection, 5-point landmarks
2. **Geometry** — distance, yaw, pitch, roll → live guidance feedback
3. **IR quality** — saturation check, Laplacian blur score
4. **IR liveness** — LBP entropy + local contrast CV (rejects phone screens)
5. **Temporal stability** — ≥80% pass rate in rolling 10-frame window
6. **Face alignment** — 5-point similarity transform → 112×112 canonical crop
7. **CLAHE** — contrast-limited adaptive histogram equalization
8. **ArcFace MobileFaceNet w600k** — 512-dim L2-normalized embedding
9. **Cosine similarity** — max over all stored enrollment embeddings

---

## Uninstall

```bash
sudo /var/lib/face-auth/scripts/uninstall.sh
# Or: sudo make uninstall
```

This removes PAM entries, the systemd service, and optionally the binaries and enrollment data.

---

## License

Apache-2.0 — see [LICENSE](LICENSE).

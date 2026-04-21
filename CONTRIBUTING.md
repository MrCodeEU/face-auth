# Contributing to face-auth

## Prerequisites

### System packages (Fedora)
```bash
sudo dnf install pam-devel v4l-utils clang lld cmake
```

### ONNX Runtime
The build uses `ort` with `ORT_STRATEGY=download` — it fetches the native library automatically.
No manual ONNX Runtime installation needed.

### Models
Models are not in git (too large). Download before running:
```bash
bash scripts/download-models.sh
```
Alternatively place these files in `models/`:
- `det_500m.onnx` — SCRFD-500M face detector (~2.4 MB)
- `w600k_mbf.onnx` — ArcFace MobileFaceNet (~13 MB)

## Build

```bash
# Debug build
cargo build

# Release build
make build

# Run tests
cargo test

# Lint
cargo clippy -- -D warnings

# Format check
cargo fmt --check
```

## Project Structure

```
crates/
  face-auth-core/     — protocol types, config schema, geometry analysis
  face-auth-models/   — ONNX inference (SCRFD detection, ArcFace embeddings)
  face-auth-platform/ — platform-specific helpers
  face-authd/         — daemon (camera, inference, session, PAM IPC)
  face-enroll/        — CLI tool (enroll, test, configure, install)
  pam-face/           — PAM module (C FFI)
tests/phase0/         — integration tests
docs/                 — architecture docs, decisions log
platform/             — default config, service files, SELinux policy
scripts/              — model download, build helpers
packaging/            — RPM spec, OCI Containerfile
```

## Code Style

- Rust edition 2021, stable toolchain
- `cargo fmt` before committing
- `cargo clippy -- -D warnings` must pass
- No `unwrap()` on fallible paths in daemon code — propagate errors
- Tracing macros for logging (`tracing::{info, warn, error, debug}`)
- Prefer `thiserror` error types over `anyhow` in library crates

## Pull Requests

- One logical change per PR
- Describe *why*, not just *what*
- Tests for new logic where feasible (see `crates/face-auth-core/src/lib.rs` for examples)
- Run `cargo test && cargo clippy -- -D warnings && cargo fmt --check` locally before pushing

## Camera / Hardware Testing

Real hardware tests require:
- `/dev/video3` IR camera (Luxvisions 30c9:00ec or compatible)
- Root access for IR emitter UVC extension unit control

Use `face-enroll --test-camera` to verify camera access before running integration tests.

## Daemon Development

```bash
# Build and run daemon (requires root for PAM socket)
sudo cargo run -p face-authd

# In another terminal — test PAM auth
cargo run -p phase0 --bin test-pam-phase2

# Hot-reload config after changes
sudo systemctl kill --signal=SIGHUP face-authd
```

## Reporting Issues

Open an issue at https://github.com/MrCodeEU/face-auth/issues.
Include: OS version, kernel version, camera model, and relevant daemon logs (`journalctl -u face-authd`).

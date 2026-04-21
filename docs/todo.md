# face-auth TODO

## Completed Phases

- [x] **Phase 0** — Prerequisites (PAM, ONNX, V4L2, SDDM tests)
- [x] **Phase 1** — face-auth-core (protocol, config, geometry, framing, 25 tests)
- [x] **Phase 2** — pam-face (PAM module with raw C FFI, all 4 test scenarios pass)
- [x] **Phase 3** — face-authd (daemon skeleton, camera pipeline, IPC, session management)
- [x] **Phase 4** — ML Inference (SCRFD-500M detection, geometry feedback, IR quality checks)
- [x] **Phase 5** — Face Recognition (alignment, ArcFace embedding, enrollment, matching ~0.865 similarity)
- [x] **Phase 6** — Anti-Spoofing (IR texture liveness, temporal stability, phone rejection)
- [x] **Phase 7** — System Integration & Distribution
- [x] **Phase 8** — Debug Visualization UI + Backlog

### Phase 7 Summary
- Makefile, install/uninstall scripts, face-enroll --install/--uninstall
- GPU/NPU execution providers (ROCm, CUDA, OpenVINO, XDNA/Vitis)
- CLAHE preprocessing for lighting-invariant embeddings
- Performance: 0.4–1.3s auth (send-before-cleanup, flush=0, liveness warmup=1, high-confidence shortcut)
- Diagnostic logging (1s interval, similarity range, liveness, match count)
- KDE lock screen auth, all PAM services scannable

### Phase 8 Summary
- **Debug UI** (`face-enroll --test-auth --debug` / `face-enroll --debug`): 840×360 minifb window, bbox/landmark overlays, similarity/liveness/FPS panel, raw + CLAHE 112×112 crop thumbnails, embedded 5×7 bitmap font
- **`face-enroll --configure`**: ratatui TUI config editor, all 20 tunable fields with validation, `s` to save + auto SIGHUP reload of daemon
- **Config hot-reload**: daemon handles SIGHUP — reloads `/etc/face-auth/config.toml` live, reloads models if `execution_provider` changed, zero session disruption
- **`face-enroll --check-config`**: validates config file, models, camera, IR emitter, enrollment version, daemon socket, PAM module, threshold sanity
- **Enrollment versioning**: `ENROLLMENT_VERSION=2` field in embeddings.bin, stale-enrollment warning on auth
- **Enrollment quality scoring**: pairwise cosine similarity grade (A/B/C/F) and outlier detection on `face-enroll --status`
- **Auto-threshold suggestion**: computed from enrollment similarity distribution during `face-enroll`
- **Enrollment migration** (`face-enroll --migrate`): re-embeds stored faces when preprocessing pipeline changes
- **systemd sd_notify**: daemon sends `READY=1` after models loaded (`Type=notify`)
- **Desktop notifications**: opt-in `[notify]` config, `notify-send` via user's D-Bus session after successful auth
- **Idle model unloading**: `idle_unload_s` config (default 0 = disabled), `ModelStore` wrapper with `Arc::try_unwrap` safety
- **polkit-1 PAM**: installer creates `/etc/pam.d/polkit-1` from template when file is absent

---

## Phase 9 — GitHub Project & CI/CD

### 9.1 — Repository polish
- [ ] README.md (project overview, demo GIF/screenshot, install instructions, architecture diagram)
- [ ] LICENSE (MIT or Apache-2.0)
- [ ] CONTRIBUTING.md (build instructions, PR guidelines, code style)
- [ ] .gitignore (models/, target/, *.onnx, ir-emitter.toml)
- [ ] Model download script (`scripts/download-models.sh` — models too large for git)

### 9.2 — GitHub Actions CI
- [ ] Build matrix: Fedora 43 container
- [ ] `cargo test` (all crates)
- [ ] `cargo clippy -- -D warnings`
- [ ] `cargo fmt --check`
- [ ] Build release binaries (x86_64)
- [ ] Cache cargo registry + target dir

### 9.3 — Release automation
- [ ] Tag-triggered workflow: `v*` tags → build release → create GitHub Release
- [ ] Attach release binaries + PAM module as assets
- [ ] Generate changelog from commit history
- [ ] Checksum file (SHA256SUMS)

### 9.4 — Packaging
- [ ] RPM spec (`packaging/face-auth.spec`)
- [ ] COPR build configuration
- [ ] Fedora Atomic OCI Containerfile (for baking into custom images)
- [ ] AUR package (Arch Linux) — future
- [ ] Homebrew formula (macOS — would need CoreMediaIO camera backend) — future
- [ ] Debian/Ubuntu .deb packaging — future
- [ ] Nix flake — future

---

## Future Improvements (backlog)

| Feature | Difficulty | Priority |
|---------|-----------|----------|
| SDDM UI Overlay (QML feedback via ui.sock) | High | Medium |
| Platform porting guide (GDM, COSMIC, LightDM) | Low | Low |
| Multi-angle enrollment progress UI (--debug mode) | Low | Low |
| GDM backend support | Medium | Low |
| ARM / non-x86 architecture validation | Low | Low |

## Notes

- sddm must be in `video` group for camera access: `sudo usermod -aG video sddm`
- IR emitter config: `ir-emitter.toml` (unit=7, sel=6) — installed to /etc/face-auth/
- KDE lock screen uses /etc/pam.d/kde, NOT /etc/pam.d/sddm
- polkit uses /etc/pam.d/polkit-1 — installer creates it if absent
- Camera recovery if broken: see ACPI reset in plan.md
- Auth performance: 0.4s best, 1.3s typical, hardware floor ~0.3s (IR emitter settle)
- Config hot-reload: `sudo systemctl kill --signal=SIGHUP face-authd` (or happens automatically via --configure save)

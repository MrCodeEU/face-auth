# face-auth — GNU Makefile
# Usage:
#   make release      Build all binaries in release mode
#   make install      Install to system paths (requires root)
#   make uninstall    Remove from system (delegates to scripts/uninstall.sh)
#   make clean        cargo clean
#
# On Fedora Atomic (/usr is read-only), files go to /var/lib/face-auth/ and
# /etc/ instead. The Makefile auto-detects this.

# Auto-detect immutable /usr (Atomic/ostree)
ATOMIC := $(shell test -w /usr/libexec 2>/dev/null && echo 0 || echo 1)

ifeq ($(ATOMIC),1)
  # Atomic: writable paths only
  INSTALLDIR  ?= /var/lib/face-auth
  LIBEXECDIR  ?= $(INSTALLDIR)/bin
  PAMDIR      ?= $(INSTALLDIR)
  DATADIR     ?= $(INSTALLDIR)
  APPSDIR     ?= /usr/local/share/applications
  ICONDIR     ?= /usr/local/share/icons/hicolor/scalable/apps
else
  # Traditional: standard FHS paths
  PREFIX      ?= /usr
  INSTALLDIR  ?= $(PREFIX)
  LIBEXECDIR  ?= $(PREFIX)/libexec
  PAMDIR      ?= $(PREFIX)/lib64/security
  DATADIR     ?= $(PREFIX)/share/face-auth
  APPSDIR     ?= $(PREFIX)/share/applications
  ICONDIR     ?= $(PREFIX)/share/icons/hicolor/scalable/apps
endif

SYSCONFDIR  ?= /etc
SYSTEMDDIR  ?= $(SYSCONFDIR)/systemd/system

CARGO       ?= cargo
INSTALL     ?= install

# Binary names
DAEMON      := face-authd
ENROLL      := face-enroll
GUI         := face-auth-gui
PAM_MODULE  := libpam_face.so
PAM_TARGET  := pam_face.so

.PHONY: release install uninstall reinstall clean

release:
	$(CARGO) build --release -p face-authd -p face-enroll -p pam-face -p face-auth-gui

install:
	@test -f target/release/$(DAEMON) || { echo "Run 'make release' first (without sudo)"; exit 1; }
	@echo "=== Installing face-auth ==="
ifeq ($(ATOMIC),1)
	@echo "  Detected: Fedora Atomic (immutable /usr)"
	@echo "  Install dir: $(INSTALLDIR)"
else
	@echo "  Detected: Traditional system"
endif
	# Binaries
	$(INSTALL) -d $(DESTDIR)$(LIBEXECDIR)
	$(INSTALL) -Dm755 target/release/$(DAEMON)     $(DESTDIR)$(LIBEXECDIR)/$(DAEMON)
	$(INSTALL) -Dm755 target/release/$(ENROLL)      $(DESTDIR)$(LIBEXECDIR)/$(ENROLL)
	$(INSTALL) -Dm755 target/release/$(GUI)         $(DESTDIR)$(LIBEXECDIR)/$(GUI)
	# PAM module
	$(INSTALL) -d $(DESTDIR)$(PAMDIR)
	$(INSTALL) -Dm755 target/release/$(PAM_MODULE)  $(DESTDIR)$(PAMDIR)/$(PAM_TARGET)
	# SELinux: relabel binaries so systemd can exec them from non-standard paths
	@if command -v chcon >/dev/null 2>&1; then \
		chcon -t bin_t $(DESTDIR)$(LIBEXECDIR)/$(DAEMON) 2>/dev/null || true; \
		chcon -t bin_t $(DESTDIR)$(LIBEXECDIR)/$(ENROLL) 2>/dev/null || true; \
		chcon -t bin_t $(DESTDIR)$(LIBEXECDIR)/$(GUI) 2>/dev/null || true; \
		chcon -t lib_t $(DESTDIR)$(PAMDIR)/$(PAM_TARGET) 2>/dev/null || true; \
	fi
	# ONNX models
	$(INSTALL) -d $(DESTDIR)$(DATADIR)/models
	@if [ -d models ]; then \
		cp -v models/*.onnx $(DESTDIR)$(DATADIR)/models/ 2>/dev/null || true; \
	fi
	# Config (always update — tuning changes with each release)
	$(INSTALL) -d $(DESTDIR)$(SYSCONFDIR)/face-auth
	$(INSTALL) -Dm644 platform/config.toml $(DESTDIR)$(SYSCONFDIR)/face-auth/config.toml
	# IR emitter config (don't overwrite existing — user-specific hardware)
	@if [ -f ir-emitter.toml ] && [ ! -f $(DESTDIR)$(SYSCONFDIR)/face-auth/ir-emitter.toml ]; then \
		$(INSTALL) -Dm644 ir-emitter.toml $(DESTDIR)$(SYSCONFDIR)/face-auth/ir-emitter.toml; \
		echo "  Installed ir-emitter.toml"; \
	elif [ -f $(DESTDIR)$(SYSCONFDIR)/face-auth/ir-emitter.toml ]; then \
		echo "  ir-emitter.toml exists, not overwriting"; \
	fi
	# systemd service (patch ExecStart path for this install)
	$(INSTALL) -Dm644 platform/systemd/face-authd.service $(DESTDIR)$(SYSTEMDDIR)/face-authd.service
	@sed -i 's|ExecStart=.*|ExecStart=$(LIBEXECDIR)/$(DAEMON)|' $(DESTDIR)$(SYSTEMDDIR)/face-authd.service
	# PAM snippets (reference only)
	$(INSTALL) -d $(DESTDIR)$(DATADIR)/pam
	$(INSTALL) -Dm644 platform/pam/sddm.conf.snippet        $(DESTDIR)$(DATADIR)/pam/
	$(INSTALL) -Dm644 platform/pam/kscreenlocker.conf        $(DESTDIR)$(DATADIR)/pam/
	# SELinux policy source
	$(INSTALL) -d $(DESTDIR)$(DATADIR)/selinux
	$(INSTALL) -Dm644 platform/selinux/face_auth.te          $(DESTDIR)$(DATADIR)/selinux/
	# Scripts
	$(INSTALL) -d $(DESTDIR)$(DATADIR)/scripts
	$(INSTALL) -Dm755 scripts/install.sh                     $(DESTDIR)$(DATADIR)/scripts/
	$(INSTALL) -Dm755 scripts/uninstall.sh                   $(DESTDIR)$(DATADIR)/scripts/
ifeq ($(ATOMIC),0)
	# Convenience symlinks (traditional systems only)
	@ln -sf $(LIBEXECDIR)/$(ENROLL) $(DESTDIR)$(PREFIX)/bin/$(ENROLL) 2>/dev/null || true
	@ln -sf $(LIBEXECDIR)/$(GUI) $(DESTDIR)$(PREFIX)/bin/$(GUI) 2>/dev/null || true
endif
	# Desktop entry + icon (app launcher integration)
	$(INSTALL) -d $(DESTDIR)$(APPSDIR)
	$(INSTALL) -Dm644 platform/face-auth-gui.desktop $(DESTDIR)$(APPSDIR)/face-auth-gui.desktop
	@sed -i 's|Exec=.*|Exec=$(LIBEXECDIR)/$(GUI)|' $(DESTDIR)$(APPSDIR)/face-auth-gui.desktop
	$(INSTALL) -d $(DESTDIR)$(ICONDIR)
	$(INSTALL) -Dm644 platform/face-auth.svg $(DESTDIR)$(ICONDIR)/face-auth.svg
	@update-desktop-database $(DESTDIR)$(APPSDIR) 2>/dev/null || true
	@gtk-update-icon-cache -f -t $(DESTDIR)$(ICONDIR)/../../.. 2>/dev/null || true
	# Reload systemd unit files and restart daemon if already running
	@if command -v systemctl >/dev/null 2>&1; then \
		systemctl daemon-reload 2>/dev/null || true; \
		if systemctl is-active --quiet face-authd.service 2>/dev/null; then \
			systemctl restart face-authd.service 2>/dev/null && echo "  Daemon restarted" || true; \
		fi; \
	fi
	@echo ""
	@echo "=== Install complete ==="
	@echo "Binaries:   $(LIBEXECDIR)/"
	@echo "PAM module: $(PAMDIR)/$(PAM_TARGET)"
	@echo "Config:     $(SYSCONFDIR)/face-auth/config.toml"
	@echo "Desktop:    $(APPSDIR)/face-auth-gui.desktop"
	@echo ""
	@echo "Next: sudo $(DATADIR)/scripts/install.sh"

uninstall:
	@if [ -x $(DATADIR)/scripts/uninstall.sh ]; then \
		$(DATADIR)/scripts/uninstall.sh; \
	else \
		echo "uninstall.sh not found — manual cleanup needed"; \
	fi

reinstall: uninstall install

clean:
	$(CARGO) clean

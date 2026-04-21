#!/usr/bin/env bash
# face-auth installer — configures PAM, SELinux, systemd, video group
# Run after binaries are installed (make install or rpm-ostree install)
set -euo pipefail

# --- Colors ---
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BOLD='\033[1m'
NC='\033[0m'

info()  { echo -e "${GREEN}[+]${NC} $*"; }
warn()  { echo -e "${YELLOW}[!]${NC} $*"; }
error() { echo -e "${RED}[x]${NC} $*"; }
ask()   { echo -en "${BOLD}[?]${NC} $* "; }

# --- Root check ---
if [ "$(id -u)" -ne 0 ]; then
    error "Must run as root (sudo $0)"
    exit 1
fi

# --- Detect Atomic (immutable /usr) ---
if [ -w /usr/libexec ] 2>/dev/null; then
    ATOMIC=0
else
    ATOMIC=1
fi

# --- Paths ---
if [ "$ATOMIC" -eq 1 ]; then
    INSTALLDIR="${INSTALLDIR:-/var/lib/face-auth}"
    LIBEXECDIR="${LIBEXECDIR:-${INSTALLDIR}/bin}"
    PAMDIR="${PAMDIR:-${INSTALLDIR}}"
    DATADIR="${DATADIR:-${INSTALLDIR}}"
else
    LIBEXECDIR="${LIBEXECDIR:-/usr/libexec}"
    PAMDIR="${PAMDIR:-/usr/lib64/security}"
    DATADIR="${DATADIR:-/usr/share/face-auth}"
fi
SYSCONFDIR="${SYSCONFDIR:-/etc}"
PAM_BACKUP_DIR="${SYSCONFDIR}/face-auth/pam-backup"

DAEMON="${LIBEXECDIR}/face-authd"
ENROLL="${LIBEXECDIR}/face-enroll"
PAM_SO="${PAMDIR}/pam_face.so"
MODEL_DIR="${DATADIR}/models"

# PAM line uses absolute path on Atomic (module not in standard /usr/lib64/security/)
if [ "$ATOMIC" -eq 1 ]; then
    PAM_MODULE_PATH="${PAMDIR}/pam_face.so"
else
    PAM_MODULE_PATH="pam_face.so"
fi

echo ""
echo -e "${BOLD}=== face-auth installer ===${NC}"
echo ""

# --- Step 1: Verify binaries ---
info "Checking installed files..."
missing=0
for f in "$DAEMON" "$ENROLL" "$PAM_SO"; do
    if [ ! -f "$f" ]; then
        error "Missing: $f"
        missing=1
    fi
done
if [ "$missing" -eq 1 ]; then
    error "Run 'make install' first."
    exit 1
fi
info "Binaries found."

# --- Step 2: Verify models ---
model_count=0
if [ -d "$MODEL_DIR" ]; then
    model_count=$(find "$MODEL_DIR" -name '*.onnx' 2>/dev/null | wc -l)
fi
if [ "$model_count" -lt 2 ]; then
    warn "Found $model_count ONNX models in $MODEL_DIR (need at least 2: detection + recognition)"
    warn "Copy models to $MODEL_DIR before using face-auth."
else
    info "Found $model_count ONNX models."
fi

# --- Step 3: Detect display manager ---
detect_dm() {
    if systemctl is-active --quiet sddm 2>/dev/null; then
        echo "sddm"
    elif systemctl is-active --quiet gdm 2>/dev/null; then
        echo "gdm"
    elif systemctl is-active --quiet lightdm 2>/dev/null; then
        echo "lightdm"
    else
        echo "unknown"
    fi
}
DM=$(detect_dm)
info "Detected display manager: $DM"

# --- Step 4: Video group ---
DM_USER="$DM"
if [ "$DM" = "unknown" ]; then
    DM_USER="sddm"
    warn "Could not detect DM, defaulting to user 'sddm'"
fi

if id "$DM_USER" &>/dev/null; then
    if id -nG "$DM_USER" | grep -qw video; then
        info "$DM_USER already in video group."
    else
        usermod -aG video "$DM_USER"
        info "Added $DM_USER to video group."
    fi
else
    warn "User '$DM_USER' does not exist — skip video group setup."
fi

# --- Step 5: Select PAM files ---
PAM_LINE="auth    sufficient  ${PAM_MODULE_PATH}"

# Services where face auth makes sense — default Y
# Display managers (login screen)
# Lock screens
# Privilege escalation
# Policy kit (GUI auth prompts)
RECOMMENDED_SERVICES="sddm gdm lightdm login kde kde-fingerprint kscreensaver kscreenlocker xscreensaver sudo su polkit-1 xfce4-screensaver cinnamon-screensaver mate-screensaver"

# Services where face auth should NOT be offered (no local session / no camera)
SKIP_SERVICES="sddm-greeter sddm-autologin other password-auth system-auth smartcard-auth fingerprint-auth postlogin config-util runuser runuser-l remote crond atd cups httpd cockpit vlock systemd-user"

is_recommended() {
    echo " $RECOMMENDED_SERVICES " | grep -q " $1 "
}

is_skipped() {
    echo " $SKIP_SERVICES " | grep -q " $1 "
}

# Has an 'auth' section (not just account/session/password)
has_auth_section() {
    grep -q '^auth' "$1" 2>/dev/null || grep -q '^-auth' "$1" 2>/dev/null
}

echo ""
info "Select which PAM services to enable face auth for:"
info "(Services with [Y/n] are recommended, [y/N] are optional)"
echo ""

selected_pam=()
already_count=0
skipped_count=0

for pam_file in /etc/pam.d/*; do
    [ -f "$pam_file" ] || continue
    service=$(basename "$pam_file")

    # Skip services that don't make sense
    if is_skipped "$service"; then
        skipped_count=$((skipped_count + 1))
        continue
    fi

    # Skip files without auth section
    if ! has_auth_section "$pam_file"; then
        continue
    fi

    # Already configured
    if grep -q "pam_face\.so" "$pam_file" 2>/dev/null; then
        echo "  $service — already configured ✓"
        already_count=$((already_count + 1))
        continue
    fi

    if is_recommended "$service"; then
        ask "Enable for ${BOLD}$service${NC} ($pam_file)? [Y/n]"
        read -r answer
        answer="${answer:-y}"
    else
        ask "Enable for $service ($pam_file)? [y/N]"
        read -r answer
        answer="${answer:-n}"
    fi

    if [[ "$answer" =~ ^[Yy] ]]; then
        selected_pam+=("$pam_file")
    fi
done

if [ "$already_count" -gt 0 ]; then
    info "$already_count service(s) already configured."
fi
if [ "$skipped_count" -gt 0 ]; then
    info "$skipped_count system service(s) skipped (no local camera access)."
fi

# --- Step 6: Backup and inject PAM ---
if [ ${#selected_pam[@]} -gt 0 ]; then
    mkdir -p "$PAM_BACKUP_DIR"
    for pam_file in "${selected_pam[@]}"; do
        backup="${PAM_BACKUP_DIR}/$(basename "$pam_file").bak"

        # Backup original
        if [ ! -f "$backup" ]; then
            cp "$pam_file" "$backup"
            info "Backed up $pam_file → $backup"
        fi

        # Insert before the first 'auth' line (preserves comment headers)
        if grep -qn '^auth' "$pam_file"; then
            first_auth=$(grep -n '^auth' "$pam_file" | head -1 | cut -d: -f1)
            sed -i "${first_auth}i\\${PAM_LINE}" "$pam_file"
        elif grep -qn '^-auth' "$pam_file"; then
            first_auth=$(grep -n '^-auth' "$pam_file" | head -1 | cut -d: -f1)
            sed -i "${first_auth}i\\${PAM_LINE}" "$pam_file"
        else
            # No auth line found — prepend
            sed -i "1i\\${PAM_LINE}" "$pam_file"
        fi
        info "Added face-auth to $pam_file"
    done
else
    info "No PAM files modified."
fi

# --- Step 7: SELinux ---
if command -v getenforce &>/dev/null && [ "$(getenforce)" != "Disabled" ]; then
    SELINUX_TE="${DATADIR}/selinux/face_auth.te"
    if [ -f "$SELINUX_TE" ]; then
        ask "Install SELinux policy? [Y/n]"
        read -r answer
        answer="${answer:-y}"
        if [[ "$answer" =~ ^[Yy] ]]; then
            tmpdir=$(mktemp -d)
            cp "$SELINUX_TE" "$tmpdir/"
            (
                cd "$tmpdir"
                checkmodule -M -m -o face_auth.mod face_auth.te 2>/dev/null
                semodule_package -o face_auth.pp -m face_auth.mod 2>/dev/null
                semodule -i face_auth.pp 2>/dev/null
            )
            rm -rf "$tmpdir"
            info "SELinux policy installed."
        fi
    else
        warn "SELinux policy source not found at $SELINUX_TE"
    fi
else
    info "SELinux not active, skipping policy install."
fi

# --- Step 8: systemd ---
if command -v systemctl &>/dev/null; then
    systemctl daemon-reload
    systemctl enable face-authd.service 2>/dev/null || true
    # Restart (not just start) to pick up new binaries
    systemctl restart face-authd.service 2>/dev/null || true
    if systemctl is-active --quiet face-authd.service; then
        info "face-authd service started."
    else
        warn "face-authd service failed to start. Check: journalctl -u face-authd"
    fi
fi

# --- Step 9: PATH setup (Atomic only) ---
if [ "$ATOMIC" -eq 1 ]; then
    # Add to login shell PATH
    PROFILE_SCRIPT="/etc/profile.d/face-auth.sh"
    if [ ! -f "$PROFILE_SCRIPT" ]; then
        cat > "$PROFILE_SCRIPT" <<EOF
# Added by face-auth installer
export PATH="\$PATH:${LIBEXECDIR}"
EOF
        info "Added ${LIBEXECDIR} to PATH via $PROFILE_SCRIPT"
        info "Open a new terminal or run: source $PROFILE_SCRIPT"
    else
        info "PATH profile already configured."
    fi

    # Add face-auth bin to sudo's secure_path so 'sudo face-enroll' works
    SUDOERS_DROP="/etc/sudoers.d/face-auth"
    if [ ! -f "$SUDOERS_DROP" ]; then
        # Read current secure_path from sudo, append our dir
        CURRENT_SECURE=$(sudo -V 2>/dev/null | grep "Default value for" | grep -oP '(?<=").*(?=")' || echo "")
        if [ -z "$CURRENT_SECURE" ]; then
            CURRENT_SECURE="/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin"
        fi
        cat > "$SUDOERS_DROP" <<EOF
# Added by face-auth installer
Defaults secure_path="${CURRENT_SECURE}:${LIBEXECDIR}"
EOF
        chmod 440 "$SUDOERS_DROP"
        if visudo -c -f "$SUDOERS_DROP" >/dev/null 2>&1; then
            info "Added ${LIBEXECDIR} to sudo PATH"
        else
            rm -f "$SUDOERS_DROP"
            warn "Could not configure sudo PATH. Use full paths: sudo ${LIBEXECDIR}/face-enroll"
        fi
    fi
fi

# --- Step 10: Smoke test ---
echo ""
if [ -x "$ENROLL" ]; then
    info "Run 'sudo ${ENROLL} --test-camera' to verify camera access."
fi

# --- Done ---
echo ""
echo -e "${BOLD}=== Installation complete ===${NC}"
echo ""
echo "Next steps:"
echo "  1. Run 'sudo ${ENROLL}' to register your face"
echo "  2. Lock screen and test face authentication"
echo ""
echo "To undo: sudo ${DATADIR}/scripts/uninstall.sh"
echo ""

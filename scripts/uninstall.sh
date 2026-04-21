#!/usr/bin/env bash
# face-auth uninstaller — reverses install.sh changes
set -euo pipefail

RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BOLD='\033[1m'
NC='\033[0m'

info()  { echo -e "${GREEN}[+]${NC} $*"; }
warn()  { echo -e "${YELLOW}[!]${NC} $*"; }
error() { echo -e "${RED}[x]${NC} $*"; }
ask()   { echo -en "${BOLD}[?]${NC} $* "; }

if [ "$(id -u)" -ne 0 ]; then
    error "Must run as root (sudo $0)"
    exit 1
fi

PURGE=0
if [ "${1:-}" = "--purge" ]; then
    PURGE=1
fi

# --- Detect Atomic ---
if [ -w /usr/libexec ] 2>/dev/null; then
    ATOMIC=0
else
    ATOMIC=1
fi

SYSCONFDIR="${SYSCONFDIR:-/etc}"
PAM_BACKUP_DIR="${SYSCONFDIR}/face-auth/pam-backup"

if [ "$ATOMIC" -eq 1 ]; then
    INSTALLDIR="${INSTALLDIR:-/var/lib/face-auth}"
    LIBEXECDIR="${LIBEXECDIR:-${INSTALLDIR}/bin}"
    PAMDIR="${PAMDIR:-${INSTALLDIR}}"
    DATADIR="${DATADIR:-${INSTALLDIR}}"
else
    LIBEXECDIR="${LIBEXECDIR:-/usr/libexec}"
    DATADIR="${DATADIR:-/usr/share/face-auth}"
    PAMDIR="${PAMDIR:-/usr/lib64/security}"
fi

echo ""
echo -e "${BOLD}=== face-auth uninstaller ===${NC}"
echo ""

# --- Step 1: Stop and disable service ---
if command -v systemctl &>/dev/null; then
    systemctl stop face-authd.service 2>/dev/null || true
    systemctl disable face-authd.service 2>/dev/null || true
    info "Service stopped and disabled."
fi

# --- Step 2: Restore PAM files ---
# First restore any files we have backups for
if [ -d "$PAM_BACKUP_DIR" ]; then
    for backup in "$PAM_BACKUP_DIR"/*.bak; do
        [ -f "$backup" ] || continue
        service=$(basename "$backup" .bak)
        pam_file="/etc/pam.d/$service"
        if [ -f "$pam_file" ]; then
            cp "$backup" "$pam_file"
            info "Restored $pam_file from backup"
        fi
    done
    rm -rf "$PAM_BACKUP_DIR"
    info "Removed PAM backups."
fi

# Then scan all PAM files for any remaining pam_face.so lines (catches manual edits)
for pam_file in /etc/pam.d/*; do
    [ -f "$pam_file" ] || continue
    if grep -q "pam_face\.so" "$pam_file" 2>/dev/null; then
        sed -i '/pam_face\.so/d' "$pam_file"
        info "Removed pam_face.so from $pam_file"
    fi
done
fi

# --- Step 3: Remove SELinux policy ---
if command -v semodule &>/dev/null; then
    if semodule -l 2>/dev/null | grep -q face_auth; then
        semodule -r face_auth 2>/dev/null || true
        info "SELinux policy removed."
    fi
fi

# --- Step 4: Video group ---
detect_dm_user() {
    for dm in sddm gdm lightdm; do
        if id "$dm" &>/dev/null; then
            echo "$dm"
            return
        fi
    done
    echo ""
}
DM_USER=$(detect_dm_user)
if [ -n "$DM_USER" ] && id -nG "$DM_USER" 2>/dev/null | grep -qw video; then
    ask "Remove $DM_USER from video group? (may have been there before face-auth) [y/N]"
    read -r answer
    if [[ "$answer" =~ ^[Yy] ]]; then
        gpasswd -d "$DM_USER" video 2>/dev/null || true
        info "Removed $DM_USER from video group."
    else
        info "Left $DM_USER in video group."
    fi
fi

# --- Step 5: Remove installed files ---
if [ "$ATOMIC" -eq 1 ]; then
    rm -rf "${INSTALLDIR}"
    info "Removed ${INSTALLDIR}/"
else
    rm -f "${LIBEXECDIR}/face-authd"
    rm -f "${LIBEXECDIR}/face-enroll"
    rm -f "${PAMDIR}/pam_face.so"
    rm -f "/usr/bin/face-enroll"
    rm -rf "${DATADIR}"
    info "Removed binaries, models, and data files."
fi
rm -f "/etc/systemd/system/face-authd.service"
rm -f "/etc/profile.d/face-auth.sh"
rm -f "/etc/sudoers.d/face-auth"

# --- Step 6: Config ---
if [ "$PURGE" -eq 1 ]; then
    rm -rf "${SYSCONFDIR}/face-auth"
    info "Purged config directory."
    # Remove enrollment data for all users
    ask "Also remove enrollment data from all user home directories? [y/N]"
    read -r answer
    if [[ "$answer" =~ ^[Yy] ]]; then
        for home in /home/*/; do
            enrollment="${home}.local/share/face-auth"
            if [ -d "$enrollment" ]; then
                rm -rf "$enrollment"
                info "Removed $enrollment"
            fi
        done
    fi
else
    # Keep config but clean backup dir
    rm -rf "${SYSCONFDIR}/face-auth/pam-backup"
    info "Config preserved at ${SYSCONFDIR}/face-auth/ (use --purge to remove)"
fi

# --- Step 7: Reload systemd ---
if command -v systemctl &>/dev/null; then
    systemctl daemon-reload
fi

echo ""
echo -e "${BOLD}=== Uninstall complete ===${NC}"
echo ""

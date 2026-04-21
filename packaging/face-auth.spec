Name:           face-auth
Version:        0.9.0
Release:        1%{?dist}
Summary:        IR-based face authentication for Linux via PAM

License:        Apache-2.0
URL:            https://github.com/MrCodeEU/face-auth
Source0:        %{name}-%{version}.tar.gz
# Models are too large for Source — downloaded by install script
# Source1: models.tar.gz

BuildRequires:  rust cargo
BuildRequires:  gcc clang lld cmake pkg-config
BuildRequires:  pam-devel
BuildRequires:  libv4l-devel
BuildRequires:  openssl-devel
BuildRequires:  fontconfig-devel
BuildRequires:  dbus-devel
BuildRequires:  systemd-devel
BuildRequires:  selinux-policy-devel
BuildRequires:  checkpolicy policycoreutils policycoreutils-python-utils

Requires:       pam
Requires:       v4l-utils
Requires:       systemd

%description
face-auth provides IR-based face authentication for Linux systems using
the built-in IR camera. It integrates with PAM to secure sudo, polkit,
KDE lock screen, and other PAM-enabled services.

Features:
- Anti-spoofing via IR texture liveness analysis (rejects phone screens)
- Multi-angle enrollment with quality scoring
- CLAHE preprocessing for lighting-invariant recognition
- Hot-reload configuration without service restart

%prep
%autosetup

%build
export ORT_STRATEGY=download
cargo build --release -p face-authd -p face-enroll
cargo build --release -p pam-face

%install
install -Dm755 target/release/face-authd  %{buildroot}%{_libexecdir}/face-authd
install -Dm755 target/release/face-enroll %{buildroot}%{_bindir}/face-enroll
install -Dm755 target/release/libpam_face.so %{buildroot}/%{_lib}/security/pam_face.so

# Config
install -Dm644 platform/config.toml %{buildroot}%{_sysconfdir}/face-auth/config.toml

# Systemd service
install -Dm644 platform/systemd/face-authd.service \
    %{buildroot}%{_unitdir}/face-authd.service

# SELinux policy
install -Dm644 platform/selinux/face_auth.pp \
    %{buildroot}%{_datadir}/selinux/packages/face_auth.pp

%post
# Install SELinux policy
semodule -i %{_datadir}/selinux/packages/face_auth.pp 2>/dev/null || true
# Enable and start service
%systemd_post face-authd.service

%preun
%systemd_preun face-authd.service

%postun
%systemd_postun_with_restart face-authd.service
if [ $1 -eq 0 ]; then
    semodule -r face_auth 2>/dev/null || true
fi

%files
%license LICENSE
%doc README.md
%{_libexecdir}/face-authd
%{_bindir}/face-enroll
/%{_lib}/security/pam_face.so
%config(noreplace) %{_sysconfdir}/face-auth/config.toml
%{_unitdir}/face-authd.service
%{_datadir}/selinux/packages/face_auth.pp

%changelog
* Mon Apr 21 2025 MrCodeEU <noreply@github.com> - 0.9.0-1
- Phase 9: initial RPM packaging

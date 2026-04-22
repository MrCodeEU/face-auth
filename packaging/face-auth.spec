Name:           face-auth
Version:        0.1.0
Release:        1%{?dist}
Summary:        IR-based face authentication for Linux via PAM

License:        Apache-2.0
URL:            https://github.com/MrCodeEU/face-auth

# Source tarball includes: source tree + vendor/ (cargo vendor) + ort/ (ORT 1.24.2 native lib)
# Built by scripts/build-srpm.sh or the COPR GitHub Actions workflow
Source0:        %{name}-%{version}.tar.gz

BuildRequires:  rust cargo
BuildRequires:  gcc clang cmake pkg-config
BuildRequires:  pam-devel
BuildRequires:  libv4l-devel
BuildRequires:  openssl-devel
BuildRequires:  fontconfig-devel
BuildRequires:  dbus-devel
BuildRequires:  systemd-devel
BuildRequires:  selinux-policy-devel
BuildRequires:  checkpolicy policycoreutils policycoreutils-python-utils
BuildRequires:  xz lzma

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
# Use pre-vendored cargo deps (no network needed)
mkdir -p .cargo
cat >> .cargo/config.toml << 'EOF'
[source.crates-io]
replace-with = "vendored-sources"

[source.vendored-sources]
directory = "vendor"
EOF

# Point ort-sys at the pre-downloaded ORT 1.24.2 native lib (no network needed)
export ORT_LIB_PATH=%{_builddir}/%{name}-%{version}/ort
export ORT_SKIP_DOWNLOAD=1
export CARGO_NET_OFFLINE=1

cargo build --release --frozen -p face-authd -p face-enroll
cargo build --release --frozen -p pam-face

%install
install -Dm755 target/release/face-authd   %{buildroot}%{_libexecdir}/face-authd
install -Dm755 target/release/face-enroll  %{buildroot}%{_bindir}/face-enroll
install -Dm755 target/release/libpam_face.so %{buildroot}/%{_lib}/security/pam_face.so

# Bundle ORT private lib (not a system dep)
install -Dm755 ort/libonnxruntime.so.1.24.2 \
    %{buildroot}%{_libdir}/face-auth/libonnxruntime.so.1.24.2
ln -sf libonnxruntime.so.1.24.2 \
    %{buildroot}%{_libdir}/face-auth/libonnxruntime.so

# Config
install -Dm644 platform/config.toml %{buildroot}%{_sysconfdir}/face-auth/config.toml

# Systemd service
install -Dm644 platform/systemd/face-authd.service \
    %{buildroot}%{_unitdir}/face-authd.service

# SELinux policy (pre-compiled .pp bundled in source)
install -Dm644 platform/selinux/face_auth.pp \
    %{buildroot}%{_datadir}/selinux/packages/face_auth.pp

%post
semodule -i %{_datadir}/selinux/packages/face_auth.pp 2>/dev/null || true
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
%{_libdir}/face-auth/libonnxruntime.so.1.24.2
%{_libdir}/face-auth/libonnxruntime.so
%config(noreplace) %{_sysconfdir}/face-auth/config.toml
%{_unitdir}/face-authd.service
%{_datadir}/selinux/packages/face_auth.pp

%changelog
* Tue Apr 22 2025 MrCodeEU <noreply@github.com> - 0.1.0-1
- Initial COPR release

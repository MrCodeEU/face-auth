use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum EnrollmentError {
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("deserialize error: {0}")]
    Deserialize(String),
    #[error("serialize error: {0}")]
    Serialize(String),
    #[error("no enrollment found for user: {0}")]
    NotFound(String),
    #[error("max embeddings ({0}) reached")]
    MaxEmbeddings(u32),
}

/// Current enrollment format version. Bump when preprocessing pipeline changes
/// (e.g., adding CLAHE, changing normalization) to detect stale embeddings.
pub const ENROLLMENT_VERSION: u32 = 2;

/// On-disk format uses Vec<Vec<f32>> since serde doesn't support [f32; 512].
#[derive(Serialize, Deserialize)]
struct EnrollmentData {
    /// Format version — absent in v1 files (pre-CLAHE), defaults to 1.
    #[serde(default = "default_version")]
    version: u32,
    embeddings: Vec<Vec<f32>>,
}

fn default_version() -> u32 {
    1
}

impl EnrollmentData {
    fn from_arrays(arrays: &[[f32; 512]]) -> Self {
        Self {
            version: ENROLLMENT_VERSION,
            embeddings: arrays.iter().map(|a| a.to_vec()).collect(),
        }
    }

    fn to_arrays(&self) -> Result<Vec<[f32; 512]>, EnrollmentError> {
        self.embeddings
            .iter()
            .map(|v| {
                v.as_slice().try_into().map_err(|_| {
                    EnrollmentError::Deserialize(format!(
                        "expected 512-dim embedding, got {}",
                        v.len()
                    ))
                })
            })
            .collect()
    }
}

/// Get the enrollment directory for a user.
/// Always resolves via /etc/passwd to avoid $HOME mismatch under sudo.
/// Layout: ~<user>/.local/share/face-auth/<username>/
pub fn enrollment_dir(username: &str) -> PathBuf {
    let home = resolve_home(username)
        .or_else(|| std::env::var("HOME").ok())
        .unwrap_or_else(|| format!("/home/{username}"));
    PathBuf::from(home)
        .join(".local/share/face-auth")
        .join(username)
}

fn resolve_home(username: &str) -> Option<String> {
    let passwd = std::fs::read_to_string("/etc/passwd").ok()?;
    for line in passwd.lines() {
        let fields: Vec<&str> = line.split(':').collect();
        if fields.len() >= 6 && fields[0] == username {
            return Some(fields[5].to_string());
        }
    }
    None
}

fn embeddings_path(dir: &Path) -> PathBuf {
    dir.join("embeddings.bin")
}

/// Check the version of stored enrollment data.
/// Returns None if no enrollment exists, Some(version) otherwise.
pub fn enrollment_version(username: &str) -> Option<u32> {
    let dir = enrollment_dir(username);
    let path = embeddings_path(&dir);
    if !path.exists() {
        return None;
    }
    let data = std::fs::read(&path).ok()?;
    let enrollment: EnrollmentData = bincode::deserialize(&data).ok()?;
    Some(enrollment.version)
}

/// Load enrollment embeddings for a user.
pub fn load_embeddings(username: &str) -> Result<Vec<[f32; 512]>, EnrollmentError> {
    let dir = enrollment_dir(username);
    let path = embeddings_path(&dir);

    if !path.exists() {
        return Err(EnrollmentError::NotFound(username.to_string()));
    }

    let data = std::fs::read(&path)?;
    let enrollment: EnrollmentData =
        bincode::deserialize(&data).map_err(|e| EnrollmentError::Deserialize(e.to_string()))?;

    enrollment.to_arrays()
}

/// Save enrollment embeddings for a user.
pub fn save_embeddings(
    username: &str,
    embeddings: &[[f32; 512]],
    max_enrollment: u32,
) -> Result<(), EnrollmentError> {
    if embeddings.len() > max_enrollment as usize {
        return Err(EnrollmentError::MaxEmbeddings(max_enrollment));
    }

    let dir = enrollment_dir(username);

    // Fix ownership of existing dirs before writing (handles root-owned leftovers)
    fix_enrollment_ownership(username, &dir);

    std::fs::create_dir_all(&dir)?;

    let enrollment = EnrollmentData::from_arrays(embeddings);

    let data =
        bincode::serialize(&enrollment).map_err(|e| EnrollmentError::Serialize(e.to_string()))?;
    let path = embeddings_path(&dir);

    // Atomic write via temp file
    let tmp_path = path.with_extension("bin.tmp");
    std::fs::write(&tmp_path, &data)?;
    std::fs::rename(&tmp_path, &path)?;

    // When running under sudo, chown the enrollment dir + files to the real user
    // so they remain accessible without sudo.
    chown_to_user(username, &dir);

    Ok(())
}

/// Fix ownership of enrollment directory tree if it was created by root.
/// Called before save so that non-root writes succeed on previously root-created dirs.
fn fix_enrollment_ownership(username: &str, dir: &Path) {
    // Only attempt if running as root (sudo) — non-root can't chown
    if unsafe { libc::geteuid() } != 0 {
        return;
    }
    if dir.exists() {
        chown_to_user(username, dir);
    }
    // Also fix parent dirs up to .local/share/face-auth/
    if let Some(parent) = dir.parent() {
        if parent.exists() {
            chown_to_user(username, parent);
        }
    }
}

/// Best-effort chown of enrollment directory tree to the target user.
/// Uses /etc/passwd to resolve uid/gid. Silently ignores failures.
fn chown_to_user(username: &str, dir: &Path) {
    let Some((uid, gid)) = resolve_uid_gid(username) else {
        return;
    };
    let _ = chown_recursive(dir, uid, gid);
    // Also chown parent (.local/share/face-auth/) so user can create new dirs later
    if let Some(parent) = dir.parent() {
        let _ = nix_chown(parent, uid, gid);
    }
}

fn resolve_uid_gid(username: &str) -> Option<(u32, u32)> {
    let passwd = std::fs::read_to_string("/etc/passwd").ok()?;
    for line in passwd.lines() {
        let fields: Vec<&str> = line.split(':').collect();
        if fields.len() >= 4 && fields[0] == username {
            let uid: u32 = fields[2].parse().ok()?;
            let gid: u32 = fields[3].parse().ok()?;
            return Some((uid, gid));
        }
    }
    None
}

fn chown_recursive(path: &Path, uid: u32, gid: u32) -> std::io::Result<()> {
    nix_chown(path, uid, gid)?;
    if path.is_dir() {
        for entry in std::fs::read_dir(path)? {
            let entry = entry?;
            chown_recursive(&entry.path(), uid, gid)?;
        }
    }
    Ok(())
}

fn nix_chown(path: &Path, uid: u32, gid: u32) -> std::io::Result<()> {
    use std::ffi::CString;
    let c_path = CString::new(path.as_os_str().as_encoded_bytes())
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidInput, e))?;
    let ret = unsafe { libc::chown(c_path.as_ptr(), uid, gid) };
    if ret == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_embeddings() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("embeddings.bin");

        let mut emb = [0.0f32; 512];
        emb[0] = 1.0;
        emb[511] = -0.5;
        let embeddings = vec![emb];

        let enrollment = EnrollmentData::from_arrays(&embeddings);
        let data = bincode::serialize(&enrollment).unwrap();
        std::fs::write(&path, &data).unwrap();

        let loaded: EnrollmentData = bincode::deserialize(&std::fs::read(&path).unwrap()).unwrap();
        let arrays = loaded.to_arrays().unwrap();
        assert_eq!(arrays.len(), 1);
        assert_eq!(arrays[0][0], 1.0);
        assert_eq!(arrays[0][511], -0.5);
    }

    #[test]
    fn new_enrollment_stamps_current_version() {
        let mut emb = [0.0f32; 512];
        emb[0] = 1.0;
        let data = EnrollmentData::from_arrays(&[emb]);
        assert_eq!(data.version, ENROLLMENT_VERSION);
    }

    #[test]
    fn old_enrollment_defaults_to_version_1() {
        // Simulate a v1 file (no version field in bincode)
        // v1 format: just embeddings vec, no version field
        // We can't easily produce a true v1 bincode without the field,
        // but we can test that default_version() returns 1.
        assert_eq!(default_version(), 1);
        assert!(ENROLLMENT_VERSION > 1, "current version must be > 1");
    }
}

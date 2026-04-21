use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub platform: PlatformConfig,
    pub daemon: DaemonConfig,
    pub camera: CameraConfig,
    pub recognition: RecognitionConfig,
    pub liveness: LivenessConfig,
    pub geometry: GeometryConfig,
    pub logging: LoggingConfig,
    pub notify: NotifyConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct PlatformConfig {
    pub display_manager: String,
    pub init_system: String,
    pub selinux: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct DaemonConfig {
    pub socket_path: String,
    pub ui_socket_path: String,
    pub session_timeout_s: u64,
    pub idle_unload_s: u64,
    pub max_concurrent: u32,
    /// ONNX Runtime execution provider: "cpu", "rocm", "cuda", "xdna"
    pub execution_provider: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct CameraConfig {
    pub device_path: String,
    pub flush_frames: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct RecognitionConfig {
    pub model: String,
    pub threshold: f32,
    pub frames_required: u32,
    pub max_enrollment: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct LivenessConfig {
    pub enabled: bool,
    /// Minimum LBP entropy for IR texture liveness (real skin ~6.0–6.2, screens 0.4–5.5).
    pub lbp_entropy_min: f32,
    /// Minimum local contrast CV for IR texture liveness (real face ~0.28–0.36, screens 0.0 or >0.5).
    pub local_contrast_cv_min: f32,
    /// Maximum local contrast CV (real face ~0.28–0.72; screens/photos often >0.8 due to edge artifacts).
    pub local_contrast_cv_max: f32,
    /// Enable ML anti-spoof model (only useful with RGB cameras, not IR).
    pub model_enabled: bool,
    pub model: String,
    pub model_threshold: f32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct GeometryConfig {
    pub distance_min: f32,
    pub distance_max: f32,
    pub yaw_max_deg: f32,
    pub pitch_max_deg: f32,
    pub roll_max_deg: f32,
    pub guidance_debounce_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct LoggingConfig {
    pub level: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct NotifyConfig {
    /// Send desktop notification on successful auth.
    pub enabled: bool,
    /// Notification timeout in milliseconds (0 = server default).
    pub timeout_ms: i32,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            platform: PlatformConfig::default(),
            daemon: DaemonConfig::default(),
            camera: CameraConfig::default(),
            recognition: RecognitionConfig::default(),
            liveness: LivenessConfig::default(),
            geometry: GeometryConfig::default(),
            logging: LoggingConfig::default(),
            notify: NotifyConfig::default(),
        }
    }
}

impl Default for PlatformConfig {
    fn default() -> Self {
        Self {
            display_manager: "sddm".into(),
            init_system: "systemd".into(),
            selinux: true,
        }
    }
}

impl Default for DaemonConfig {
    fn default() -> Self {
        Self {
            socket_path: "/run/face-auth/pam.sock".into(),
            ui_socket_path: "/run/face-auth/ui.sock".into(),
            session_timeout_s: 7,
            idle_unload_s: 0,
            max_concurrent: 1,
            execution_provider: "cpu".into(),
        }
    }
}

impl Default for CameraConfig {
    fn default() -> Self {
        Self {
            device_path: String::new(),
            flush_frames: 0,
        }
    }
}

impl Default for RecognitionConfig {
    fn default() -> Self {
        Self {
            model: "arcface_r50".into(),
            threshold: 0.70,
            frames_required: 2,
            max_enrollment: 20,
        }
    }
}

impl Default for LivenessConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            lbp_entropy_min: 5.5,
            local_contrast_cv_min: 0.20,
            local_contrast_cv_max: 0.80,
            model_enabled: false,
            model: "minifasnet_v2".into(),
            model_threshold: 0.5,
        }
    }
}

impl Default for GeometryConfig {
    fn default() -> Self {
        Self {
            distance_min: 0.06,
            distance_max: 0.55,
            yaw_max_deg: 45.0,
            pitch_max_deg: 45.0,
            roll_max_deg: 35.0,
            guidance_debounce_ms: 100,
        }
    }
}

impl Default for LoggingConfig {
    fn default() -> Self {
        Self {
            level: "info".into(),
        }
    }
}

impl Default for NotifyConfig {
    fn default() -> Self {
        Self {
            enabled: false, // opt-in
            timeout_ms: 3000,
        }
    }
}

impl Config {
    /// Load config from a TOML file, falling back to defaults for missing fields.
    pub fn load(path: &Path) -> Result<Self, ConfigError> {
        if !path.exists() {
            return Ok(Self::default());
        }
        let settings = config::Config::builder()
            .add_source(config::File::from(path))
            .build()
            .map_err(|e| ConfigError::Parse(e.to_string()))?;
        settings
            .try_deserialize()
            .map_err(|e| ConfigError::Parse(e.to_string()))
    }

    /// Load from the default system path `/etc/face-auth/config.toml`.
    pub fn load_system() -> Result<Self, ConfigError> {
        Self::load(Path::new("/etc/face-auth/config.toml"))
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("failed to parse config: {0}")]
    Parse(String),
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn default_config_values() {
        let cfg = Config::default();
        assert_eq!(cfg.daemon.session_timeout_s, 7);
        assert_eq!(cfg.geometry.distance_min, 0.06);
        assert_eq!(cfg.recognition.threshold, 0.70);
        assert!(cfg.liveness.enabled);
    }

    #[test]
    fn load_missing_file_returns_defaults() {
        let cfg = Config::load(Path::new("/tmp/nonexistent-face-auth-test.toml")).unwrap();
        assert_eq!(cfg.daemon.session_timeout_s, 7);
    }

    #[test]
    fn load_partial_toml_fills_defaults() {
        let mut f = tempfile::Builder::new().suffix(".toml").tempfile().unwrap();
        writeln!(
            f,
            r#"
[recognition]
threshold = 0.35

[geometry]
yaw_max_deg = 30.0
"#
        )
        .unwrap();

        let cfg = Config::load(f.path()).unwrap();
        assert_eq!(cfg.recognition.threshold, 0.35);
        assert_eq!(cfg.geometry.yaw_max_deg, 30.0);
        // Other fields keep defaults
        assert_eq!(cfg.daemon.session_timeout_s, 7);
        assert_eq!(cfg.geometry.distance_min, 0.06);
    }
}

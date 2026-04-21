// IR emitter activation via UVC Extension Unit (XU) ioctls.
//
// The IR emitter on many laptop cameras (Luxvisions, Bison, Realtek UVC) is
// not a standard V4L2 control. It is activated by sending a vendor-specific
// UVC_SET_CUR request to a particular XU unit/selector with a specific byte
// sequence.
//
// This module replays a pre-discovered configuration (stored in
// /etc/face-auth/ir-emitter.toml) to activate the emitter when an auth
// session begins, and optionally restores the original value after the session.
//
// Discovery is done once by `face-auth ir-discover` (see tests/phase0/src/ir_discover.rs).
//
// Credit: linux-enable-ir-emitter (EmixamPP et al.)
//   https://github.com/EmixamPP/linux-enable-ir-emitter
//   Mechanism independently re-implemented in Rust to avoid external dependency.

use std::fs;
use std::os::fd::RawFd;
use thiserror::Error;

// ──────────────────────────────────────────────
// UVC XU ioctl — UVCIOC_CTRL_QUERY
//
// Defined in linux/uvcvideo.h:
//   struct uvc_xu_control_query { u8 unit; u8 selector; u8 query; u16 size; u8 *data; };
//   UVCIOC_CTRL_QUERY = _IOWR('u', 0x21, struct uvc_xu_control_query)
//
// On x86_64:
//   sizeof(uvc_xu_control_query) = 16  (3×u8 + pad + u16 + pad + ptr)
//   _IOWR = (3<<30) | (16<<16) | ('u'<<8) | 0x21 = 0xC010_7521
// ──────────────────────────────────────────────

const UVCIOC_CTRL_QUERY: u64 = 0xC010_7521;
const UVC_SET_CUR: u8 = 0x01;

#[repr(C)]
struct UvcXuControlQuery {
    unit: u8,
    selector: u8,
    query: u8,
    _pad1: u8,
    size: u16,
    _pad2: u16,
    data: *mut u8,
}

fn uvc_set_cur(fd: RawFd, unit: u8, selector: u8, data: &[u8]) -> std::io::Result<()> {
    let mut buf = data.to_vec();
    let mut req = UvcXuControlQuery {
        unit,
        selector,
        query: UVC_SET_CUR,
        _pad1: 0,
        size: buf.len() as u16,
        _pad2: 0,
        data: buf.as_mut_ptr(),
    };
    let ret = unsafe { libc::ioctl(fd, UVCIOC_CTRL_QUERY, &mut req as *mut _) };
    if ret < 0 {
        Err(std::io::Error::last_os_error())
    } else {
        Ok(())
    }
}

// ──────────────────────────────────────────────
// Config
// ──────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct IrEmitterConfig {
    pub unit: u8,
    pub selector: u8,
    pub enable_data: Vec<u8>,
    /// Original value before activation — used to restore after session.
    /// `None` if not captured during discovery.
    pub disable_data: Option<Vec<u8>>,
}

#[derive(Debug, Error)]
pub enum IrEmitterError {
    #[error("config file not found at {0}")]
    NoConfig(String),
    #[error("config parse error: {0}")]
    ParseError(String),
    #[error("ioctl failed: {0}")]
    IoctlError(#[from] std::io::Error),
}

impl IrEmitterConfig {
    /// Load from /etc/face-auth/ir-emitter.toml (or given path).
    pub fn load(path: &str) -> Result<Self, IrEmitterError> {
        let content =
            fs::read_to_string(path).map_err(|_| IrEmitterError::NoConfig(path.to_string()))?;
        Self::parse(&content)
    }

    fn parse(toml: &str) -> Result<Self, IrEmitterError> {
        // Minimal TOML parser — avoids pulling in a full serde/toml dep in platform crate.
        // Handles: unit = N, selector = N, enable_data = [0x.., ..], disable_data = [..]
        let mut unit = None::<u8>;
        let mut selector = None::<u8>;
        let mut enable_data = None::<Vec<u8>>;
        let mut disable_data = None::<Vec<u8>>;

        for line in toml.lines() {
            let line = line.trim();
            if line.starts_with('#') || line.is_empty() {
                continue;
            }

            if let Some(rest) = line.strip_prefix("unit") {
                let v = parse_int(rest).ok_or_else(|| IrEmitterError::ParseError(line.into()))?;
                unit = Some(v as u8);
            } else if let Some(rest) = line.strip_prefix("selector") {
                let v = parse_int(rest).ok_or_else(|| IrEmitterError::ParseError(line.into()))?;
                selector = Some(v as u8);
            } else if let Some(rest) = line.strip_prefix("enable_data") {
                enable_data = Some(
                    parse_byte_array(rest)
                        .ok_or_else(|| IrEmitterError::ParseError(line.into()))?,
                );
            } else if let Some(rest) = line.strip_prefix("disable_data") {
                disable_data = Some(
                    parse_byte_array(rest)
                        .ok_or_else(|| IrEmitterError::ParseError(line.into()))?,
                );
            }
        }

        Ok(IrEmitterConfig {
            unit: unit.ok_or_else(|| IrEmitterError::ParseError("missing 'unit'".into()))?,
            selector: selector
                .ok_or_else(|| IrEmitterError::ParseError("missing 'selector'".into()))?,
            enable_data: enable_data
                .ok_or_else(|| IrEmitterError::ParseError("missing 'enable_data'".into()))?,
            disable_data,
        })
    }

    /// Send UVC_SET_CUR to activate the IR emitter on the given open camera fd.
    pub fn activate(&self, fd: RawFd) -> Result<(), IrEmitterError> {
        uvc_set_cur(fd, self.unit, self.selector, &self.enable_data)?;
        Ok(())
    }

    /// Restore the original value (deactivate emitter). No-op if disable_data is None.
    pub fn deactivate(&self, fd: RawFd) -> Result<(), IrEmitterError> {
        if let Some(ref data) = self.disable_data {
            uvc_set_cur(fd, self.unit, self.selector, data)?;
        }
        Ok(())
    }
}

// ──────────────────────────────────────────────
// Minimal parsers
// ──────────────────────────────────────────────

fn parse_int(s: &str) -> Option<u64> {
    let s = s.trim().trim_start_matches('=').trim();
    if let Some(hex) = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
        u64::from_str_radix(hex, 16).ok()
    } else {
        s.parse().ok()
    }
}

fn parse_byte_array(s: &str) -> Option<Vec<u8>> {
    // Expects: = [0x01, 0x02, ...]
    let s = s.trim().trim_start_matches('=').trim();
    let s = s.trim_start_matches('[').trim_end_matches(']');
    let mut bytes = Vec::new();
    for tok in s.split(',') {
        let tok = tok.trim();
        if tok.is_empty() {
            continue;
        }
        let b = parse_int(tok)? as u8;
        bytes.push(b);
    }
    Some(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_config() {
        let toml = r#"
# IR emitter config
unit        = 3
selector    = 8
enable_data  = [0x01]
disable_data = [0x00]
"#;
        let cfg = IrEmitterConfig::parse(toml).unwrap();
        assert_eq!(cfg.unit, 3);
        assert_eq!(cfg.selector, 8);
        assert_eq!(cfg.enable_data, vec![0x01]);
        assert_eq!(cfg.disable_data, Some(vec![0x00]));
    }

    #[test]
    fn parse_multi_byte() {
        let toml = "unit = 4\nselector = 10\nenable_data = [0x10, 0x00, 0x01]\n";
        let cfg = IrEmitterConfig::parse(toml).unwrap();
        assert_eq!(cfg.enable_data, vec![0x10, 0x00, 0x01]);
        assert!(cfg.disable_data.is_none());
    }
}

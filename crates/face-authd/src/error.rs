use thiserror::Error;

#[derive(Debug, Error)]
pub enum DaemonError {
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("camera error: {0}")]
    Camera(String),

    #[error("IR emitter error: {0}")]
    IrEmitter(#[from] face_auth_platform::ir_emitter::IrEmitterError),

    #[error("session already active")]
    SessionBusy,

    #[error("task join error: {0}")]
    Join(String),
}

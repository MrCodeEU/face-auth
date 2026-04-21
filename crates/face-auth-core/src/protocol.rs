use serde::{Deserialize, Serialize};

pub const PROTOCOL_VERSION: u32 = 1;

#[derive(Serialize, Deserialize, Debug)]
pub enum PamRequest {
    Auth {
        version: u32,
        username: String,
        session_id: u64,
    },
    Cancel {
        session_id: u64,
    },
}

#[derive(Serialize, Deserialize, Debug)]
pub enum DaemonMessage {
    Feedback {
        session_id: u64,
        state: FeedbackState,
    },
    AuthResult {
        session_id: u64,
        outcome: AuthOutcome,
    },
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub enum FeedbackState {
    Scanning,
    TooFar,
    TooClose,
    TurnLeft,
    TurnRight,
    TiltUp,
    TiltDown,
    IRSaturated,
    EyesNotVisible,
    LookAtCamera,
    Authenticating,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub enum AuthOutcome {
    Success,
    Failed,
    Timeout,
    DaemonUnavailable,
    Cancelled,
}

#[derive(Serialize, Deserialize, Debug)]
pub enum UiMessage {
    SessionStarted { username: String },
    Feedback { state: FeedbackState },
    SessionEnded { outcome: AuthOutcome },
}

use serde::{de::DeserializeOwned, Serialize};
use std::io::{self, Read, Write};

/// Write a bincode-serialized message with 4-byte LE length prefix.
pub fn write_message<W: Write, T: Serialize>(writer: &mut W, msg: &T) -> io::Result<()> {
    let payload =
        bincode::serialize(msg).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    let len = payload.len() as u32;
    writer.write_all(&len.to_le_bytes())?;
    writer.write_all(&payload)
}

/// Read a bincode-serialized message with 4-byte LE length prefix.
pub fn read_message<R: Read, T: DeserializeOwned>(reader: &mut R) -> io::Result<T> {
    let mut len_buf = [0u8; 4];
    reader.read_exact(&mut len_buf)?;
    let len = u32::from_le_bytes(len_buf) as usize;
    let mut payload = vec![0u8; len];
    reader.read_exact(&mut payload)?;
    bincode::deserialize(&payload).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::{AuthOutcome, DaemonMessage};
    use std::io::Cursor;

    #[test]
    fn roundtrip_daemon_message() {
        let msg = DaemonMessage::AuthResult {
            session_id: 42,
            outcome: AuthOutcome::Success,
        };
        let mut buf = Vec::new();
        write_message(&mut buf, &msg).unwrap();
        let mut cursor = Cursor::new(buf);
        let decoded: DaemonMessage = read_message(&mut cursor).unwrap();
        match decoded {
            DaemonMessage::AuthResult {
                session_id: 42,
                outcome: AuthOutcome::Success,
            } => {}
            _ => panic!("unexpected decoded message"),
        }
    }
}

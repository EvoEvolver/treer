use super::*;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum TerminalBinaryKind {
    Ready = 1,
    Output = 2,
    Input = 3,
}

impl TryFrom<u8> for TerminalBinaryKind {
    type Error = ProtocolError;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            1 => Ok(Self::Ready),
            2 => Ok(Self::Output),
            3 => Ok(Self::Input),
            _ => Err(ProtocolError::new(
                "invalid_terminal_frame",
                format!("unknown terminal binary frame kind {value}"),
            )),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TerminalBinaryFrame {
    pub kind: TerminalBinaryKind,
    pub session_id: String,
    pub revision: u64,
    pub payload: Vec<u8>,
}

impl TerminalBinaryFrame {
    pub fn encode(&self) -> Result<Vec<u8>, ProtocolError> {
        let session = self.session_id.as_bytes();
        let session_len = u16::try_from(session.len()).map_err(|_| {
            ProtocolError::new("invalid_terminal_frame", "terminal session id is too long")
        })?;
        if session.is_empty() {
            return Err(ProtocolError::new(
                "invalid_terminal_frame",
                "terminal session id is empty",
            ));
        }
        let mut encoded =
            Vec::with_capacity(TERMINAL_BINARY_HEADER_LEN + session.len() + self.payload.len());
        encoded.push(TERMINAL_BINARY_VERSION);
        encoded.push(self.kind as u8);
        encoded.extend_from_slice(&session_len.to_be_bytes());
        encoded.extend_from_slice(&self.revision.to_be_bytes());
        encoded.extend_from_slice(session);
        encoded.extend_from_slice(&self.payload);
        Ok(encoded)
    }

    pub fn decode(encoded: &[u8]) -> Result<Self, ProtocolError> {
        if encoded.len() < TERMINAL_BINARY_HEADER_LEN {
            return Err(ProtocolError::new(
                "invalid_terminal_frame",
                "terminal binary frame is shorter than its header",
            ));
        }
        if encoded[0] != TERMINAL_BINARY_VERSION {
            return Err(ProtocolError::new(
                "terminal_binary_version_mismatch",
                format!(
                    "terminal binary frame uses version {}, expected {}",
                    encoded[0], TERMINAL_BINARY_VERSION
                ),
            ));
        }
        let kind = TerminalBinaryKind::try_from(encoded[1])?;
        let session_len = usize::from(u16::from_be_bytes([encoded[2], encoded[3]]));
        let payload_offset = TERMINAL_BINARY_HEADER_LEN
            .checked_add(session_len)
            .filter(|offset| *offset <= encoded.len())
            .ok_or_else(|| {
                ProtocolError::new(
                    "invalid_terminal_frame",
                    "terminal session id exceeds the binary frame",
                )
            })?;
        if session_len == 0 {
            return Err(ProtocolError::new(
                "invalid_terminal_frame",
                "terminal session id is empty",
            ));
        }
        let revision = u64::from_be_bytes(
            encoded[4..12]
                .try_into()
                .map_err(|_| ProtocolError::new("invalid_terminal_frame", "invalid revision"))?,
        );
        let session_id = std::str::from_utf8(&encoded[12..payload_offset])
            .map_err(|_| {
                ProtocolError::new("invalid_terminal_frame", "terminal session id is not UTF-8")
            })?
            .to_string();
        Ok(Self {
            kind,
            session_id,
            revision,
            payload: encoded[payload_offset..].to_vec(),
        })
    }
}

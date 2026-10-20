//! Source-side encoding boundary. No route, transport or partial publication.
use chrono::{DateTime, Utc};
use std::io::{self, Write};
use treer_protocol::{
    MachineMetricsQuery, MachineMetricsResponse, MACHINE_METRICS_MAX_RESPONSE_BYTES,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EncodingError {
    InvalidResponse,
    TooLarge,
    Cancelled,
    Serialization,
}

/// Validate the typed model before serializing into a private capped buffer.
/// Callers must publish only the returned complete bytes, never writer chunks.
/// The caller owns deadline/disconnect and final epoch authorization checks.
pub fn encode_bounded(
    response: &MachineMetricsResponse,
    query: &MachineMetricsQuery,
    server_id: &str,
    now: DateTime<Utc>,
    cancelled: &mut impl FnMut() -> bool,
) -> Result<Vec<u8>, EncodingError> {
    if cancelled() {
        return Err(EncodingError::Cancelled);
    }
    response
        .validate_model(query, server_id, now)
        .map_err(|_| EncodingError::InvalidResponse)?;
    let mut writer = BoundedWriter {
        // Allocate only after bounded model validation. Avoid Vec growth beyond
        // the wire cap; allocator metadata/RSS are not part of this byte bound.
        bytes: Vec::with_capacity(MACHINE_METRICS_MAX_RESPONSE_BYTES),
        cancelled,
        error: None,
    };
    if serde_json::to_writer(&mut writer, response).is_err() {
        return Err(writer.error.unwrap_or(EncodingError::Serialization));
    }
    writer
        .flush()
        .map_err(|_| writer.error.unwrap_or(EncodingError::Serialization))?;
    Ok(writer.bytes)
}

struct BoundedWriter<'a, F> {
    bytes: Vec<u8>,
    cancelled: &'a mut F,
    error: Option<EncodingError>,
}

impl<F: FnMut() -> bool> Write for BoundedWriter<'_, F> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.flush()?;
        if bytes.len() > MACHINE_METRICS_MAX_RESPONSE_BYTES - self.bytes.len() {
            self.error = Some(EncodingError::TooLarge);
            return Err(io::Error::other("machine_metrics_response_too_large"));
        }
        self.bytes.extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        if (self.cancelled)() {
            self.error = Some(EncodingError::Cancelled);
        }
        if self.error.is_some() {
            Err(io::Error::other("machine_metrics_encoding_refused"))
        } else {
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wire_cap_accepts_exact_bytes_and_refuses_one_more_before_append() {
        let mut cancelled = || false;
        let mut writer = BoundedWriter {
            bytes: Vec::with_capacity(MACHINE_METRICS_MAX_RESPONSE_BYTES),
            cancelled: &mut cancelled,
            error: None,
        };
        writer
            .write_all(&vec![b'x'; MACHINE_METRICS_MAX_RESPONSE_BYTES])
            .unwrap();
        assert_eq!(writer.bytes.len(), MACHINE_METRICS_MAX_RESPONSE_BYTES);
        assert!(writer.write_all(b"x").is_err());
        assert_eq!(writer.error, Some(EncodingError::TooLarge));
        assert_eq!(writer.bytes.len(), MACHINE_METRICS_MAX_RESPONSE_BYTES);
        assert!(writer.flush().is_err());
    }
}

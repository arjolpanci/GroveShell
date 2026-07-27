use crate::Envelope;
use std::io::{Read, Write};
use groveshell_common::{Error, Result};

/// Refuses to allocate a read buffer larger than this for a single frame,
/// so a corrupt or malicious length prefix can't trigger an unbounded
/// allocation.
const MAX_FRAME_BYTES: u32 = 1_048_576; // 1 MiB

/// Writes one length-prefixed, JSON-encoded envelope: a 4-byte
/// little-endian length followed by that many bytes of JSON.
pub fn write_envelope<W: Write>(writer: &mut W, envelope: &Envelope) -> Result<()> {
    let body = serde_json::to_vec(envelope)?;
    let len = u32::try_from(body.len())
        .map_err(|_| Error::Protocol("envelope too large to frame".to_string()))?;
    writer.write_all(&len.to_le_bytes())?;
    writer.write_all(&body)?;
    writer.flush()?;
    Ok(())
}

/// Reads and decodes exactly one length-prefixed envelope. Blocks until a
/// full frame is available or the stream ends/errors. Rejects zero-length
/// frames, oversized frames, and envelopes whose `protocol_version` this
/// build doesn't understand.
pub fn read_envelope<R: Read>(reader: &mut R) -> Result<Envelope> {
    let mut len_bytes = [0u8; 4];
    reader.read_exact(&mut len_bytes)?;
    let len = u32::from_le_bytes(len_bytes);
    if len == 0 || len > MAX_FRAME_BYTES {
        return Err(Error::Protocol(format!("invalid frame length {len}")));
    }

    let mut body = vec![0u8; len as usize];
    reader.read_exact(&mut body)?;
    let envelope: Envelope = serde_json::from_slice(&body)?;

    if envelope.protocol_version != crate::PROTOCOL_VERSION {
        return Err(Error::Protocol(format!(
            "unsupported protocol_version {} (expected {})",
            envelope.protocol_version,
            crate::PROTOCOL_VERSION
        )));
    }

    Ok(envelope)
}

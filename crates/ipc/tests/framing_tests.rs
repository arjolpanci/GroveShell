use std::io::Cursor;
use groveshell_ipc::framing::{read_envelope, write_envelope};
use groveshell_ipc::{message_type, Envelope, PROTOCOL_VERSION};

#[test]
fn round_trips_a_single_envelope_through_an_in_memory_buffer() {
    let original = Envelope::new("groveshell-cli", message_type::PING, serde_json::json!({}));

    let mut buffer = Vec::new();
    write_envelope(&mut buffer, &original).expect("write should succeed");

    let mut cursor = Cursor::new(buffer);
    let decoded = read_envelope(&mut cursor).expect("read should succeed");

    assert_eq!(decoded.protocol_version, PROTOCOL_VERSION);
    assert_eq!(decoded.request_id, original.request_id);
    assert_eq!(decoded.sender, "groveshell-cli");
    assert_eq!(decoded.message_type, message_type::PING);
}

#[test]
fn round_trips_multiple_envelopes_back_to_back_on_the_same_stream() {
    let first = Envelope::new("a", message_type::PING, serde_json::json!(1));
    let second = Envelope::new("b", message_type::PONG, serde_json::json!(2));

    let mut buffer = Vec::new();
    write_envelope(&mut buffer, &first).unwrap();
    write_envelope(&mut buffer, &second).unwrap();

    let mut cursor = Cursor::new(buffer);
    let decoded_first = read_envelope(&mut cursor).unwrap();
    let decoded_second = read_envelope(&mut cursor).unwrap();

    assert_eq!(decoded_first.sender, "a");
    assert_eq!(decoded_second.sender, "b");
}

#[test]
fn rejects_a_zero_length_frame() {
    let mut buffer = Vec::new();
    buffer.extend_from_slice(&0u32.to_le_bytes());

    let mut cursor = Cursor::new(buffer);
    let result = read_envelope(&mut cursor);

    assert!(result.is_err());
}

#[test]
fn rejects_an_oversized_frame_length() {
    let mut buffer = Vec::new();
    buffer.extend_from_slice(&u32::MAX.to_le_bytes());

    let mut cursor = Cursor::new(buffer);
    let result = read_envelope(&mut cursor);

    assert!(result.is_err());
}

#[test]
fn rejects_an_envelope_with_the_wrong_protocol_version() {
    let mut envelope = Envelope::new("x", message_type::PING, serde_json::json!({}));
    envelope.protocol_version = PROTOCOL_VERSION + 1;
    let body = serde_json::to_vec(&envelope).unwrap();

    let mut buffer = Vec::new();
    buffer.extend_from_slice(&(body.len() as u32).to_le_bytes());
    buffer.extend_from_slice(&body);

    let mut cursor = Cursor::new(buffer);
    let result = read_envelope(&mut cursor);

    assert!(result.is_err());
}

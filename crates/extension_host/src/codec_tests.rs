use std::io::Cursor;

use extension_protocol::{Method, RequestEnvelope};
use serde_json::json;

use super::*;

fn reader(payload: &str) -> Cursor<Vec<u8>> {
    Cursor::new(payload.as_bytes().to_vec())
}

#[test]
fn a_frame_round_trips_through_the_codec() {
    let message = Message::Request(RequestEnvelope::new(
        "1",
        Method::WorkspaceGetContext,
        json!({}),
    ));
    let mut buffer = Vec::new();
    write_message(&mut buffer, &message).expect("frame writes");
    assert!(buffer.ends_with(b"\n"), "frames are newline terminated");

    let decoded = read_message(&mut Cursor::new(buffer)).expect("frame reads");
    assert_eq!(decoded, message);
}

#[test]
fn several_frames_are_read_in_order() {
    let payload = concat!(
        r#"{"protocol":1,"request_id":"1","method":"file.open","params":{}}"#,
        "\n",
        r#"{"protocol":1,"request_id":"2","method":"notification.show","params":{}}"#,
        "\n",
    );
    let mut stream = reader(payload);

    let Message::Request(first) = read_message(&mut stream).expect("first frame") else {
        panic!("expected a request");
    };
    let Message::Request(second) = read_message(&mut stream).expect("second frame") else {
        panic!("expected a request");
    };
    assert_eq!(first.request_id, "1");
    assert_eq!(second.request_id, "2");
    assert!(matches!(read_message(&mut stream), Err(CodecError::Eof)));
}

#[test]
fn blank_lines_are_skipped_rather_than_reported() {
    let payload = concat!(
        "\n\n",
        r#"{"protocol":1,"request_id":"1","method":"file.open","params":{}}"#,
        "\n",
    );
    let message = read_message(&mut reader(payload)).expect("the real frame is found");
    assert!(matches!(message, Message::Request(_)));
}

#[test]
fn a_truncated_final_frame_is_malformed_not_silently_accepted() {
    let payload = r#"{"protocol":1,"request_id":"1","method":"file.open""#;
    let error = read_message(&mut reader(payload)).expect_err("a truncated frame fails");
    assert!(matches!(error, CodecError::Malformed(_)));
}

#[test]
fn malformed_json_is_reported_as_invalid_request() {
    let error = read_message(&mut reader("not json\n")).expect_err("garbage fails");
    assert_eq!(
        error.to_extension_error().code,
        extension_protocol::ErrorCode::InvalidRequest
    );
}

#[test]
fn a_wrong_protocol_version_is_reported_as_a_mismatch() {
    let payload = r#"{"protocol":99,"request_id":"1","method":"file.open","params":{}}"#;
    let error = read_message(&mut reader(&format!("{payload}\n"))).expect_err("version fails");
    assert!(matches!(
        error,
        CodecError::ProtocolMismatch {
            found: 99,
            expected: 1
        }
    ));
    assert_eq!(
        error.to_extension_error().code,
        extension_protocol::ErrorCode::ProtocolMismatch
    );
}

#[test]
fn an_oversized_frame_is_rejected_and_the_stream_resynchronises() {
    let oversized = "x".repeat(MAX_MESSAGE_BYTES + 1);
    let payload = format!(
        "{oversized}\n{}\n",
        r#"{"protocol":1,"request_id":"2","method":"file.open","params":{}}"#
    );
    let mut stream = reader(&payload);

    let error = read_message(&mut stream).expect_err("the oversized frame is rejected");
    assert!(matches!(error, CodecError::FrameTooLarge { .. }));
    assert_eq!(
        error.to_extension_error().code,
        extension_protocol::ErrorCode::InvalidRequest
    );

    let Message::Request(next) = read_message(&mut stream).expect("the next frame still reads")
    else {
        panic!("expected a request");
    };
    assert_eq!(next.request_id, "2");
}

#[test]
fn an_unterminated_oversized_frame_is_still_rejected() {
    let oversized = "x".repeat(MAX_MESSAGE_BYTES + 1);
    let error = read_message(&mut reader(&oversized)).expect_err("the frame is rejected");
    assert!(matches!(error, CodecError::FrameTooLarge { .. }));
}

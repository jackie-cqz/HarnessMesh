use hmesh_adapters::{JsonlDecoder, ProtocolObserver, native_record_limit};
use hmesh_core::{ErrorCode, HarnessKind};
use serde_json::json;

#[test]
fn pinned_protocol_fixtures_complete_only_at_the_correct_boundary() {
    for (kind, source) in [
        (
            HarnessKind::Pi,
            include_str!("../../../tests/fixtures/pi-0.84.2.jsonl"),
        ),
        (
            HarnessKind::Dsh,
            include_str!("../../../tests/fixtures/dsh-0.2.0-rc.2.jsonl"),
        ),
        (
            HarnessKind::Opencode,
            include_str!("../../../tests/fixtures/opencode-1.18.20.jsonl"),
        ),
    ] {
        let mut decoder = JsonlDecoder::new(65536);
        let mut observer = ProtocolObserver::new(kind);
        for record in decoder.push(source.as_bytes()).expect("fixture JSONL") {
            observer.observe(&record);
        }
        decoder.finish().expect("complete records");
        observer
            .finish(Some(0))
            .expect("successful fixture outcome");
    }
}

#[test]
fn jsonl_preserves_unicode_line_separators_and_handles_fragmented_utf8() {
    let bytes = "{\"text\":\"a\u{2028}中\u{2029}b\"}\r\n".as_bytes();
    let mut decoder = JsonlDecoder::new(1024);
    let mut records = vec![];
    for byte in bytes {
        records.extend(decoder.push(&[*byte]).expect("fragment"));
    }
    assert_eq!(records, vec![json!({"text":"a\u{2028}中\u{2029}b"})]);
    decoder.finish().expect("complete");
}

#[test]
fn malformed_truncated_and_overlong_jsonl_are_rejected() {
    assert!(JsonlDecoder::new(20).push(b"invalid\n").is_err());
    assert!(JsonlDecoder::new(4).push(b"12345").is_err());
    let mut decoder = JsonlDecoder::new(20);
    decoder.push(b"{\"x\":1}").expect("partial");
    assert!(decoder.finish().is_err());
}

#[test]
fn native_frames_are_bounded_independently_of_mesh_event_size() {
    let budget = 8 * 1024 * 1024;
    assert_eq!(
        native_record_limit(HarnessKind::Pi, budget),
        4 * 1024 * 1024
    );
    for kind in [HarnessKind::Pi, HarnessKind::Dsh, HarnessKind::Opencode] {
        assert_eq!(native_record_limit(kind, 32768), 32768);
        let record = serde_json::to_vec(&json!({"snapshot":"中".repeat(50000)})).unwrap();
        assert!(record.len() > 65536);
        let mut decoder = JsonlDecoder::new(native_record_limit(kind, budget));
        let mut decoded = vec![];
        for chunk in record.chunks(8191) {
            decoded.extend(decoder.push(chunk).expect("fragmented large snapshot"));
        }
        decoded.extend(decoder.push(b"\n").unwrap());
        assert_eq!(decoded.len(), 1);
        assert_eq!(decoded[0]["snapshot"], "中".repeat(50000));
        assert_eq!(decoder.largest_record_bytes(), record.len());
        assert_eq!(decoder.record_count(), 1);
        decoder.finish().unwrap();
    }
    let record = br#"{"x":1}"#;
    let mut decoder = JsonlDecoder::new(record.len());
    decoder.push(record).unwrap();
    assert_eq!(decoder.push(b"\n").unwrap(), vec![json!({"x":1})]);
    let mut over = JsonlDecoder::new(record.len());
    over.push(record).unwrap();
    let error = over.push(b" ").expect_err("one byte beyond limit");
    assert_eq!(error.code, ErrorCode::RateLimited);
}

#[test]
fn pi_prompt_ack_and_agent_end_do_not_prove_success() {
    let mut observer = ProtocolObserver::new(HarnessKind::Pi);
    observer.observe(&json!({"type":"response","command":"prompt","success":true}));
    observer.observe(&json!({"type":"agent_end"}));
    assert_eq!(
        observer.finish(Some(0)).expect_err("not settled").code,
        ErrorCode::TaskUnknown
    );
    observer
        .observe(&json!({"type":"message_end","message":{"role":"assistant","stopReason":"stop"}}));
    observer.observe(&json!({"type":"agent_settled"}));
    observer
        .finish(Some(0))
        .expect("settled successful assistant");
}

#[test]
fn dsh_final_does_not_override_nonzero_exit_or_error() {
    let mut observer = ProtocolObserver::new(HarnessKind::Dsh);
    observer.observe(&json!({"type":"final","text":"done"}));
    assert!(observer.finish(Some(1)).is_err());
    observer.observe(&json!({"type":"error","message":"failed"}));
    assert!(observer.finish(Some(0)).is_err());
}

#[test]
fn pi_internal_retry_uses_last_assistant_outcome_without_closing_input_early() {
    let mut observer = ProtocolObserver::new(HarnessKind::Pi);
    observer.observe(
        &json!({"type":"message_end","message":{"role":"assistant","stopReason":"error"}}),
    );
    assert!(!observer.failed_or_waiting());
    assert!(!observer.settled());
    observer
        .observe(&json!({"type":"message_end","message":{"role":"assistant","stopReason":"stop"}}));
    observer.observe(&json!({"type":"agent_settled"}));
    observer.finish(Some(0)).expect("successful internal retry");
    observer.observe(
        &json!({"type":"message_end","message":{"role":"assistant","stopReason":"error"}}),
    );
    assert!(observer.finish(Some(0)).is_err());
}

#[test]
fn opencode_tool_step_is_not_a_successful_final_and_permissions_do_not_auto_approve() {
    let mut observer = ProtocolObserver::new(HarnessKind::Opencode);
    observer.observe(&json!({"type":"step_finish","part":{"reason":"tool-calls"}}));
    assert!(observer.finish(Some(0)).is_err());
    observer.observe(&json!({"type":"error","error":{"name":"PermissionRejectedError"}}));
    assert_eq!(
        observer
            .finish(Some(0))
            .expect_err("approval required")
            .code,
        ErrorCode::FailedNeedsInput
    );
}

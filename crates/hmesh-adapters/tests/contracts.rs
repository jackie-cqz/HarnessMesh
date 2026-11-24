use hmesh_adapters::acp::{self, Incoming};
use hmesh_adapters::{AcpTaskDriver, JsonlDecoder, ObservedEvent, native_record_limit};
use hmesh_core::ErrorCode;
use serde_json::{Value, json};
use std::path::Path;

fn driver(allowed: &[&str]) -> AcpTaskDriver {
    let allowed: Vec<String> = allowed.iter().map(|s| (*s).to_owned()).collect();
    AcpTaskDriver::new(Path::new("/project"), "do it", vec![], &allowed)
}
fn initialized(driver: &mut AcpTaskDriver) {
    let first = driver.take_outbound();
    assert_eq!(first[0]["method"], "initialize");
    assert_eq!(first[0]["params"]["protocolVersion"], 1);
    assert_eq!(
        first[0]["params"]["clientCapabilities"]["fs"]["readTextFile"],
        false
    );
    driver.observe(&json!({"jsonrpc":"2.0","id":1,"result":{"protocolVersion":1}}));
    let new = driver.take_outbound();
    assert_eq!(new[0]["method"], "session/new");
    assert_eq!(new[0]["params"]["cwd"], "/project");
    // mcpServers is required by session/new even when empty.
    assert_eq!(new[0]["params"]["mcpServers"], json!([]));
    let events = driver.observe(&json!({"jsonrpc":"2.0","id":2,"result":{"sessionId":"s1"}}));
    assert!(matches!(events[..], [ObservedEvent::Started]));
    let prompt = driver.take_outbound();
    assert_eq!(prompt[0]["method"], "session/prompt");
    assert_eq!(prompt[0]["params"]["sessionId"], "s1");
    assert_eq!(
        prompt[0]["params"]["prompt"],
        json!([{"type":"text","text":"do it"}])
    );
}
fn update(update: Value) -> Value {
    json!({"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"s1","update":update}})
}

#[test]
fn a_turn_succeeds_only_when_the_prompt_ends_with_end_turn() {
    let mut d = driver(&["read"]);
    initialized(&mut d);
    let events = d.observe(&update(
        json!({"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"hello"}}),
    ));
    assert!(matches!(&events[..], [ObservedEvent::TextDelta { text }] if text == "hello"));
    assert!(!d.settled());
    assert_eq!(d.finish().unwrap_err().code, ErrorCode::TaskUnknown);
    d.observe(&json!({"jsonrpc":"2.0","id":3,"result":{"stopReason":"end_turn"}}));
    assert!(d.settled());
    d.finish().expect("finished turn");
}

#[test]
fn other_stop_reasons_and_errors_fail_the_run() {
    for (reason, code) in [
        ("refusal", ErrorCode::AgentUnavailable),
        ("max_tokens", ErrorCode::AgentUnavailable),
        ("cancelled", ErrorCode::TaskUnknown),
    ] {
        let mut d = driver(&[]);
        initialized(&mut d);
        d.observe(&json!({"jsonrpc":"2.0","id":3,"result":{"stopReason":reason}}));
        assert!(d.settled());
        assert_eq!(d.finish().unwrap_err().code, code, "{reason}");
    }
    let mut d = driver(&[]);
    d.take_outbound();
    d.observe(&json!({"jsonrpc":"2.0","id":1,"error":{"code":-32000,"message":"auth required"}}));
    let error = d.finish().unwrap_err();
    assert!(error.message.contains("initialize") && error.message.contains("auth required"));
    let mut d = driver(&[]);
    d.take_outbound();
    d.observe(&json!({"jsonrpc":"2.0","id":1,"result":{"protocolVersion":2}}));
    assert_eq!(d.finish().unwrap_err().code, ErrorCode::AdapterNotSupported);
}

#[test]
fn tool_calls_become_start_and_end_events() {
    let mut d = driver(&["read"]);
    initialized(&mut d);
    let start = d.observe(&update(json!({"sessionUpdate":"tool_call","toolCallId":"t1","title":"Read file","kind":"read","status":"pending"})));
    assert!(
        matches!(&start[..], [ObservedEvent::ToolEvent { name, phase }] if name == "read" && phase == "tool_execution_start")
    );
    assert!(
        d.observe(&update(
            json!({"sessionUpdate":"tool_call_update","toolCallId":"t1","status":"in_progress"})
        ))
        .is_empty()
    );
    let end = d.observe(&update(
        json!({"sessionUpdate":"tool_call_update","toolCallId":"t1","status":"completed"}),
    ));
    assert!(
        matches!(&end[..], [ObservedEvent::ToolEvent { name, phase }] if name == "read" && phase == "tool_execution_end")
    );
}

#[test]
fn permission_requests_follow_the_project_allowlist() {
    let mut d = driver(&["read", "write", "edit"]);
    initialized(&mut d);
    let options = json!([{"optionId":"yes","name":"Allow once","kind":"allow_once"},{"optionId":"always","name":"Always","kind":"allow_always"},{"optionId":"no","name":"Reject","kind":"reject_once"}]);
    d.observe(&json!({"jsonrpc":"2.0","id":"p1","method":"session/request_permission","params":{"sessionId":"s1","toolCall":{"toolCallId":"t1","kind":"edit"},"options":options}}));
    let answer = d.take_outbound();
    assert_eq!(answer[0]["id"], "p1");
    assert_eq!(
        answer[0]["result"]["outcome"],
        json!({"outcome":"selected","optionId":"yes"})
    );
    // A shell command is outside this allowlist: refused, and the run needed input.
    let events = d.observe(&json!({"jsonrpc":"2.0","id":7,"method":"session/request_permission","params":{"sessionId":"s1","toolCall":{"toolCallId":"t2","kind":"execute"},"options":options}}));
    assert!(matches!(events[..], [ObservedEvent::ApprovalRequested]));
    assert_eq!(
        d.take_outbound()[0]["result"]["outcome"],
        json!({"outcome":"selected","optionId":"no"})
    );
    d.observe(&json!({"jsonrpc":"2.0","id":3,"result":{"stopReason":"end_turn"}}));
    assert_eq!(d.finish().unwrap_err().code, ErrorCode::FailedNeedsInput);
}

#[test]
fn unoffered_client_methods_get_method_not_found() {
    let mut d = driver(&[]);
    initialized(&mut d);
    d.observe(&json!({"jsonrpc":"2.0","id":9,"method":"fs/read_text_file","params":{"path":"/etc/passwd"}}));
    let answer = d.take_outbound();
    assert_eq!(answer[0]["id"], 9);
    assert_eq!(answer[0]["error"]["code"], -32601);
}

#[test]
fn usage_comes_from_the_prompt_response() {
    let mut d = driver(&[]);
    initialized(&mut d);
    let events = d.observe(&json!({"jsonrpc":"2.0","id":3,"result":{"stopReason":"end_turn","usage":{"inputTokens":30,"outputTokens":10,"totalTokens":40,"cachedReadTokens":2}}}));
    let usage = events
        .iter()
        .find_map(|e| match e {
            ObservedEvent::Usage { usage } => Some(usage.clone()),
            _ => None,
        })
        .expect("usage");
    assert_eq!(usage.total_tokens, Some(40));
    assert_eq!(usage.cache_read_tokens, Some(2));
    assert_eq!(usage.cache_write_tokens, None);
    // Out-of-range or textual numbers are never invented into totals.
    assert!(
        acp::usage_from_prompt(&json!({"usage":{"inputTokens":"30","totalTokens":2000000000u64}}))
            .is_none()
    );
}

#[test]
fn frames_classify_as_responses_requests_and_notifications() {
    assert_eq!(
        acp::classify(&json!({"jsonrpc":"2.0","id":4,"result":{}})),
        Some(Incoming::Response {
            id: 4,
            result: Ok(json!({}))
        })
    );
    assert!(matches!(
        acp::classify(
            &json!({"jsonrpc":"2.0","id":"x","method":"session/request_permission","params":{}})
        ),
        Some(Incoming::Request { .. })
    ));
    assert!(matches!(
        acp::classify(&json!({"jsonrpc":"2.0","method":"session/update","params":{}})),
        Some(Incoming::Notification { .. })
    ));
    // Permission choice never picks a remembered option when a one-time one exists.
    let only_always = [json!({"optionId":"a","kind":"allow_always"})];
    assert_eq!(
        acp::permission_outcome(&only_always, true)["outcome"]["optionId"],
        "a"
    );
    assert_eq!(
        acp::permission_outcome(&only_always, false)["outcome"],
        json!({"outcome":"cancelled"})
    );
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
    assert_eq!(native_record_limit(budget), 4 * 1024 * 1024);
    assert_eq!(native_record_limit(32768), 32768);
    let record = serde_json::to_vec(&json!({"snapshot":"中".repeat(50000)})).unwrap();
    assert!(record.len() > 65536);
    let mut decoder = JsonlDecoder::new(native_record_limit(budget));
    let mut decoded = vec![];
    for chunk in record.chunks(8191) {
        decoded.extend(decoder.push(chunk).expect("fragmented large snapshot"));
    }
    decoded.extend(decoder.push(b"\n").unwrap());
    assert_eq!(decoded.len(), 1);
    assert_eq!(decoded[0]["snapshot"], "中".repeat(50000));
    assert_eq!(decoder.largest_record_bytes(), record.len());
    decoder.finish().unwrap();
    let record = br#"{"x":1}"#;
    let mut over = JsonlDecoder::new(record.len());
    over.push(record).unwrap();
    let error = over.push(b" ").expect_err("one byte beyond limit");
    assert_eq!(error.code, ErrorCode::RateLimited);
}

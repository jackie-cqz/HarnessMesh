//! Executable ACP contract checks run against scripted frames. This matrix is
//! fixture evidence of the client's protocol handling, never of a model run.
use crate::{AcpTaskDriver, JsonlDecoder};
use serde_json::{Value, json};
use std::path::Path;

fn driver() -> AcpTaskDriver {
    AcpTaskDriver::new(Path::new("/project"), "hello", vec![], &["read".into()])
}
fn feed(driver: &mut AcpTaskDriver, frames: &[Value]) {
    for frame in frames {
        driver.observe(frame);
    }
}
fn handshake() -> Vec<Value> {
    vec![
        json!({"jsonrpc":"2.0","id":1,"result":{"protocolVersion":1,"agentCapabilities":{}}}),
        json!({"jsonrpc":"2.0","id":2,"result":{"sessionId":"s"}}),
    ]
}
/// Scripted ACP turns covering success, framing and every refusal path.
pub fn matrix() -> Value {
    let mut ok = driver();
    let mut frames = handshake();
    frames.push(json!({"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"s","update":{"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"done"}}}}));
    frames.push(json!({"jsonrpc":"2.0","id":3,"result":{"stopReason":"end_turn"}}));
    feed(&mut ok, &frames);
    let success = ok.finish().is_ok();

    let wire: String = frames.iter().map(|f| format!("{f}\n")).collect();
    let mut decoder = JsonlDecoder::new(1024 * 1024);
    let mut fragmented = driver();
    for chunk in wire.as_bytes().chunks(7) {
        for frame in decoder.push(chunk).unwrap_or_default() {
            fragmented.observe(&frame);
        }
    }
    let fragments = fragmented.finish().is_ok() && decoder.finish().is_ok();

    let mut unfinished = driver();
    feed(&mut unfinished, &handshake());
    let mut refused = driver();
    let mut refusal = handshake();
    refusal.push(json!({"jsonrpc":"2.0","id":3,"result":{"stopReason":"refusal"}}));
    feed(&mut refused, &refusal);
    let mut wrong_version = driver();
    feed(
        &mut wrong_version,
        &[json!({"jsonrpc":"2.0","id":1,"result":{"protocolVersion":99}})],
    );
    let mut asked = driver();
    let mut ask = handshake();
    ask.push(json!({"jsonrpc":"2.0","id":"p1","method":"session/request_permission","params":{"sessionId":"s","toolCall":{"toolCallId":"t","kind":"execute"},"options":[{"optionId":"no","name":"Reject","kind":"reject_once"}]}}));
    ask.push(json!({"jsonrpc":"2.0","id":3,"result":{"stopReason":"end_turn"}}));
    feed(&mut asked, &ask);

    json!({
        "protocol":"acp",
        "protocol_version":crate::acp::PROTOCOL_VERSION,
        "rows":[{
            "harness":"acp",
            "fixture_success":success,
            "fragmented_protocol":fragments,
            "missing_completion_rejected":unfinished.finish().is_err(),
            "refusal_rejected":refused.finish().is_err(),
            "protocol_version_mismatch_rejected":wrong_version.finish().is_err(),
            "disallowed_tool_needs_input":asked.finish().is_err(),
            "malformed_json_rejected":JsonlDecoder::new(128).push(b"invalid\n").is_err(),
            "oversized_frame_rejected":JsonlDecoder::new(4).push(b"12345").is_err(),
            "usage_schema":"session/prompt result.usage",
            "real_model":"not_checked",
        }],
        "real_model_called":false
    })
}

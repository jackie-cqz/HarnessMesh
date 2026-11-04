//! This matrix is executable fixture evidence, never evidence of a model run.
use crate::{JsonlDecoder, ProtocolObserver};
use hmesh_core::HarnessKind;
use serde_json::{Value, json};
pub fn versions(kind: HarnessKind) -> &'static [&'static str] {
    match kind {
        HarnessKind::Pi => &["0.84.2", "1.4.0"],
        HarnessKind::Dsh => &["0.2.0-rc.2"],
        HarnessKind::Opencode => &["1.18.20"],
    }
}
pub fn matrix() -> Value {
    let rows:Vec<_>=[(HarnessKind::Pi,include_str!("../../../tests/fixtures/pi-0.84.2.jsonl")),(HarnessKind::Dsh,include_str!("../../../tests/fixtures/dsh-0.2.0-rc.2.jsonl")),(HarnessKind::Opencode,include_str!("../../../tests/fixtures/opencode-1.18.20.jsonl"))].into_iter().map(|(kind,source)| {
        let mut decoder=JsonlDecoder::new(65536);let mut observer=ProtocolObserver::new(kind);
        let valid=decoder.push(source.as_bytes()).map(|rs|{for r in rs {observer.observe(&r);}}).is_ok()&&decoder.finish().is_ok()&&observer.finish(Some(0)).is_ok();
        let mut fragmented=JsonlDecoder::new(65536);let mut o=ProtocolObserver::new(kind);let mut fragments=true;
        for byte in source.as_bytes() {match fragmented.push(&[*byte]) {Ok(rs)=>for r in rs {o.observe(&r);},Err(_)=>fragments=false,}}
        let fragment_pass=fragments&&fragmented.finish().is_ok()&&o.finish(Some(0)).is_ok();
        json!({"harness":kind,"approved_versions":versions(kind),"fixture_success":valid,"fragmented_protocol":fragment_pass,"nonzero_exit_rejected":observer.finish(Some(1)).is_err(),"missing_completion_rejected":ProtocolObserver::new(kind).finish(Some(0)).is_err(),"malformed_json_rejected":JsonlDecoder::new(128).push(b"invalid\n").is_err(),"oversized_frame_rejected":JsonlDecoder::new(4).push(b"12345").is_err(),"usage_schema":match kind{HarnessKind::Pi=>"message_end.usage",HarnessKind::Opencode=>"step_finish.tokens",HarnessKind::Dsh=>"not_established"},"real_model":"not_checked","native_cancel":"requires_process_regression"})
    }).collect();
    json!({"evidence":"synthetic-protocol-fixtures","rows":rows,"real_model_called":false})
}

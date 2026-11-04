use hmesh_adapters::{ProtocolObserver, compatibility};
use hmesh_core::HarnessKind;
use serde_json::json;
#[test]
fn usage_is_from_numeric_reports_not_output_bytes_and_duplicate_ids_are_ignored() {
    let mut o = ProtocolObserver::new(HarnessKind::Pi);
    o.observe(&json!({"type":"message_update","assistantMessageEvent":{"type":"text_delta","delta":"long output"}}));
    assert!(o.usage().total_tokens.is_none());
    let first = json!({"type":"message_end","message":{"id":"a","role":"assistant","stopReason":"toolUse","usage":{"input":30,"output":10,"cacheRead":0,"cacheWrite":0,"totalTokens":40}}});
    o.observe(&first);
    o.observe(&first);
    assert_eq!(o.usage().total_tokens, Some(40));
    assert_eq!(o.usage().reports, 1);
    o.observe(&json!({"type":"message_end","message":{"id":"b","role":"assistant","stopReason":"stop","usage":{"input":10,"output":5,"cacheRead":2,"cacheWrite":1,"totalTokens":18}}}));
    assert_eq!(o.usage().total_tokens, Some(58));
    assert_eq!(o.usage().cache_read_tokens, Some(2));
    let mut dsh = ProtocolObserver::new(HarnessKind::Dsh);
    dsh.observe(&json!({"type":"final","usage":{"total_tokens":900}}));
    assert!(dsh.usage().total_tokens.is_none());
}
#[test]
fn partial_and_malformed_usage_does_not_invent_totals_or_pricing() {
    let mut o = ProtocolObserver::new(HarnessKind::Opencode);
    o.observe(&json!({"type":"step_finish","part":{"id":"a","reason":"stop","tokens":{"input":20,"output":5,"cache":{"read":3,"write":0}}}}));
    assert_eq!(o.usage().input_tokens, Some(20));
    assert!(o.usage().total_tokens.is_none());
    let mut pi = ProtocolObserver::new(HarnessKind::Pi);
    pi.observe(&json!({"type":"message_end","message":{"role":"assistant","usage":{"input":-1,"output":"123","totalTokens":1000000001u64}}}));
    assert_eq!(pi.usage().reports, 0);
}
#[test]
fn compatibility_matrix_runs_negative_and_fragmented_cases_for_every_adapter() {
    let matrix = compatibility::matrix();
    assert_eq!(matrix["rows"].as_array().unwrap().len(), 3);
    for row in matrix["rows"].as_array().unwrap() {
        for name in [
            "fixture_success",
            "fragmented_protocol",
            "nonzero_exit_rejected",
            "missing_completion_rejected",
            "malformed_json_rejected",
            "oversized_frame_rejected",
        ] {
            assert_eq!(row[name], true, "{} {name}", row["harness"]);
        }
    }
    assert_eq!(matrix["real_model_called"], false);
}

use hmesh_adapters::compatibility;

#[test]
fn compatibility_matrix_runs_negative_and_fragmented_acp_cases() {
    let matrix = compatibility::matrix();
    assert_eq!(matrix["protocol"], "acp");
    let rows = matrix["rows"].as_array().unwrap();
    assert_eq!(rows.len(), 1);
    for name in [
        "fixture_success",
        "fragmented_protocol",
        "missing_completion_rejected",
        "refusal_rejected",
        "protocol_version_mismatch_rejected",
        "disallowed_tool_needs_input",
        "malformed_json_rejected",
        "oversized_frame_rejected",
    ] {
        assert_eq!(rows[0][name], true, "{name}");
    }
    assert_eq!(matrix["real_model_called"], false);
}

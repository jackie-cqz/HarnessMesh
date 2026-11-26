//! What other computers read. A paired computer may run an older
//! HarnessMesh, which reads agent cards strictly: an unknown field or an
//! unknown `harness` value makes it reject the whole agent list. Agents added
//! to the catalog later must still look like one of the original kinds there.
use hmesh_core::{AgentCard, AgentKindNote, AgentSpec, CATALOG, HarnessKind, MeshNode, agents};
use serde_json::{Value, json};
use uuid::Uuid;

/// The agent card as HarnessMesh read it before the catalog (v0.0.2,
/// `crates/hmesh-core/src/lib.rs` at 7bc7995), copied so later changes to the
/// real one cannot drift from it.
mod before_the_catalog {
    use serde::{Deserialize, Serialize};
    use serde_json::Value;
    use uuid::Uuid;

    #[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
    #[serde(rename_all = "lowercase")]
    pub enum HarnessKind {
        Pi,
        Dsh,
        Opencode,
        #[serde(rename = "claude-code")]
        ClaudeCode,
        Codex,
        Acp,
    }
    #[derive(Debug, Serialize, Deserialize)]
    #[serde(deny_unknown_fields)]
    pub struct ProbeReport {
        pub installed: bool,
        pub version: Option<String>,
        pub version_supported: bool,
        pub protocol_probe: String,
        pub authentication: String,
        pub managed_run: String,
        pub streaming: String,
        pub cancel: String,
        pub session_resume: String,
        pub unavailable_reason: Option<String>,
    }
    #[derive(Debug, Serialize, Deserialize)]
    #[serde(deny_unknown_fields)]
    pub struct AgentCard {
        pub agent_id: Uuid,
        pub node_id: Uuid,
        pub name: String,
        pub harness: HarnessKind,
        pub project_ids: Vec<String>,
        pub availability: String,
        pub capabilities: Vec<String>,
        pub probe: ProbeReport,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        pub config_options: Vec<Value>,
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        pub service_configured: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub default_model: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub own_login: Option<String>,
    }
}

/// An agent added to the catalog after the original six.
static LATER: AgentSpec = AgentSpec {
    kind: "later-agent",
    label: "Later Agent",
    command: Some("later-acp"),
    args: &["acp"],
    ..agents::BLANK
};

fn card_json(harness: &str) -> Value {
    json!({
        "agent_id": Uuid::new_v4(), "node_id": Uuid::new_v4(), "name": format!("studio-{harness}"),
        "harness": harness, "project_ids": ["demo"], "availability": "idle", "capabilities": [],
        "probe": {"installed": true, "version": "1.0", "version_supported": true,
            "protocol_probe": "acp_initialize_passed", "authentication": "not_checked",
            "managed_run": "passed", "streaming": "supported", "cancel": "supported",
            "session_resume": "unsupported", "unavailable_reason": null},
        "service_configured": true, "default_model": "m", "own_login": "configured"
    })
}

#[test]
fn an_older_computer_reads_the_card_of_an_agent_added_later() {
    let mut card: AgentCard = serde_json::from_value(card_json("acp")).unwrap();
    // Even set to the newer row directly, the card is written as its family.
    card.harness = HarnessKind::of(&LATER);
    let written = serde_json::to_value(&card).unwrap();
    assert_eq!(written["harness"], "acp");
    let mut list = vec![written];
    for spec in CATALOG {
        let mut card: AgentCard = serde_json::from_value(card_json("acp")).unwrap();
        card.harness = HarnessKind::of(spec);
        list.push(serde_json::to_value(&card).unwrap());
    }
    let read: Vec<before_the_catalog::AgentCard> =
        serde_json::from_value(Value::Array(list)).unwrap();
    assert_eq!(read[0].harness, before_the_catalog::HarnessKind::Acp);
    assert_eq!(read.len(), CATALOG.len() + 1);
    // And reading it back here keeps the family.
    let back: AgentCard = serde_json::from_value(serde_json::to_value(&card).unwrap()).unwrap();
    assert_eq!(back.harness, card.harness.family());
}

#[test]
fn the_original_kinds_are_written_exactly_as_older_computers_write_them() {
    for kind in ["pi", "dsh", "opencode", "claude-code", "codex", "acp"] {
        let sent = card_json(kind);
        let old: before_the_catalog::AgentCard = serde_json::from_value(sent.clone()).unwrap();
        let new: AgentCard = serde_json::from_value(sent).unwrap();
        assert_eq!(new.harness.to_string(), kind);
        assert_eq!(new.harness.family(), new.harness);
        // Same fields, same values, both ways.
        assert_eq!(
            serde_json::to_value(&new).unwrap(),
            serde_json::to_value(&old).unwrap()
        );
        let again: before_the_catalog::AgentCard =
            serde_json::from_value(serde_json::to_value(&new).unwrap()).unwrap();
        assert_eq!(again.harness, old.harness);
    }
}

#[test]
fn a_card_stays_strict_about_what_it_does_not_know() {
    assert!(serde_json::from_value::<AgentCard>(card_json("later-agent")).is_err());
    let mut extra = card_json("pi");
    extra["kind"] = json!("later-agent");
    assert!(serde_json::from_value::<AgentCard>(extra).is_err());
}

#[test]
fn kind_notes_tolerate_what_later_versions_add() {
    let id = Uuid::new_v4();
    let notes: Vec<AgentKindNote> = serde_json::from_value(json!([
        {"agent_id": id, "kind": "some-future-agent", "label": "Future", "icon": "f.svg"}
    ]))
    .unwrap();
    assert_eq!(notes[0].kind, "some-future-agent");
    assert!(notes[0].plausible());
    let noisy = AgentKindNote {
        label: "x".repeat(101),
        ..notes[0].clone()
    };
    assert!(!noisy.plausible());
    let control = AgentKindNote {
        label: "Bad\nLabel".into(),
        ..notes[0].clone()
    };
    assert!(!control.plausible());

    // Today's agents need none, so a node's view stays as it was.
    let node: MeshNode = serde_json::from_value(json!({
        "node_id": id, "name": "studio", "local": true, "reachable": true, "agents": []
    }))
    .unwrap();
    assert!(node.agent_kinds.is_empty());
    assert!(
        serde_json::to_value(&node)
            .unwrap()
            .get("agent_kinds")
            .is_none()
    );
    let note = AgentKindNote::of(id, HarnessKind::of(&LATER)).unwrap();
    assert_eq!(
        (note.kind.as_str(), note.label.as_str()),
        ("later-agent", "Later Agent")
    );
}

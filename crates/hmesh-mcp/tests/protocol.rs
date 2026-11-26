use hmesh_core::*;
use hmesh_mcp::{Backend, Protocol, ToolCall, agent_ref, parse_call, resolve};
use serde_json::{Value, json};
use std::{future::Future, pin::Pin};
use uuid::Uuid;
struct Mock;
impl Backend for Mock {
    fn call(&mut self, call: ToolCall) -> Pin<Box<dyn Future<Output = Result<Value>> + Send + '_>> {
        Box::pin(async move {
            match call {
                ToolCall::Agents(_) => Ok(json!({"agents":[]})),
                _ => Err(MeshError::new(
                    ErrorCode::PermissionDenied,
                    "fixture denied",
                )),
            }
        })
    }
}
async fn initialized(version: &str) -> (Protocol<Mock>, Value) {
    let mut server = Protocol::new(Mock);
    let reply = server
        .handle(json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":version,"capabilities":{},"clientInfo":{"name":"fixture","version":"1"}}}))
        .await
        .unwrap();
    (server, reply)
}
#[tokio::test]
async fn handshake_tool_list_notifications_and_errors_follow_mcp_boundaries() {
    let mut server = Protocol::new(Mock);
    assert_eq!(
        server
            .handle(json!({"jsonrpc":"2.0","id":1,"method":"tools/list"}))
            .await
            .unwrap()["error"]["code"],
        -32600
    );
    let (mut server, reply) = initialized("2025-11-25").await;
    assert_eq!(reply["result"]["protocolVersion"], "2025-11-25");
    // Clients may list tools before or after sending notifications/initialized,
    // and may pass a cursor; neither is rejected.
    let list = server
        .handle(json!({"jsonrpc":"2.0","id":"list","method":"tools/list","params":{"cursor":null}}))
        .await
        .unwrap();
    let names = list["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|tool| tool["name"].as_str().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(
        names,
        [
            "mesh_list_agents",
            "mesh_send",
            "mesh_wait",
            "mesh_get_session",
            "mesh_cancel",
            "mesh_close"
        ]
    );
    assert!(
        server
            .handle(json!({"jsonrpc":"2.0","method":"notifications/initialized"}))
            .await
            .is_none()
    );
    let response = server
        .handle(json!({"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"mesh_list_agents","_meta":{"progressToken":3}}}))
        .await
        .unwrap();
    assert_eq!(response["result"]["isError"], false);
    assert_eq!(response["result"]["structuredContent"]["agents"], json!([]));
    let denied = server
        .handle(json!({"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"mesh_wait","arguments":{"session_id":Uuid::new_v4()}}}))
        .await
        .unwrap();
    assert_eq!(denied["result"]["isError"], true);
    assert!(denied.get("error").is_none());
    assert_eq!(
        server
            .handle(json!({"jsonrpc":"2.0","id":null,"method":"ping"}))
            .await
            .unwrap()["error"]["code"],
        -32600
    );
    assert_eq!(
        server
            .handle(json!({"jsonrpc":"2.0","id":5,"method":"owner.approve"}))
            .await
            .unwrap()["error"]["code"],
        -32601
    );
    assert!(
        server
            .handle(
                json!({"jsonrpc":"2.0","method":"notifications/cancelled","params":{"requestId":3}})
            )
            .await
            .is_none()
    );
}
#[tokio::test]
async fn older_client_protocol_versions_are_echoed_and_unknown_ones_get_the_latest() {
    let (_, reply) = initialized("2025-06-18").await;
    assert_eq!(reply["result"]["protocolVersion"], "2025-06-18");
    let (_, reply) = initialized("2099-01-01").await;
    assert_eq!(reply["result"]["protocolVersion"], "2025-11-25");
}
#[test]
fn tools_reject_arbitrary_execution_options_invalid_ids_and_unbounded_requests() {
    assert!(parse_call("mesh_list_agents", json!({"token":"not-accepted"})).is_err());
    assert!(parse_call("mesh_get_session", json!({"session_id":"not-uuid"})).is_err());
    assert!(
        parse_call(
            "mesh_get_session",
            json!({"session_id":Uuid::new_v4(),"after":-1})
        )
        .is_err()
    );
    assert!(
        parse_call(
            "mesh_wait",
            json!({"session_id":Uuid::new_v4(),"wait_seconds":601})
        )
        .is_err()
    );
    let mut args = json!({"agent":"windows/pi-backend","message":"write a fixture"});
    assert!(parse_call("mesh_send", args.clone()).is_ok());
    // Either a new session (agent) or a follow-up (session_id), never both.
    args["session_id"] = json!(Uuid::new_v4());
    assert!(parse_call("mesh_send", args.clone()).is_err());
    args.as_object_mut().unwrap().remove("agent");
    assert!(parse_call("mesh_send", args.clone()).is_ok());
    args.as_object_mut().unwrap().remove("session_id");
    assert!(parse_call("mesh_send", args.clone()).is_err());
    args["agent"] = json!("windows/pi-backend");
    args["executable"] = json!("arbitrary.exe");
    assert!(parse_call("mesh_send", args.clone()).is_err());
    args.as_object_mut().unwrap().remove("executable");
    args["wait_seconds"] = json!(601);
    assert!(parse_call("mesh_send", args.clone()).is_err());
    args["wait_seconds"] = json!(600);
    args["message"] = json!("中".repeat(30000));
    assert!(parse_call("mesh_send", args.clone()).is_err());
    args["message"] = json!("   ");
    assert!(parse_call("mesh_send", args).is_err());
    // Task-era tool names are gone.
    assert!(parse_call("mesh_get_task", json!({"task_id":Uuid::new_v4()})).is_err());
    assert!(parse_call("mesh_submit_task", json!({})).is_err());
}

fn card(node: Uuid, name: &str) -> AgentCard {
    serde_json::from_value(json!({
        "agent_id":Uuid::new_v4(),"node_id":node,"name":name,"harness":"pi","project_ids":["demo"],"availability":"idle","capabilities":[],
        "probe":{"installed":true,"version":"fixture","version_supported":true,"protocol_probe":"fixture","authentication":"fixture","managed_run":"passed","streaming":"fixture","cancel":"not_checked","session_resume":"not_checked","unavailable_reason":null}
    }))
    .unwrap()
}
fn node(name: &str, local: bool, agents: &[&str]) -> MeshNode {
    let node_id = Uuid::new_v4();
    MeshNode {
        node_id,
        name: name.into(),
        local,
        reachable: true,
        access: None,
        error: None,
        agents: agents.iter().map(|a| card(node_id, a)).collect(),
        projects: vec![],
    }
}
#[test]
fn projects_resolve_by_name_or_id_among_those_a_computer_offers() {
    use hmesh_core::projects::SharedProject;
    let shared = |id: &str, name: &str| SharedProject {
        project_id: id.into(),
        name: name.into(),
    };
    let mut studio = node("studio", false, &["codex"]);
    assert!(
        hmesh_mcp::resolve_project(&studio, "web")
            .unwrap_err()
            .message
            .contains("offers no projects")
    );
    studio.projects = vec![
        shared("p-1", "web"),
        shared("p-2", "api"),
        shared("p-3", "api"),
    ];
    assert_eq!(hmesh_mcp::resolve_project(&studio, " web ").unwrap(), "p-1");
    assert_eq!(hmesh_mcp::resolve_project(&studio, "p-3").unwrap(), "p-3");
    assert!(
        hmesh_mcp::resolve_project(&studio, "api")
            .unwrap_err()
            .message
            .contains("pass a project ID")
    );
    let unknown = hmesh_mcp::resolve_project(&studio, "docs")
        .unwrap_err()
        .message;
    assert!(unknown.contains("available: web, api, api"), "{unknown}");
    // A follow-up cannot move a session to another project.
    let call = hmesh_mcp::parse_call(
        "mesh_send",
        serde_json::json!({"session_id":Uuid::new_v4(),"message":"hi","project":"web"}),
    );
    assert!(call.is_err());
}
#[test]
fn agent_references_resolve_by_node_and_name_id_or_unique_name() {
    let mut nodes = vec![
        node("mac", true, &["codex", "pi"]),
        node("windows", false, &["pi", "dsh"]),
    ];
    let (found_node, found) = resolve(&nodes, "Windows/PI").unwrap();
    assert_eq!(found_node.name, "windows");
    assert_eq!(found.name, "pi");
    assert_eq!(resolve(&nodes, "dsh").unwrap().0.name, "windows");
    let id = nodes[0].agents[0].agent_id;
    assert_eq!(resolve(&nodes, &id.to_string()).unwrap().1.agent_id, id);
    let prefix = &id.to_string()[..8];
    assert_eq!(
        resolve(&nodes, &format!("mac/{prefix}"))
            .unwrap()
            .1
            .agent_id,
        id
    );
    let ambiguous = resolve(&nodes, "pi").unwrap_err().message;
    assert!(ambiguous.contains("mac/pi") && ambiguous.contains("windows/pi"));
    let unknown = resolve(&nodes, "linux/pi").unwrap_err().message;
    assert!(unknown.contains("known agents"));
    nodes.push(MeshNode {
        reachable: false,
        error: Some("timeout".into()),
        ..node("linux", false, &[])
    });
    assert!(
        resolve(&nodes, "linux/pi")
            .unwrap_err()
            .message
            .contains("unreachable nodes: linux")
    );
    // Duplicate names on one node fall back to the agent ID in the reference.
    let twins = node("box", false, &["pi", "pi"]);
    let reference = agent_ref(std::slice::from_ref(&twins), &twins, &twins.agents[1]);
    assert_eq!(reference, format!("box/{}", twins.agents[1].agent_id));
    // Two nodes left at one name are told apart by node ID prefix, and the
    // reference resolves.
    let first = node("local-node", true, &["pi"]);
    let second = node("local-node", false, &["pi"]);
    let pair = vec![first.clone(), second.clone()];
    let reference = agent_ref(&pair, &second, &second.agents[0]);
    assert_eq!(
        reference,
        format!("{}/pi", &second.node_id.to_string()[..8])
    );
    assert_eq!(
        resolve(&pair, &reference).unwrap().0.node_id,
        second.node_id
    );
}
#[test]
fn invalid_arguments_report_the_reason() {
    let missing = parse_call("mesh_send", json!({"message":"hi"})).unwrap_err();
    assert!(
        missing.message.contains("agent") && missing.message.contains("session_id"),
        "{}",
        missing.message
    );
}

use hmesh_core::*;
use hmesh_mcp::{Backend, Protocol, ToolCall, parse_call};
use serde_json::{Value, json};
use std::{future::Future, pin::Pin};
use uuid::Uuid;
struct Mock;
impl Backend for Mock {
    fn call(&mut self, call: ToolCall) -> Pin<Box<dyn Future<Output = Result<Value>> + Send + '_>> {
        Box::pin(async move {
            match call {
                ToolCall::Peers => Ok(json!({"peers":[]})),
                _ => Err(MeshError::new(
                    ErrorCode::PermissionDenied,
                    "fixture denied",
                )),
            }
        })
    }
}
async fn ready() -> Protocol<Mock> {
    let mut server = Protocol::new(Mock);
    let reply=server.handle(json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"fixture","version":"1"}}})).await.unwrap();
    assert_eq!(reply["result"]["protocolVersion"], "2025-11-25");
    assert!(
        server
            .handle(json!({"jsonrpc":"2.0","method":"notifications/initialized"}))
            .await
            .is_none()
    );
    server
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
    let mut server = ready().await;
    let list = server
        .handle(json!({"jsonrpc":"2.0","id":"list","method":"tools/list"}))
        .await
        .unwrap();
    let names = list["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|tool| tool["name"].as_str().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(names.len(), 5);
    assert!(
        !names
            .iter()
            .any(|name| name.contains("approve") || name.contains("policy"))
    );
    let response=server.handle(json!({"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"mesh_list_peers","_meta":{"progressToken":3}}})).await.unwrap();
    assert_eq!(response["result"]["isError"], false);
    assert_eq!(response["result"]["structuredContent"]["peers"], json!([]));
    let denied=server.handle(json!({"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"mesh_list_agents","arguments":{}}})).await.unwrap();
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
#[test]
fn tools_reject_arbitrary_execution_options_invalid_ids_and_unbounded_requests() {
    assert!(parse_call("mesh_list_peers", json!({"token":"not-accepted"})).is_err());
    assert!(parse_call("mesh_get_task", json!({"task_id":"not-uuid"})).is_err());
    assert!(
        parse_call(
            "mesh_get_task",
            json!({"task_id":Uuid::new_v4(),"after":-1})
        )
        .is_err()
    );
    assert!(
        parse_call(
            "mesh_get_task",
            json!({"task_id":Uuid::new_v4(),"wait_seconds":21})
        )
        .is_err()
    );
    let mut args = json!({"peer_id":Uuid::new_v4(),"agent_id":Uuid::new_v4(),"project_id":"test","request_id":Uuid::new_v4(),"goal":"write a fixture"});
    assert!(parse_call("mesh_submit_task", args.clone()).is_ok());
    args["executable"] = json!("arbitrary.exe");
    assert!(parse_call("mesh_submit_task", args.clone()).is_err());
    args.as_object_mut().unwrap().remove("executable");
    args["goal"] = json!("中".repeat(30000));
    assert!(parse_call("mesh_submit_task", args.clone()).is_err());
}

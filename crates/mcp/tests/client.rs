//! Client contract test: real MCP handshake + tool calls over an
//! in-process duplex. The deny-all policy makes this hermetic — the
//! action is rejected by policy before any driver work, so no macOS
//! permissions or UI state are needed.

use dexter_mcp::DexterMcp;
use dexter_policy::Policy;
use dexter_sim::SimDriver;
use rmcp::model::CallToolRequestParam;
use rmcp::service::{RunningService, ServiceExt};
use serde_json::json;

async fn client_server(policy_toml: &str) -> RunningService<rmcp::service::RoleClient, ()> {
    let (client_io, server_io) = tokio::io::duplex(1 << 16);
    let server = DexterMcp::new(
        Policy::from_toml(policy_toml).unwrap(),
        Box::new(SimDriver::new(vec![])),
    );
    tokio::spawn(async move {
        if let Ok(running) = server.serve(tokio::io::split(server_io)).await {
            let _ = running.waiting().await;
        }
    });
    ().serve(tokio::io::split(client_io))
        .await
        .expect("client connects")
}

#[tokio::test]
async fn handshake_and_tool_list() {
    let client = client_server("").await;
    let tools = client.list_all_tools().await.expect("list tools");
    let names: Vec<_> = tools.iter().map(|t| t.name.to_string()).collect();
    for expected in [
        "dexter_observe",
        "dexter_act",
        "dexter_grant",
        "dexter_verify",
        "dexter_task",
        "dexter_journal",
    ] {
        assert!(names.contains(&expected.to_string()), "missing {expected}");
    }
    client.cancel().await.ok();
}

#[tokio::test]
async fn act_denied_by_policy_never_touches_driver() {
    // Deny everything — the policy gate fires before the driver, so this
    // exercises the full MCP path hermetically.
    let client = client_server(
        r#"
        [[rule]]
        action = "*"
        decision = "deny"
        reason = "locked down"
    "#,
    )
    .await;
    let res = client
        .call_tool(CallToolRequestParam {
            name: "dexter_act".into(),
            arguments: Some(
                json!({
                    "action": {"type":"click","target":{"name":"Save"},"button":"left"},
                    "app": "AnyApp",
                })
                .as_object()
                .unwrap()
                .clone(),
            ),
        })
        .await
        .expect("call");
    let text = res.content[0].raw.as_text().expect("text content");
    let status: serde_json::Value = serde_json::from_str(&text.text).unwrap();
    assert_eq!(status["status"], "denied");
    assert!(status["reason"].as_str().unwrap().contains("locked down"));
    client.cancel().await.ok();
}

#[tokio::test]
async fn act_needs_approval_returns_grantable_fingerprint() {
    // Embedded policy -> mutation requires approval. The fingerprint the
    // agent receives is what a human grants out-of-band.
    let client = client_server("").await; // empty file = embedded default
    let res = client
        .call_tool(CallToolRequestParam {
            name: "dexter_act".into(),
            arguments: Some(
                json!({
                    "action": {"type":"click","target":{"name":"Save"},"button":"left"},
                })
                .as_object()
                .unwrap()
                .clone(),
            ),
        })
        .await
        .expect("call");
    let text = res.content[0].raw.as_text().expect("text content");
    let status: serde_json::Value = serde_json::from_str(&text.text).unwrap();
    assert_eq!(status["status"], "needs_approval");
    assert!(status["fingerprint"].as_str().unwrap().contains("Save"));

    // Grant it, then verify the journal shows the approval path.
    let fp = status["fingerprint"].as_str().unwrap().to_string();
    client
        .call_tool(CallToolRequestParam {
            name: "dexter_grant".into(),
            arguments: Some(json!({"fingerprint": fp}).as_object().unwrap().clone()),
        })
        .await
        .expect("grant");

    let res = client
        .call_tool(CallToolRequestParam {
            name: "dexter_journal".into(),
            arguments: None,
        })
        .await
        .expect("journal");
    let text = res.content[0].raw.as_text().expect("text content");
    let journal: serde_json::Value = serde_json::from_str(&text.text).unwrap();
    let kinds: Vec<_> = journal["events"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["kind"].as_str().unwrap().to_string())
        .collect();
    assert!(kinds.contains(&"ActionProposed".to_string()));
    assert!(kinds.contains(&"HumanApprovalRequired".to_string()));
    client.cancel().await.ok();
}

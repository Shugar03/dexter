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
        "dexter_candidates",
        "dexter_cancel",
        "dexter_status",
    ] {
        assert!(names.contains(&expected.to_string()), "missing {expected}");
    }
    client.cancel().await.ok();
}

#[tokio::test]
async fn status_reports_driver_engine_and_journal() {
    let client = client_server("").await;
    let res = client
        .call_tool(CallToolRequestParam {
            name: "dexter_status".into(),
            arguments: None,
        })
        .await
        .expect("status call");
    let text = res.content[0].raw.as_text().unwrap().text.clone();
    let v: serde_json::Value = serde_json::from_str(&text).unwrap();
    assert_eq!(v["driver"]["name"], "sim");
    assert_eq!(v["engine"]["name"], "rule-based");
    assert_eq!(v["engine"]["health"]["status"], "ready");
    assert_eq!(v["task_running"], false);
    assert!(v["journal"]["events"].is_number());
    assert!(v["journal"]["dropped"].is_number());
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
async fn agent_cannot_self_approve_or_request_physical() {
    // Trust is a server-startup decision, not a per-call param: passing
    // approve/coords in the tool call must have no effect.
    let client = client_server("").await;
    let res = client
        .call_tool(CallToolRequestParam {
            name: "dexter_act".into(),
            arguments: Some(
                json!({
                    "action": {"type":"click","target":{"name":"Save"},"button":"left"},
                    "approve": true,
                    "coords": true,
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
    assert_eq!(
        status["status"], "needs_approval",
        "approve:true must not bypass the human grant flow"
    );

    // Physical input likewise: coords:true must not permit a point
    // target when the server didn't opt in.
    let res = client
        .call_tool(CallToolRequestParam {
            name: "dexter_act".into(),
            arguments: Some(
                json!({
                    "action": {"type":"click","target":{"x":10.0,"y":10.0},"button":"left"},
                    "coords": true,
                    "approve": true,
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
    assert_eq!(
        status["status"], "denied",
        "coords:true must not enable physical input: {status}"
    );
    client.cancel().await.ok();
}

#[tokio::test]
async fn operator_opt_in_allows_coords() {
    // The operator opted in at server start: physical actions go through
    // the normal mutation path (needs_approval under embedded policy).
    let (client_io, server_io) = tokio::io::duplex(1 << 16);
    let server = DexterMcp::with_decider(
        Policy::from_toml("").unwrap(),
        Box::new(SimDriver::new(vec![])),
        None,
        dexter_mcp::ServerConfig {
            approve_all: false,
            allow_coords: true,
        },
    );
    tokio::spawn(async move {
        if let Ok(running) = server.serve(tokio::io::split(server_io)).await {
            let _ = running.waiting().await;
        }
    });
    let client = ().serve(tokio::io::split(client_io)).await.unwrap();
    let res = client
        .call_tool(CallToolRequestParam {
            name: "dexter_act".into(),
            arguments: Some(
                json!({
                    "action": {"type":"click","target":{"x":10.0,"y":10.0},"button":"left"},
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
    assert_eq!(
        status["status"], "needs_approval",
        "operator coords opt-in should reach the normal approval path: {status}"
    );
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

fn save_button() -> dexter_core::Element {
    dexter_core::Element {
        id: dexter_core::ElementId(4),
        role: Some("button".into()),
        name: Some("Save".into()),
        actions: vec!["press".into()],
        enabled: Some(true),
        ..Default::default()
    }
}

#[tokio::test]
async fn observe_returns_structured_elements() {
    let (client_io, server_io) = tokio::io::duplex(1 << 16);
    let server = DexterMcp::new(
        Policy::from_toml("").unwrap(),
        Box::new(SimDriver::new(vec![save_button()])),
    );
    tokio::spawn(async move {
        if let Ok(running) = server.serve(tokio::io::split(server_io)).await {
            let _ = running.waiting().await;
        }
    });
    let client = ().serve(tokio::io::split(client_io)).await.unwrap();

    let res = client
        .call_tool(CallToolRequestParam {
            name: "dexter_observe".into(),
            arguments: None,
        })
        .await
        .expect("observe");
    let text = res.content[0].raw.as_text().expect("text");
    let v: serde_json::Value = serde_json::from_str(&text.text).unwrap();
    let els = v["elements"].as_array().expect("elements array");
    assert_eq!(els[0]["id"], "e_4");
    assert_eq!(els[0]["role"], "button");
    assert_eq!(els[0]["name"], "Save");
    assert_eq!(els[0]["enabled"], true);
    client.cancel().await.ok();
}

#[tokio::test]
async fn candidates_returns_ranked_menu_for_the_goal() {
    let (client_io, server_io) = tokio::io::duplex(1 << 16);
    let server = DexterMcp::new(
        Policy::from_toml("").unwrap(),
        Box::new(SimDriver::new(vec![save_button()])),
    );
    tokio::spawn(async move {
        if let Ok(running) = server.serve(tokio::io::split(server_io)).await {
            let _ = running.waiting().await;
        }
    });
    let client = ().serve(tokio::io::split(client_io)).await.unwrap();

    let res = client
        .call_tool(CallToolRequestParam {
            name: "dexter_candidates".into(),
            arguments: Some(
                json!({"goal": "save the document"})
                    .as_object()
                    .unwrap()
                    .clone(),
            ),
        })
        .await
        .expect("candidates");
    let text = res.content[0].raw.as_text().expect("text");
    let v: serde_json::Value = serde_json::from_str(&text.text).unwrap();
    let cands = v["candidates"].as_array().expect("candidates array");
    assert!(
        !cands.is_empty(),
        "goal 'save' should match the Save button"
    );
    assert_eq!(cands[0]["action"]["type"], "click");
    assert!(cands[0]["prior"].as_f64().unwrap() > 0.0);
    client.cancel().await.ok();
}

/// Decider that always routes a long Wait — the task sits in the
/// interruptible sleep so we can cancel it mid-run.
struct WaitForever;
impl dexter_decision::DecisionEngine for WaitForever {
    fn name(&self) -> &str {
        "wait-forever"
    }
    fn decide(
        &self,
        _ctx: &dexter_decision::DecisionContext,
    ) -> Result<dexter_decision::Decision, dexter_decision::DecisionError> {
        Ok(dexter_decision::Decision::Route {
            route: dexter_decision::Route::Wait { millis: 60_000 },
            rationale: "waiting".into(),
        })
    }
}

#[tokio::test]
async fn dexter_cancel_stops_a_waiting_task() {
    let (client_io, server_io) = tokio::io::duplex(1 << 16);
    let server = DexterMcp::with_decider(
        Policy::from_toml("").unwrap(),
        Box::new(SimDriver::new(vec![])),
        Some(Box::new(WaitForever)),
        dexter_mcp::ServerConfig::default(),
    );
    tokio::spawn(async move {
        if let Ok(running) = server.serve(tokio::io::split(server_io)).await {
            let _ = running.waiting().await;
        }
    });
    let client = ().serve(tokio::io::split(client_io)).await.unwrap();

    // Kick off a task that will wait forever; cancel it concurrently.
    let task = client.call_tool(CallToolRequestParam {
        name: "dexter_task".into(),
        arguments: Some(
            json!({
                "goal": "never done",
                "done": {"type":"element_exists","target":{"name":"Nope"}},
                "max_secs": 600,
            })
            .as_object()
            .unwrap()
            .clone(),
        ),
    });
    let cancel = async {
        tokio::time::sleep(std::time::Duration::from_millis(400)).await;
        client
            .call_tool(CallToolRequestParam {
                name: "dexter_cancel".into(),
                arguments: None,
            })
            .await
            .expect("cancel call")
    };
    let (task_res, cancel_res) = tokio::join!(task, cancel);
    let text = cancel_res.content[0].raw.as_text().unwrap().text.clone();
    assert!(text.contains("\"cancelled\":true"), "cancel no-op? {text}");
    let text = task_res.expect("task").content[0]
        .raw
        .as_text()
        .unwrap()
        .text
        .clone();
    let status: serde_json::Value = serde_json::from_str(&text).unwrap();
    assert_eq!(
        status["status"], "cancelled",
        "expected cancelled: {status}"
    );
    client.cancel().await.ok();
}

#[tokio::test]
async fn cancel_with_no_task_is_noop() {
    let client = client_server("").await;
    let res = client
        .call_tool(CallToolRequestParam {
            name: "dexter_cancel".into(),
            arguments: None,
        })
        .await
        .expect("call");
    let text = res.content[0].raw.as_text().unwrap().text.clone();
    assert!(text.contains("\"cancelled\":false"), "{text}");
    client.cancel().await.ok();
}

#[tokio::test]
async fn task_rejects_oversized_goal() {
    let client = client_server("").await;
    let res = client
        .call_tool(CallToolRequestParam {
            name: "dexter_task".into(),
            arguments: Some(
                json!({
                    "goal": "x".repeat(5_000),
                    "done": {"type":"element_exists","target":{"name":"Nope"}},
                })
                .as_object()
                .unwrap()
                .clone(),
            ),
        })
        .await;
    let e = res.expect_err("oversized goal must be rejected");
    assert!(e.to_string().contains("4KB"), "{e}");
    client.cancel().await.ok();
}

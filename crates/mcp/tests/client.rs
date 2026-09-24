//! Client contract test: real MCP handshake + tool calls over an
//! in-process duplex. The deny-all policy makes this hermetic — the
//! action is rejected by policy before any driver work, so no macOS
//! permissions or UI state are needed.

use dexter_mcp::DexterMcp;
use dexter_policy::Policy;
use dexter_sim::{Effect, SimDriver};
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
        "dexter_map",
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

/// Session accounting: every successful tool call is one agent
/// round-trip; the status probe reports the totals and the response
/// volume. `est_response_tokens` is a labelled bytes/4 estimate.
#[tokio::test]
async fn status_reports_session_agent_cost() {
    let client = client_server("").await;
    let res = client
        .call_tool(CallToolRequestParam {
            name: "dexter_observe".into(),
            arguments: None,
        })
        .await
        .expect("observe call");
    assert!(res.content[0].raw.as_text().is_some());

    let res = client
        .call_tool(CallToolRequestParam {
            name: "dexter_status".into(),
            arguments: None,
        })
        .await
        .expect("status call");
    let text = res.content[0].raw.as_text().unwrap().text.clone();
    let v: serde_json::Value = serde_json::from_str(&text).unwrap();
    let session = &v["session"];
    assert_eq!(
        session["tool_calls"]["dexter_observe"], 1,
        "the observe call must be counted: {v}"
    );
    assert_eq!(session["tool_calls_total"], 1);
    assert!(
        session["response_bytes"].as_u64().unwrap() > 0,
        "the observe response's bytes are counted"
    );
    assert!(session["est_response_tokens"].is_number());
    assert_eq!(session["task_internal_steps"], 0);
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
            allow_coords: true,
            ..Default::default()
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
    // agent receives is what a human grants out-of-band — opaque in v2.
    let (client_io, server_io) = tokio::io::duplex(1 << 16);
    let sim = SimDriver::new(vec![save_button()]);
    // The click must change the world — a no-op reports failed, not done.
    sim.on_press(
        dexter_core::SemanticTarget {
            name: Some("Save".into()),
            ..Default::default()
        },
        dexter_sim::Effect::Spawn(dexter_core::Element {
            role: Some("static_text".into()),
            name: Some("saved".into()),
            ..Default::default()
        }),
    );
    let server = DexterMcp::new(
        Policy::from_toml("").unwrap(), // empty file = embedded default
        Box::new(sim),
    );
    tokio::spawn(async move {
        if let Ok(running) = server.serve(tokio::io::split(server_io)).await {
            let _ = running.waiting().await;
        }
    });
    let client = ().serve(tokio::io::split(client_io)).await.unwrap();

    let act = || CallToolRequestParam {
        name: "dexter_act".into(),
        arguments: Some(
            json!({
                "action": {"type":"click","target":{"name":"Save"},"button":"left"},
            })
            .as_object()
            .unwrap()
            .clone(),
        ),
    };
    let res = client.call_tool(act()).await.expect("call");
    let text = res.content[0].raw.as_text().expect("text content");
    let status: serde_json::Value = serde_json::from_str(&text.text).unwrap();
    assert_eq!(status["status"], "needs_approval");
    let fp = status["fingerprint"].as_str().unwrap().to_string();
    assert!(fp.starts_with("sha256:"), "{fp}");

    // Grant it, retry — the identical act now runs to done.
    client
        .call_tool(CallToolRequestParam {
            name: "dexter_grant".into(),
            arguments: Some(json!({"fingerprint": fp}).as_object().unwrap().clone()),
        })
        .await
        .expect("grant");
    let res = client.call_tool(act()).await.expect("retry");
    let text = res.content[0].raw.as_text().expect("text content");
    let status: serde_json::Value = serde_json::from_str(&text.text).unwrap();
    assert_eq!(status["status"], "done", "{status}");

    // The journal shows the approval path.
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

/// The `vision` opt-in is part of the wire contract: the param must be
/// accepted, and on a driver without a vision provider (sim) the
/// observation is simply unchanged — no invented elements, no errors.
#[tokio::test]
async fn observe_accepts_vision_flag() {
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
            arguments: Some(json!({"vision": true}).as_object().unwrap().clone()),
        })
        .await
        .expect("observe");
    let text = res.content[0].raw.as_text().expect("text");
    let v: serde_json::Value = serde_json::from_str(&text.text).unwrap();
    assert_eq!(v["element_count"], 1);
    assert_eq!(v["elements"][0]["source"], "accessibility");
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

#[tokio::test]
async fn second_concurrent_task_is_rejected() {
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

    let task_args = || {
        json!({
            "goal": "never done",
            "done": {"type":"element_exists","target":{"name":"Nope"}},
            "max_secs": 600,
        })
        .as_object()
        .unwrap()
        .clone()
    };
    // Run both concurrently — the second waits 100ms so the first
    // claims the slot, then must be rejected. Afterwards cancel the
    // first so the server drains.
    let first = client.call_tool(CallToolRequestParam {
        name: "dexter_task".into(),
        arguments: Some(task_args()),
    });
    let second = async {
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        client
            .call_tool(CallToolRequestParam {
                name: "dexter_task".into(),
                arguments: Some(task_args()),
            })
            .await
    };
    let cleanup = async {
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
        client
            .call_tool(CallToolRequestParam {
                name: "dexter_cancel".into(),
                arguments: None,
            })
            .await
            .expect("cancel");
    };
    let (r1, r2, _) = tokio::join!(first, second, cleanup);
    let e = r2.expect_err("concurrent task must be rejected");
    assert!(e.to_string().contains("already running"), "{e}");
    let text = r1.expect("first task").content[0]
        .raw
        .as_text()
        .unwrap()
        .text
        .clone();
    assert!(text.contains("cancelled"), "first task cancelled: {text}");
    client.cancel().await.ok();
}

/// `dexter_map` borrows the stage to read window content — that
/// activation is a visible side effect and must pass through policy.
/// A windowless world under embedded policy surfaces a grantable
/// stage fingerprint instead of waking the app; a deny maps the
/// windowless world and reports why.
#[tokio::test]
async fn map_stage_borrow_is_policy_gated() {
    // Sim world has no `role:"window"` element → windowless → the map
    // wants a stage borrow. Embedded policy: launch_app is mutating.
    let client = client_server("").await;
    let res = client
        .call_tool(CallToolRequestParam {
            name: "dexter_map".into(),
            arguments: Some(json!({"app": "other-app"}).as_object().unwrap().clone()),
        })
        .await
        .expect("map call");
    let text = res.content[0].raw.as_text().expect("text");
    let v: serde_json::Value = serde_json::from_str(&text.text).unwrap();
    assert!(
        v["stage"]["fingerprint"]
            .as_str()
            .unwrap_or_default()
            .starts_with("sha256:"),
        "unapproved borrow must surface its fingerprint: {v}"
    );
    assert!(v["stage"]["needs_approval"].is_string(), "{v}");
    client.cancel().await.ok();

    // Under deny-all the borrow is refused outright — the map still
    // returns (windowless world), and no activation occurred.
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
            name: "dexter_map".into(),
            arguments: Some(json!({"app": "other-app"}).as_object().unwrap().clone()),
        })
        .await
        .expect("map call");
    let text = res.content[0].raw.as_text().expect("text");
    let v: serde_json::Value = serde_json::from_str(&text.text).unwrap();
    assert!(
        v["stage"]["denied"]
            .as_str()
            .unwrap_or_default()
            .contains("locked down"),
        "denied borrow must say why: {v}"
    );
    client.cancel().await.ok();
}

/// `wake: false` maps the windowless world without asking for a stage
/// borrow at all — the opt-out must not even evaluate activation.
#[tokio::test]
async fn map_with_wake_disabled_skips_the_borrow() {
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
            name: "dexter_map".into(),
            arguments: Some(
                json!({"app": "other-app", "wake": false})
                    .as_object()
                    .unwrap()
                    .clone(),
            ),
        })
        .await
        .expect("map call");
    let text = res.content[0].raw.as_text().expect("text");
    let v: serde_json::Value = serde_json::from_str(&text.text).unwrap();
    assert_eq!(v["stage"], "not_needed", "{v}");
    client.cancel().await.ok();
}

#[tokio::test]
async fn observe_reports_output_truncation() {
    let elements: Vec<dexter_core::Element> = (1..=501u64)
        .map(|i| dexter_core::Element {
            id: dexter_core::ElementId(i),
            role: Some("button".into()),
            name: Some(format!("Button {i}")),
            actions: vec!["press".into()],
            enabled: Some(true),
            ..Default::default()
        })
        .collect();
    let (client_io, server_io) = tokio::io::duplex(1 << 20);
    let server = DexterMcp::new(
        Policy::from_toml("").unwrap(),
        Box::new(SimDriver::new(elements)),
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
            arguments: Some(json!({"max_elements": 600}).as_object().unwrap().clone()),
        })
        .await
        .expect("observe");
    let text = res.content[0].raw.as_text().expect("text");
    let v: serde_json::Value = serde_json::from_str(&text.text).unwrap();
    assert_eq!(v["element_count"], 501);
    assert_eq!(v["elements_returned"], 500);
    assert_eq!(v["elements_output_truncated"], true);
    client.cancel().await.ok();
}

#[tokio::test]
async fn journal_never_contains_typed_secret() {
    let secret = "DEXTER_SECRET_SENTINEL";
    let field = dexter_core::Element {
        id: dexter_core::ElementId(1),
        role: Some("text_field".into()),
        name: Some("Body".into()),
        actions: vec!["set_value".into(), "focus".into()],
        enabled: Some(true),
        focused: true,
        ..Default::default()
    };
    let (client_io, server_io) = tokio::io::duplex(1 << 16);
    let server = DexterMcp::new(
        Policy::from_toml(
            r#"
            [defaults]
            mutating = "allow"
            "#,
        )
        .unwrap(),
        Box::new(SimDriver::new(vec![field])),
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
                    "action": {"type":"type_text","text":secret,"target":{"role":"text_field","name":"Body"}},
                })
                .as_object()
                .unwrap()
                .clone(),
            ),
        })
        .await
        .expect("act");
    let text = res.content[0].raw.as_text().expect("text content");
    let status: serde_json::Value = serde_json::from_str(&text.text).unwrap();
    assert_eq!(status["status"], "done", "{status}");

    let res = client
        .call_tool(CallToolRequestParam {
            name: "dexter_journal".into(),
            arguments: None,
        })
        .await
        .expect("journal");
    let text = res.content[0].raw.as_text().expect("text content");
    let journal: serde_json::Value = serde_json::from_str(&text.text).unwrap();
    let serialized = serde_json::to_string(&journal).unwrap();
    assert!(!serialized.contains(secret), "journal leaked typed secret");
    client.cancel().await.ok();
}

/// v2 round-trip: the element id observe emits (`"e_1"`) feeds straight
/// into dexter_act's untagged Element target — no parsing, no
/// re-observation required.
#[tokio::test]
async fn observe_element_id_round_trips_into_act() {
    let button = dexter_core::Element {
        id: dexter_core::ElementId(1),
        role: Some("button".into()),
        name: Some("Save".into()),
        actions: vec!["press".into()],
        enabled: Some(true),
        ..Default::default()
    };
    let (client_io, server_io) = tokio::io::duplex(1 << 16);
    let sim = SimDriver::new(vec![button]);
    // The click must change the world — a no-op reports failed, not done.
    sim.on_press(
        dexter_core::SemanticTarget {
            name: Some("Save".into()),
            ..Default::default()
        },
        dexter_sim::Effect::Spawn(dexter_core::Element {
            role: Some("static_text".into()),
            name: Some("saved".into()),
            ..Default::default()
        }),
    );
    let server = DexterMcp::new(
        Policy::from_toml(
            r#"
            [defaults]
            mutating = "allow"
            "#,
        )
        .unwrap(),
        Box::new(sim),
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
    let element = v["elements"][0]["id"].as_str().expect("element id");
    assert_eq!(element, "e_1");
    let observation = v["observation"].as_u64().expect("observation id");

    let res = client
        .call_tool(CallToolRequestParam {
            name: "dexter_act".into(),
            arguments: Some(
                json!({
                    "action": {
                        "type": "click",
                        "target": {"observation": observation, "element": element},
                        "button": "left",
                    },
                })
                .as_object()
                .unwrap()
                .clone(),
            ),
        })
        .await
        .expect("act");
    let text = res.content[0].raw.as_text().expect("text content");
    let status: serde_json::Value = serde_json::from_str(&text.text).unwrap();
    assert_eq!(status["status"], "done", "{status}");
    client.cancel().await.ok();
}

/// A task that hits the approval gate returns `needs_approval` and
/// releases the engine — granting the fingerprint and re-invoking the
/// task consumes the grant and completes.
#[tokio::test]
async fn task_needs_approval_then_grant_and_retry() {
    let sim = SimDriver::new(vec![save_button()]);
    // One press spawns the "Saved" label done_when verifies on.
    sim.on_press(
        dexter_core::SemanticTarget {
            name: Some("Save".into()),
            ..Default::default()
        },
        Effect::Spawn(dexter_core::Element {
            role: Some("static_text".into()),
            name: Some("Saved".into()),
            ..Default::default()
        }),
    );
    let (client_io, server_io) = tokio::io::duplex(1 << 16);
    let server = DexterMcp::new(Policy::embedded(), Box::new(sim));
    tokio::spawn(async move {
        if let Ok(running) = server.serve(tokio::io::split(server_io)).await {
            let _ = running.waiting().await;
        }
    });
    let client = ().serve(tokio::io::split(client_io)).await.unwrap();

    let task = || CallToolRequestParam {
        name: "dexter_task".into(),
        arguments: Some(
            json!({
                "goal": "click save",
                "done": {"type":"element_exists","target":{"name":"Saved"}},
            })
            .as_object()
            .unwrap()
            .clone(),
        ),
    };
    let res = client.call_tool(task()).await.expect("task");
    let text = res.content[0].raw.as_text().expect("text");
    let mut status: serde_json::Value = serde_json::from_str(&text.text).unwrap();
    assert_eq!(status["status"], "needs_approval", "{status}");
    assert!(status["fingerprint"].is_string(), "{status}");

    // Grant + retry: the replanned route produces the same fingerprint,
    // the grant is consumed and the press completes the task. If a step
    // pauses on approval again, grant once more — the contract is that
    // the task pauses cleanly (never spins) and grant+retry progresses.
    let mut grants = 0;
    while status["status"] == "needs_approval" && grants < 3 {
        let fp = status["fingerprint"].as_str().unwrap().to_string();
        client
            .call_tool(CallToolRequestParam {
                name: "dexter_grant".into(),
                arguments: Some(json!({"fingerprint": fp}).as_object().unwrap().clone()),
            })
            .await
            .expect("grant");
        grants += 1;
        let res = client.call_tool(task()).await.expect("task retry");
        let text = res.content[0].raw.as_text().expect("text");
        status = serde_json::from_str(&text.text).unwrap();
    }
    assert_eq!(status["status"], "completed", "{status}");
    client.cancel().await.ok();
}

#[tokio::test]
async fn no_grants_rejects_self_served_approval() {
    // The same channel that returns a needs_approval fingerprint can
    // grant it back — fine for a human on the wire, a self-serve for an
    // autonomous agent. `no_grants` removes that hook: the grant call
    // is rejected outright, approval must arrive out of band.
    let sim = SimDriver::new(vec![save_button()]);
    let (client_io, server_io) = tokio::io::duplex(1 << 16);
    let server = DexterMcp::with_decider(
        Policy::embedded(),
        Box::new(sim),
        None,
        dexter_mcp::ServerConfig {
            no_grants: true,
            ..Default::default()
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
    assert_eq!(status["status"], "needs_approval", "{status}");
    let fp = status["fingerprint"].as_str().unwrap().to_string();

    // The grant call itself fails — the fingerprint cannot be spent
    // from inside the agent channel.
    let grant = client
        .call_tool(CallToolRequestParam {
            name: "dexter_grant".into(),
            arguments: Some(json!({"fingerprint": fp}).as_object().unwrap().clone()),
        })
        .await;
    assert!(
        grant.is_err(),
        "dexter_grant must be rejected under no_grants: {grant:?}"
    );
    client.cancel().await.ok();
}

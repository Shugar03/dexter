//! Hermetic tests for `OpenAiProvider`: a local TcpListener stands in
//! for the API — no network, no keys.

use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::mpsc::{channel, Receiver};
use std::thread;
use std::time::Duration;

use dexter_core::{Action, SemanticTarget, Target};
use dexter_decision::{
    CandidateAction, Decision, DecisionContext, DecisionEngine, DecisionError, EngineHealth,
    OpenAiProvider, Route,
};

/// Serve one canned `body` per accepted connection; returns the
/// endpoint base url and a channel of raw requests received.
fn stub_server(body: &'static str) -> (String, Receiver<String>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let (tx, rx) = channel();
    thread::spawn(move || {
        for stream in listener.incoming() {
            let mut stream = stream.unwrap();
            let mut buf = Vec::new();
            let mut chunk = [0u8; 4096];
            // One request per connection in these tests: read until
            // the body length is satisfied or the socket closes.
            loop {
                let n = stream.read(&mut chunk).unwrap_or(0);
                if n == 0 {
                    break;
                }
                buf.extend_from_slice(&chunk[..n]);
                let text = String::from_utf8_lossy(&buf);
                if let Some(len) = content_length(&text) {
                    if let Some(pos) = text.find("\r\n\r\n") {
                        if text.len() >= pos + 4 + len {
                            break;
                        }
                    }
                }
            }
            let req = String::from_utf8_lossy(&buf).to_string();
            let _ = tx.send(req);
            let resp = format!(
                "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            let _ = stream.write_all(resp.as_bytes());
        }
    });
    (format!("http://127.0.0.1:{port}"), rx)
}

fn stub_server_status(status: u16, body: &'static str) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    thread::spawn(move || {
        if let Some(Ok(mut stream)) = listener.incoming().next() {
            let mut buf = [0u8; 8192];
            let _ = stream.read(&mut buf);
            let resp = format!(
                "HTTP/1.1 {status} ERR\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                body.len(),
            );
            let _ = stream.write_all(resp.as_bytes());
        }
    });
    format!("http://127.0.0.1:{port}")
}

fn content_length(req: &str) -> Option<usize> {
    req.lines().find_map(|l| {
        let (k, v) = l.split_once(':')?;
        k.trim()
            .eq_ignore_ascii_case("content-length")
            .then(|| v.trim().parse().ok())?
    })
}

fn ctx() -> DecisionContext {
    DecisionContext {
        goal: "guardar el documento".into(),
        state_digest: "win TextEdit [button 'Guardar' @10,20]".into(),
        candidates: vec![CandidateAction {
            action: Action::Click {
                target: Target::Semantic(SemanticTarget {
                    role: Some("button".into()),
                    name: Some("Guardar".into()),
                    ..Default::default()
                }),
                button: dexter_core::MouseButton::Left,
            },
            rationale: "label match".into(),
            prior: 0.9,
        }],
        last_error: None,
        step: 1,
    }
}

fn provider(base: &str) -> OpenAiProvider {
    std::env::set_var("DEXTER_TEST_KEY", "sk-test");
    OpenAiProvider::new(
        base,
        "test-model",
        "DEXTER_TEST_KEY",
        Duration::from_secs(5),
    )
}

#[test]
fn act_reply_maps_to_candidate() {
    let (base, rx) = stub_server(
        r#"{"choices":[{"message":{"content":"{\"act\":0,\"why\":\"save button\"}"}}]}"#,
    );
    let engine = provider(&base);
    match engine.decide(&ctx()).unwrap() {
        Decision::Act {
            candidate_index,
            action,
            ..
        } => {
            assert_eq!(candidate_index, Some(0));
            assert!(matches!(action, Action::Click { .. }));
        }
        other => panic!("expected Act, got {other:?}"),
    }
    let req = rx.recv().unwrap();
    assert!(req.contains("authorization: Bearer sk-test"));
    assert!(req.contains("\"test-model\""));
}

#[test]
fn route_reply_maps_to_route() {
    let (base, _rx) = stub_server(
        r#"{"choices":[{"message":{"content":"{\"route\":{\"wait\":500},\"why\":\"loading\"}"}}]}"#,
    );
    let engine = provider(&base);
    match engine.decide(&ctx()).unwrap() {
        Decision::Route { route, .. } => {
            assert!(matches!(route, Route::Wait { millis: 500 }));
        }
        other => panic!("expected Route, got {other:?}"),
    }
}

#[test]
fn garbage_reply_abstains() {
    let (base, _rx) =
        stub_server(r#"{"choices":[{"message":{"content":"I think you should click it"}}]}"#);
    let engine = provider(&base);
    match engine.decide(&ctx()).unwrap() {
        Decision::Route { route, .. } => assert_eq!(route, Route::Abstain),
        other => panic!("expected abstain, got {other:?}"),
    }
}

#[test]
fn out_of_range_candidate_abstains() {
    let (base, _rx) = stub_server(r#"{"choices":[{"message":{"content":"{\"act\":7}"}}]}"#);
    let engine = provider(&base);
    match engine.decide(&ctx()).unwrap() {
        Decision::Route { route, .. } => assert_eq!(route, Route::Abstain),
        other => panic!("expected abstain, got {other:?}"),
    }
}

#[test]
fn http_error_is_engine_error() {
    let base = stub_server_status(429, r#"{"error":{"message":"quota"}}"#);
    let engine = provider(&base);
    match engine.decide(&ctx()) {
        Err(DecisionError::Engine { message, .. }) => assert!(message.contains("429")),
        other => panic!("expected engine error, got {other:?}"),
    }
}

#[test]
fn missing_key_is_engine_error_and_down_health() {
    std::env::remove_var("DEXTER_MISSING_KEY");
    let engine = OpenAiProvider::new(
        "http://127.0.0.1:1",
        "m",
        "DEXTER_MISSING_KEY",
        Duration::from_secs(1),
    );
    assert!(matches!(engine.health(), EngineHealth::Down(_)));
    assert!(matches!(
        engine.decide(&ctx()),
        Err(DecisionError::Engine { .. })
    ));
}

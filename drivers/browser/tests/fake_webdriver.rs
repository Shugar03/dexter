//! Hermetic protocol tests: `BrowserDriver` against a fake W3C
//! WebDriver HTTP endpoint on localhost. No browser involved — the
//! server returns fixture JSON for the DOM walker and records the
//! action scripts it receives.
//!
//! Real Safari E2E is separate, gated on `DEXTER_E2E_BROWSER=1`.

use dexter_browser::BrowserDriver;
use dexter_core::{
    Action, ActionStatus, ElementSource, KeyChord, Mechanism, MouseButton, ObservationScope,
    ScrollDelta, SemanticTarget, Target,
};
use dexter_driver::{ActContext, ComputerDriver, DriverError};
use serde_json::{json, Value};
use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::{Arc, Mutex};

/// What the walker "found" in the fake page.
fn walker_fixture() -> Value {
    json!([
        {"id":0,"parent":null,"depth":0,"role":"main","raw_role":"main",
         "name":null,"value":null,
         "bounds":{"x":0.0,"y":0.0,"w":1280.0,"h":800.0},
         "enabled":true,"focused":false,"actions":["scroll_into_view"],"identifier":null},
        {"id":1,"parent":0,"depth":1,"role":"heading","raw_role":"h1",
         "name":"Checkout","value":"Checkout",
         "bounds":{"x":40.0,"y":20.0,"w":300.0,"h":40.0},
         "enabled":true,"focused":false,"actions":["scroll_into_view"],"identifier":null},
        {"id":2,"parent":0,"depth":1,"role":"text_field","raw_role":"input",
         "name":"Card number","value":null,
         "bounds":{"x":40.0,"y":80.0,"w":400.0,"h":32.0},
         "enabled":true,"focused":false,
         "actions":["press","set_value","focus","scroll_into_view"],"identifier":"card"},
        {"id":3,"parent":0,"depth":1,"role":"button","raw_role":"button",
         "name":"Pay now","value":null,
         "bounds":{"x":40.0,"y":130.0,"w":140.0,"h":36.0},
         "enabled":true,"focused":false,
         "actions":["press","scroll_into_view"],"identifier":"pay"},
        {"id":4,"parent":0,"depth":1,"role":"check_box","raw_role":"input",
         "name":"Save card","value":"false",
         "bounds":{"x":40.0,"y":180.0,"w":20.0,"h":20.0},
         "enabled":true,"focused":false,
         "actions":["press","set_value","focus","scroll_into_view"],"identifier":null},
        {"id":5,"parent":0,"depth":1,"role":"button","raw_role":"button",
         "name":"Apply coupon","value":null,
         "bounds":{"x":200.0,"y":130.0,"w":160.0,"h":36.0},
         "enabled":false,"focused":false,
         "actions":["press","scroll_into_view"],"identifier":"coupon"}
    ])
}

struct FakeServer {
    url: String,
    /// Scripts received at /execute/sync, in order.
    scripts: Arc<Mutex<Vec<String>>>,
    /// JSON bodies posted to /session/:id/actions, in order.
    action_bodies: Arc<Mutex<Vec<Value>>>,
    /// Toggle: make the walker return a *different* tree to simulate a
    /// mutated DOM (stale detection test).
    mutated: Arc<Mutex<bool>>,
    /// Report N iframe collection errors from the walker result.
    iframe_errors: Arc<Mutex<u32>>,
}

/// Minimal HTTP/1.1 WebDriver double: enough protocol for the client,
/// plus script interception.
fn fake_webdriver() -> FakeServer {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let scripts = Arc::new(Mutex::new(Vec::new()));
    let action_bodies = Arc::new(Mutex::new(Vec::new()));
    let mutated = Arc::new(Mutex::new(false));
    let handles = Arc::new(Mutex::new(vec!["h1".to_string()]));
    let current = Arc::new(Mutex::new("h1".to_string()));
    let iframe_errors = Arc::new(Mutex::new(0u32));
    let (s2, m2, h2, c2, e2, a2) = (
        scripts.clone(),
        mutated.clone(),
        handles.clone(),
        current.clone(),
        iframe_errors.clone(),
        action_bodies.clone(),
    );
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let mut stream = match stream {
                Ok(s) => s,
                Err(_) => continue,
            };
            // Read a full request: headers + exactly content-length body
            // bytes (large walker scripts arrive in several TCP reads).
            let mut buf = Vec::new();
            let mut chunk = [0u8; 16384];
            loop {
                let n = stream.read(&mut chunk).unwrap_or(0);
                if n == 0 {
                    break;
                }
                buf.extend_from_slice(&chunk[..n]);
                let s = String::from_utf8_lossy(&buf);
                if let Some(hdr_end) = s.find("\r\n\r\n") {
                    let clen = s[..hdr_end]
                        .lines()
                        .find_map(|l| {
                            l.to_ascii_lowercase()
                                .strip_prefix("content-length:")
                                .and_then(|v| v.trim().parse::<usize>().ok())
                        })
                        .unwrap_or(0);
                    if s.len() >= hdr_end + 4 + clen {
                        break;
                    }
                }
            }
            if buf.is_empty() {
                continue;
            }
            let req = String::from_utf8_lossy(&buf).to_string();
            let (method, path) = {
                let mut parts = req.split_whitespace();
                (
                    parts.next().unwrap_or("").to_string(),
                    parts.next().unwrap_or("").to_string(),
                )
            };
            let body = req.split("\r\n\r\n").nth(1).unwrap_or("");
            let session_re = regex_lite(&path);
            let resp_body: Value = match (method.as_str(), path.as_str()) {
                ("GET", "/status") => json!({"value":{"ready":true,"message":""}}),
                ("POST", "/session") => {
                    json!({"value":{"sessionId":"fake-sid-1","capabilities":{}}})
                }
                ("GET", p) if p.ends_with("/window/handles") => {
                    json!({"value": h2.lock().unwrap().clone()})
                }
                ("GET", p) if p.ends_with("/window") => {
                    json!({"value": c2.lock().unwrap().clone()})
                }
                ("POST", p) if p.ends_with("/window/new") => {
                    let mut hs = h2.lock().unwrap();
                    let h = format!("h{}", hs.len() + 1);
                    hs.push(h.clone());
                    *c2.lock().unwrap() = h.clone();
                    json!({"value":{"handle":h,"type":"tab"}})
                }
                ("POST", p) if p.ends_with("/window") => {
                    let handle = serde_json::from_str::<Value>(body)
                        .ok()
                        .and_then(|b| b["handle"].as_str().map(String::from))
                        .unwrap_or_default();
                    if h2.lock().unwrap().contains(&handle) {
                        *c2.lock().unwrap() = handle;
                        json!({"value":null})
                    } else {
                        json!({"value":{"error":"no such window","message":handle}})
                    }
                }
                ("DELETE", p) if p.ends_with("/window") => {
                    let mut hs = h2.lock().unwrap();
                    let cur = c2.lock().unwrap().clone();
                    hs.retain(|h| *h != cur);
                    if let Some(first) = hs.first() {
                        *c2.lock().unwrap() = first.clone();
                    }
                    json!({"value": hs.clone()})
                }
                ("GET", p) if p.ends_with("/title") => {
                    json!({"value": format!("Fake Checkout ({})", c2.lock().unwrap())})
                }
                ("GET", p) if p.ends_with("/url") => {
                    json!({"value":"https://fake.test/checkout"})
                }
                ("POST", p) if p.ends_with("/url") => json!({"value":null}),
                ("GET", p) if p.ends_with("/screenshot") => {
                    // 1x1 transparent PNG.
                    json!({"value":"iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mNk+M9QDwADhgGAWjR9awAAAABJRU5ErkJggg=="})
                }
                ("POST", p) if p.ends_with("/actions") => {
                    a2.lock()
                        .unwrap()
                        .push(serde_json::from_str::<Value>(body).unwrap_or(json!({})));
                    json!({"value":null})
                }
                ("DELETE", p) if p.ends_with("/actions") => json!({"value":null}),
                ("POST", p) if p.ends_with("/execute/sync") => {
                    let script = serde_json::from_str::<Value>(body)
                        .ok()
                        .and_then(|b| b["script"].as_str().map(String::from))
                        .unwrap_or_default();
                    s2.lock().unwrap().push(script.clone());
                    if script.contains("__dexterNodes?.[5]") {
                        // Node 5 is the disabled "Apply coupon" button:
                        // a real DOM node's el.disabled is true, so the
                        // in-page guard reports 'disabled' — emulate it.
                        json!({"value":{"__dexter_err":"disabled"}})
                    } else if script.contains("return el;") {
                        // Element-reference probe (`return el`): the
                        // driver hands this to pointer actions' origin.
                        json!({"value":{"element-6066-11e4-a52e-4f735466cecf":"fake-el-ref"}})
                    } else if script.contains("innerWidth") {
                        json!({"value":[1280,800]})
                    } else if script.contains("__dexterNodes = nodes") {
                        let errs = *e2.lock().unwrap();
                        if *m2.lock().unwrap() {
                            // DOM changed: "Pay now" renamed → stale.
                            let mut fx = walker_fixture();
                            fx[3]["name"] = json!("Confirm payment");
                            json!({"value":{"elements":fx,"errors":errs}})
                        } else {
                            json!({"value":{"elements":walker_fixture(),"errors":errs}})
                        }
                    } else {
                        json!({"value":"ok"})
                    }
                }
                ("DELETE", p) if session_re && p.matches('/').count() == 2 => {
                    json!({"value":null})
                }
                _ => {
                    json!({"value":{"error":"unknown command","message":format!("{method} {path}")}})
                }
            };
            let out = serde_json::to_vec(&resp_body).unwrap();
            let response = format!(
                "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
                out.len()
            );
            let _ = stream.write_all(response.as_bytes());
            let _ = stream.write_all(&out);
            let _ = stream.flush();
        }
    });
    FakeServer {
        url: format!("http://127.0.0.1:{port}"),
        scripts,
        action_bodies,
        mutated,
        iframe_errors,
    }
}

fn regex_lite(path: &str) -> bool {
    // "/session/<sid>" → exactly 2 slashes, starts with /session/
    path.starts_with("/session/") && path[9..].chars().all(|c| c != '/')
}

#[test]
fn observation_maps_dom_to_elements() {
    let server = fake_webdriver();
    let driver = BrowserDriver::connect(&server.url, "safari").unwrap();

    let obs = driver
        .observe(&ObservationScope::default())
        .expect("observe");

    assert_eq!(obs.elements.len(), 6);
    assert!(!obs.ax_limited);
    assert_eq!(
        obs.app,
        Some(dexter_core::AppSelector::Name("safari".into()))
    );

    let pay = &obs.elements[3];
    assert_eq!(pay.role.as_deref(), Some("button"));
    assert_eq!(pay.name.as_deref(), Some("Pay now"));
    assert!(pay.actions.contains(&"press".to_string()));
    assert_eq!(pay.source, ElementSource::Dom);
    assert!(!obs.digest.is_empty());
}

#[test]
fn semantic_click_dispatches_dom_click() {
    let server = fake_webdriver();
    let driver = BrowserDriver::connect(&server.url, "safari").unwrap();

    let target = Target::Semantic(SemanticTarget {
        role: Some("button".into()),
        name: Some("Pay now".into()),
        ..Default::default()
    });
    let result = driver
        .act(
            &Action::Click {
                target,
                button: MouseButton::Left,
            },
            &ActContext::default(),
        )
        .expect("click");

    assert_eq!(result.mechanism, Mechanism::Dom);
    // The action script ran el.click() on the resolved node index (3).
    let scripts = server.scripts.lock().unwrap();
    let click = scripts.iter().find(|s| s.contains("el.click()")).unwrap();
    assert!(click.contains("__dexterNodes?.[3]"));
}

#[test]
fn set_value_sends_text_via_args() {
    let server = fake_webdriver();
    let driver = BrowserDriver::connect(&server.url, "safari").unwrap();

    let target = Target::Semantic(SemanticTarget {
        role: Some("text_field".into()),
        name: Some("Card number".into()),
        ..Default::default()
    });
    driver
        .act(
            &Action::SetValue {
                target,
                value: "4242".into(),
            },
            &ActContext::default(),
        )
        .expect("set value");

    let scripts = server.scripts.lock().unwrap();
    let set = scripts
        .iter()
        .find(|s| s.contains("el.value = arguments[0]"))
        .unwrap();
    assert!(set.contains("__dexterNodes?.[2]"));
}

#[test]
fn ambiguous_semantic_target_fails_closed() {
    let server = fake_webdriver();
    let driver = BrowserDriver::connect(&server.url, "safari").unwrap();

    // No name → matches every button-ish element? Use a partial name
    // shared by nothing... use role alone matching multiple? "button"
    // role matches only #3, so match on enabled-ness instead: a
    // SemanticTarget with only `role: static_text`... actually heading
    // is unique too. Use role "check_box" — only #4. Multiple match:
    // name_contains "card" matches "Card number" (text_field) and
    // "Save card" (check_box) → 2 matches → ambiguous.
    let target = Target::Semantic(SemanticTarget {
        name_contains: Some("card".into()),
        ..Default::default()
    });
    let err = driver
        .act(
            &Action::Click {
                target,
                button: MouseButton::Left,
            },
            &ActContext::default(),
        )
        .unwrap_err();
    assert!(matches!(err, DriverError::Ambiguous(_)), "{err}");
}

#[test]
fn element_target_rejects_stale_dom() {
    let server = fake_webdriver();
    let driver = BrowserDriver::connect(&server.url, "safari").unwrap();

    let obs = driver.observe(&ObservationScope::default()).unwrap();
    let pay_id = obs.elements[3].id;

    // DOM mutates — next walk returns a renamed button.
    *server.mutated.lock().unwrap() = true;

    let err = driver
        .act(
            &Action::Click {
                target: Target::Element {
                    observation: obs.id,
                    element: pay_id,
                },
                button: MouseButton::Left,
            },
            &ActContext::default(),
        )
        .unwrap_err();
    assert!(matches!(err, DriverError::StaleReference(_)), "{err}");
}

#[test]
fn point_targets_are_unsupported() {
    let server = fake_webdriver();
    let driver = BrowserDriver::connect(&server.url, "safari").unwrap();

    let result = driver
        .act(
            &Action::Click {
                target: Target::Point { x: 10.0, y: 20.0 },
                button: MouseButton::Left,
            },
            &ActContext::default(),
        )
        .expect("result");
    assert_eq!(result.status, dexter_core::ActionStatus::Unsupported);
}

#[test]
fn screenshot_writes_png() {
    let server = fake_webdriver();
    let driver = BrowserDriver::connect(&server.url, "safari").unwrap();
    let dir = std::env::temp_dir().join(format!("dexter-shot-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("page.png");

    let obs = driver
        .observe(&ObservationScope {
            screenshot: true,
            screenshot_path: Some(path.to_string_lossy().into()),
            ..Default::default()
        })
        .unwrap();

    assert!(path.exists());
    assert_eq!(
        obs.screenshot.as_deref(),
        Some(path.to_string_lossy().as_ref())
    );
    // PNG magic bytes.
    let bytes = std::fs::read(&path).unwrap();
    assert_eq!(&bytes[..4], &[0x89, 0x50, 0x4E, 0x47]);
}

// ---- multi-tab ----

#[test]
fn tabs_are_windows_with_stable_ids() {
    let server = fake_webdriver();
    let driver = BrowserDriver::connect(&server.url, "safari").unwrap();

    let w1 = driver.windows().unwrap();
    assert_eq!(w1.len(), 1);
    assert!(w1[0].on_screen);
    assert_eq!(
        w1[0].title.as_deref(),
        Some("Fake Checkout (h1) — https://fake.test/checkout")
    );

    let new_id = driver.new_tab().unwrap();
    let w2 = driver.windows().unwrap();
    assert_eq!(w2.len(), 2);
    assert_eq!(w2[1].id, new_id);
    assert!(w2[1].on_screen); // new tab is active per WebDriver spec
    assert!(!w2[0].on_screen);
    assert!(w2[1].title.is_some());
    assert!(w2[0].title.is_none()); // background tab: honest None

    // Ids are stable across enumerations.
    assert_eq!(driver.windows().unwrap()[1].id, new_id);
}

#[test]
fn focus_window_switches_active_tab() {
    let server = fake_webdriver();
    let driver = BrowserDriver::connect(&server.url, "safari").unwrap();
    let first_id = driver.windows().unwrap()[0].id;
    driver.new_tab().unwrap();

    driver
        .act(
            &Action::Focus {
                target: Target::Window {
                    window_id: first_id,
                },
            },
            &ActContext::default(),
        )
        .expect("switch");

    let wins = driver.windows().unwrap();
    assert!(wins[0].on_screen);
    assert!(!wins[1].on_screen);
    assert_eq!(
        wins[0].title.as_deref(),
        Some("Fake Checkout (h1) — https://fake.test/checkout")
    );

    // Focusing a window id that is not a live tab fails closed.
    let err = driver
        .act(
            &Action::Focus {
                target: Target::Window { window_id: 999 },
            },
            &ActContext::default(),
        )
        .unwrap_err();
    assert!(matches!(err, DriverError::NotFound(_)), "{err}");
}

#[test]
fn observe_window_scope_switches_to_that_tab() {
    let server = fake_webdriver();
    let driver = BrowserDriver::connect(&server.url, "safari").unwrap();
    let first_id = driver.windows().unwrap()[0].id;
    driver.new_tab().unwrap();

    let obs = driver
        .observe(&ObservationScope {
            window: Some(first_id),
            ..Default::default()
        })
        .expect("observe tab 1");

    // Scoping switched the session: h1 is the observed, active window.
    assert!(
        obs.windows
            .iter()
            .find(|w| w.id == first_id)
            .unwrap()
            .on_screen
    );
    assert_eq!(obs.elements.len(), 6);
}

#[test]
fn element_from_other_tab_is_stale() {
    let server = fake_webdriver();
    let driver = BrowserDriver::connect(&server.url, "safari").unwrap();

    let obs = driver.observe(&ObservationScope::default()).unwrap();
    let pay_id = obs.elements[3].id;

    // Open a second tab — element refs belong to the first.
    driver.new_tab().unwrap();
    let err = driver
        .act(
            &Action::Click {
                target: Target::Element {
                    observation: obs.id,
                    element: pay_id,
                },
                button: MouseButton::Left,
            },
            &ActContext::default(),
        )
        .unwrap_err();
    assert!(matches!(err, DriverError::StaleReference(_)), "{err}");

    // Switch back to the observed tab → the same ref resolves again.
    driver
        .act(
            &Action::Focus {
                target: Target::Window {
                    window_id: obs.windows[0].id,
                },
            },
            &ActContext::default(),
        )
        .unwrap();
    driver
        .act(
            &Action::Click {
                target: Target::Element {
                    observation: obs.id,
                    element: pay_id,
                },
                button: MouseButton::Left,
            },
            &ActContext::default(),
        )
        .expect("click on original tab");
}

#[test]
fn close_tab_selects_a_remaining_tab() {
    let server = fake_webdriver();
    let driver = BrowserDriver::connect(&server.url, "safari").unwrap();
    driver.new_tab().unwrap();

    // Current is h2; closing selects h1.
    assert!(driver.close_tab().unwrap());
    let wins = driver.windows().unwrap();
    assert_eq!(wins.len(), 1);
    assert!(wins[0].on_screen);

    // Closing the last tab leaves no windows.
    assert!(!driver.close_tab().unwrap());
    assert!(driver.windows().unwrap().is_empty());
}

#[test]
fn observe_reports_iframe_collection_errors() {
    let server = fake_webdriver();
    let driver = BrowserDriver::connect(&server.url, "safari").unwrap();
    *server.iframe_errors.lock().unwrap() = 2;

    // Two frames could not be walked — counted, not fatal.
    let obs = driver.observe(&ObservationScope::default()).unwrap();
    assert_eq!(obs.collection_errors, 2);
    assert_eq!(obs.elements.len(), 6);
}

#[test]
fn disabled_element_act_reports_failed_not_success() {
    // The generator skips disabled elements, but Target::Element
    // binds bypass it — the in-page guard is the last honesty line:
    // a programmatic click on a disabled control must report Failed,
    // not "clicked".
    let server = fake_webdriver();
    let driver = BrowserDriver::connect(&server.url, "safari").unwrap();

    let target = Target::Semantic(SemanticTarget {
        role: Some("button".into()),
        name: Some("Apply coupon".into()),
        ..Default::default()
    });
    let result = driver
        .act(
            &Action::Click {
                target,
                button: MouseButton::Left,
            },
            &ActContext::default(),
        )
        .expect("act returns an honest result, not a driver error");

    assert_eq!(result.status, ActionStatus::Failed);
    assert_eq!(result.mechanism, Mechanism::Dom);
    let scripts = server.scripts.lock().unwrap();
    assert!(scripts.iter().any(|s| s.contains("__dexterNodes?.[5]")));
}

// ---- W3C /actions endpoint (real input tier, `allow_coordinates`) ----

fn coords() -> ActContext {
    ActContext {
        allow_coordinates: true,
        ..Default::default()
    }
}

#[test]
fn click_with_coords_posts_pointer_action_on_element() {
    let server = fake_webdriver();
    let driver = BrowserDriver::connect(&server.url, "safari").unwrap();

    let result = driver
        .act(
            &Action::Click {
                target: Target::Semantic(SemanticTarget {
                    role: Some("button".into()),
                    name: Some("Pay now".into()),
                    ..Default::default()
                }),
                button: MouseButton::Left,
            },
            &coords(),
        )
        .expect("click");

    assert_eq!(result.mechanism, Mechanism::Coordinates);
    // The element-ref probe ran on node 3; no DOM click ran.
    let scripts = server.scripts.lock().unwrap();
    assert!(scripts
        .iter()
        .any(|s| s.contains("return el;") && s.contains("__dexterNodes?.[3]")));
    assert!(!scripts.iter().any(|s| s.contains("el.click()")));

    let bodies = server.action_bodies.lock().unwrap();
    assert_eq!(bodies.len(), 1);
    let pointer = &bodies[0]["actions"][0];
    assert_eq!(pointer["type"], "pointer");
    assert_eq!(pointer["parameters"]["pointerType"], "mouse");
    let mv = &pointer["actions"][0];
    assert_eq!(mv["type"], "pointerMove");
    assert_eq!(
        mv["origin"]["element-6066-11e4-a52e-4f735466cecf"],
        "fake-el-ref"
    );
    assert_eq!(pointer["actions"][1]["type"], "pointerDown");
    assert_eq!(pointer["actions"][1]["button"], 0);
    assert_eq!(pointer["actions"][2]["type"], "pointerUp");
}

#[test]
fn right_click_with_coords_uses_pointer_button_2() {
    let server = fake_webdriver();
    let driver = BrowserDriver::connect(&server.url, "safari").unwrap();

    driver
        .act(
            &Action::Click {
                target: Target::Semantic(SemanticTarget {
                    role: Some("button".into()),
                    name: Some("Pay now".into()),
                    ..Default::default()
                }),
                button: MouseButton::Right,
            },
            &coords(),
        )
        .expect("right click");

    let bodies = server.action_bodies.lock().unwrap();
    assert_eq!(bodies[0]["actions"][0]["actions"][1]["button"], 2);
}

#[test]
fn key_chord_with_coords_uses_key_source() {
    let server = fake_webdriver();
    let driver = BrowserDriver::connect(&server.url, "safari").unwrap();

    let result = driver
        .act(
            &Action::Key {
                chord: KeyChord::parse("ctrl+shift+p").unwrap(),
            },
            &coords(),
        )
        .expect("key");

    assert_eq!(result.mechanism, Mechanism::Coordinates);
    let bodies = server.action_bodies.lock().unwrap();
    assert_eq!(bodies.len(), 1);
    let keys = &bodies[0]["actions"][0];
    assert_eq!(keys["type"], "key");
    let seq = &keys["actions"];
    // Modifiers down in order, key down/up, modifiers up in reverse.
    assert_eq!(seq[0], json!({"type":"keyDown","value":"\u{e009}"}));
    assert_eq!(seq[1], json!({"type":"keyDown","value":"\u{e008}"}));
    assert_eq!(seq[2], json!({"type":"keyDown","value":"p"}));
    assert_eq!(seq[3], json!({"type":"keyUp","value":"p"}));
    assert_eq!(seq[4], json!({"type":"keyUp","value":"\u{e008}"}));
    assert_eq!(seq[5], json!({"type":"keyUp","value":"\u{e009}"}));
}

#[test]
fn type_text_with_coords_types_real_keys() {
    let server = fake_webdriver();
    let driver = BrowserDriver::connect(&server.url, "safari").unwrap();

    let result = driver
        .act(
            &Action::TypeText {
                text: "42".into(),
                target: Some(Target::Semantic(SemanticTarget {
                    role: Some("text_field".into()),
                    name: Some("Card number".into()),
                    ..Default::default()
                })),
            },
            &coords(),
        )
        .expect("type");

    assert_eq!(result.mechanism, Mechanism::Coordinates);
    // The resolved element got a DOM focus before real key input.
    let scripts = server.scripts.lock().unwrap();
    assert!(scripts
        .iter()
        .any(|s| s.contains("el.focus()") && s.contains("__dexterNodes?.[2]")));

    let bodies = server.action_bodies.lock().unwrap();
    let keys = &bodies[0]["actions"][0];
    assert_eq!(keys["type"], "key");
    let seq = &keys["actions"];
    assert_eq!(seq[0], json!({"type":"keyDown","value":"4"}));
    assert_eq!(seq[1], json!({"type":"keyUp","value":"4"}));
    assert_eq!(seq[2], json!({"type":"keyDown","value":"2"}));
    assert_eq!(seq[3], json!({"type":"keyUp","value":"2"}));
}

#[test]
fn untargeted_type_with_coords_sends_keys_without_focus_probe() {
    let server = fake_webdriver();
    let driver = BrowserDriver::connect(&server.url, "safari").unwrap();

    driver
        .act(
            &Action::TypeText {
                text: "x".into(),
                target: None,
            },
            &coords(),
        )
        .expect("type");

    // No element to focus — keys land on whatever is focused.
    let scripts = server.scripts.lock().unwrap();
    assert!(!scripts.iter().any(|s| s.contains("el.focus()")));
    let bodies = server.action_bodies.lock().unwrap();
    assert_eq!(
        bodies[0]["actions"][0]["actions"][0],
        json!({"type":"keyDown","value":"x"})
    );
}

#[test]
fn untargeted_scroll_with_coords_posts_wheel() {
    let server = fake_webdriver();
    let driver = BrowserDriver::connect(&server.url, "safari").unwrap();

    let result = driver
        .act(
            &Action::Scroll {
                delta: ScrollDelta {
                    dx: 0.0,
                    dy: -240.0,
                },
                target: None,
            },
            &coords(),
        )
        .expect("scroll");

    assert_eq!(result.mechanism, Mechanism::Coordinates);
    let bodies = server.action_bodies.lock().unwrap();
    let wheel = &bodies[0]["actions"][0];
    assert_eq!(wheel["type"], "wheel");
    let scroll = &wheel["actions"][0];
    assert_eq!(scroll["type"], "scroll");
    assert_eq!(scroll["origin"], "viewport");
    assert_eq!(scroll["x"], 640);
    assert_eq!(scroll["y"], 400);
    assert_eq!(scroll["deltaY"], -240.0);
    // No DOM scrollBy ran.
    let scripts = server.scripts.lock().unwrap();
    assert!(!scripts.iter().any(|s| s.contains("window.scrollBy")));
}

#[test]
fn disabled_element_coords_click_reports_failed_without_actions_post() {
    let server = fake_webdriver();
    let driver = BrowserDriver::connect(&server.url, "safari").unwrap();

    let result = driver
        .act(
            &Action::Click {
                target: Target::Semantic(SemanticTarget {
                    role: Some("button".into()),
                    name: Some("Apply coupon".into()),
                    ..Default::default()
                }),
                button: MouseButton::Left,
            },
            &coords(),
        )
        .expect("honest result");

    assert_eq!(result.status, ActionStatus::Failed);
    // The ref probe ran — and no pointer action was ever posted.
    assert!(server.action_bodies.lock().unwrap().is_empty());
}

#[test]
fn point_click_remains_unsupported_with_coords() {
    // Deliberate stance: browser always offers semantic targets;
    // pointer actions anchor to elements, never raw coordinates.
    let server = fake_webdriver();
    let driver = BrowserDriver::connect(&server.url, "safari").unwrap();

    let result = driver
        .act(
            &Action::Click {
                target: Target::Point { x: 10.0, y: 20.0 },
                button: MouseButton::Left,
            },
            &coords(),
        )
        .expect("result");

    assert_eq!(result.status, ActionStatus::Unsupported);
    assert!(server.action_bodies.lock().unwrap().is_empty());
}

#[test]
fn key_chord_without_coords_keeps_dom_dispatch() {
    let server = fake_webdriver();
    let driver = BrowserDriver::connect(&server.url, "safari").unwrap();

    let result = driver
        .act(
            &Action::Key {
                chord: KeyChord::parse("ctrl+s").unwrap(),
            },
            &ActContext::default(),
        )
        .expect("key");

    assert_eq!(result.mechanism, Mechanism::Dom);
    let scripts = server.scripts.lock().unwrap();
    assert!(scripts.iter().any(|s| s.contains("KeyboardEvent")));
    assert!(server.action_bodies.lock().unwrap().is_empty());
}

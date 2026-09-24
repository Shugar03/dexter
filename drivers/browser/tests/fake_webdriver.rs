//! Hermetic protocol tests: `BrowserDriver` against a fake W3C
//! WebDriver HTTP endpoint on localhost. No browser involved — the
//! server returns fixture JSON for the DOM walker and records the
//! action scripts it receives.
//!
//! Real Safari E2E is separate, gated on `DEXTER_E2E_BROWSER=1`.

use dexter_browser::BrowserDriver;
use dexter_core::{
    Action, ElementSource, Mechanism, MouseButton, ObservationScope, SemanticTarget, Target,
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
         "actions":["press","set_value","focus","scroll_into_view"],"identifier":null}
    ])
}

struct FakeServer {
    url: String,
    /// Scripts received at /execute/sync, in order.
    scripts: Arc<Mutex<Vec<String>>>,
    /// WebDriver `args` arrays parallel to `scripts`.
    args: Arc<Mutex<Vec<Value>>>,
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
    let args = Arc::new(Mutex::new(Vec::new()));
    let mutated = Arc::new(Mutex::new(false));
    let handles = Arc::new(Mutex::new(vec!["h1".to_string()]));
    let current = Arc::new(Mutex::new("h1".to_string()));
    let iframe_errors = Arc::new(Mutex::new(0u32));
    let (s2, a2, m2, h2, c2, e2) = (
        scripts.clone(),
        args.clone(),
        mutated.clone(),
        handles.clone(),
        current.clone(),
        iframe_errors.clone(),
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
                ("POST", p) if p.ends_with("/execute/sync") => {
                    let parsed = serde_json::from_str::<Value>(body).unwrap_or_default();
                    let script = parsed["script"].as_str().unwrap_or_default().to_string();
                    s2.lock().unwrap().push(script.clone());
                    a2.lock().unwrap().push(parsed["args"].clone());
                    if script.contains("__dexterNodes = nodes") {
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
        args,
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

    assert_eq!(obs.elements.len(), 5);
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
                count: 1,
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
                count: 1,
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
                count: 1,
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
                count: 1,
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
    assert_eq!(obs.elements.len(), 5);
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
                count: 1,
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
                count: 1,
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
    assert_eq!(obs.elements.len(), 5);
}

// ---- generated-JS behavior: run captured scripts under Node ----

/// Run a captured /execute/sync script the same way WebDriver does:
/// `new Function(script)` applied to the args array (the script's
/// `arguments`), with stubbed `window.__dexterNodes`. Returns
/// `{result, events:[[type, node]...]}` — None when Node is absent so
/// the structural asserts still run standalone.
fn run_script_in_node(script: &str, args: &Value, nodes: &[(u64, &str)]) -> Option<Value> {
    let node_defs: Vec<Value> = nodes
        .iter()
        .map(|(id, who)| json!([id.to_string(), who]))
        .collect();
    let harness = format!(
        r#"
        const out = {{ events: [] }};
        const ev = (t, w) => out.events.push([t, w]);
        class Ev {{ constructor(t, i) {{ this.type = t; Object.assign(this, i || {{}}); }} }}
        globalThis.MouseEvent = Ev; globalThis.PointerEvent = Ev; globalThis.DragEvent = Ev;
        const mk = w => ({{
            click() {{ ev('click', w); }},
            focus() {{ ev('focus', w); }},
            scrollIntoView() {{ ev('scrollIntoView', w); }},
            dispatchEvent(e) {{ ev(e.type, w); return true; }},
            getBoundingClientRect() {{ return {{x:0,y:0,width:10,height:10}}; }},
            value: '', setAttribute() {{}},
        }});
        globalThis.window = {{ __dexterNodes: {{}} }};
        for (const [id, who] of {node_defs}) window.__dexterNodes[id] = mk(who);
        try {{
            out.result = new Function({script}).apply(null, {args});
        }} catch (e) {{ out.thrown = String(e); }}
        console.log(JSON.stringify(out));
        "#,
        node_defs = serde_json::to_string(&node_defs).unwrap(),
        script = serde_json::to_string(script).unwrap(),
        args = serde_json::to_string(args).unwrap(),
    );
    let path = std::env::temp_dir().join(format!(
        "dexter-js-{}-{}.js",
        std::process::id(),
        nodes.len() * 31 + script.len()
    ));
    std::fs::write(&path, harness).ok()?;
    let out = std::process::Command::new("node")
        .arg(&path)
        .output()
        .ok()?;
    let _ = std::fs::remove_file(&path);
    serde_json::from_slice(&out.stdout).ok()
}

fn captured(
    scripts: &Mutex<Vec<String>>,
    args: &Mutex<Vec<Value>>,
    marker: &str,
) -> (String, Value) {
    let scripts = scripts.lock().unwrap();
    let args = args.lock().unwrap();
    let idx = scripts
        .iter()
        .rposition(|s| s.contains(marker))
        .unwrap_or_else(|| panic!("no captured script containing {marker:?}"));
    (scripts[idx].clone(), args[idx].clone())
}

#[test]
fn multi_click_dispatches_a_real_event_sequence() {
    let server = fake_webdriver();
    let driver = BrowserDriver::connect(&server.url, "safari").unwrap();

    let target = Target::Semantic(SemanticTarget {
        role: Some("button".into()),
        name: Some("Pay now".into()),
        ..Default::default()
    });
    for count in [2u8, 3] {
        driver
            .act(
                &Action::Click {
                    target: target.clone(),
                    button: MouseButton::Left,
                    count,
                },
                &ActContext::default(),
            )
            .expect("multi-click");

        let (script, sargs) = captured(&server.scripts, &server.args, "'multi-clicked'");
        assert_eq!(sargs, json!([count]));
        // Forwarded args reach the inner function as `a`, never
        // `arguments` (which is the inner call's own list there).
        assert!(script.contains("a[0]"), "{script}");
        assert!(!script.contains("i<arguments[0]"), "{script}");

        let Some(out) = run_script_in_node(&script, &sargs, &[(3, "pay")]) else {
            eprintln!("node unavailable — ran structural asserts only");
            continue;
        };
        assert!(
            out["thrown"].is_null() && out["result"]["__dexter_err"].is_null(),
            "{out}"
        );
        let got: Vec<String> = serde_json::from_value::<Vec<Vec<String>>>(out["events"].clone())
            .unwrap()
            .into_iter()
            .map(|e| e[0].clone())
            .collect();
        let mut want: Vec<String> = Vec::new();
        for _ in 0..count {
            want.extend(
                ["mousedown", "mouseup", "click"]
                    .iter()
                    .map(|s| s.to_string()),
            );
        }
        want.push("dblclick".into());
        assert_eq!(got, want, "count={count}");
    }
}

#[test]
fn drag_dispatches_press_on_source_and_drop_on_destination() {
    let server = fake_webdriver();
    let driver = BrowserDriver::connect(&server.url, "safari").unwrap();

    driver
        .act(
            &Action::Drag {
                from: Target::Semantic(SemanticTarget {
                    name: Some("Pay now".into()),
                    ..Default::default()
                }),
                to: Target::Semantic(SemanticTarget {
                    name: Some("Save card".into()),
                    ..Default::default()
                }),
                duration_ms: 0,
            },
            &ActContext::default(),
        )
        .expect("drag");

    let (script, sargs) = captured(&server.scripts, &server.args, "'dragged'");
    // a[0] is the DESTINATION node index (4 = "Save card"), never the
    // source — `el` is already bound to the source (3 = "Pay now").
    assert_eq!(sargs, json!([4]));
    assert!(script.contains("__dexterNodes?.[a[0]]"), "{script}");

    let Some(out) = run_script_in_node(&script, &sargs, &[(3, "src"), (4, "dst")]) else {
        eprintln!("node unavailable — ran structural asserts only");
        return;
    };
    assert!(
        out["thrown"].is_null() && out["result"]["__dexter_err"].is_null(),
        "{out}"
    );
    let got: Vec<(String, String)> =
        serde_json::from_value::<Vec<Vec<String>>>(out["events"].clone())
            .unwrap()
            .into_iter()
            .map(|e| (e[0].clone(), e[1].clone()))
            .collect();
    let mut want: Vec<(String, String)> = [
        ("pointerdown", "src"),
        ("mousedown", "src"),
        ("dragstart", "src"),
    ]
    .into_iter()
    .map(|(a, b)| (a.to_string(), b.to_string()))
    .collect();
    for _ in 0..6 {
        want.push(("pointermove".into(), "dst".into()));
        want.push(("dragover".into(), "dst".into()));
    }
    want.extend(
        [
            ("drop", "dst"),
            ("pointerup", "dst"),
            ("mouseup", "dst"),
            ("dragend", "src"),
        ]
        .into_iter()
        .map(|(a, b)| (a.to_string(), b.to_string())),
    );
    assert_eq!(got, want);
}

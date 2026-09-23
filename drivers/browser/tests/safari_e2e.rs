//! Real Safari E2E — requires:
//!   Safari Settings → Developer → Allow Remote Automation
//! Run with: DEXTER_E2E_BROWSER=1 cargo test -p dexter-browser --test safari_e2e

use dexter_browser::BrowserDriver;
use dexter_core::{Action, MouseButton, ObservationScope, SemanticTarget, Target};
use dexter_driver::{ActContext, ComputerDriver};

fn safari() -> Option<BrowserDriver> {
    if std::env::var("DEXTER_E2E_BROWSER").ok().as_deref() != Some("1") {
        eprintln!("skipping: set DEXTER_E2E_BROWSER=1 (and enable Safari remote automation)");
        return None;
    }
    BrowserDriver::safari().ok()
}

#[test]
fn safari_observe_and_click() {
    let Some(driver) = safari() else { return };
    driver
        .navigate("data:text/html,<title>Demo</title><main><h1>Demo</h1><input id=card aria-label='Card number'><button id=pay onclick=\"document.body.dataset.clicked='1'\">Pay now</button></main>")
        .expect("navigate");
    std::thread::sleep(std::time::Duration::from_millis(800));

    let obs = driver
        .observe(&ObservationScope::default())
        .expect("observe");
    assert!(!obs.elements.is_empty(), "walker returned no elements");
    eprintln!("digest:\n{}", obs.digest);

    // Semantic click → DOM click → body dataset flag.
    driver
        .act(
            &Action::Click {
                target: Target::Semantic(SemanticTarget {
                    role: Some("button".into()),
                    name: Some("Pay now".into()),
                    ..Default::default()
                }),
                button: MouseButton::Left,
            },
            &ActContext::default(),
        )
        .expect("click");

    // Verify via re-observation of state (the flag lives on body; read
    // it through a fresh execute).
    let val = driver
        .client_exec("return document.body.dataset.clicked || '0';", vec![])
        .unwrap_or_default();
    assert_eq!(val.as_str().unwrap_or(""), "1", "click had no DOM effect");
}

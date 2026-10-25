use dexter_core::{Element, ElementId, ElementSource};
use dexter_decision::Route;
use dexter_eval::{
    harvest::{harvest, harvest_file, HarvestManifest},
    load_jsonl, to_jsonl, Gold,
};
use dexter_sim::SimDriver;
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT_TEMP_FILE: AtomicU64 = AtomicU64::new(0);

fn element(id: u64, role: &str, name: &str) -> Element {
    Element {
        id: ElementId(id),
        role: Some(role.into()),
        name: Some(name.into()),
        actions: vec!["press".into()],
        enabled: Some(true),
        source: ElementSource::Dom,
        ..Default::default()
    }
}

fn parse_manifest(text: &str) -> HarvestManifest {
    toml::from_str(text).expect("inline harvest manifest parses")
}

#[test]
fn act_gold_resolves_to_sim_element() {
    let expected = ElementId(7);
    let driver = SimDriver::new(vec![element(expected.0, "button", "Continue")]);
    let manifest = parse_manifest(
        r#"
[[page]]
id = "continue"
goal = "continue"
settle_ms = 0
[page.gold]
type = "act"
[page.gold.target]
role = "button"
name = "Continue"
"#,
    );

    let items = harvest(&driver, &manifest, std::path::Path::new(".")).unwrap();
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].id, "continue");
    assert_eq!(items[0].goal, "continue");
    assert_eq!(items[0].source, "harvest");
    assert!(items[0].meta["url"].is_null());
    assert!(matches!(
        &items[0].gold,
        Gold::Act { element, .. } if *element == expected
    ));
}

#[test]
fn route_gold_is_preserved() {
    let driver = SimDriver::new(vec![]);
    let manifest = parse_manifest(
        r#"
[[page]]
id = "blocked"
goal = "continue"
settle_ms = 0
[page.gold]
type = "route"
[page.gold.route]
type = "abstain"
"#,
    );

    let items = harvest(&driver, &manifest, std::path::Path::new(".")).unwrap();
    assert!(matches!(
        &items[0].gold,
        Gold::Route {
            route: Route::Abstain
        }
    ));
}

#[test]
fn expect_questions_are_carried_from_manifest_and_default_to_none() {
    let driver = SimDriver::new(vec![]);
    let manifest = parse_manifest(
        r#"
[[page]]
id = "with-questions"
goal = "continue"
settle_ms = 0
expect_questions = ["blocking_modal"]
[page.gold]
type = "route"
[page.gold.route]
type = "abstain"

[[page]]
id = "without-questions"
goal = "continue"
settle_ms = 0
[page.gold]
type = "route"
[page.gold.route]
type = "abstain"
"#,
    );

    let items = harvest(&driver, &manifest, std::path::Path::new(".")).unwrap();
    assert_eq!(
        items[0].expect_questions.as_deref(),
        Some(["blocking_modal".to_string()].as_slice())
    );
    assert!(items[1].expect_questions.is_none());
}

#[test]
fn missing_gold_target_returns_page_and_stage() {
    let driver = SimDriver::new(vec![element(1, "button", "Continue")]);
    let manifest = parse_manifest(
        r#"
[[page]]
id = "missing"
goal = "continue"
settle_ms = 0
[page.gold]
type = "act"
[page.gold.target]
role = "button"
name = "Submit"
"#,
    );

    let error = harvest(&driver, &manifest, std::path::Path::new(".")).unwrap_err();
    assert_eq!(error.page, "missing");
    assert_eq!(error.stage, "gold target did not resolve uniquely");
}

#[test]
fn ambiguous_gold_target_fails_closed() {
    let driver = SimDriver::new(vec![
        element(1, "button", "Continue"),
        element(2, "button", "Continue"),
    ]);
    let manifest = parse_manifest(
        r#"
[[page]]
id = "ambiguous"
goal = "continue"
settle_ms = 0
[page.gold]
type = "act"
[page.gold.target]
role = "button"
name = "Continue"
"#,
    );

    let error = harvest(&driver, &manifest, std::path::Path::new(".")).unwrap_err();
    assert_eq!(error.page, "ambiguous");
    assert_eq!(error.stage, "gold target did not resolve uniquely");
}

#[test]
fn harvest_file_round_trips_jsonl_ids() {
    let path = std::env::temp_dir().join(format!(
        "dexter-harvest-{}-{}.toml",
        std::process::id(),
        NEXT_TEMP_FILE.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::write(
        &path,
        r#"
[[page]]
id = "round-trip"
goal = "continue"
settle_ms = 0
[page.gold]
type = "act"
[page.gold.target]
role = "button"
name = "Continue"
"#,
    )
    .unwrap();

    let driver = SimDriver::new(vec![element(9, "button", "Continue")]);
    let items = harvest_file(&driver, &path).unwrap();
    let loaded = load_jsonl(&to_jsonl(&items).unwrap()).unwrap();
    let item_ids: Vec<_> = items.iter().map(|item| &item.id).collect();
    let loaded_ids: Vec<_> = loaded.iter().map(|item| &item.id).collect();
    assert_eq!(loaded_ids, item_ids);

    std::fs::remove_file(path).unwrap();
}

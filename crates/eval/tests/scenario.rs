//! Task-scenario harness: spec parsing, closed-loop runs, metrics.

use dexter_decision::{HeuristicGenerator, RuleBased};
use dexter_driver::ComputerDriver;
use dexter_eval::scenario::*;

const WIZARD: &str = r#"
[scenario]
id = "wizard-install"
goal = "aceptar los términos y continuar"
optimal_steps = 2
app = "Instalador"

[task]
done_when = { type = "element_exists", target = { role = "static_text", name_contains = "completada" } }
max_steps = 8

[[world.element]]
id = 1
role = "window"
name = "Asistente de instalación"

[[world.element]]
id = 6
role = "check_box"
name = "Acepto los términos"
actions = ["press"]
enabled = true

[[world.element]]
id = 8
role = "button"
name = "Continuar"
actions = ["press"]
enabled = false

[[world.element]]
id = 9
role = "button"
name = "Cancelar"
actions = ["press"]

[[world.rule]]
when = { name_contains = "términos" }
effect = { type = "set_enabled_of", target = { name = "Continuar" }, enabled = true }

[[world.rule]]
when = { name = "Continuar" }
effect = { type = "spawn", element = { id = 0, role = "static_text", name = "Instalación completada" } }
"#;

const DOWNLOAD: &str = r#"
[scenario]
id = "download-wait"
goal = "esperar a que termine la descarga"
optimal_steps = 1

[task]
done_when = { type = "element_value", target = { role = "progress_indicator" }, predicate = { contains = "100" } }
max_steps = 8

[[world.element]]
id = 3
role = "static_text"
name = "Descargando actualización…"

[[world.element]]
id = 4
role = "progress_indicator"
name = "Progreso de descarga"
value = "62"

[[world.element]]
id = 5
role = "button"
name = "Cancelar"
actions = ["press"]

[[world.tick]]
type = "cycle_value_of"
target = { role = "progress_indicator" }
values = ["81", "100"]
"#;

const ABSENT: &str = r#"
[scenario]
id = "admin-absent"
goal = "abrir el panel de administración"
optimal_steps = 0

[task]
done_when = { type = "element_exists", target = { role = "window", name_contains = "Admin" } }
expected = "abstained"
max_steps = 4

[[world.element]]
id = 3
role = "button"
name = "Nueva nota"
actions = ["press"]

[[world.element]]
id = 4
role = "text_field"
name = "Buscar"
actions = ["set_value", "focus"]
"#;

const WEB_LOGIN: &str = r#"
[scenario]
id = "web-login"
driver = "browser"
goal = "escribir \"demo\" en usuario y entrar"
optimal_steps = 2
app = "browser"

[browser]
page = "pages/web-login.html"
settle_ms = 500

[task]
done_when = { type = "text_present", text = "Bienvenido" }
max_steps = 8
"#;

const CLOCK: &str = r#"
[scenario]
id = "clock-timer"
driver = "macos"
goal = "ir a cronómetro e iniciarlo"
optimal_steps = 2
app = "Clock"

[live]
app = "com.apple.clock"
prep = "open -a Clock"
teardown = "osascript -e 'tell application id \"com.apple.clock\" to quit'"
settle_ms = 800

[task]
done_when = { type = "element_exists", target = { role = "button", name = "Detener" } }
max_steps = 8
"#;

fn spec(toml_text: &str) -> ScenarioSpec {
    toml::from_str(toml_text).expect("scenario spec parses")
}

#[test]
fn spec_parses_browser_driver_section() {
    let s = spec(WEB_LOGIN);
    assert_eq!(s.driver(), "browser");
    let b = s.browser.as_ref().expect("browser section parsed");
    assert_eq!(b.page.as_deref(), Some("pages/web-login.html"));
    assert_eq!(b.settle_ms, 500);
    assert_eq!(spec(ABSENT).driver(), "sim", "absent driver key = sim");
}

#[test]
fn spec_parses_live_macos_section() {
    let s = spec(CLOCK);
    assert_eq!(s.driver(), "macos");
    let l = s.live.as_ref().expect("live section parsed");
    assert_eq!(l.app, "com.apple.clock");
    assert_eq!(l.prep.as_deref(), Some("open -a Clock"));
    assert!(l.teardown.as_deref().unwrap().contains("quit"));
    assert_eq!(l.settle_ms, 800);
    assert!(spec(WEB_LOGIN).live.is_none());
}

/// The committed dataset is a product surface: every spec must parse,
/// and any live scenario that launches a real app must pin the app's
/// locale to es-ES (`defaults write <bundle> AppleLanguages -array es`)
/// so Spanish AX names resolve on any host locale — CI runners are
/// en-US and unpinned specs abstain deterministically. Preps must also
/// never `tell application ... to quit`: AppleScript *launches* the app
/// to deliver the quit, so on a cold start the app boots in the host
/// locale before the pin is written and `open -a` then reactivates that
/// English instance. `pkill` terminates without launching.
#[test]
fn dataset_specs_parse_and_live_prep_pins_locale() {
    let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/../../datasets/scenarios");
    let mut parsed = 0;
    for entry in std::fs::read_dir(dir).expect("datasets/scenarios dir") {
        let path = entry.unwrap().path();
        let name = path.file_name().unwrap().to_string_lossy().to_string();
        if path.extension().and_then(|e| e.to_str()) != Some("toml") || name == "baseline.toml" {
            continue;
        }
        let text = std::fs::read_to_string(&path).unwrap();
        let s: ScenarioSpec =
            toml::from_str(&text).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
        if let Some(live) = &s.live {
            let prep = live.prep.as_deref().unwrap_or("");
            assert!(
                !(prep.contains("tell application") && prep.contains("to quit")),
                "{name}: live prep uses 'tell application ... to quit', which launches \
                 the app pre-pin on a cold start — use pkill instead"
            );
            if prep.contains("open -a") {
                assert!(
                    prep.contains("AppleLanguages"),
                    "{name}: live prep launches an app but does not pin AppleLanguages"
                );
            }
        }
        parsed += 1;
    }
    assert!(parsed >= 10, "expected the full dataset, parsed {parsed}");
}

#[test]
fn spec_parses_world_rules_and_ticks() {
    let s = spec(WIZARD);
    assert_eq!(s.scenario.id, "wizard-install");
    assert_eq!(s.world.element.len(), 4);
    assert_eq!(s.world.rule.len(), 2);
    let d = spec(DOWNLOAD);
    assert_eq!(d.world.tick.len(), 1);
    let a = spec(ABSENT);
    assert_eq!(a.task.expected, "abstained");
}

#[test]
fn build_driver_applies_press_rules() {
    use dexter_driver::ComputerDriver;
    let driver = build_driver(&spec(WIZARD));
    let obs = driver.observe(&Default::default()).unwrap();
    let next = obs
        .elements
        .iter()
        .find(|e| e.name.as_deref() == Some("Continuar"))
        .unwrap();
    assert_eq!(next.enabled, Some(false), "Continuar starts disabled");
}

#[test]
fn wizard_completes_via_enable_chain() {
    let s = spec(WIZARD);
    let run = run_scenario(&s, &HeuristicGenerator::default(), &RuleBased::default());
    assert_eq!(run.outcome, "completed");
    assert!(run.success, "completed matches expected");
    assert_eq!(run.steps, 2, "checkbox → continuar is the 2-step path");
    assert_eq!(run.decide_ms.len(), 2, "one decide per act step");
    assert_eq!(run.physical_acts, 0);
    assert_eq!(run.recoveries, 0);
}

#[test]
fn download_waits_instead_of_pressing() {
    let s = spec(DOWNLOAD);
    let run = run_scenario(&s, &HeuristicGenerator::default(), &RuleBased::default());
    assert_eq!(run.outcome, "completed");
    // Waited, never pressed: no act latency rows at all.
    assert!(run.act_ms.is_empty(), "a correct wait never acts");
    assert_eq!(run.steps, 1);
}

#[test]
fn unsatisfiable_goal_succeeds_by_abstaining() {
    let s = spec(ABSENT);
    let run = run_scenario(&s, &HeuristicGenerator::default(), &RuleBased::default());
    assert_eq!(run.outcome, "abstained");
    assert!(run.success, "expected=abstained makes abstention a pass");
}

#[test]
fn aggregation_reports_efficiency_and_latency() {
    let s = spec(WIZARD);
    let runs: Vec<_> = (0..2)
        .map(|_| run_scenario(&s, &HeuristicGenerator::default(), &RuleBased::default()))
        .collect();
    let m = aggregate(&s.scenario.id, s.scenario.optimal_steps, runs);
    assert_eq!(m.reps, 2);
    assert_eq!(m.succeeded, 2);
    assert_eq!(m.mean_steps, 2.0);
    assert_eq!(m.mean_over_optimal, Some(0.0));
    assert!(m.decide_p95_ms < 1000, "rule-based decides in ms");

    let roll = suite_rollup(std::slice::from_ref(&m));
    assert_eq!(roll.success_rate, 1.0);
}

#[test]
fn baseline_check_bites_on_regression() {
    let s = spec(WIZARD);
    let run = run_scenario(&s, &HeuristicGenerator::default(), &RuleBased::default());
    let m = aggregate(&s.scenario.id, s.scenario.optimal_steps, vec![run]);

    let pass: Baseline = toml::from_str(
        r#"
        [scenario.wizard-install]
        min_success = 1.0
        max_mean_steps = 3.0
        "#,
    )
    .unwrap();
    assert!(check_baseline(std::slice::from_ref(&m), &pass).is_empty());

    let strict: Baseline = toml::from_str(
        r#"
        [scenario.wizard-install]
        max_mean_steps = 1.0
        "#,
    )
    .unwrap();
    let violations = check_baseline(std::slice::from_ref(&m), &strict);
    assert_eq!(violations.len(), 1);
    assert!(violations[0].message.contains("mean steps"));

    let suite_gate: Baseline = toml::from_str(
        r#"
        [suite]
        min_success_rate = 1.0
        "#,
    )
    .unwrap();
    assert!(check_baseline(std::slice::from_ref(&m), &suite_gate).is_empty());
}

#[test]
fn baseline_flags_scenarios_missing_from_results() {
    let s = spec(WIZARD);
    let run = run_scenario(&s, &HeuristicGenerator::default(), &RuleBased::default());
    let m = aggregate(&s.scenario.id, s.scenario.optimal_steps, vec![run]);
    let base: Baseline = toml::from_str(
        r#"
        [scenario.wizard-install]
        min_success = 1.0

        [scenario.renamed-away]
        min_success = 1.0
        "#,
    )
    .unwrap();
    let violations = check_baseline(std::slice::from_ref(&m), &base);
    assert_eq!(violations.len(), 1);
    assert_eq!(violations[0].scenario, "renamed-away");
    assert!(violations[0].message.contains("missing"));
}

#[test]
fn successful_run_exports_training_rows() {
    let s = spec(WIZARD);
    let run = run_scenario(&s, &HeuristicGenerator::default(), &RuleBased::default());
    assert!(run.success);
    let (rows, skipped) = rows_from_events(&run.events, &s.scenario.id, "sim");
    // Two act decisions: the checkbox, then Continuar.
    assert_eq!(skipped, 0);
    assert_eq!(rows.len(), 2);
    let r0 = &rows[0];
    assert_eq!(r0.id, "wizard-install#step1");
    assert_eq!(r0.gold_route, None);
    let gi = r0.gold_index.expect("act decision yields a gold index");
    assert!(gi < r0.n_candidates, "gold points at an offered option");
    assert_eq!(r0.options.len(), r0.n_candidates + 4, "options + routes");
    assert!(r0.options[gi].contains("términos"));

    // Route decisions label too — a correct wait is a training row.
    let d = spec(DOWNLOAD);
    let drun = run_scenario(&d, &HeuristicGenerator::default(), &RuleBased::default());
    let (drows, _) = rows_from_events(&drun.events, &d.scenario.id, "sim");
    assert!(drows.iter().any(|r| r.gold_route == Some("wait")));
}

/// Every sim scenario in the dataset must reach its declared `expected`
/// outcome under the rule-based engine — the hermetic half of the
/// `eval scenario --check` gate (live/browser specs need real drivers
/// and stay CI-only).
#[test]
fn sim_dataset_scenarios_reach_expected_outcome() {
    let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/../../datasets/scenarios");
    let mut ran = 0;
    for entry in std::fs::read_dir(dir).expect("datasets/scenarios dir") {
        let path = entry.unwrap().path();
        if path.extension().and_then(|e| e.to_str()) != Some("toml") {
            continue;
        }
        let name = path.file_name().unwrap().to_string_lossy().to_string();
        if name == "baseline.toml" {
            continue;
        }
        let text = std::fs::read_to_string(&path).unwrap();
        let s: ScenarioSpec = toml::from_str(&text).unwrap_or_else(|e| panic!("{name}: {e}"));
        if s.driver() != "sim" {
            continue;
        }
        let run = run_scenario(&s, &HeuristicGenerator::default(), &RuleBased::default());
        assert_eq!(
            run.outcome, s.task.expected,
            "{name}: outcome != expected (steps={})",
            run.steps
        );
        ran += 1;
    }
    assert!(ran >= 8, "expected the sim dataset, ran {ran}");
}

#[test]
fn spec_parses_element_source() {
    let s = spec(
        r#"
[scenario]
id = "t"
goal = "x"

[task]
done_when = { type = "element_exists", target = {} }

[[world.element]]
id = 1
role = "button"
name = "Jugar"
source = "ocr"

[[world.element]]
id = 2
role = "button"
name = "Normal"
"#,
    );
    let driver = build_driver(&s);
    let obs = driver
        .observe(&dexter_core::ObservationScope::default())
        .unwrap();
    let by_name = |n: &str| {
        obs.elements
            .iter()
            .find(|e| e.name.as_deref() == Some(n))
            .unwrap()
    };
    assert_eq!(by_name("Jugar").source, dexter_core::ElementSource::Ocr);
    assert_eq!(
        by_name("Normal").source,
        dexter_core::ElementSource::Accessibility
    );
    assert!(
        obs.digest.contains("[ocr]"),
        "ocr provenance in digest: {}",
        obs.digest
    );
}

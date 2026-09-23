//! Task-scenario harness: spec parsing, closed-loop runs, metrics.

use dexter_decision::{HeuristicGenerator, RuleBased};
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

fn spec(toml_text: &str) -> ScenarioSpec {
    toml::from_str(toml_text).expect("scenario spec parses")
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

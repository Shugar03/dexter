//! Sim-suite report: runs every sim scenario in `datasets/scenarios`
//! under the rule-based engine and prints a markdown metrics table —
//! the reproducible half of the published eval numbers (live/browser
//! specs need real drivers and are skipped with a note).
//!
//! Run: `cargo run -p dexter-eval --example suite_report [reps]`

use dexter_decision::{HeuristicGenerator, RuleBased};
use dexter_eval::scenario::{aggregate, run_scenario, suite_rollup, ScenarioSpec};
use std::path::Path;

fn main() {
    let reps: usize = std::env::args()
        .nth(1)
        .and_then(|a| a.parse().ok())
        .unwrap_or(3);
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../datasets/scenarios");
    let mut entries: Vec<_> = std::fs::read_dir(&dir)
        .expect("datasets/scenarios dir")
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().and_then(|e| e.to_str()) == Some("toml"))
        .collect();
    entries.sort();

    let generator = HeuristicGenerator::default();
    let engine = RuleBased::default();
    let mut metrics = Vec::new();
    let mut skipped = Vec::new();

    for path in entries {
        let name = path.file_name().unwrap().to_string_lossy().to_string();
        if name == "baseline.toml" {
            continue;
        }
        let text = std::fs::read_to_string(&path).unwrap();
        let spec: ScenarioSpec = toml::from_str(&text).unwrap_or_else(|e| panic!("{name}: {e}"));
        if spec.driver() != "sim" {
            skipped.push(format!("{} ({})", spec.scenario.id, spec.driver()));
            continue;
        }
        let runs: Vec<_> = (0..reps)
            .map(|_| run_scenario(&spec, &generator, &engine))
            .collect();
        metrics.push(aggregate(
            &spec.scenario.id,
            spec.scenario.optimal_steps,
            runs,
        ));
    }

    let roll = suite_rollup(&metrics);
    println!(
        "| scenario | outcome | steps (opt) | over-opt | decide p50/p95 ms | recoveries | phys |"
    );
    println!("|---|---|---|---|---|---|---|");
    for m in &metrics {
        let outcome = m
            .outcomes
            .iter()
            .map(|(o, n)| {
                if *n > 1 {
                    format!("{o}×{n}")
                } else {
                    o.clone()
                }
            })
            .collect::<Vec<_>>()
            .join("+");
        println!(
            "| {} | {} | {:.1} | {} | {}/{} | {} | {} |",
            m.id,
            outcome,
            m.mean_steps,
            m.mean_over_optimal
                .map(|v| format!("{v:.1}"))
                .unwrap_or_else(|| "—".into()),
            m.decide_p50_ms,
            m.decide_p95_ms,
            m.recoveries,
            m.physical_acts,
        );
    }
    println!(
        "\nsuite: {}/{} runs succeeded across {} scenarios — success_rate {:.0}%, worst decide p95 {}ms, physical acts {}",
        roll.succeeded,
        roll.reps,
        roll.scenarios,
        roll.success_rate * 100.0,
        roll.decide_p95_ms,
        roll.physical_acts,
    );
    if !skipped.is_empty() {
        println!("skipped (non-sim driver): {}", skipped.join(", "));
    }
}

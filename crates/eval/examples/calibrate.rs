//! Threshold calibration report: sweeps the rule-based act threshold
//! over every frozen dataset in `datasets/*/items.jsonl` and prints a
//! markdown table plus the fail-closed pick (`calibrate::pick`).
//!
//! Run: `cargo run -p dexter-eval --example calibrate`

use dexter_decision::HeuristicGenerator;
use dexter_eval::calibrate::{pick, sweep_act_threshold, SweepPoint};
use dexter_eval::{load_jsonl, EvalItem};
use std::path::Path;

const DATASETS: &[&str] = &["browser", "macos", "sim", "vision"];

fn row(label: &str, p: &SweepPoint) {
    let r = &p.report;
    println!(
        "| {label} | {:.2} | {:.0}% ({}/{}) | {}/{} ({:.0}%) | {}/{} | {} | {} | {} |",
        p.value,
        r.coverage() * 100.0,
        r.covered,
        r.act_items,
        r.correct,
        r.covered,
        r.accuracy() * 100.0,
        r.routes_correct,
        r.route_items,
        r.false_acts,
        r.false_routes,
        p.score(),
    );
}

fn main() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../datasets");
    let thresholds: Vec<f32> = (8..=20).map(|i| i as f32 * 0.05).collect();
    let generator = HeuristicGenerator::default();

    let mut all: Vec<EvalItem> = Vec::new();
    println!(
        "| dataset | act_threshold | coverage | act | routes | false acts | false routes | score |"
    );
    println!("|---|---|---|---|---|---|---|---|");
    for ds in DATASETS {
        let text = std::fs::read_to_string(root.join(ds).join("items.jsonl"))
            .unwrap_or_else(|e| panic!("{ds}: {e}"));
        let items = load_jsonl(&text).unwrap_or_else(|e| panic!("{ds}: {e}"));
        for p in sweep_act_threshold(&items, &generator, &thresholds) {
            row(ds, &p);
        }
        all.extend(items);
    }
    let points = sweep_act_threshold(&all, &generator, &thresholds);
    for p in &points {
        row("(all)", p);
    }
    match pick(&points) {
        Some(t) => {
            println!("\npick (max score − 2·false acts → fewest false acts → highest): {t:.2}")
        }
        None => println!("\nno sweep points"),
    }
}

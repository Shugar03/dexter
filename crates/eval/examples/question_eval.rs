use dexter_decision::{CandidateGenerator, Cascade, DecisionEngine, HeuristicGenerator, RuleBased};
use dexter_eval::{load_jsonl, run_eval, EvalItem, EvalReport};
use dexter_laya::LayaEngine;
use std::error::Error;
use std::path::PathBuf;
use std::time::Duration;

const QUESTION_IDS: [&str; 4] = [
    "blocking_modal",
    "disambiguate",
    "action_effect",
    "goal_complete",
];

fn load_dataset(name: &str) -> Result<Vec<EvalItem>, Box<dyn Error>> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join(format!("../../datasets/{name}/items.jsonl"));
    Ok(load_jsonl(&std::fs::read_to_string(path)?)?)
}

fn dev_laya() -> Result<LayaEngine, Box<dyn Error>> {
    let worker = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../workers/laya/worker.py");
    Ok(LayaEngine::spawn(
        &format!("python3 \"{}\"", worker.display()),
        Duration::from_secs(10),
    )?)
}

fn print_report(
    label: &str,
    items: &[EvalItem],
    generator: &dyn CandidateGenerator,
    engine: &dyn DecisionEngine,
) {
    let report = run_eval(items, generator, engine);
    println!("## {label}");
    print_overall(&report);
    print_question_table(&report);
}

fn print_overall(report: &EvalReport) {
    println!(
        "overall: items={} coverage={}/{} ({:.0}%) act={}/{} ({:.0}%) routes={}/{} ({:.0}%) false_acts={} false_routes={}",
        report.items,
        report.covered,
        report.act_items,
        report.coverage() * 100.0,
        report.correct,
        report.covered,
        report.accuracy() * 100.0,
        report.routes_correct,
        report.route_items,
        report.route_accuracy() * 100.0,
        report.false_acts,
        report.false_routes
    );
}

fn print_question_table(report: &EvalReport) {
    println!("| question | labeled | detected | spurious | asked | correct | false_acts | false_routes |");
    println!("|---|---:|---:|---:|---:|---:|---:|---:|");
    for id in QUESTION_IDS {
        let stats = &report.questions[id];
        println!(
            "| {id} | {} | {} | {} | {} | {} | {} | {} |",
            stats.labeled,
            stats.detected,
            stats.spurious,
            stats.asked,
            stats.correct,
            stats.false_acts,
            stats.false_routes
        );
    }
}

fn main() -> Result<(), Box<dyn Error>> {
    let questions = load_dataset("questions")?;
    let sim = load_dataset("sim")?;
    let browser = load_dataset("browser")?;
    let generator = HeuristicGenerator::default();

    let rule = RuleBased::default();
    print_report(
        "datasets/questions — RuleBased::default()",
        &questions,
        &generator,
        &rule,
    );

    let cascade = Cascade::new(vec![Box::new(RuleBased::default()), Box::new(dev_laya()?)]);
    print_report(
        "datasets/questions — Cascade[RuleBased, Laya dev]",
        &questions,
        &generator,
        &cascade,
    );

    let laya = dev_laya()?;
    print_report(
        "datasets/questions — Laya dev",
        &questions,
        &generator,
        &laya,
    );

    let rule = RuleBased::default();
    print_report(
        "datasets/sim — RuleBased::default() (unlabeled)",
        &sim,
        &generator,
        &rule,
    );
    print_report(
        "datasets/browser — RuleBased::default() (unlabeled)",
        &browser,
        &generator,
        &rule,
    );
    Ok(())
}

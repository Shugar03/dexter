use dexter_decision::{HeuristicGenerator, RuleBased};
use dexter_eval::recording::record_run;
use dexter_eval::scenario::{run_scenario, ScenarioSpec};
use std::error::Error;
use std::fs;
use std::path::PathBuf;

fn main() -> Result<(), Box<dyn Error>> {
    let scenarios_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../datasets/scenarios");
    let recordings_dir =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../datasets/recordings");
    fs::create_dir_all(&recordings_dir)?;

    let mut paths: Vec<_> = fs::read_dir(&scenarios_dir)?
        .map(|entry| entry.map(|entry| entry.path()))
        .collect::<Result<_, _>>()?;
    paths.sort();

    let generator = HeuristicGenerator::default();
    let decider = RuleBased::default();
    let mut recorded = 0;
    for path in paths {
        if path.extension().and_then(|ext| ext.to_str()) != Some("toml")
            || path.file_name().and_then(|name| name.to_str()) == Some("baseline.toml")
        {
            continue;
        }
        let spec: ScenarioSpec = toml::from_str(&fs::read_to_string(&path)?)?;
        if spec.driver() != "sim" {
            continue;
        }
        let run = run_scenario(&spec, &generator, &decider);
        let recording = record_run(&spec, &run)?;
        let output = serde_json::to_string_pretty(&recording)?;
        fs::write(
            recordings_dir.join(format!("{}.json", spec.scenario.id)),
            format!("{output}\n"),
        )?;
        recorded += 1;
    }

    println!(
        "recorded {recorded} sim scenarios to {}",
        recordings_dir.display()
    );
    Ok(())
}

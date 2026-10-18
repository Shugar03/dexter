use dexter_core::{Action, Element, ElementId, MouseButton, SemanticTarget, Target};
use dexter_engine::{Engine, RunConfig, Step, StepStatus};
use dexter_policy::Policy;
use dexter_sim::SimDriver;
use std::fs::{self, OpenOptions};
use std::path::PathBuf;
use std::time::Duration;

fn step(label: &str) -> Step {
    Step {
        note: None,
        action: Action::Click {
            target: Target::Semantic(SemanticTarget {
                role: Some("button".into()),
                name: Some(label.into()),
                ..Default::default()
            }),
            button: MouseButton::Left,
        },
        expect: None,
        max_attempts: None,
        app: None,
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let events = args
        .next()
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/home/ubuntu/approvals-demo/events.jsonl"));
    let inbox = args
        .next()
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/home/ubuntu/approvals-demo/inbox"));
    if let Some(parent) = events.parent() {
        fs::create_dir_all(parent)?;
    }
    if let Some(parent) = inbox.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut inbox_options = OpenOptions::new();
    inbox_options.create(true).truncate(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        inbox_options.mode(0o600);
    }
    drop(inbox_options.open(&inbox)?);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&inbox, fs::Permissions::from_mode(0o600))?;
    }

    let world = vec![
        Element {
            id: ElementId(1),
            role: Some("button".into()),
            name: Some("Guardar pendiente".into()),
            enabled: Some(true),
            actions: vec!["press".into()],
            ..Default::default()
        },
        Element {
            id: ElementId(2),
            role: Some("button".into()),
            name: Some("Guardar aprobada".into()),
            enabled: Some(true),
            actions: vec!["press".into()],
            ..Default::default()
        },
    ];
    let mut engine = Engine::new(
        SimDriver::new(world),
        Policy::embedded(),
        Duration::from_secs(60),
    );
    engine.set_journal_sink(&events)?;
    engine.set_approval_inbox(inbox.clone())?;

    assert!(matches!(
        engine.run_step(&step("Guardar pendiente"), &RunConfig::default()),
        StepStatus::NeedsApproval { .. }
    ));
    let fingerprint = match engine.run_step(&step("Guardar aprobada"), &RunConfig::default()) {
        StepStatus::NeedsApproval { fingerprint, .. } => fingerprint,
        other => return Err(format!("expected pending approval, got {other:?}").into()),
    };
    if !engine.approve_pending(&fingerprint) {
        return Err("engine rejected its live approval request".into());
    }
    if !engine
        .run_step(&step("Guardar aprobada"), &RunConfig::default())
        .done()
    {
        return Err("approved demo step did not complete".into());
    }
    println!("wrote {} and {}", events.display(), inbox.display());
    Ok(())
}

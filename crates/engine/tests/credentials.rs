use dexter_core::{
    Action, ActionResult, CredentialField, Element, Mechanism, Observation, ObservationScope,
    Target, Window,
};
use dexter_driver::{ActContext, ComputerDriver, DriverCapabilities, DriverError};
use dexter_engine::credentials::CredentialStore;
use dexter_engine::{Engine, RunConfig, Step, StepStatus};
use dexter_policy::Policy;
use dexter_sim::SimDriver;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;
use std::time::Duration;

static TEST_ID: AtomicUsize = AtomicUsize::new(0);

struct TempDir(PathBuf);

impl TempDir {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "dexter-credentials-{}-{}",
            std::process::id(),
            TEST_ID.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir(&path).unwrap();
        Self(path)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn credential_toml(name: &str, origin: &str, extra: &str) -> String {
    format!("[[credential]]\nname = {name:?}\norigin = {origin:?}\n{extra}")
}

#[test]
fn credential_store_normalizes_and_validates_origins() {
    assert_eq!(
        CredentialStore::normalize_origin("https://GitHub.com/").unwrap(),
        "https://github.com"
    );
    assert_eq!(
        CredentialStore::normalize_origin("https://a.com:443").unwrap(),
        "https://a.com"
    );
    assert_eq!(
        CredentialStore::normalize_origin("http://127.0.0.1:8080").unwrap(),
        "http://127.0.0.1:8080"
    );
    for invalid in [
        "http://example.com",
        "https://user@a.com",
        "https://a.com/login",
        "https://a.com?x=1",
        "https://a.com#section",
    ] {
        assert!(
            CredentialStore::normalize_origin(invalid).is_err(),
            "accepted forbidden origin {invalid}"
        );
    }
}

#[test]
fn credential_store_rejects_duplicate_names_and_unknown_fields() {
    let duplicate = r#"
        [[credential]]
        name = "same"
        origin = "https://example.com"
        username = "alice"

        [[credential]]
        name = "same"
        origin = "https://example.com"
        password_env = "DEXTER_TEST_SECRET"
    "#;
    assert!(CredentialStore::from_toml_str(duplicate).is_err());

    let unknown = r#"
        [[credential]]
        name = "test"
        origin = "https://example.com"
        username = "alice"
        password = "must not be stored"
    "#;
    assert!(CredentialStore::from_toml_str(unknown).is_err());
}

#[cfg(unix)]
#[test]
fn credential_store_rejects_symlinks_and_non_regular_files() {
    use std::os::unix::fs::symlink;

    let dir = TempDir::new();
    let valid = dir.0.join("credentials.toml");
    std::fs::write(
        &valid,
        credential_toml("test", "https://example.com", "username = \"alice\"\n"),
    )
    .unwrap();
    let link = dir.0.join("credentials-link.toml");
    symlink(&valid, &link).unwrap();
    assert!(CredentialStore::load(&link).is_err());
    assert!(CredentialStore::load(&dir.0).is_err());
}

#[cfg(unix)]
#[test]
fn credential_store_rejects_group_or_world_writable_files() {
    use std::os::unix::fs::PermissionsExt;

    let dir = TempDir::new();
    let path = dir.0.join("credentials.toml");
    std::fs::write(
        &path,
        credential_toml("test", "https://example.com", "username = \"alice\"\n"),
    )
    .unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o620)).unwrap();
    assert!(CredentialStore::load(&path).is_err());
}

#[test]
fn credential_store_resolution_rejects_an_unset_password_environment_variable() {
    let env_name = format!(
        "DEXTER_TEST_UNSET_CREDENTIAL_{}_{}",
        std::process::id(),
        TEST_ID.fetch_add(1, Ordering::Relaxed)
    );
    std::env::remove_var(&env_name);
    let store = CredentialStore::from_toml_str(&credential_toml(
        "github",
        "https://github.com",
        &format!("password_env = {env_name:?}\n"),
    ))
    .unwrap();
    let err = store
        .resolve("github", CredentialField::Password)
        .unwrap_err();
    assert!(err.contains("github"));
    assert!(err.contains(&env_name));
}

struct RecordingDriver {
    fills: Mutex<Vec<(CredentialField, String, String)>>,
    act_calls: AtomicUsize,
    fill_error: bool,
}

impl RecordingDriver {
    fn new(fill_error: bool) -> Self {
        Self {
            fills: Mutex::new(Vec::new()),
            act_calls: AtomicUsize::new(0),
            fill_error,
        }
    }
}

impl ComputerDriver for RecordingDriver {
    fn capabilities(&self) -> DriverCapabilities {
        DriverCapabilities {
            name: "recording",
            element_tree: true,
            screenshots: false,
            background_input: true,
        }
    }

    fn windows(&self) -> Result<Vec<Window>, DriverError> {
        Ok(Vec::new())
    }

    fn observe(&self, _scope: &ObservationScope) -> Result<Observation, DriverError> {
        Ok(Observation::default())
    }

    fn act(&self, _action: &Action, _ctx: &ActContext) -> Result<ActionResult, DriverError> {
        self.act_calls.fetch_add(1, Ordering::SeqCst);
        Err(DriverError::Unsupported("act should not be called".into()))
    }

    fn fill_secret(
        &self,
        _target: &Target,
        field: CredentialField,
        secret: &str,
        expected_origin: &str,
        _ctx: &ActContext,
    ) -> Result<ActionResult, DriverError> {
        self.fills
            .lock()
            .unwrap()
            .push((field, secret.to_owned(), expected_origin.to_owned()));
        if self.fill_error {
            Err(DriverError::Platform(format!("driver saw {secret}")))
        } else {
            Ok(ActionResult::success(
                Mechanism::Dom,
                Some(format!("filled secret {secret}")),
            ))
        }
    }
}

struct SecretEnv {
    name: String,
    value: String,
}

impl SecretEnv {
    fn new() -> Self {
        let id = TEST_ID.fetch_add(1, Ordering::Relaxed);
        let name = format!("DEXTER_TEST_CREDENTIAL_{}_{}", std::process::id(), id);
        let value = format!("credential-secret-{id}");
        std::env::set_var(&name, &value);
        Self { name, value }
    }
}

impl Drop for SecretEnv {
    fn drop(&mut self) {
        std::env::remove_var(&self.name);
    }
}

fn store_for(env_name: &str) -> CredentialStore {
    CredentialStore::from_toml_str(&credential_toml(
        "github",
        "https://github.com",
        &format!("password_env = {env_name:?}\n"),
    ))
    .unwrap()
}

fn fill_step() -> Step {
    Step {
        note: None,
        action: Action::FillCredential {
            target: Target::Semantic(Default::default()),
            credential: "github".into(),
            field: CredentialField::Password,
        },
        expect: None,
        max_attempts: None,
        app: None,
    }
}

fn fill_policy(decision: &str) -> Policy {
    Policy::from_toml(&format!(
        r#"
        [[rule]]
        action = "fill_credential"
        decision = "{decision}"
        "#
    ))
    .unwrap()
}

#[test]
fn engine_resolves_explicitly_allowed_credential_and_scrubs_results_and_journal() {
    let env = SecretEnv::new();
    let driver = RecordingDriver::new(false);
    let mut engine = Engine::new(driver, fill_policy("allow"), Duration::from_secs(60));
    engine.set_credentials(store_for(&env.name));

    let status = engine.run_step(&fill_step(), &RunConfig::default());
    let formatted = format!("{status:?}");
    assert!(!formatted.contains(&env.value));
    match &status {
        StepStatus::Done { result, .. } => {
            let detail = result.detail.as_deref().unwrap();
            assert_eq!(detail, "filled secret <redacted>");
            assert!(!detail.contains(&env.value));
        }
        other => panic!("expected successful fill, got {other:?}"),
    }
    assert_eq!(
        engine.driver().fills.lock().unwrap().as_slice(),
        &[(
            CredentialField::Password,
            env.value.clone(),
            "https://github.com".into()
        )]
    );
    let journal = serde_json::to_string(&engine.events()).unwrap();
    assert!(!journal.contains(&env.value));
}

#[test]
fn approve_all_does_not_grant_credential_fill() {
    let env = SecretEnv::new();
    let driver = RecordingDriver::new(false);
    let mut engine = Engine::new(
        driver,
        fill_policy("require_approval"),
        Duration::from_secs(60),
    );
    engine.set_credentials(store_for(&env.name));
    let config = RunConfig {
        approve_all: true,
        ..Default::default()
    };

    assert!(matches!(
        engine.run_step(&fill_step(), &config),
        StepStatus::NeedsApproval { .. }
    ));
    assert!(engine.driver().fills.lock().unwrap().is_empty());
    assert_eq!(engine.driver().act_calls.load(Ordering::SeqCst), 0);
}

#[test]
fn missing_explicit_policy_rule_denies_without_calling_the_driver() {
    let driver = RecordingDriver::new(false);
    let mut engine = Engine::new(driver, Policy::embedded(), Duration::from_secs(60));
    assert!(matches!(
        engine.run_step(&fill_step(), &RunConfig::default()),
        StepStatus::Denied { .. }
    ));
    assert!(engine.driver().fills.lock().unwrap().is_empty());
}

#[test]
fn missing_credential_store_errors_without_calling_the_driver() {
    let driver = RecordingDriver::new(false);
    let mut engine = Engine::new(driver, fill_policy("allow"), Duration::from_secs(60));
    assert!(matches!(
        engine.run_step(&fill_step(), &RunConfig::default()),
        StepStatus::Errored {
            error: DriverError::Unsupported(_)
        }
    ));
    assert!(engine.driver().fills.lock().unwrap().is_empty());
    assert_eq!(engine.driver().act_calls.load(Ordering::SeqCst), 0);
}

#[test]
fn fill_secret_errors_are_scrubbed_before_return_and_journaling() {
    let env = SecretEnv::new();
    let driver = RecordingDriver::new(true);
    let mut engine = Engine::new(driver, fill_policy("allow"), Duration::from_secs(60));
    engine.set_credentials(store_for(&env.name));

    let status = engine.run_step(&fill_step(), &RunConfig::default());
    assert!(!format!("{status:?}").contains(&env.value));
    match status {
        StepStatus::Errored { error } => assert!(!error.to_string().contains(&env.value)),
        other => panic!("expected scrubbed driver error, got {other:?}"),
    }
    assert!(!serde_json::to_string(&engine.events())
        .unwrap()
        .contains(&env.value));
}

#[test]
fn sim_driver_default_fill_secret_refuses_credentials() {
    let sim = SimDriver::new(vec![Element::default()]);
    let error = sim
        .fill_secret(
            &Target::Semantic(Default::default()),
            CredentialField::Password,
            "never-forwarded",
            "https://example.com",
            &ActContext::default(),
        )
        .unwrap_err();
    assert!(
        matches!(error, DriverError::Unsupported(message) if message.contains("verify the page origin"))
    );

    let directive = Action::FillCredential {
        target: Target::Semantic(Default::default()),
        credential: "github".into(),
        field: CredentialField::Password,
    };
    let error = sim.act(&directive, &ActContext::default()).unwrap_err();
    assert!(
        matches!(error, DriverError::Unsupported(message) if message.contains("engine calls fill_secret"))
    );
}

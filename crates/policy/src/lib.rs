//! dexter-policy — fail-closed authorization for actions.
//!
//! Models propose. Policy authorizes. No model output — including a
//! decision engine's confidence — can turn a `Deny` into an `Allow`, and
//! the crate has no notion of "trusted caller": the CLI, MCP and SDK all
//! pass through the same `evaluate` path.

use dexter_core::{Action, AppSelector, DexterError, Intrusiveness};
use serde::Deserialize;
use std::collections::HashMap;
use std::path::Path;
use std::time::{Duration, Instant};

/// What the policy concluded for one action.
#[derive(Debug, Clone, PartialEq)]
pub enum PolicyDecision {
    Allow,
    Deny { reason: String },
    RequireApproval { reason: String },
}

/// Context an action is evaluated against: which app is scoped, and a
/// human-readable hint of the resolved target (for audit messages only —
/// never trusted for matching).
#[derive(Debug, Clone, Default)]
pub struct ActionContext {
    pub app: Option<AppSelector>,
    pub target_hint: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Deserialize)]
#[serde(rename_all = "snake_case")]
enum DecisionKind {
    Allow,
    Deny,
    RequireApproval,
}

#[derive(Debug, Deserialize)]
struct RawPolicy {
    #[serde(default)]
    defaults: Defaults,
    #[serde(default)]
    rule: Vec<RawRule>,
}

#[derive(Debug, Deserialize)]
struct Defaults {
    #[serde(default = "default_mutating")]
    mutating: DecisionKind,
    /// Actions that move or capture the real pointer/keyboard. `None`
    /// means "deny" — and lets `permit_physical` distinguish an explicit
    /// `physical = "deny"` (which a CLI flag must not override) from an
    /// absent key.
    #[serde(default)]
    physical: Option<DecisionKind>,
}

impl Default for Defaults {
    fn default() -> Self {
        Self {
            mutating: default_mutating(),
            physical: None,
        }
    }
}

fn default_mutating() -> DecisionKind {
    DecisionKind::RequireApproval
}

#[derive(Debug, Deserialize)]
struct RawRule {
    /// `click`, `type_text`, `key`, `scroll`, `focus`, `set_value`,
    /// `observe`, `wait`, or `*` for every mutating action.
    action: String,
    /// Optional app filter: substring of app name, `bundle:com.x`, or
    /// `pid:123`. Absent = matches any app.
    app: Option<String>,
    /// Optional intrusiveness filter. Absent = matches any level.
    intrusiveness: Option<Intrusiveness>,
    decision: DecisionKind,
    #[serde(default)]
    reason: String,
}

/// A compiled policy: ordered rules + defaults.
pub struct Policy {
    defaults: Defaults,
    rules: Vec<RawRule>,
}

impl Policy {
    /// The shipped default: reads allowed, every mutation requires
    /// approval. Used when no policy file exists — safe, never permissive.
    pub fn embedded() -> Self {
        Self {
            defaults: Defaults::default(),
            rules: Vec::new(),
        }
    }

    /// Parse a policy from TOML text. Malformed input is an error, never a
    /// silent default.
    pub fn from_toml(text: &str) -> Result<Self, DexterError> {
        let raw: RawPolicy = toml::from_str(text)
            .map_err(|e| DexterError::InvalidInput(format!("policy TOML: {e}")))?;
        for (i, r) in raw.rule.iter().enumerate() {
            if action_kind_of(&r.action).is_none() {
                return Err(DexterError::InvalidInput(format!(
                    "policy rule {i}: unknown action '{}'",
                    r.action
                )));
            }
            validate_app_pattern(&r.app)?;
        }
        Ok(Self {
            defaults: raw.defaults,
            rules: raw.rule,
        })
    }

    /// Load from a TOML file.
    pub fn load(path: &Path) -> Result<Self, DexterError> {
        let text = std::fs::read_to_string(path)?;
        Self::from_toml(&text)
    }

    /// Consent to physical input for this invocation (the CLI's
    /// `--coords` flag). Only fills an *absent* default — an explicit
    /// `physical = "deny"` or `require_approval` in the file always wins.
    pub fn permit_physical(&mut self) {
        self.defaults.physical.get_or_insert(DecisionKind::Allow);
    }

    /// Evaluate an action. First matching rule wins; no match falls back
    /// to the intrusiveness-appropriate default.
    pub fn evaluate(&self, action: &Action, ctx: &ActionContext) -> PolicyDecision {
        let Some(kind) = action_kind(action) else {
            return PolicyDecision::Allow; // reads: Observe / Wait
        };
        let intrusiveness = action.intrusiveness();
        for rule in &self.rules {
            if !rule_matches(rule, kind, intrusiveness, ctx) {
                continue;
            }
            let reason = if rule.reason.is_empty() {
                format!(
                    "policy rule: {} {} for {}",
                    rule.decision_str(),
                    kind,
                    describe_ctx(ctx)
                )
            } else {
                rule.reason.clone()
            };
            return match rule.decision {
                DecisionKind::Allow => PolicyDecision::Allow,
                DecisionKind::Deny => PolicyDecision::Deny { reason },
                DecisionKind::RequireApproval => PolicyDecision::RequireApproval { reason },
            };
        }
        // No rule matched. Physical input has its own floor — a batch
        // approval for mutations never silently covers moving the real
        // cursor.
        let default = match intrusiveness {
            Intrusiveness::Physical => self.defaults.physical.unwrap_or(DecisionKind::Deny),
            _ => self.defaults.mutating,
        };
        let reason = match intrusiveness {
            Intrusiveness::Physical => format!(
                "physical input for {} on {} — moving the user's pointer is not allowed by default",
                kind,
                describe_ctx(ctx)
            ),
            _ => format!(
                "no policy rule for {} on {} — {}",
                kind,
                describe_ctx(ctx),
                "mutations are not allowed by default"
            ),
        };
        match default {
            DecisionKind::Allow => PolicyDecision::Allow,
            DecisionKind::Deny => PolicyDecision::Deny { reason },
            DecisionKind::RequireApproval => PolicyDecision::RequireApproval { reason },
        }
    }
}

impl RawRule {
    fn decision_str(&self) -> &'static str {
        match self.decision {
            DecisionKind::Allow => "allow",
            DecisionKind::Deny => "deny",
            DecisionKind::RequireApproval => "require_approval",
        }
    }
}

/// Internal action classification — mutating kinds only; reads return None.
fn action_kind(action: &Action) -> Option<&'static str> {
    match action {
        Action::Click { .. } => Some("click"),
        Action::TypeText { .. } => Some("type_text"),
        Action::Key { .. } => Some("key"),
        Action::Scroll { .. } => Some("scroll"),
        Action::Focus { .. } => Some("focus"),
        Action::SetValue { .. } => Some("set_value"),
        Action::Navigate { .. } => Some("navigate"),
        Action::Observe | Action::Wait { .. } => None,
    }
}

/// Rule `action` field → kind or `*` wildcard. `observe`/`wait` are valid
/// strings but never match a mutating kind.
fn action_kind_of(s: &str) -> Option<()> {
    match s {
        "*" | "click" | "type_text" | "key" | "scroll" | "focus" | "set_value" | "observe"
        | "wait" | "navigate" => Some(()),
        _ => None,
    }
}

fn rule_matches(
    rule: &RawRule,
    kind: &'static str,
    intrusiveness: Intrusiveness,
    ctx: &ActionContext,
) -> bool {
    let action_ok = rule.action == "*" || rule.action == kind;
    if !action_ok {
        return false;
    }
    if let Some(tier) = rule.intrusiveness {
        if tier != intrusiveness {
            return false;
        }
    }
    let Some(pattern) = &rule.app else {
        return true;
    };
    let Some(sel) = &ctx.app else {
        return false; // rule requires an app; unscoped action can't match
    };
    if let Some(bundle) = pattern.strip_prefix("bundle:") {
        return matches!(sel, AppSelector::BundleId(b) if b.eq_ignore_ascii_case(bundle));
    }
    if let Some(pid_s) = pattern.strip_prefix("pid:") {
        return matches!(sel, AppSelector::Pid(p) if pid_s.parse::<i32>() == Ok(*p));
    }
    match sel {
        AppSelector::Name(n) => n.to_lowercase().contains(&pattern.to_lowercase()),
        AppSelector::BundleId(b) => b.to_lowercase().contains(&pattern.to_lowercase()),
        AppSelector::Pid(_) => false,
    }
}

fn validate_app_pattern(app: &Option<String>) -> Result<(), DexterError> {
    if let Some(p) = app {
        if let Some(pid_s) = p.strip_prefix("pid:") {
            pid_s.parse::<i32>().map_err(|_| {
                DexterError::InvalidInput(format!("policy app pattern '{p}': bad pid"))
            })?;
        }
    }
    Ok(())
}

fn describe_ctx(ctx: &ActionContext) -> String {
    match &ctx.app {
        Some(AppSelector::Name(n)) => format!("app '{n}'"),
        Some(AppSelector::BundleId(b)) => format!("bundle '{b}'"),
        Some(AppSelector::Pid(p)) => format!("pid {p}"),
        None => "any app".into(),
    }
}

/// Canonical fingerprint binding an approval to (action, app).
/// Serialized JSON is deterministic for the same input.
pub fn fingerprint(action: &Action, ctx: &ActionContext) -> String {
    serde_json::to_string(&(action, &ctx.app)).unwrap_or_else(|_| "unserializable".into())
}

/// Granted approvals: bound to a fingerprint, single-use, time-boxed.
pub struct ApprovalStore {
    ttl: Duration,
    granted: HashMap<String, Instant>,
}

impl ApprovalStore {
    pub fn new(ttl: Duration) -> Self {
        Self {
            ttl,
            granted: HashMap::new(),
        }
    }

    /// Record an approval for a fingerprint.
    pub fn grant(&mut self, fingerprint: &str) {
        self.granted.insert(fingerprint.to_string(), Instant::now());
    }

    /// Check + consume: returns true exactly once per grant, and never
    /// after the TTL elapsed.
    pub fn check_and_consume(&mut self, fingerprint: &str) -> bool {
        let Some(t) = self.granted.get(fingerprint).copied() else {
            return false;
        };
        self.granted.remove(fingerprint);
        t.elapsed() <= self.ttl
    }
}

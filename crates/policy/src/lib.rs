//! dexter-policy — fail-closed authorization for actions.
//!
//! Models propose. Policy authorizes. No model output — including a
//! decision engine's confidence — can turn a `Deny` into an `Allow`, and
//! the crate has no notion of "trusted caller": the CLI, MCP and SDK all
//! pass through the same `evaluate` path.

use dexter_core::{
    Action, AppSelector, DexterError, ExecutionRoute, Intrusiveness, TargetDescriptor,
};
use serde::Deserialize;
use sha2::Digest;
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
    /// Optional target filter: case-insensitive substring matched
    /// against the resolved target's role, name or identifier. This is
    /// what lets a rule distinguish "Save" from "Delete" — it only sees
    /// what the driver resolved, never model claims.
    target: Option<String>,
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

    /// Evaluate an action by its declared shape — the v1 path kept for
    /// callers that never plan. The engine uses [`Policy::evaluate_route`]
    /// so the *route's* real tier is what gets authorized.
    pub fn evaluate(&self, action: &Action, ctx: &ActionContext) -> PolicyDecision {
        let Some(kind) = action_kind(action) else {
            return PolicyDecision::Allow; // reads: Observe / Wait
        };
        self.decide(
            kind,
            &ExecutionRoute {
                action: action.clone(),
                target: TargetDescriptor::from_action(action),
                mechanism: None,
                intrusiveness: action.intrusiveness(),
                // The floors travel on sensitivity — a caller that
                // never plans must still declare what the drivers
                // would, or secrets and destructive acts slip a batch
                // `mutating = "allow"`.
                sensitivity: match action {
                    Action::ReadClipboardText | Action::WriteClipboardText { .. } => {
                        dexter_core::Sensitivity::Secrets
                    }
                    Action::QuitApp { .. }
                    | Action::Window {
                        operation: dexter_core::WindowOperation::Close,
                        ..
                    } => dexter_core::Sensitivity::Destructive,
                    _ => dexter_core::Sensitivity::Standard,
                },
                requires_foreground: action.intrusiveness() == Intrusiveness::Physical,
            },
            ctx,
        )
    }

    /// Evaluate a planned route — the mechanism and tier the driver
    /// actually intends to use, plus the resolved target it resolved.
    /// This is the authorization boundary: a `type_text` routed to
    /// CGEvent is judged as `physical`, not `background`.
    pub fn evaluate_route(&self, route: &ExecutionRoute, ctx: &ActionContext) -> PolicyDecision {
        let Some(kind) = action_kind(&route.action) else {
            return PolicyDecision::Allow; // reads: Observe / Wait
        };
        self.decide(kind, route, ctx)
    }

    /// The shared decision: ordered rules first (first match wins),
    /// then the secrets floor, the physical floor, and the mutating
    /// default.
    fn decide(
        &self,
        kind: &'static str,
        route: &ExecutionRoute,
        ctx: &ActionContext,
    ) -> PolicyDecision {
        let intrusiveness = route.intrusiveness;
        let target = &route.target;
        for rule in &self.rules {
            if !rule_matches(rule, kind, intrusiveness, target, ctx) {
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
        // No rule matched. Secret-bearing routes (clipboard, secure
        // fields) have their own floor: a batch `mutating = "allow"`
        // never silently authorizes reading or writing secrets — each
        // one needs an explicit rule or an operator's approval.
        if route.sensitivity == dexter_core::Sensitivity::Secrets {
            return PolicyDecision::RequireApproval {
                reason: format!(
                    "sensitive {} on {} — secrets require explicit approval",
                    kind,
                    describe_ctx(ctx)
                ),
            };
        }
        // Destructive routes (quit, window close) can discard unsaved
        // state — a batch `mutating = "allow"` must not silently cover
        // them either. Same floor shape as secrets.
        if route.sensitivity == dexter_core::Sensitivity::Destructive {
            return PolicyDecision::RequireApproval {
                reason: format!(
                    "destructive {} on {} — discarding state requires explicit approval",
                    kind,
                    describe_ctx(ctx)
                ),
            };
        }
        // No rule matched. Physical input has its own floor — a batch
        // approval for mutations never silently covers moving the real
        // cursor. The floor is a gate, not the verdict: once it passes,
        // the mutating default still applies.
        if intrusiveness == Intrusiveness::Physical {
            let floor_reason = format!(
                "physical input for {} on {} — moving the user's pointer is not allowed by default",
                kind,
                describe_ctx(ctx)
            );
            match self.defaults.physical.unwrap_or(DecisionKind::Deny) {
                DecisionKind::Deny => {
                    return PolicyDecision::Deny {
                        reason: floor_reason,
                    }
                }
                DecisionKind::RequireApproval => {
                    return PolicyDecision::RequireApproval {
                        reason: floor_reason,
                    }
                }
                DecisionKind::Allow => {} // floor passed — mutating gate below
            }
        }
        let default = self.defaults.mutating;
        let reason = format!(
            "no policy rule for {} on {} — {}",
            kind,
            describe_ctx(ctx),
            "mutations are not allowed by default"
        );
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
        Action::Invoke { .. } => Some("invoke"),
        Action::LaunchApp { .. } => Some("launch_app"),
        Action::QuitApp { .. } => Some("quit_app"),
        Action::Window { .. } => Some("window"),
        Action::ReadClipboardText => Some("clipboard_read"),
        Action::WriteClipboardText { .. } => Some("clipboard_write"),
        Action::Drag { .. } => Some("drag"),
        Action::Observe | Action::Wait { .. } => None,
    }
}

/// Rule `action` field → kind or `*` wildcard. `observe`/`wait` are valid
/// strings but never match a mutating kind.
fn action_kind_of(s: &str) -> Option<()> {
    match s {
        "*" | "click" | "type_text" | "key" | "scroll" | "focus" | "set_value" | "observe"
        | "wait" | "navigate" | "invoke" | "launch_app" | "quit_app" | "window"
        | "clipboard_read" | "clipboard_write" | "drag" => Some(()),
        _ => None,
    }
}

fn rule_matches(
    rule: &RawRule,
    kind: &'static str,
    intrusiveness: Intrusiveness,
    target: &TargetDescriptor,
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
    if let Some(pattern) = &rule.target {
        let needle = pattern.to_lowercase();
        let hit = [&target.role, &target.name, &target.identifier]
            .into_iter()
            .flatten()
            .any(|f| f.to_lowercase().contains(&needle));
        if !hit {
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

/// Opaque digest of a payload (typed text, set values) — the journal
/// carries the digest so an approval can bind the exact content without
/// ever exposing it.
pub fn payload_digest(text: &str) -> String {
    hex_sha256(text.as_bytes())
}

fn hex_sha256(bytes: &[u8]) -> String {
    use sha2::Sha256;
    let mut h = Sha256::new();
    h.update(bytes);
    h.finalize().iter().map(|b| format!("{b:02x}")).collect()
}

/// The payload an action carries, when it is content (not structure).
/// Every content-bearing action binds its payload — a granted approval
/// is never fungible across different text.
fn payload_of(action: &Action) -> Option<&str> {
    match action {
        Action::TypeText { text, .. } => Some(text),
        Action::SetValue { value, .. } => Some(value),
        Action::WriteClipboardText { text } => Some(text),
        _ => None,
    }
}

/// The non-secret parameters of an action — everything the operator
/// approved that is not already bound by the target descriptor or the
/// payload digest. Without these a grant is fungible across the whole
/// action class: approving `key "return"` would cover `cmd+shift+q`,
/// and approving `minimize` on a window would cover `close` on it.
/// Secret-adjacent values (the URL — query strings can carry tokens)
/// bind by digest, never in plaintext.
fn params_of(action: &Action) -> serde_json::Value {
    match action {
        Action::Click { button, count, .. } => {
            serde_json::json!({"button": button, "count": count})
        }
        Action::Key { chord } => serde_json::json!({"chord": chord}),
        Action::Scroll { delta, .. } => serde_json::json!({"delta": delta}),
        Action::Wait { millis } => serde_json::json!({"millis": millis}),
        Action::Navigate { url } => serde_json::json!({"url_sha256": payload_digest(url)}),
        Action::Invoke { action, .. } => serde_json::json!({"action": action}),
        Action::LaunchApp { app, activate } => {
            serde_json::json!({"app": app, "activate": activate})
        }
        Action::QuitApp { app } => serde_json::json!({"app": app}),
        Action::Window {
            window_id,
            operation,
        } => serde_json::json!({"window_id": window_id, "operation": operation}),
        // `from` rides the main descriptor; `to` binds the same way so
        // a grant for one destination never covers another.
        Action::Drag {
            to, duration_ms, ..
        } => serde_json::json!({
            "to": grant_target(&dexter_core::TargetDescriptor::from_target(Some(to))),
            "duration_ms": duration_ms,
        }),
        _ => serde_json::Value::Null,
    }
}

/// The target identity a grant binds. For element targets the minted
/// element handle is bound — an approval is scoped to that element,
/// not to any same-shaped target in the app. The observation id is
/// *not* bound: it is a per-snapshot nonce and grant+retry re-observes,
/// so binding it would make approvals unreachable. Semantic identity
/// fields ride alongside so a grant still reads as "the Guardar
/// button", not an opaque token — and under engine enrichment they
/// make the fingerprint drift when the world changed. Explicit point
/// coordinates stay: a point target *is* its coordinates.
fn grant_target(t: &dexter_core::TargetDescriptor) -> serde_json::Value {
    serde_json::json!({
        "role": t.role,
        "name": t.name,
        "identifier": t.identifier,
        // The element handle binds which element the grant covers;
        // `observation` deliberately does not — it is a per-snapshot
        // nonce, and binding it would make a granted approval
        // unreachable once the world is re-observed (grant+retry).
        // Element ids mint deterministically, so the same element
        // re-observed in an unchanged world keeps its handle; when the
        // engine's live observation matches the descriptor's,
        // enrichment adds role/name/identifier on top, so a changed
        // world still drifts the fingerprint back to needs_approval.
        "element": t.element,
        "window_id": t.window_id,
        "point": t.point,
        "focused": t.focused,
    })
}

/// Canonical tuple a grant binds: the action kind, the *route's*
/// mechanism and tier, the resolved target identity, the app scope and
/// a digest of the payload — hashed so the fingerprint itself carries
/// nothing readable. Deterministic: re-planning the same action under
/// an unchanged world reproduces the same fingerprint.
fn canonical_fingerprint(route: &ExecutionRoute, ctx: &ActionContext) -> String {
    let canonical = serde_json::json!({
        "kind": action_kind(&route.action).unwrap_or("read"),
        "mechanism": route.mechanism,
        "intrusiveness": route.intrusiveness,
        "sensitivity": route.sensitivity,
        "requires_foreground": route.requires_foreground,
        "target": grant_target(&route.target),
        "params": params_of(&route.action),
        "app": ctx.app,
        "payload_sha256": payload_of(&route.action).map(payload_digest),
    });
    format!("sha256:{}", hex_sha256(canonical.to_string().as_bytes()))
}

/// Opaque fingerprint binding an approval to a planned route.
pub fn fingerprint_route(route: &ExecutionRoute, ctx: &ActionContext) -> String {
    canonical_fingerprint(route, ctx)
}

/// Opaque fingerprint for the action's declared shape — the v1 binding
/// kept for callers that never plan (`dexter act` compat, tests).
pub fn fingerprint(action: &Action, ctx: &ActionContext) -> String {
    canonical_fingerprint(&ExecutionRoute::legacy(action), ctx)
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

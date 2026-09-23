use crate::target::Target;
use serde::{Deserialize, Serialize};

/// Mouse button for click actions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MouseButton {
    #[default]
    Left,
    Right,
    Middle,
}

/// A key plus optional modifiers, e.g. `cmd+s`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct KeyChord {
    /// Canonical key name: `a`, `return`, `tab`, `escape`, `left`, `f5`, `space`, ...
    pub key: String,
    /// Modifier names: `cmd`, `ctrl`, `alt`, `shift`.
    pub modifiers: Vec<String>,
}

impl KeyChord {
    /// Parse `"cmd+shift+s"` style strings.
    pub fn parse(s: &str) -> Result<Self, crate::DexterError> {
        let mut modifiers = Vec::new();
        let mut key = None;
        for part in s.split('+').map(|p| p.trim().to_lowercase()) {
            match part.as_str() {
                "cmd" | "command" | "meta" => modifiers.push("cmd".into()),
                "ctrl" | "control" => modifiers.push("ctrl".into()),
                "alt" | "option" | "opt" => modifiers.push("alt".into()),
                "shift" => modifiers.push("shift".into()),
                "" => {}
                other => {
                    if key.is_some() {
                        return Err(crate::DexterError::InvalidInput(format!(
                            "key chord '{s}' has more than one non-modifier key"
                        )));
                    }
                    key = Some(other.to_string());
                }
            }
        }
        let key = key.ok_or_else(|| {
            crate::DexterError::InvalidInput(format!("key chord '{s}' has no key"))
        })?;
        Ok(Self { key, modifiers })
    }
}

/// Scroll in pixels (positive y scrolls content down).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct ScrollDelta {
    pub dx: f64,
    pub dy: f64,
}

/// How much an action can disturb the human using the machine.
/// Derived from the action's target — never model-declared.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Intrusiveness {
    /// Semantic mutation (DOM/AX) — the physical cursor never moves and
    /// focus is not stolen. The user keeps full control.
    Background,
    /// Visible but non-capturing — a window raises, a page navigates.
    Visual,
    /// Moves or captures the real pointer/keyboard (CGEvent, typing into
    /// whatever happens to be focused). Gated separately by policy.
    Physical,
}

impl Target {
    /// The intrusiveness of acting through this target alone.
    pub fn intrusiveness(&self) -> Intrusiveness {
        match self {
            // Element, semantic and focused targets resolve to a control —
            // drivers act on it semantically (AXPress, DOM click).
            Target::Element { .. } | Target::Semantic(_) | Target::Focused => {
                Intrusiveness::Background
            }
            // A window target activates/raises — visible, captures nothing.
            Target::Window { .. } => Intrusiveness::Visual,
            // Raw coordinates can only be reached with real input events.
            Target::Point { .. } => Intrusiveness::Physical,
        }
    }
}

impl Action {
    /// Worst-case intrusiveness of this action — the tier of its most
    /// invasive target. A click is `Background` on an element and
    /// `Physical` on coordinates; the verb alone decides nothing.
    pub fn intrusiveness(&self) -> Intrusiveness {
        match self {
            Action::Click { target, .. } | Action::Focus { target } => target.intrusiveness(),
            Action::SetValue { target, .. } => target.intrusiveness(),
            Action::Scroll { target, .. } | Action::TypeText { target, .. } => target
                .as_ref()
                .map(Target::intrusiveness)
                // No target = act on whatever is focused / at the pointer —
                // real keystrokes or pointer-relative scroll.
                .unwrap_or(Intrusiveness::Physical),
            // Key chords are physical input: no semantic equivalent exists.
            Action::Key { .. } => Intrusiveness::Physical,
            // Raising a window / opening a URL is visible but captures no
            // input — the user tolerates it without losing control.
            Action::Navigate { .. } => Intrusiveness::Visual,
            Action::Observe | Action::Wait { .. } => Intrusiveness::Background,
        }
    }
}

/// A single physical/semantic action against the computer.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Action {
    /// Semantic or coordinate click.
    Click {
        target: Target,
        #[serde(default)]
        button: MouseButton,
    },
    /// Type text into the currently focused element (or `target` first focuses it).
    TypeText {
        text: String,
        #[serde(default)]
        target: Option<Target>,
    },
    /// Press a key chord.
    Key { chord: KeyChord },
    /// Scroll at the pointer location (or a target).
    Scroll {
        delta: ScrollDelta,
        #[serde(default)]
        target: Option<Target>,
    },
    /// Focus a window or element.
    Focus { target: Target },
    /// Set an element's value directly (semantic set, e.g. AXValue).
    SetValue { target: Target, value: String },
    /// Re-observe the world.
    Observe,
    /// Wait a fixed amount of time.
    Wait { millis: u64 },
    /// Open a URL. Browser drivers navigate the session; the macOS
    /// driver hands it to LaunchServices (`open`). A mutation — policy
    /// applies like any other action.
    Navigate { url: String },
}

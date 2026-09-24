use crate::observation::AppSelector;
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

/// What a window operation does. `window_id: None` on the action means
/// "the frontmost window of the scoped app".
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum WindowOperation {
    /// Open a new window/tab.
    New,
    /// Give the window keyboard focus.
    Focus,
    /// Raise without necessarily focusing.
    Raise,
    Close,
    Minimize,
    /// Un-minimize / un-hide.
    Restore,
    Move {
        x: f64,
        y: f64,
    },
    Resize {
        width: f64,
        height: f64,
    },
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
            Action::SetValue { target, .. } | Action::Invoke { target, .. } => {
                target.intrusiveness()
            }
            Action::Drag { from, to, .. } => {
                // The tier of the more invasive endpoint — a coordinate
                // endpoint forces the physical route.
                let a = from.intrusiveness();
                let b = to.intrusiveness();
                if a == Intrusiveness::Physical || b == Intrusiveness::Physical {
                    Intrusiveness::Physical
                } else if a == Intrusiveness::Visual || b == Intrusiveness::Visual {
                    Intrusiveness::Visual
                } else {
                    Intrusiveness::Background
                }
            }
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
            Action::Navigate { .. } | Action::Window { .. } => Intrusiveness::Visual,
            // Launching/quitting an app is visible but captures no input.
            Action::LaunchApp { .. } | Action::QuitApp { .. } => Intrusiveness::Visual,
            // Clipboard acts are semantic — no pointer, no focus theft.
            // Their risk is sensitivity, not intrusiveness (policy binds
            // the sensitivity floor separately).
            Action::ReadClipboardText | Action::WriteClipboardText { .. } => {
                Intrusiveness::Background
            }
            Action::Observe | Action::Wait { .. } => Intrusiveness::Background,
        }
    }
}

/// A single physical/semantic action against the computer.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Action {
    /// Semantic or coordinate click. `count` is the click count —
    /// 2 = double-click (open), valid 1..=3. Drivers plan a semantic
    /// `open` when the element advertises it; a physical multi-click is
    /// the gated last resort.
    Click {
        target: Target,
        #[serde(default)]
        button: MouseButton,
        #[serde(default = "default_click_count")]
        count: u8,
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
    /// Perform a named semantic action an element advertises (`press`,
    /// `open`, `show_menu`, `raise`, ...). The element must still
    /// advertise it on fresh resolution — unknown names are
    /// unsupported, never passed through blindly.
    Invoke { target: Target, action: String },
    /// Launch an application by name or bundle id. Pid selectors are
    /// invalid — you can't launch a process that must already exist.
    LaunchApp { app: AppSelector, activate: bool },
    /// Ask an application to quit normally (no force-quit in v2).
    QuitApp { app: AppSelector },
    /// Operate on a window: focus, raise, close, minimize, geometry.
    /// `None` targets the scoped app's frontmost window.
    Window {
        window_id: Option<u32>,
        operation: WindowOperation,
    },
    /// Read the pasteboard's text. Sensitive read — policy-gated, never
    /// journaled or fingerprinted in plaintext.
    ReadClipboardText,
    /// Replace the pasteboard's text. Sensitive write — bounded payload,
    /// policy-gated, never journaled.
    WriteClipboardText { text: String },
    /// Drag from one target to another. Both endpoints resolve before
    /// policy and revalidate before the button goes down.
    Drag {
        from: Target,
        to: Target,
        #[serde(default = "default_drag_ms")]
        duration_ms: u64,
    },
}

fn default_click_count() -> u8 {
    1
}
fn default_drag_ms() -> u64 {
    300
}

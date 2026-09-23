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

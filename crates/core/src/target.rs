use crate::element::ElementId;
use crate::observation::ObservationId;
use serde::{Deserialize, Serialize};

/// u64 as a decimal string — `WorldChanged::from` crosses the JS
/// boundary (MCP/SDKs) where f64 can't hold every u64.
mod u64_str {
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(v: &u64, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&v.to_string())
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<u64, D::Error> {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Repr {
            S(String),
            N(u64),
        }
        match Repr::deserialize(d)? {
            Repr::S(s) => s.parse().map_err(serde::de::Error::custom),
            Repr::N(n) => Ok(n),
        }
    }
}

/// Semantic lookup for an element inside an observation.
/// All present fields must match; absent fields are wildcards.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct SemanticTarget {
    /// Normalized role (`button`, `text_field`, ...) or raw (`AXButton`).
    pub role: Option<String>,
    /// Exact (case-insensitive) match against the element's name.
    pub name: Option<String>,
    /// Case-insensitive substring match against the element's name.
    pub name_contains: Option<String>,
    /// Case-insensitive substring match against the element's value.
    pub value_contains: Option<String>,
    pub identifier: Option<String>,
    pub enabled: Option<bool>,
    /// Disambiguate when several elements match. `None` requires exactly one
    /// match (fail-closed on ambiguity); `Some(n)` explicitly picks the nth
    /// match in tree order.
    pub index: Option<usize>,
}

/// Where an action lands.
///
/// Serde `untagged` tries variants in declaration order: the structurally
/// constrained ones go first, and `Semantic` — whose fields are all
/// optional — stays last as the catch-all. Reordering breaks parsing.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Target {
    /// Element from a previous observation — carries the ObservationId so
    /// stale references (element ids from an older snapshot) can be rejected.
    Element {
        observation: ObservationId,
        element: ElementId,
    },
    /// Raw screen coordinates — the fallback of last resort.
    Point { x: f64, y: f64 },
    /// A whole window (by CGWindowID).
    Window { window_id: u32 },
    /// Resolved semantically at action time. Catch-all: must stay after
    /// every variant with required fields.
    Semantic(SemanticTarget),
    /// The currently focused element (serializes as `null` under untagged).
    Focused,
}

/// Predicate over an element's value, used by [`ExpectedState::ElementValue`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ValuePredicate {
    Equals(String),
    Contains(String),
    Matches(String),
}

/// A verifiable expectation about the world after an action.
/// Struct variants so internally-tagged serde handles every shape.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ExpectedState {
    /// At least one element matches.
    ElementExists {
        target: SemanticTarget,
    },
    /// No element matches.
    ElementAbsent {
        target: SemanticTarget,
    },
    /// A matching element's value satisfies the predicate.
    ElementValue {
        target: SemanticTarget,
        predicate: ValuePredicate,
    },
    /// Substring appears anywhere in the observation digest.
    TextPresent {
        text: String,
    },
    /// The focused element matches.
    FocusedElement {
        target: SemanticTarget,
    },
    /// Some window's title contains the substring.
    WindowTitleContains {
        text: String,
    },
    /// An application with this name/bundle has at least one window.
    AppRunning {
        name: String,
    },
    /// The world signature differs from a previously captured one — the
    /// catch-all "something changed" for acts whose effect cannot be
    /// predicted (clicks). `from` is a `world_model::signature` value,
    /// serialized as a string so JSON consumers (JS) don't lose bits.
    WorldChanged {
        #[serde(with = "u64_str")]
        from: u64,
    },
    All {
        all: Vec<ExpectedState>,
    },
    Any {
        any: Vec<ExpectedState>,
    },
    Not {
        not: Box<ExpectedState>,
    },
}

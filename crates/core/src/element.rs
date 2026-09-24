use serde::{Deserialize, Serialize};

/// Stable identifier for an element within a single [`crate::Observation`].
/// Element ids are not valid across observations.
///
/// Wire contract (v2): serializes as `"e_4"`. Deserialization accepts
/// the v2 string form, the v1 bare number `4`, and a bare-digit string
/// `"4"` — so a `dexter_observe` response round-trips into `dexter_act`
/// and old payloads still parse.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ElementId(pub u64);

impl Serialize for ElementId {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&self.to_string())
    }
}

impl<'de> Deserialize<'de> for ElementId {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct V;
        impl serde::de::Visitor<'_> for V {
            type Value = ElementId;
            fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                write!(f, "an element id (`4` or `\"e_4\"`)")
            }
            fn visit_u64<E>(self, v: u64) -> Result<Self::Value, E> {
                Ok(ElementId(v))
            }
            fn visit_i64<E: serde::de::Error>(self, v: i64) -> Result<Self::Value, E> {
                u64::try_from(v)
                    .map(ElementId)
                    .map_err(|_| E::custom("negative element id"))
            }
            fn visit_str<E: serde::de::Error>(self, v: &str) -> Result<Self::Value, E> {
                let digits = v.strip_prefix("e_").unwrap_or(v);
                digits
                    .parse::<u64>()
                    .map(ElementId)
                    .map_err(|_| E::custom(format!("invalid element id '{v}'")))
            }
        }
        d.deserialize_any(V)
    }
}

impl std::fmt::Display for ElementId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "e_{}", self.0)
    }
}

/// Which perception layer produced this element.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ElementSource {
    Accessibility,
    Dom,
    Ocr,
    Vision,
}

/// Rectangle in global screen coordinates (pixels, top-left origin).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Rect {
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
}

impl Rect {
    pub fn center(&self) -> crate::Point {
        crate::Point {
            x: self.x + self.w / 2.0,
            y: self.y + self.h / 2.0,
        }
    }

    pub fn contains(&self, p: crate::Point) -> bool {
        p.x >= self.x && p.x <= self.x + self.w && p.y >= self.y && p.y <= self.y + self.h
    }
}

/// A normalized UI element. The same shape is produced by the macOS
/// Accessibility tree, a DOM snapshot, OCR, or vision — the agent never needs
/// to know which.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Element {
    pub id: ElementId,
    /// Parent element within the same observation, if any.
    pub parent: Option<ElementId>,
    /// Depth in the source tree (0 = application/root).
    pub depth: u32,
    /// Normalized role: `button`, `text_field`, `window`, ... (the raw role,
    /// e.g. `AXButton`, is kept in `raw_role`).
    pub role: Option<String>,
    pub raw_role: Option<String>,
    pub subrole: Option<String>,
    /// Human-facing name: AXTitle, AXDescription or label.
    pub name: Option<String>,
    /// Stringified value (text contents, slider position, checked state).
    pub value: Option<String>,
    /// Frame in screen coordinates when the source reports one.
    pub bounds: Option<Rect>,
    pub enabled: Option<bool>,
    pub focused: bool,
    /// Semantic actions the element advertises (`AXPress` -> `press`, ...).
    pub actions: Vec<String>,
    /// Stable-ish platform identifier (AXIdentifier, DOM id) when present.
    pub identifier: Option<String>,
    pub source: ElementSource,
}

impl Default for Element {
    /// Placeholder element (id 0, no attributes) — tests fill what matters.
    fn default() -> Self {
        Self {
            id: ElementId(0),
            parent: None,
            depth: 0,
            role: None,
            raw_role: None,
            subrole: None,
            name: None,
            value: None,
            bounds: None,
            enabled: None,
            focused: false,
            actions: Vec::new(),
            identifier: None,
            source: ElementSource::Accessibility,
        }
    }
}

impl Element {
    /// Display name used by matching and digests.
    pub fn label(&self) -> Option<&str> {
        self.name
            .as_deref()
            .filter(|s| !s.is_empty())
            .or(self.identifier.as_deref())
    }
}

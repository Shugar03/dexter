//! UI Automation tree walk: `IUIAutomationElement` → normalized `Element`s.
//!
//! Per-element failures are tolerated (windows vanish mid-walk all the
//! time) and counted once per element; truncation is explicit, never
//! silent. Only the ControlView is walked — the semantic tree, not the
//! raw provider graph.

use dexter_core::{Element, ElementId, ElementSource, Observation, ObservationScope, Rect};
use dexter_driver::DriverError;
use std::sync::Mutex;
use std::time::SystemTime;
use windows::core::{Interface, BOOL};
use windows::Win32::Foundation::HWND;
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CoUninitialize, CLSCTX_ALL, COINIT_MULTITHREADED,
};
use windows::Win32::UI::Accessibility::CUIAutomation;
use windows::Win32::UI::Accessibility::{
    IUIAutomation, IUIAutomationElement, IUIAutomationTogglePattern, IUIAutomationTreeWalker,
    IUIAutomationValuePattern, ToggleState, ToggleState_Indeterminate, ToggleState_Off,
    UIA_ExpandCollapsePatternId, UIA_InvokePatternId, UIA_ScrollItemPatternId, UIA_ScrollPatternId,
    UIA_SelectionItemPatternId, UIA_TogglePatternId, UIA_ValuePatternId, UIA_PATTERN_ID,
};

/// Value length cap — mirrors the AX walker's 500-char bound so a
/// multi-megabyte document doesn't pin the observation.
const MAX_VALUE_CHARS: usize = 500;

/// COM initialized for the duration of one observe/act call. UIA is a
/// client-side COM API: init MTA, uninit only what we initialized.
pub struct ComGuard(bool);

impl Drop for ComGuard {
    fn drop(&mut self) {
        if self.0 {
            unsafe { CoUninitialize() };
        }
    }
}

fn com_init() -> Result<ComGuard, DriverError> {
    let hr = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
    if hr.is_ok() {
        // S_OK/S_FALSE both refcount our init — uninit on drop.
        return Ok(ComGuard(true));
    }
    // The caller's thread is already a single-threaded apartment —
    // UIA works from an STA (MTA is only the recommendation), and
    // nothing of ours was initialized to uninit.
    if hr == windows::Win32::Foundation::RPC_E_CHANGED_MODE {
        return Ok(ComGuard(false));
    }
    Err(DriverError::Platform(format!("CoInitializeEx: {hr}")))
}

/// One UIA call at a time: concurrent `CoCreateInstance` + walker
/// setup races the UIA core's lazy init and fails with E_FAIL.
/// Observations serialize anyway — a process-wide lock is honest, and
/// `act()` re-walks under the same lock.
pub static OBSERVE_LOCK: Mutex<()> = Mutex::new(());

/// A live UIA session: the client object plus the ControlView walker
/// every tree walk uses.
pub struct Uia {
    automation: IUIAutomation,
    walker: IUIAutomationTreeWalker,
}

impl Uia {
    /// Turn a raw element (e.g. straight from `ElementFromHandle`) into
    /// its ControlView ancestor — the walker only sees ControlView. A
    /// failed normalize keeps the raw element: its reads will flag
    /// themselves as collection errors.
    pub fn normalize(&self, el: IUIAutomationElement) -> IUIAutomationElement {
        unsafe { self.walker.NormalizeElement(&el) }.unwrap_or(el)
    }

    pub fn element_from(&self, hwnd: HWND) -> Result<IUIAutomationElement, DriverError> {
        unsafe { self.automation.ElementFromHandle(hwnd) }
            .map_err(|e| DriverError::Platform(format!("ElementFromHandle: {e}")))
    }

    /// The element holding keyboard focus right now.
    pub fn focused(&self) -> Result<IUIAutomationElement, DriverError> {
        unsafe { self.automation.GetFocusedElement() }
            .map_err(|e| DriverError::Platform(format!("GetFocusedElement: {e}")))
    }
}

pub fn connect() -> Result<(ComGuard, Uia), DriverError> {
    let guard = com_init()?;
    let automation: IUIAutomation = unsafe {
        CoCreateInstance(
            &CUIAutomation,
            Option::<&windows::core::IUnknown>::None,
            CLSCTX_ALL,
        )
    }
    .map_err(|e| DriverError::Platform(format!("CoCreateInstance(UIA): {e}")))?;
    let walker = unsafe { automation.ControlViewWalker() }
        .map_err(|e| DriverError::Platform(format!("ControlViewWalker: {e}")))?;
    Ok((guard, Uia { automation, walker }))
}

pub struct UiaTree {
    pub elements: Vec<Element>,
    pub truncated: bool,
    /// Elements whose attributes could not be fully read.
    pub errors: u32,
}

struct Ctx {
    max_depth: u32,
    max_elements: usize,
    elements: Vec<Element>,
    /// Live handles aligned with `elements` — only collected for `act`,
    /// which resolves an element then calls patterns on its handle.
    nodes: Option<Vec<IUIAutomationElement>>,
    truncated: bool,
    errors: u32,
    next_id: u64,
}

/// `observe` on Windows: list windows, resolve the selector, anchor
/// each of the app's HWNDs in UIA and walk. `scope.window` narrows the
/// walk to that one HWND's subtree — the native scoping
/// `scope_to_window` detects (no bounds post-filter needed).
pub fn observe(
    id: dexter_core::ObservationId,
    scope: &ObservationScope,
) -> Result<Observation, DriverError> {
    let mut obs = Observation {
        id,
        timestamp: SystemTime::now(),
        app: scope.app.clone(),
        pid: None,
        windows: crate::win::list_windows()?,
        ..Default::default()
    };

    let Some(selector) = &scope.app else {
        obs.digest = dexter_world_model::digest(&obs, 250);
        return Ok(obs);
    };

    let pid = crate::apps::resolve_pid(selector)?;
    obs.pid = Some(pid);
    obs.windows.retain(|w| w.pid == pid);

    let _serialize = OBSERVE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (_guard, uia) = connect()?;
    let tree = match scope.window {
        Some(wid) => {
            if !obs.windows.iter().any(|w| w.id == wid) {
                return Err(DriverError::NotFound(format!(
                    "window {wid} not in app pid {pid}"
                )));
            }
            obs.windows.retain(|w| w.id == wid);
            let hwnd = crate::win::hwnd_of(wid);
            let root = uia.normalize(uia.element_from(hwnd)?);
            collect(&uia, root, scope.max_depth, scope.max_elements)
        }
        None => {
            let mut ctx = Ctx {
                max_depth: scope.max_depth,
                max_elements: scope.max_elements,
                elements: Vec::new(),
                nodes: None,
                truncated: false,
                errors: 0,
                next_id: 1,
            };
            for w in obs.windows.iter() {
                let hwnd = crate::win::hwnd_of(w.id);
                match uia.element_from(hwnd) {
                    Ok(el) => {
                        let root = uia.normalize(el);
                        walk(&uia, &root, None, 0, &mut ctx);
                    }
                    // A window closed between EnumWindows and here —
                    // partial data is honest, count it.
                    Err(_) => ctx.errors += 1,
                }
                if ctx.truncated {
                    break;
                }
            }
            UiaTree {
                elements: ctx.elements,
                truncated: ctx.truncated,
                errors: ctx.errors,
            }
        }
    };

    obs.elements_truncated = tree.truncated;
    obs.collection_errors = tree.errors;
    obs.elements = tree.elements;
    // The window layer reports real windows but UIA produced nothing —
    // elevated process, dead provider, or UIA off. `not found` results
    // against this observation are not definitive.
    obs.ax_limited = !obs.windows.is_empty() && obs.elements.is_empty();

    // Opt-in OCR: warranted when UIA produced nothing usable (limited
    // or empty tree) or the caller narrowed to one window — the same
    // opt-in-only contract macOS uses, same degrade-on-failure rule.
    if scope.vision && (obs.ax_limited || obs.elements.is_empty() || scope.window.is_some()) {
        crate::vision::augment(&mut obs, scope);
    }

    if scope.screenshot {
        let path = scope
            .screenshot_path
            .clone()
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| std::env::temp_dir().join(format!("dexter-obs-{}.png", id.0)));
        crate::capture::capture_app_window(&obs.windows, &path)?;
        obs.screenshot = Some(path.display().to_string());
    }

    obs.digest = dexter_world_model::digest(&obs, 250);
    Ok(obs)
}

fn collect(uia: &Uia, root: IUIAutomationElement, max_depth: u32, max_elements: usize) -> UiaTree {
    let mut ctx = Ctx {
        max_depth,
        max_elements,
        elements: Vec::new(),
        nodes: None,
        truncated: false,
        errors: 0,
        next_id: 1,
    };
    walk(uia, &root, None, 0, &mut ctx);
    UiaTree {
        elements: ctx.elements,
        truncated: ctx.truncated,
        errors: ctx.errors,
    }
}

/// One COM read: success yields `Some`, failure flags the element so
/// it counts once in `collection_errors` regardless of how many of its
/// properties we lost.
fn rd<T>(r: windows::core::Result<T>, failed: &mut bool) -> Option<T> {
    match r {
        Ok(v) => Some(v),
        Err(_) => {
            *failed = true;
            None
        }
    }
}

fn rd_bstr(r: windows::core::Result<windows::core::BSTR>, failed: &mut bool) -> Option<String> {
    rd(r, failed)
        .map(|b| String::from_utf16_lossy(&b))
        .filter(|s| !s.is_empty())
}

/// A pattern is present when `GetCurrentPatternAs` returns its
/// interface — the probe is the honest "can this element do X".
pub fn has_pattern<T: Interface>(el: &IUIAutomationElement, id: UIA_PATTERN_ID) -> Option<T> {
    unsafe { el.GetCurrentPatternAs::<T>(id) }.ok()
}

/// The walk `observe` produces plus the live `IUIAutomationElement`
/// handle at every index — act resolves an element by its flat
/// position, then calls patterns on `nodes[index]`.
pub struct LiveWalk {
    pub elements: Vec<Element>,
    pub nodes: Vec<IUIAutomationElement>,
    pub truncated: bool,
}

/// Walk `roots` in observe's order (each root at depth 0, sequential
/// ids), keeping the live handle of every element. Truncation stops
/// the walk at the same limits a scoped observe uses.
pub fn walk_roots(
    uia: &Uia,
    roots: &[IUIAutomationElement],
    max_depth: u32,
    max_elements: usize,
) -> LiveWalk {
    let mut ctx = Ctx {
        max_depth,
        max_elements,
        elements: Vec::new(),
        nodes: Some(Vec::new()),
        truncated: false,
        errors: 0,
        next_id: 1,
    };
    for root in roots {
        walk(uia, root, None, 0, &mut ctx);
        if ctx.truncated {
            break;
        }
    }
    LiveWalk {
        elements: ctx.elements,
        nodes: ctx.nodes.unwrap_or_default(),
        truncated: ctx.truncated,
    }
}

pub fn bounds_of(el: &IUIAutomationElement, failed: &mut bool) -> Option<Rect> {
    let rc = rd(unsafe { el.CurrentBoundingRectangle() }, failed)?;
    // UIA reports an all-zero rect for elements that have no on-screen
    // extent (offscreen items, collapsed nodes) — `None`, not a fake
    // 0×0 box at the origin.
    let (w, h) = (rc.right - rc.left, rc.bottom - rc.top);
    (w > 0 && h > 0).then_some(Rect {
        x: rc.left as f64,
        y: rc.top as f64,
        w: w as f64,
        h: h as f64,
    })
}

/// Semantic actions, normalized to the vocabulary the engine and the
/// other walkers share (`press`, `set_value`, `focus`, `scroll`, ...).
fn actions_of(el: &IUIAutomationElement, writable_value: bool) -> Vec<String> {
    let mut actions: Vec<&'static str> = Vec::new();
    use windows::Win32::UI::Accessibility as a;
    // Invoke/Toggle/SelectionItem are real activation semantics —
    // `press` means "this element does something when activated".
    // LegacyIAccessible is deliberately NOT a press: the MSAA bridge
    // exposes it on nearly every bridged element (title bars, groups),
    // which would drown `press` in no-op noise.
    if has_pattern::<a::IUIAutomationInvokePattern>(el, UIA_InvokePatternId).is_some()
        || has_pattern::<IUIAutomationTogglePattern>(el, UIA_TogglePatternId).is_some()
        || has_pattern::<a::IUIAutomationSelectionItemPattern>(el, UIA_SelectionItemPatternId)
            .is_some()
    {
        actions.push("press");
    }
    if writable_value {
        actions.push("set_value");
    }
    if has_pattern::<a::IUIAutomationExpandCollapsePattern>(el, UIA_ExpandCollapsePatternId)
        .is_some()
    {
        actions.push("expand_collapse");
    }
    if has_pattern::<a::IUIAutomationScrollPattern>(el, UIA_ScrollPatternId).is_some() {
        actions.push("scroll");
    }
    if has_pattern::<a::IUIAutomationScrollItemPattern>(el, UIA_ScrollItemPatternId).is_some() {
        actions.push("scroll_into_view");
    }
    actions.into_iter().map(String::from).collect()
}

fn read_element(
    el: &IUIAutomationElement,
    id: ElementId,
    parent: Option<ElementId>,
    depth: u32,
    errors: &mut u32,
) -> Element {
    let mut failed = false;
    // A vanished element still contributes what was read — each failed
    // property flags it, the element counts once in `errors`.
    let control = rd(unsafe { el.CurrentControlType() }, &mut failed).map(|c| c.0);
    let raw_role = control.and_then(crate::control_type_name);
    let role = raw_role.and_then(crate::uia_role).map(String::from);
    let name = rd_bstr(unsafe { el.CurrentName() }, &mut failed);
    let identifier = rd_bstr(unsafe { el.CurrentAutomationId() }, &mut failed);
    let enabled = rd(unsafe { el.CurrentIsEnabled() }, &mut failed).map(|b: BOOL| b.as_bool());
    let focused = rd(unsafe { el.CurrentHasKeyboardFocus() }, &mut failed)
        .map(|b: BOOL| b.as_bool())
        .unwrap_or(false);
    let bounds = bounds_of(el, &mut failed);
    // Password/secure fields never leak a value — redacted here, before
    // the observation exists, same rule as AX secure fields and DOM
    // `type=password`.
    let sensitive = rd(unsafe { el.CurrentIsPassword() }, &mut failed)
        .map(|b: BOOL| b.as_bool())
        .unwrap_or(false);

    let mut value = None;
    let mut writable = false;
    if let Some(v) = has_pattern::<IUIAutomationValuePattern>(el, UIA_ValuePatternId) {
        writable = !rd(unsafe { v.CurrentIsReadOnly() }, &mut failed)
            .map(|b: BOOL| b.as_bool())
            .unwrap_or(true);
        if !sensitive {
            value = rd_bstr(unsafe { v.CurrentValue() }, &mut failed)
                .filter(|s| s.chars().count() <= MAX_VALUE_CHARS);
        }
    }
    if !sensitive {
        if let Some(t) = has_pattern::<IUIAutomationTogglePattern>(el, UIA_TogglePatternId) {
            value = rd(unsafe { t.CurrentToggleState() }, &mut failed).map(|s: ToggleState| {
                if s == ToggleState_Off {
                    "off"
                } else if s == ToggleState_Indeterminate {
                    "indeterminate"
                } else {
                    "on"
                }
                .to_string()
            });
        }
    }

    let mut actions = actions_of(el, writable);
    if rd(unsafe { el.CurrentIsKeyboardFocusable() }, &mut failed)
        .map(|b: BOOL| b.as_bool())
        .unwrap_or(false)
        && !actions.iter().any(|a| a == "focus")
    {
        actions.push("focus".into());
    }

    if failed {
        *errors += 1;
    }
    Element {
        id,
        parent,
        depth,
        role,
        raw_role: raw_role.map(String::from),
        subrole: None,
        name,
        value,
        bounds,
        enabled,
        focused,
        actions,
        identifier,
        source: ElementSource::Accessibility,
    }
}

fn walk(
    uia: &Uia,
    el: &IUIAutomationElement,
    parent: Option<ElementId>,
    depth: u32,
    ctx: &mut Ctx,
) {
    if ctx.elements.len() >= ctx.max_elements || depth > ctx.max_depth {
        ctx.truncated = true;
        return;
    }
    let id = ElementId(ctx.next_id);
    ctx.next_id += 1;
    ctx.elements
        .push(read_element(el, id, parent, depth, &mut ctx.errors));
    if let Some(nodes) = &mut ctx.nodes {
        nodes.push(el.clone());
    }
    if depth >= ctx.max_depth {
        return;
    }
    let mut child = unsafe { uia.walker.GetFirstChildElement(el) };
    while let Ok(c) = child {
        let next = unsafe { uia.walker.GetNextSiblingElement(&c) };
        walk(uia, &c, Some(id), depth + 1, ctx);
        if ctx.truncated {
            return;
        }
        child = next;
    }
    // `Err` just means "no (more) children" or a vanished sibling —
    // not a collection failure; only element reads count into `errors`.
}

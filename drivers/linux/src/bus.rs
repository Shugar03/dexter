//! AT-SPI2 bus backend (Linux only): the a11y bus connection, the
//! registry's application list, top-level frame enumeration and the
//! accessible-tree walk behind `LinuxDriver::windows()` / `observe()`.
//!
//! Everything is blocking `zbus` — one short-lived connection per call,
//! properties read uncached (a `Lazily` cache would register a match
//! rule per node, thousands per walk). Every per-node failure is a
//! `collection_errors` increment, never a panic or a fabricated value:
//! applications exit mid-walk and GTK removes objects as popovers
//! close, so `ServiceUnknown` / `UnknownObject` are the normal weather
//! of a walk, not exceptional.

use std::collections::HashMap;
use std::time::SystemTime;

use dexter_core::{
    AppSelector, Element, ElementId, ElementSource, Observation, ObservationScope, Window,
};
use dexter_driver::DriverError;
use zbus::blocking::{proxy::Builder, Connection, Proxy};
use zbus::names::BusName;
use zbus::proxy::CacheProperties;
use zbus::zvariant::{OwnedObjectPath, OwnedValue};

use crate::atspi::{action_name, app_name_matches, extents_rect, toggle_value, window_id, States};
use crate::{atspi_role, atspi_role_name};

const IFACE_ACCESSIBLE: &str = "org.a11y.atspi.Accessible";
const IFACE_ACTION: &str = "org.a11y.atspi.Action";
const IFACE_COMPONENT: &str = "org.a11y.atspi.Component";
const IFACE_EDITABLE_TEXT: &str = "org.a11y.atspi.EditableText";
const IFACE_TEXT: &str = "org.a11y.atspi.Text";
const IFACE_VALUE: &str = "org.a11y.atspi.Value";
const REGISTRY_NAME: &str = "org.a11y.atspi.Registry";
const ROOT_PATH: &str = "/org/a11y/atspi/accessible/root";
/// `AtspiCoordType::Screen`.
const COORD_SCREEN: u32 = 0;
const MAX_VALUE_CHARS: usize = 500;
const MAX_ACTIONS: i32 = 8;

fn platform(ctx: &str, e: impl std::fmt::Display) -> DriverError {
    DriverError::Platform(format!("atspi: {ctx}: {e}"))
}

/// Connect to the a11y bus: ask `org.a11y.Bus` on the session bus for
/// its address, then dial it. Fails (honestly, as `Platform`) when
/// there is no session bus or no `at-spi-bus-launcher` — the state a
/// headless CI box or a tty login is in.
fn connect() -> Result<Connection, DriverError> {
    let session = Connection::session().map_err(|e| platform("session bus", e))?;
    let launcher = Proxy::new(&session, "org.a11y.Bus", "/org/a11y/bus", "org.a11y.Bus")
        .map_err(|e| platform("org.a11y.Bus proxy", e))?;
    let address: String = launcher
        .call("GetAddress", &())
        .map_err(|e| platform("org.a11y.Bus.GetAddress", e))?;
    if address.is_empty() {
        return Err(DriverError::Platform(
            "atspi: org.a11y.Bus.GetAddress returned no address".into(),
        ));
    }
    zbus::blocking::connection::Builder::address(address.as_str())
        .and_then(|b| b.build())
        .map_err(|e| platform("a11y bus connect", e))
}

/// Whether the a11y bus answers right now — the capability probe.
pub fn available() -> bool {
    connect().is_ok()
}

/// One accessible object: the owning connection's unique name plus the
/// object path — the `(so)` pair AT-SPI uses for every reference.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Acc {
    pub name: String,
    pub path: OwnedObjectPath,
}

impl Acc {
    fn proxy<'a>(&'a self, conn: &Connection, iface: &'a str) -> zbus::Result<Proxy<'a>> {
        Builder::new(conn)
            .destination(self.name.as_str())?
            .path(self.path.as_ref())?
            .interface(iface)?
            .cache_properties(CacheProperties::No)
            .build()
    }

    fn children(&self, conn: &Connection) -> zbus::Result<Vec<Acc>> {
        let p = self.proxy(conn, IFACE_ACCESSIBLE)?;
        let raw: Vec<(String, OwnedObjectPath)> = p.call("GetChildren", &())?;
        Ok(raw
            .into_iter()
            .map(|(name, path)| Acc { name, path })
            .collect())
    }

    /// Every `org.a11y.atspi.Accessible` property in one round trip.
    fn accessible_props(&self, conn: &Connection) -> zbus::Result<HashMap<String, OwnedValue>> {
        let p = self.proxy(conn, "org.freedesktop.DBus.Properties")?;
        p.call("GetAll", &(IFACE_ACCESSIBLE,))
    }

    fn role(&self, conn: &Connection) -> zbus::Result<u32> {
        self.proxy(conn, IFACE_ACCESSIBLE)?.call("GetRole", &())
    }

    fn states(&self, conn: &Connection) -> zbus::Result<States> {
        let words: Vec<u32> = self.proxy(conn, IFACE_ACCESSIBLE)?.call("GetState", &())?;
        Ok(States::from_words(&words))
    }

    fn interfaces(&self, conn: &Connection) -> zbus::Result<Vec<String>> {
        self.proxy(conn, IFACE_ACCESSIBLE)?
            .call("GetInterfaces", &())
    }

    fn extents(&self, conn: &Connection) -> zbus::Result<(i32, i32, i32, i32)> {
        self.proxy(conn, IFACE_COMPONENT)?
            .call("GetExtents", &(COORD_SCREEN,))
    }

    fn n_actions(&self, conn: &Connection) -> zbus::Result<i32> {
        self.proxy(conn, IFACE_ACTION)?.get_property("NActions")
    }

    fn action_name(&self, conn: &Connection, index: i32) -> zbus::Result<String> {
        self.proxy(conn, IFACE_ACTION)?.call("GetName", &(index,))
    }

    fn current_value(&self, conn: &Connection) -> zbus::Result<f64> {
        self.proxy(conn, IFACE_VALUE)?.get_property("CurrentValue")
    }

    fn text(&self, conn: &Connection, max_chars: i32) -> zbus::Result<String> {
        self.proxy(conn, IFACE_TEXT)?
            .call("GetText", &(0i32, max_chars))
    }
}

fn string_prop(props: &HashMap<String, OwnedValue>, key: &str) -> Option<String> {
    props
        .get(key)
        .and_then(|v| <&str>::try_from(v).ok())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

fn int_prop(props: &HashMap<String, OwnedValue>, key: &str) -> Option<i32> {
    props.get(key).and_then(|v| i32::try_from(v).ok())
}

/// A registered application: its root object, pid and names.
#[derive(Debug, Clone)]
pub struct App {
    pub root: Acc,
    pub pid: i32,
    /// `Accessible.Name` of the application root (what the toolkit
    /// registered), falling back to the process `comm`.
    pub name: String,
    pub comm: Option<String>,
}

fn comm_of(pid: i32) -> Option<String> {
    std::fs::read_to_string(format!("/proc/{pid}/comm"))
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

/// The registry's children: one root object per registered
/// application. Applications whose pid the bus can't resolve (gone
/// between listing and lookup) are skipped — there is nothing honest
/// to attribute their windows to.
fn applications(conn: &Connection) -> Result<Vec<App>, DriverError> {
    let registry = Acc {
        name: REGISTRY_NAME.into(),
        path: OwnedObjectPath::try_from(ROOT_PATH).map_err(|e| platform("root path", e))?,
    };
    let roots = registry
        .children(conn)
        .map_err(|e| platform("registry GetChildren", e))?;
    let dbus = zbus::blocking::fdo::DBusProxy::new(conn).map_err(|e| platform("DBus proxy", e))?;
    let mut apps = Vec::new();
    for root in roots {
        if root.name == REGISTRY_NAME {
            continue;
        }
        let Ok(bus_name) = BusName::try_from(root.name.as_str()) else {
            continue;
        };
        let Ok(pid) = dbus.get_connection_unix_process_id(bus_name) else {
            continue;
        };
        let Ok(pid) = i32::try_from(pid) else {
            continue;
        };
        let comm = comm_of(pid);
        let name = root
            .accessible_props(conn)
            .ok()
            .and_then(|p| string_prop(&p, "Name"))
            .or_else(|| comm.clone())
            .unwrap_or_else(|| root.name.clone());
        apps.push(App {
            root,
            pid,
            name,
            comm,
        });
    }
    Ok(apps)
}

/// Top-level frames of one application: its root's children that have
/// a screen extent. A child without one (unrealized or hidden window,
/// GTK's `(G_MININT, G_MININT, 1, 1)` sentinel) is not a window a user
/// could see or we could anchor on, so it is not listed.
fn frames_of(conn: &Connection, app: &App) -> Vec<(Window, Acc)> {
    let Ok(children) = app.root.children(conn) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for child in children {
        let Some(bounds) = child.extents(conn).ok().and_then(extents_rect) else {
            continue;
        };
        let states = child.states(conn).unwrap_or_default();
        let title = child
            .accessible_props(conn)
            .ok()
            .and_then(|p| string_prop(&p, "Name"));
        out.push((
            Window {
                id: window_id(&child.name, child.path.as_str()),
                pid: app.pid,
                app: app.name.clone(),
                bundle_id: None,
                title,
                bounds,
                on_screen: states.on_screen(),
                layer: 0,
            },
            child,
        ));
    }
    out
}

fn windows_on(conn: &Connection, apps: &[App]) -> Vec<(Window, Acc)> {
    apps.iter().flat_map(|a| frames_of(conn, a)).collect()
}

/// Every top-level frame on the bus with its accessible handle.
pub fn list_windows() -> Result<Vec<(Window, Acc)>, DriverError> {
    let conn = connect()?;
    let apps = applications(&conn)?;
    Ok(windows_on(&conn, &apps))
}

/// `AppSelector` → pid of a registered application. `Name` matches the
/// registered application name or the process `comm`; two distinct
/// pids matching is `Ambiguous`, none `AppNotFound`. `BundleId` has no
/// Linux meaning and is `Unsupported` rather than guessed.
pub fn resolve_pid(selector: &AppSelector, apps: &[App]) -> Result<i32, DriverError> {
    match selector {
        AppSelector::Pid(pid) => {
            if apps.iter().any(|a| a.pid == *pid) {
                Ok(*pid)
            } else {
                Err(DriverError::AppNotFound(format!(
                    "pid {pid} is not registered on the a11y bus"
                )))
            }
        }
        AppSelector::Name(name) => {
            let mut pids: Vec<i32> = apps
                .iter()
                .filter(|a| app_name_matches(name, Some(&a.name), a.comm.as_deref()))
                .map(|a| a.pid)
                .collect();
            pids.sort_unstable();
            pids.dedup();
            match pids.as_slice() {
                [] => Err(DriverError::AppNotFound(format!(
                    "no registered application named {name:?}"
                ))),
                [pid] => Ok(*pid),
                many => Err(DriverError::Ambiguous(format!(
                    "{} applications named {name:?} (pids {many:?})",
                    many.len()
                ))),
            }
        }
        AppSelector::BundleId(id) => Err(DriverError::Unsupported(format!(
            "bundle ids have no linux equivalent ({id})"
        ))),
    }
}

struct Ctx<'c> {
    conn: &'c Connection,
    max_depth: u32,
    max_elements: usize,
    elements: Vec<Element>,
    truncated: bool,
    errors: u32,
    next_id: u64,
}

/// Record a failed read: `None` plus the node's failure flag.
fn rd<T>(r: zbus::Result<T>, failed: &mut bool) -> Option<T> {
    match r {
        Ok(v) => Some(v),
        Err(_) => {
            *failed = true;
            None
        }
    }
}

/// Walk one accessible: read it, push its `Element`, recurse into its
/// children. Nodes that are not showing (hidden notebook pages,
/// unmapped dialogs) are skipped with their subtrees — invisible
/// controls are not targets. Defunct nodes count as errors and stop.
fn walk(acc: &Acc, parent: Option<ElementId>, depth: u32, ctx: &mut Ctx<'_>) {
    if ctx.truncated {
        return;
    }
    if ctx.elements.len() >= ctx.max_elements {
        ctx.truncated = true;
        return;
    }
    let mut failed = false;
    let states = rd(acc.states(ctx.conn), &mut failed);
    if let Some(s) = states {
        if s.has(States::DEFUNCT) {
            ctx.errors += 1;
            return;
        }
        if !s.showing() {
            return;
        }
    }
    let props = rd(acc.accessible_props(ctx.conn), &mut failed).unwrap_or_default();
    let raw_role = rd(acc.role(ctx.conn), &mut failed).and_then(atspi_role_name);
    let role = raw_role.and_then(atspi_role);
    let ifaces = rd(acc.interfaces(ctx.conn), &mut failed).unwrap_or_default();
    let has = |i: &str| ifaces.iter().any(|x| x == i);

    let bounds = if has(IFACE_COMPONENT) {
        rd(acc.extents(ctx.conn), &mut failed).and_then(extents_rect)
    } else {
        None
    };
    // GTK dialogs and frames are `SHOWING` without `VISIBLE` bits on
    // some children; a showing node without a positive extent still
    // has nothing a pointer could reach — walk it (its children may),
    // but it carries no bounds.

    let sensitive = raw_role == Some("password text");
    let mut value = raw_role.zip(states).and_then(|(r, s)| toggle_value(r, s));
    if value.is_none() && !sensitive {
        if has(IFACE_VALUE) {
            value = rd(acc.current_value(ctx.conn), &mut failed).map(|v| format!("{v}"));
        } else if has(IFACE_EDITABLE_TEXT)
            || (has(IFACE_TEXT) && matches!(role, Some("text_field" | "text_area")))
        {
            value = rd(acc.text(ctx.conn, MAX_VALUE_CHARS as i32 + 1), &mut failed)
                .map(|t| t.trim_end().to_string())
                .filter(|t| !t.is_empty() && t.chars().count() <= MAX_VALUE_CHARS);
        }
    }

    let mut actions: Vec<String> = Vec::new();
    if has(IFACE_ACTION) {
        if let Some(n) = rd(acc.n_actions(ctx.conn), &mut failed) {
            for i in 0..n.min(MAX_ACTIONS) {
                if let Some(a) = rd(acc.action_name(ctx.conn, i), &mut failed)
                    .as_deref()
                    .and_then(action_name)
                {
                    if !actions.iter().any(|x| x == a) {
                        actions.push(a.to_string());
                    }
                }
            }
        }
    }
    if let Some(s) = states {
        if (s.has(States::EDITABLE) || has(IFACE_EDITABLE_TEXT))
            && !s.has(States::READ_ONLY)
            && !actions.iter().any(|a| a == "set_value")
        {
            actions.push("set_value".into());
        }
        if s.has(States::FOCUSABLE) && !actions.iter().any(|a| a == "focus") {
            actions.push("focus".into());
        }
    }

    if failed {
        ctx.errors += 1;
    }

    let id = ElementId(ctx.next_id);
    ctx.next_id += 1;
    ctx.elements.push(Element {
        id,
        parent,
        depth,
        role: role.map(str::to_string),
        raw_role: raw_role.map(str::to_string),
        subrole: None,
        name: string_prop(&props, "Name").or_else(|| string_prop(&props, "Description")),
        value,
        bounds,
        enabled: states.and_then(States::enabled),
        focused: states.is_some_and(|s| s.has(States::FOCUSED)),
        actions,
        identifier: string_prop(&props, "AccessibleId"),
        source: ElementSource::Accessibility,
    });

    if depth >= ctx.max_depth || int_prop(&props, "ChildCount") == Some(0) {
        return;
    }
    let children = match acc.children(ctx.conn) {
        Ok(c) => c,
        Err(_) => {
            ctx.errors += 1;
            return;
        }
    };
    for child in &children {
        walk(child, Some(id), depth + 1, ctx);
        if ctx.truncated {
            return;
        }
    }
}

/// `LinuxDriver::observe`. Unscoped: windows only. App-scoped: resolve
/// the pid, keep its windows (optionally one), walk each frame's tree.
/// `ax_limited` is "windows exist but the walk produced nothing" — the
/// signal that `not found` against this observation is not definitive.
/// Screenshots have no Linux backend: a requested capture fails
/// `Unsupported` rather than being silently dropped. Opt-in vision has
/// no provider either; the request counts as one collection error so
/// the degraded perception is visible, never an implicit success.
pub fn observe(
    id: dexter_core::ObservationId,
    scope: &ObservationScope,
) -> Result<Observation, DriverError> {
    if scope.screenshot {
        return Err(DriverError::Unsupported(
            "linux screenshots not implemented".into(),
        ));
    }
    let conn = connect()?;
    let apps = applications(&conn)?;
    let mut frames = windows_on(&conn, &apps);
    let mut obs = Observation {
        id,
        timestamp: SystemTime::now(),
        app: scope.app.clone(),
        pid: None,
        ..Default::default()
    };

    let Some(selector) = &scope.app else {
        obs.windows = frames.into_iter().map(|(w, _)| w).collect();
        obs.digest = dexter_world_model::digest(&obs, 250);
        return Ok(obs);
    };

    let pid = resolve_pid(selector, &apps)?;
    obs.pid = Some(pid);
    frames.retain(|(w, _)| w.pid == pid);
    if let Some(wid) = scope.window {
        if !frames.iter().any(|(w, _)| w.id == wid) {
            return Err(DriverError::NotFound(format!(
                "window {wid} not in app pid {pid}"
            )));
        }
        frames.retain(|(w, _)| w.id == wid);
    }

    let mut ctx = Ctx {
        conn: &conn,
        max_depth: scope.max_depth,
        max_elements: scope.max_elements,
        elements: Vec::new(),
        truncated: false,
        errors: 0,
        next_id: 1,
    };
    for (_, acc) in &frames {
        walk(acc, None, 0, &mut ctx);
        if ctx.truncated {
            break;
        }
    }
    obs.windows = frames.into_iter().map(|(w, _)| w).collect();
    obs.elements = ctx.elements;
    obs.elements_truncated = ctx.truncated;
    obs.collection_errors = ctx.errors;
    obs.ax_limited = !obs.windows.is_empty() && obs.elements.is_empty();
    if scope.vision {
        obs.collection_errors += 1;
    }
    obs.digest = dexter_world_model::digest(&obs, 250);
    Ok(obs)
}

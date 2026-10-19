use dexter_core::{Rect, Window as AtspiWindow};
use dexter_driver::DriverError;
use x11rb::{
    connection::Connection,
    protocol::xproto::{
        Atom, AtomEnum, ClientMessageEvent, ConnectionExt as _, EventMask, Window as XWindow,
    },
    rust_connection::RustConnection,
};

const PROPERTY_LENGTH: u32 = 16_384;
const SIZE_TOLERANCE: f64 = 8.0;

pub(crate) enum ActivationRequest {
    Sent,
    Unsupported(String),
    NoMatch,
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct WaylandClientCandidate {
    pub(crate) window: XWindow,
    pub(crate) root_rect: Rect,
    framed_rect: Rect,
}

fn platform(context: &str, error: impl std::fmt::Display) -> DriverError {
    DriverError::Platform(format!("EWMH {context}: {error}"))
}

fn intern_atom(connection: &RustConnection, name: &str) -> Result<Atom, DriverError> {
    connection
        .intern_atom(false, name.as_bytes())
        .map_err(|error| platform("InternAtom request failed", error))?
        .reply()
        .map(|reply| reply.atom)
        .map_err(|error| platform("InternAtom reply failed", error))
}

fn property32(
    connection: &RustConnection,
    window: XWindow,
    property: Atom,
) -> Result<Option<(Atom, Vec<u32>)>, DriverError> {
    let reply = connection
        .get_property(false, window, property, AtomEnum::ANY, 0, PROPERTY_LENGTH)
        .map_err(|error| platform("GetProperty request failed", error))?
        .reply()
        .map_err(|error| platform("GetProperty reply failed", error))?;
    if reply.type_ == u32::from(AtomEnum::NONE) {
        return Ok(None);
    }
    Ok(reply
        .value32()
        .map(|values| (reply.type_, values.collect())))
}

fn utf8_property(
    connection: &RustConnection,
    window: XWindow,
    property: Atom,
    utf8_atom: Atom,
) -> Result<Option<String>, DriverError> {
    let reply = connection
        .get_property(false, window, property, utf8_atom, 0, PROPERTY_LENGTH)
        .map_err(|error| platform("GetProperty request failed", error))?
        .reply()
        .map_err(|error| platform("GetProperty reply failed", error))?;
    if reply.type_ != utf8_atom || reply.format != 8 {
        return Ok(None);
    }
    Ok(String::from_utf8(reply.value).ok())
}

fn window_name(
    connection: &RustConnection,
    window: XWindow,
    net_wm_name: Atom,
    wm_name: Atom,
    utf8_atom: Atom,
) -> Result<Option<String>, DriverError> {
    if let Some(name) = utf8_property(connection, window, net_wm_name, utf8_atom)? {
        return Ok(Some(name));
    }
    let reply = connection
        .get_property(false, window, wm_name, AtomEnum::ANY, 0, PROPERTY_LENGTH)
        .map_err(|error| platform("WM_NAME request failed", error))?
        .reply()
        .map_err(|error| platform("WM_NAME reply failed", error))?;
    if reply.type_ == u32::from(AtomEnum::NONE) || reply.format != 8 {
        return Ok(None);
    }
    let name = if reply.type_ == u32::from(AtomEnum::STRING) {
        reply.value.into_iter().map(char::from).collect()
    } else if reply.type_ == utf8_atom {
        String::from_utf8(reply.value.clone())
            .unwrap_or_else(|_| String::from_utf8_lossy(&reply.value).into_owned())
    } else {
        String::from_utf8(reply.value.clone())
            .unwrap_or_else(|_| String::from_utf8_lossy(&reply.value).into_owned())
    };
    Ok(Some(name))
}

fn geometry_matches(
    connection: &RustConnection,
    root: XWindow,
    client: XWindow,
    frame: &AtspiWindow,
    net_frame_extents: Atom,
) -> Result<bool, DriverError> {
    geometry_matches_scaled(
        connection,
        root,
        client,
        frame,
        net_frame_extents,
        1.0,
        SIZE_TOLERANCE,
    )
}

fn geometry_matches_scaled(
    connection: &RustConnection,
    root: XWindow,
    client: XWindow,
    frame: &AtspiWindow,
    net_frame_extents: Atom,
    scale: f64,
    tolerance: f64,
) -> Result<bool, DriverError> {
    let (client_rect, framed_rect) =
        client_root_rects(connection, root, client, net_frame_extents)?;
    Ok(
        rect_matches_scaled(client_rect, frame.bounds, scale, tolerance)
            || rect_matches_scaled(framed_rect, frame.bounds, scale, tolerance),
    )
}

fn client_root_rects(
    connection: &RustConnection,
    root: XWindow,
    client: XWindow,
    net_frame_extents: Atom,
) -> Result<(Rect, Rect), DriverError> {
    let geometry = connection
        .get_geometry(client)
        .map_err(|error| platform("GetGeometry request failed", error))?
        .reply()
        .map_err(|error| platform("GetGeometry reply failed", error))?;
    let translated = connection
        .translate_coordinates(client, root, 0, 0)
        .map_err(|error| platform("TranslateCoordinates request failed", error))?
        .reply()
        .map_err(|error| platform("TranslateCoordinates reply failed", error))?;
    let extents = frame_extents(connection, client, net_frame_extents)?;
    let client_rect = Rect {
        x: f64::from(translated.dst_x),
        y: f64::from(translated.dst_y),
        w: f64::from(geometry.width),
        h: f64::from(geometry.height),
    };
    let framed_rect = Rect {
        x: client_rect.x - f64::from(extents[0]),
        y: client_rect.y - f64::from(extents[2]),
        w: client_rect.w + f64::from(extents[0]) + f64::from(extents[1]),
        h: client_rect.h + f64::from(extents[2]) + f64::from(extents[3]),
    };
    Ok((client_rect, framed_rect))
}

fn frame_extents(
    connection: &RustConnection,
    client: XWindow,
    net_frame_extents: Atom,
) -> Result<[u32; 4], DriverError> {
    let reply = connection
        .get_property(
            false,
            client,
            net_frame_extents,
            AtomEnum::ANY,
            0,
            PROPERTY_LENGTH,
        )
        .map_err(|error| platform("_NET_FRAME_EXTENTS request failed", error))?
        .reply()
        .map_err(|error| platform("_NET_FRAME_EXTENTS reply failed", error))?;
    if reply.type_ == u32::from(AtomEnum::NONE) {
        return Ok([0; 4]);
    }
    if reply.type_ != u32::from(AtomEnum::CARDINAL) {
        return Err(DriverError::Platform(
            "EWMH _NET_FRAME_EXTENTS has an unexpected type".into(),
        ));
    }
    let values = reply
        .value32()
        .ok_or_else(|| {
            DriverError::Platform("EWMH _NET_FRAME_EXTENTS is not a 32-bit property".into())
        })?
        .collect::<Vec<_>>();
    values.try_into().map_err(|_| {
        DriverError::Platform("EWMH _NET_FRAME_EXTENTS must contain four values".into())
    })
}

fn rect_matches_scaled(rect: Rect, frame: Rect, scale: f64, tolerance: f64) -> bool {
    (rect.x - frame.x * scale).abs() <= tolerance
        && (rect.y - frame.y * scale).abs() <= tolerance
        && (rect.w - frame.w * scale).abs() <= tolerance
        && (rect.h - frame.h * scale).abs() <= tolerance
}

fn client_matches(
    connection: &RustConnection,
    root: XWindow,
    client: XWindow,
    frame: &AtspiWindow,
    atoms: &Atoms,
) -> Result<bool, DriverError> {
    let Some(frame_name) = frame.title.as_deref() else {
        return Ok(false);
    };
    let Some((pid_type, pid_values)) = property32(connection, client, atoms.net_wm_pid)? else {
        return Ok(false);
    };
    if pid_type != u32::from(AtomEnum::CARDINAL)
        || pid_values.first().copied() != u32::try_from(frame.pid).ok()
    {
        return Ok(false);
    }
    if window_name(
        connection,
        client,
        atoms.net_wm_name,
        atoms.wm_name,
        atoms.utf8_string,
    )?
    .as_deref()
        != Some(frame_name)
    {
        return Ok(false);
    }
    geometry_matches(connection, root, client, frame, atoms.net_frame_extents)
}

struct Atoms {
    net_active_window: Atom,
    net_client_list: Atom,
    net_supported: Atom,
    net_supporting_wm_check: Atom,
    net_frame_extents: Atom,
    net_wm_name: Atom,
    net_wm_pid: Atom,
    utf8_string: Atom,
    wm_name: Atom,
}

impl Atoms {
    fn new(connection: &RustConnection) -> Result<Self, DriverError> {
        Ok(Self {
            net_active_window: intern_atom(connection, "_NET_ACTIVE_WINDOW")?,
            net_client_list: intern_atom(connection, "_NET_CLIENT_LIST")?,
            net_supported: intern_atom(connection, "_NET_SUPPORTED")?,
            net_supporting_wm_check: intern_atom(connection, "_NET_SUPPORTING_WM_CHECK")?,
            net_frame_extents: intern_atom(connection, "_NET_FRAME_EXTENTS")?,
            net_wm_name: intern_atom(connection, "_NET_WM_NAME")?,
            net_wm_pid: intern_atom(connection, "_NET_WM_PID")?,
            utf8_string: intern_atom(connection, "UTF8_STRING")?,
            wm_name: u32::from(AtomEnum::WM_NAME),
        })
    }
}

fn has_window_manager(
    connection: &RustConnection,
    root: XWindow,
    atoms: &Atoms,
) -> Result<bool, DriverError> {
    let Some((root_type, root_values)) =
        property32(connection, root, atoms.net_supporting_wm_check)?
    else {
        return Ok(false);
    };
    if root_type != u32::from(AtomEnum::WINDOW) {
        return Ok(false);
    }
    let Some(wm_window) = root_values.first().copied() else {
        return Ok(false);
    };
    let Some((wm_type, wm_values)) =
        property32(connection, wm_window, atoms.net_supporting_wm_check)?
    else {
        return Ok(false);
    };
    Ok(wm_type == u32::from(AtomEnum::WINDOW) && wm_values.first() == Some(&wm_window))
}

fn supports_activation(
    connection: &RustConnection,
    root: XWindow,
    atoms: &Atoms,
) -> Result<bool, DriverError> {
    let Some((supported_type, supported)) = property32(connection, root, atoms.net_supported)?
    else {
        return Ok(false);
    };
    Ok(supported_type == u32::from(AtomEnum::ATOM) && supported.contains(&atoms.net_active_window))
}

pub(crate) fn request_activation(frame: &AtspiWindow) -> Result<ActivationRequest, DriverError> {
    if crate::capture::wayland_active() {
        return Ok(ActivationRequest::Unsupported(
            "EWMH activation is unavailable in Wayland sessions".into(),
        ));
    }
    let (connection, screen_num) = match x11rb::connect(None) {
        Ok(connection) => connection,
        Err(error) => {
            return Ok(ActivationRequest::Unsupported(format!(
                "EWMH could not connect to the X display: {error}"
            )));
        }
    };
    let root = connection
        .setup()
        .roots
        .get(screen_num)
        .map(|screen| screen.root)
        .ok_or_else(|| DriverError::Platform("EWMH X11 screen is unavailable".into()))?;
    let atoms = Atoms::new(&connection)?;
    if !has_window_manager(&connection, root, &atoms)? {
        return Ok(ActivationRequest::Unsupported(
            "no EWMH window manager is available".into(),
        ));
    }
    if !supports_activation(&connection, root, &atoms)? {
        return Ok(ActivationRequest::Unsupported(
            "window manager does not support _NET_ACTIVE_WINDOW".into(),
        ));
    }

    let clients = property32(&connection, root, atoms.net_client_list)?
        .filter(|(type_, _)| *type_ == u32::from(AtomEnum::WINDOW))
        .map(|(_, clients)| clients)
        .unwrap_or_default();
    let mut matches = Vec::new();
    for client in clients {
        if client_matches(&connection, root, client, frame, &atoms)? {
            matches.push(client);
        }
    }
    let client = match matches.as_slice() {
        [] => return Ok(ActivationRequest::NoMatch),
        [client] => *client,
        _ => {
            return Err(DriverError::Ambiguous(format!(
                "multiple X11 client windows match window {}",
                frame.id
            )));
        }
    };
    let event = ClientMessageEvent::new(32, client, atoms.net_active_window, [2, 0, 0, 0, 0]);
    connection
        .send_event(
            false,
            root,
            EventMask::SUBSTRUCTURE_REDIRECT | EventMask::SUBSTRUCTURE_NOTIFY,
            event,
        )
        .map_err(|error| platform("ClientMessage request failed", error))?
        .check()
        .map_err(|error| platform("ClientMessage check failed", error))?;
    Ok(ActivationRequest::Sent)
}

pub(crate) fn wayland_client_candidates(
    connection: &RustConnection,
    root: XWindow,
    pid: i32,
) -> Result<Vec<WaylandClientCandidate>, DriverError> {
    let atoms = Atoms::new(connection)?;
    let Some((client_type, clients)) = property32(connection, root, atoms.net_client_list)? else {
        return Ok(Vec::new());
    };
    if client_type != u32::from(AtomEnum::WINDOW) {
        return Ok(Vec::new());
    }

    let mut candidates = Vec::new();
    for client in clients {
        let Some((pid_type, pid_values)) = property32(connection, client, atoms.net_wm_pid)? else {
            continue;
        };
        if pid_type != u32::from(AtomEnum::CARDINAL)
            || pid_values.first().copied() != u32::try_from(pid).ok()
        {
            continue;
        }
        let (root_rect, framed_rect) =
            client_root_rects(connection, root, client, atoms.net_frame_extents)?;
        candidates.push(WaylandClientCandidate {
            window: client,
            root_rect,
            framed_rect,
        });
    }
    Ok(candidates)
}

pub(crate) fn classify_wayland_candidates(
    frame: Rect,
    gdk_scale: f64,
    candidates: &[WaylandClientCandidate],
) -> Result<usize, DriverError> {
    if candidates.is_empty() {
        return Err(DriverError::Unsupported(
            "native Wayland window: AT-SPI extents are window-relative and cannot be placed on the portal screenshot"
                .into(),
        ));
    }

    let matching = candidates
        .iter()
        .enumerate()
        .filter(|(_, candidate)| {
            rect_matches_scaled(candidate.root_rect, frame, gdk_scale, 2.0 * gdk_scale)
                || rect_matches_scaled(candidate.framed_rect, frame, gdk_scale, 2.0 * gdk_scale)
        })
        .map(|(index, _)| index)
        .collect::<Vec<_>>();
    match matching.as_slice() {
        [index] => Ok(*index),
        [] => {
            let candidate_rects = candidates
                .iter()
                .map(|candidate| {
                    format!(
                        "0x{:x} ({},{} {}x{})",
                        candidate.window,
                        candidate.root_rect.x,
                        candidate.root_rect.y,
                        candidate.root_rect.w,
                        candidate.root_rect.h
                    )
                })
                .collect::<Vec<_>>()
                .join(", ");
            Err(DriverError::StaleReference(format!(
                "XWayland window geometry does not match the AT-SPI frame (frame {},{} {}x{} ×{}; candidates: {})",
                frame.x, frame.y, frame.w, frame.h, gdk_scale, candidate_rects
            )))
        }
        _ => Err(DriverError::Ambiguous(
            "multiple XWayland client windows match the AT-SPI frame".into(),
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame() -> Rect {
        Rect {
            x: 10.0,
            y: 20.0,
            w: 30.0,
            h: 40.0,
        }
    }

    fn candidate(window: XWindow, root_rect: Rect) -> WaylandClientCandidate {
        WaylandClientCandidate {
            window,
            root_rect,
            framed_rect: root_rect,
        }
    }

    fn matching_rect() -> Rect {
        Rect {
            x: 20.0,
            y: 40.0,
            w: 60.0,
            h: 80.0,
        }
    }

    #[test]
    fn no_pid_candidates_classify_as_native_wayland() {
        assert!(matches!(
            classify_wayland_candidates(frame(), 2.0, &[]),
            Err(DriverError::Unsupported(message)) if message.contains("native Wayland")
        ));
    }

    #[test]
    fn unmatched_pid_candidates_classify_as_stale_and_include_geometry() {
        let candidates = [candidate(
            7,
            Rect {
                x: 100.0,
                y: 200.0,
                w: 300.0,
                h: 400.0,
            },
        )];
        let error = classify_wayland_candidates(frame(), 2.0, &candidates).unwrap_err();

        assert!(matches!(
            error,
            DriverError::StaleReference(message)
                if message.contains("frame 10,20 30x40 ×2")
                    && message.contains("0x7 (100,200 300x400)")
        ));
    }

    #[test]
    fn one_matching_pid_candidate_returns_its_index() {
        let candidates = [
            candidate(
                3,
                Rect {
                    x: 100.0,
                    y: 200.0,
                    w: 300.0,
                    h: 400.0,
                },
            ),
            candidate(7, matching_rect()),
        ];

        assert_eq!(
            classify_wayland_candidates(frame(), 2.0, &candidates).unwrap(),
            1
        );
    }

    #[test]
    fn multiple_matching_pid_candidates_are_ambiguous() {
        let candidates = [candidate(3, matching_rect()), candidate(7, matching_rect())];

        assert!(matches!(
            classify_wayland_candidates(frame(), 2.0, &candidates),
            Err(DriverError::Ambiguous(_))
        ));
    }
}

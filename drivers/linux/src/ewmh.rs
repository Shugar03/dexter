use dexter_core::Window as AtspiWindow;
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
    let client_rect = [
        f64::from(translated.dst_x),
        f64::from(translated.dst_y),
        f64::from(geometry.width),
        f64::from(geometry.height),
    ];
    let framed_rect = [
        client_rect[0] - f64::from(extents[0]),
        client_rect[1] - f64::from(extents[2]),
        client_rect[2] + f64::from(extents[0]) + f64::from(extents[1]),
        client_rect[3] + f64::from(extents[2]) + f64::from(extents[3]),
    ];
    Ok(rect_matches(client_rect, frame) || rect_matches(framed_rect, frame))
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

fn rect_matches(rect: [f64; 4], frame: &AtspiWindow) -> bool {
    (rect[0] - frame.bounds.x).abs() <= SIZE_TOLERANCE
        && (rect[1] - frame.bounds.y).abs() <= SIZE_TOLERANCE
        && (rect[2] - frame.bounds.w).abs() <= SIZE_TOLERANCE
        && (rect[3] - frame.bounds.h).abs() <= SIZE_TOLERANCE
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

use dexter_driver::DriverError;
use std::collections::HashMap;
use std::fs::{self, File};
use std::os::unix::ffi::OsStringExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, SystemTime};
use zbus::blocking::{Connection, Proxy};
use zbus::zvariant::{OwnedObjectPath, OwnedValue, Value};

const PORTAL_DESTINATION: &str = "org.freedesktop.portal.Desktop";
const PORTAL_PATH: &str = "/org/freedesktop/portal/desktop";
const SCREENSHOT_INTERFACE: &str = "org.freedesktop.portal.Screenshot";
const REQUEST_INTERFACE: &str = "org.freedesktop.portal.Request";
const RESPONSE_TIMEOUT: Duration = Duration::from_secs(10);
static NEXT_TOKEN: AtomicU64 = AtomicU64::new(0);

pub(crate) struct PortalImage {
    pub width: u32,
    pub height: u32,
    pub rgb: Vec<u8>,
}

pub(crate) fn screenshot() -> Result<PortalImage, DriverError> {
    let connection = Connection::session()
        .map_err(|error| DriverError::Platform(format!("connect to session bus: {error}")))?;
    let sender = connection
        .unique_name()
        .ok_or_else(|| DriverError::Platform("session bus connection has no unique name".into()))?
        .as_str()
        .trim_start_matches(':')
        .replace('.', "_");
    let nonce = NEXT_TOKEN.fetch_add(1, Ordering::Relaxed);
    let token = format!("dexter_{}_{}", std::process::id(), nonce);
    let expected_path = format!("{PORTAL_PATH}/request/{sender}/{token}");
    let portal = Proxy::new(
        &connection,
        PORTAL_DESTINATION,
        PORTAL_PATH,
        SCREENSHOT_INTERFACE,
    )
    .map_err(|error| portal_zbus_error(&error))?;

    let (ready_tx, ready_rx) = mpsc::channel::<Result<(), DriverError>>();
    let (response_tx, response_rx) = mpsc::channel::<Result<(u32, Option<String>), DriverError>>();
    let listener_connection = connection.clone();
    let listener_path = expected_path.clone();
    let listener = thread::spawn(move || {
        let request = match Proxy::new(
            &listener_connection,
            PORTAL_DESTINATION,
            listener_path.as_str(),
            REQUEST_INTERFACE,
        ) {
            Ok(request) => request,
            Err(error) => {
                let _ = ready_tx.send(Err(portal_zbus_error(&error)));
                return;
            }
        };
        let mut signals = match request.receive_signal("Response") {
            Ok(signals) => signals,
            Err(error) => {
                let _ = ready_tx.send(Err(portal_zbus_error(&error)));
                return;
            }
        };
        if ready_tx.send(Ok(())).is_err() {
            return;
        }
        let response = signals
            .next()
            .ok_or_else(|| DriverError::Platform("portal Response stream closed".into()))
            .and_then(|message| {
                let (code, mut results) = message
                    .body()
                    .deserialize::<(u32, HashMap<String, OwnedValue>)>()
                    .map_err(|error| {
                        DriverError::Platform(format!("decode portal Response: {error}"))
                    })?;
                let uri = results
                    .remove("uri")
                    .and_then(|value| String::try_from(value).ok());
                Ok((code, uri))
            });
        let _ = response_tx.send(response);
    });

    match ready_rx.recv_timeout(RESPONSE_TIMEOUT) {
        Ok(Ok(())) => {}
        Ok(Err(error)) => {
            let _ = listener.join();
            return Err(error);
        }
        Err(mpsc::RecvTimeoutError::Timeout) => {
            stop_listener(&connection, listener);
            return Err(DriverError::Timeout(
                "screenshot portal did not respond within 10s".into(),
            ));
        }
        Err(mpsc::RecvTimeoutError::Disconnected) => {
            let _ = listener.join();
            return Err(DriverError::Platform(
                "portal Response subscription stopped unexpectedly".into(),
            ));
        }
    }

    let options = HashMap::from([
        ("interactive", Value::from(false)),
        ("handle_token", Value::from(token.as_str())),
    ]);
    let requested_at = SystemTime::now();
    let handle: OwnedObjectPath = match portal.call("Screenshot", &("", options)) {
        Ok(handle) => handle,
        Err(error) => {
            stop_listener(&connection, listener);
            return Err(portal_zbus_error(&error));
        }
    };
    if handle.as_str() != expected_path {
        stop_listener(&connection, listener);
        return Err(DriverError::Platform(format!(
            "screenshot portal returned request handle {}, expected {expected_path}",
            handle.as_str()
        )));
    }

    let (code, uri) = match response_rx.recv_timeout(RESPONSE_TIMEOUT) {
        Ok(Ok(response)) => {
            let _ = listener.join();
            response
        }
        Ok(Err(error)) => {
            let _ = listener.join();
            return Err(error);
        }
        Err(mpsc::RecvTimeoutError::Timeout) => {
            stop_listener(&connection, listener);
            return Err(DriverError::Timeout(
                "screenshot portal did not respond within 10s".into(),
            ));
        }
        Err(mpsc::RecvTimeoutError::Disconnected) => {
            let _ = listener.join();
            return Err(DriverError::Platform(
                "portal Response listener stopped unexpectedly".into(),
            ));
        }
    };
    portal_response_code(code)?;
    let uri = uri.ok_or_else(|| {
        DriverError::Platform("screenshot portal response did not include a string uri".into())
    })?;
    let path = portal_file_path(&uri)?;
    decode_portal_file(&path, requested_at)
}

fn stop_listener(connection: &Connection, listener: thread::JoinHandle<()>) {
    let _ = connection.clone().close();
    let _ = listener.join();
}

pub(crate) fn available() -> bool {
    let Ok(connection) = Connection::session() else {
        return false;
    };
    Proxy::new(
        &connection,
        PORTAL_DESTINATION,
        PORTAL_PATH,
        SCREENSHOT_INTERFACE,
    )
    .and_then(|proxy| proxy.get_property::<u32>("version"))
    .is_ok()
}

fn portal_response_code(code: u32) -> Result<(), DriverError> {
    match code {
        0 => Ok(()),
        1 => Err(DriverError::PermissionDenied(
            "screenshot portal request was denied or cancelled".into(),
        )),
        _ => Err(DriverError::Platform(format!(
            "screenshot portal ended the request (code {code})"
        ))),
    }
}

fn portal_zbus_error(error: &zbus::Error) -> DriverError {
    let name = match error {
        zbus::Error::MethodError(name, _, _) => name.as_str(),
        _ => "",
    };
    portal_call_error(name, &error.to_string())
}

fn portal_call_error(name: &str, message: &str) -> DriverError {
    match name {
        "org.freedesktop.DBus.Error.ServiceUnknown"
        | "org.freedesktop.DBus.Error.NameHasNoOwner"
        | "org.freedesktop.DBus.Error.UnknownMethod"
        | "org.freedesktop.DBus.Error.UnknownInterface"
        | "org.freedesktop.DBus.Error.UnknownObject" => DriverError::Unsupported(format!(
            "no xdg-desktop-portal Screenshot backend on the session bus: {message}"
        )),
        _ => DriverError::Platform(message.into()),
    }
}

fn portal_file_path(uri: &str) -> Result<PathBuf, DriverError> {
    let unsupported = || {
        DriverError::Unsupported(format!(
            "unsupported screenshot portal URI (expected an absolute local file URI): {uri}"
        ))
    };
    let encoded = uri.strip_prefix("file://").ok_or_else(unsupported)?;
    if !encoded.starts_with('/') {
        return Err(unsupported());
    }
    let encoded = encoded.as_bytes();
    let mut decoded = Vec::with_capacity(encoded.len());
    let mut index = 0;
    while index < encoded.len() {
        if encoded[index] == b'%' {
            if index + 2 >= encoded.len() {
                return Err(unsupported());
            }
            let hex = std::str::from_utf8(&encoded[index + 1..index + 3])
                .ok()
                .and_then(|value| u8::from_str_radix(value, 16).ok())
                .ok_or_else(unsupported)?;
            decoded.push(hex);
            index += 3;
        } else {
            decoded.push(encoded[index]);
            index += 1;
        }
    }
    if decoded.contains(&0) || !decoded.starts_with(b"/") {
        return Err(unsupported());
    }
    Ok(PathBuf::from(std::ffi::OsString::from_vec(decoded)))
}

fn ensure_fresh(modified: SystemTime, requested_at: SystemTime) -> Result<(), DriverError> {
    if modified < requested_at {
        return Err(DriverError::StaleReference(
            "portal screenshot predates the request".into(),
        ));
    }
    Ok(())
}

fn decode_portal_file(path: &Path, requested_at: SystemTime) -> Result<PortalImage, DriverError> {
    let result = (|| {
        let modified = fs::metadata(path)
            .and_then(|metadata| metadata.modified())
            .map_err(|error| {
                DriverError::Platform(format!(
                    "read portal screenshot metadata {}: {error}",
                    path.display()
                ))
            })?;
        ensure_fresh(modified, requested_at)?;
        let file = File::open(path).map_err(|error| {
            DriverError::Platform(format!(
                "open portal screenshot {}: {error}",
                path.display()
            ))
        })?;
        let mut reader = png::Decoder::new(file)
            .read_info()
            .map_err(|error| DriverError::Platform(format!("decode portal PNG header: {error}")))?;
        let mut buffer = vec![0; reader.output_buffer_size()];
        let info = reader
            .next_frame(&mut buffer)
            .map_err(|error| DriverError::Platform(format!("decode portal PNG: {error}")))?;
        if info.bit_depth != png::BitDepth::Eight {
            return Err(DriverError::Unsupported(format!(
                "unsupported portal PNG bit depth: {:?}",
                info.bit_depth
            )));
        }
        let pixels = &buffer[..info.buffer_size()];
        let rgb = match info.color_type {
            png::ColorType::Rgb => pixels.to_vec(),
            png::ColorType::Rgba => {
                let mut rgb = Vec::with_capacity(
                    usize::try_from(u64::from(info.width) * u64::from(info.height) * 3).map_err(
                        |_| DriverError::Platform("portal PNG dimensions overflow".into()),
                    )?,
                );
                for pixel in pixels.chunks_exact(4) {
                    rgb.extend_from_slice(&pixel[..3]);
                }
                rgb
            }
            color_type => {
                return Err(DriverError::Unsupported(format!(
                    "unsupported portal PNG color type: {color_type:?}"
                )));
            }
        };
        Ok(PortalImage {
            width: info.width,
            height: info.height,
            rgb,
        })
    })();
    let _ = fs::remove_file(path);
    result
}

#[cfg(test)]
mod tests {
    use super::{ensure_fresh, portal_call_error, portal_file_path, portal_response_code};
    use dexter_driver::DriverError;
    use std::path::PathBuf;
    use std::time::{Duration, SystemTime};

    #[test]
    fn accepts_and_decodes_absolute_file_uris() {
        assert_eq!(
            portal_file_path("file:///tmp/out.png").unwrap(),
            PathBuf::from("/tmp/out.png")
        );
        assert_eq!(
            portal_file_path("file:///home/u/Pictures/Screenshot%20from%20x.png").unwrap(),
            PathBuf::from("/home/u/Pictures/Screenshot from x.png")
        );
    }

    #[test]
    fn rejects_non_local_or_malformed_file_uris() {
        for uri in [
            "https://example.com/image.png",
            "file://host/x.png",
            "file:relative.png",
            "file:///a%zz",
            "file:///a%00b",
        ] {
            assert!(
                matches!(portal_file_path(uri), Err(DriverError::Unsupported(_))),
                "{uri:?}"
            );
        }
    }

    #[test]
    fn rejects_a_portal_file_that_predates_the_request() {
        let requested_at = SystemTime::UNIX_EPOCH + Duration::from_secs(10);
        assert!(matches!(
            ensure_fresh(SystemTime::UNIX_EPOCH + Duration::from_secs(9), requested_at),
            Err(DriverError::StaleReference(message))
                if message == "portal screenshot predates the request"
        ));
    }

    #[test]
    fn accepts_equal_or_newer_portal_file_timestamps() {
        let requested_at = SystemTime::UNIX_EPOCH + Duration::from_secs(10);
        assert!(ensure_fresh(requested_at, requested_at).is_ok());
        assert!(ensure_fresh(requested_at + Duration::from_secs(1), requested_at).is_ok());
    }

    #[test]
    fn maps_missing_portal_screenshot_implementations_to_unsupported() {
        for name in [
            "org.freedesktop.DBus.Error.ServiceUnknown",
            "org.freedesktop.DBus.Error.NameHasNoOwner",
            "org.freedesktop.DBus.Error.UnknownMethod",
            "org.freedesktop.DBus.Error.UnknownInterface",
            "org.freedesktop.DBus.Error.UnknownObject",
        ] {
            assert!(
                matches!(
                    portal_call_error(name, "method unavailable"),
                    DriverError::Unsupported(message)
                        if message == "no xdg-desktop-portal Screenshot backend on the session bus: method unavailable"
                ),
                "{name}"
            );
        }
    }

    #[test]
    fn maps_other_portal_call_errors_to_platform() {
        assert!(matches!(
            portal_call_error("org.freedesktop.DBus.Error.Failed", "details"),
            DriverError::Platform(message) if message == "details"
        ));
    }

    #[test]
    fn maps_portal_response_codes() {
        assert!(portal_response_code(0).is_ok());
        assert!(matches!(
            portal_response_code(1),
            Err(DriverError::PermissionDenied(message))
                if message == "screenshot portal request was denied or cancelled"
        ));
        assert!(matches!(
            portal_response_code(2),
            Err(DriverError::Platform(message))
                if message == "screenshot portal ended the request (code 2)"
        ));
    }
}

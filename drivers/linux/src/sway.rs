use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::path::Path;

use dexter_core::{Point, Rect};
use dexter_driver::DriverError;
use serde_json::Value;

const MAGIC: &[u8; 6] = b"i3-ipc";
const HEADER_LEN: usize = 14;
const MAX_REPLY_SIZE: usize = 16 * 1024 * 1024;
const GET_OUTPUTS: u32 = 3;
const GET_TREE: u32 = 4;

#[derive(Debug, Clone, PartialEq)]
pub struct SwayWindow {
    pub name: String,
    pub origin: Point,
    pub width: f64,
    pub height: f64,
    pub geometry: Point,
}

fn platform(error: impl std::fmt::Display) -> DriverError {
    DriverError::Platform(format!("sway IPC: {error}"))
}

fn encode_request(message_type: u32) -> [u8; HEADER_LEN] {
    let mut request = [0; HEADER_LEN];
    request[..MAGIC.len()].copy_from_slice(MAGIC);
    request[10..14].copy_from_slice(&message_type.to_le_bytes());
    request
}

fn read_reply<R: Read>(reader: &mut R, expected_type: u32) -> Result<Vec<u8>, DriverError> {
    let mut header = [0; HEADER_LEN];
    reader
        .read_exact(&mut header)
        .map_err(|error| platform(format!("read reply header: {error}")))?;
    if &header[..MAGIC.len()] != MAGIC {
        return Err(platform("invalid reply magic"));
    }
    let payload_len = u32::from_le_bytes(header[6..10].try_into().expect("four-byte length"));
    let payload_len = usize::try_from(payload_len).map_err(platform)?;
    if payload_len > MAX_REPLY_SIZE {
        return Err(platform(format!(
            "reply payload exceeds 16 MiB ({MAX_REPLY_SIZE} byte limit)"
        )));
    }
    let message_type = u32::from_le_bytes(header[10..14].try_into().expect("four-byte type"));
    if message_type != expected_type {
        return Err(platform(format!(
            "reply type {message_type} does not match request type {expected_type}"
        )));
    }
    let mut payload = vec![0; payload_len];
    reader
        .read_exact(&mut payload)
        .map_err(|error| platform(format!("read reply payload: {error}")))?;
    Ok(payload)
}

fn number(object: &Value, key: &str) -> Option<f64> {
    object.get(key)?.as_f64()
}

fn window_candidate(node: &Value, pid: i32) -> Option<SwayWindow> {
    if node.get("shell")?.as_str()? != "xdg_shell"
        || node.get("pid")?.as_i64()? != i64::from(pid)
        || !node.get("visible")?.as_bool()?
    {
        return None;
    }
    let name = node.get("name")?.as_str()?.to_string();
    let rect = node.get("rect")?;
    let window_rect = node.get("window_rect")?;
    let geometry = node.get("geometry")?;
    Some(SwayWindow {
        name,
        origin: Point {
            x: number(rect, "x")? + number(window_rect, "x")?,
            y: number(rect, "y")? + number(window_rect, "y")?,
        },
        width: number(window_rect, "width")?,
        height: number(window_rect, "height")?,
        geometry: Point {
            x: number(geometry, "x")?,
            y: number(geometry, "y")?,
        },
    })
}

fn collect_candidates(node: &Value, pid: i32, candidates: &mut Vec<SwayWindow>) {
    if let Some(candidate) = window_candidate(node, pid) {
        candidates.push(candidate);
    }
    for key in ["nodes", "floating_nodes"] {
        if let Some(children) = node.get(key).and_then(Value::as_array) {
            for child in children {
                collect_candidates(child, pid, candidates);
            }
        }
    }
}

pub fn native_candidates(tree: &Value, pid: i32) -> Vec<SwayWindow> {
    let mut candidates = Vec::new();
    collect_candidates(tree, pid, &mut candidates);
    candidates
}

pub fn classify_native(
    frame_name: &str,
    frame: Rect,
    candidates: &[SwayWindow],
) -> Result<Point, DriverError> {
    if frame.x != 0.0 || frame.y != 0.0 {
        return Err(DriverError::Unsupported(
            "frame extents are not window-relative; toolkit coordinate space unverified".into(),
        ));
    }
    if candidates.is_empty() {
        return Err(DriverError::Unsupported(
            "no sway xdg_shell window for pid".into(),
        ));
    }
    let matches: Vec<_> = candidates
        .iter()
        .filter(|candidate| {
            candidate.name == frame_name
                && (candidate.width - frame.w).abs() <= 1.0
                && (candidate.height - frame.h).abs() <= 1.0
        })
        .collect();
    match matches.as_slice() {
        [candidate] if candidate.geometry.x != 0.0 || candidate.geometry.y != 0.0 => Err(
            DriverError::Unsupported("client-side decoration offset unverified".into()),
        ),
        [candidate] => Ok(candidate.origin),
        [] => {
            let sizes = candidates
                .iter()
                .map(|candidate| {
                    format!(
                        "{} {}x{}",
                        candidate.name, candidate.width, candidate.height
                    )
                })
                .collect::<Vec<_>>()
                .join(", ");
            Err(DriverError::StaleReference(format!(
                "AT-SPI frame {frame_name:?} {}x{} does not match Sway candidates [{sizes}]",
                frame.w, frame.h
            )))
        }
        _ => Err(DriverError::Ambiguous(format!(
            "multiple Sway xdg_shell windows match AT-SPI frame {frame_name:?} {}x{}",
            frame.w, frame.h
        ))),
    }
}

pub fn single_output(outputs: &Value) -> Result<(Rect, u32), DriverError> {
    let active: Vec<_> = outputs
        .as_array()
        .into_iter()
        .flatten()
        .filter(|output| output.get("active").and_then(Value::as_bool) == Some(true))
        .collect();
    let [output] = active.as_slice() else {
        return Err(DriverError::Unsupported(
            "native Wayland capture requires exactly one active Sway output".into(),
        ));
    };
    let scale = output
        .get("scale")
        .and_then(Value::as_f64)
        .filter(|scale| scale.is_finite() && scale.fract() == 0.0 && (1.0..=4.0).contains(scale))
        .ok_or_else(|| {
            DriverError::Unsupported(
                "native Wayland capture requires integer Sway output scale from 1 through 4".into(),
            )
        })? as u32;
    let rect = output
        .get("rect")
        .ok_or_else(|| DriverError::Unsupported("active Sway output has no logical rect".into()))?;
    let bounds = Rect {
        x: number(rect, "x").ok_or_else(|| {
            DriverError::Unsupported("active Sway output has invalid logical x".into())
        })?,
        y: number(rect, "y").ok_or_else(|| {
            DriverError::Unsupported("active Sway output has invalid logical y".into())
        })?,
        w: number(rect, "width").ok_or_else(|| {
            DriverError::Unsupported("active Sway output has invalid logical width".into())
        })?,
        h: number(rect, "height").ok_or_else(|| {
            DriverError::Unsupported("active Sway output has invalid logical height".into())
        })?,
    };
    if bounds.w <= 0.0 || bounds.h <= 0.0 {
        return Err(DriverError::Unsupported(
            "active Sway output has empty logical bounds".into(),
        ));
    }
    Ok((bounds, scale))
}

fn request(sock: &Path, message_type: u32) -> Result<Value, DriverError> {
    let mut stream = UnixStream::connect(sock)
        .map_err(|error| platform(format!("connect to {}: {error}", sock.display())))?;
    stream
        .write_all(&encode_request(message_type))
        .map_err(|error| platform(format!("write request: {error}")))?;
    let payload = read_reply(&mut stream, message_type)?;
    serde_json::from_slice(&payload)
        .map_err(|error| platform(format!("decode JSON reply: {error}")))
}

pub fn get_tree(sock: &Path) -> Result<Value, DriverError> {
    request(sock, GET_TREE)
}

pub fn get_outputs(sock: &Path) -> Result<Value, DriverError> {
    request(sock, GET_OUTPUTS)
}

pub fn placement(
    sock: Option<&Path>,
    pid: i32,
    frame_name: &str,
    frame: Rect,
) -> Result<Point, DriverError> {
    let sock = sock.ok_or_else(|| {
        DriverError::Unsupported(
            "native Wayland placement needs sway IPC (SWAYSOCK unset); other compositors are not supported"
                .into(),
        )
    })?;
    let tree = get_tree(sock)?;
    let candidates = native_candidates(&tree, pid);
    classify_native(frame_name, frame, &candidates)
}

#[cfg(test)]
mod tests {
    use super::{
        classify_native, encode_request, native_candidates, placement, read_reply, single_output,
        GET_TREE, HEADER_LEN, MAGIC, MAX_REPLY_SIZE,
    };
    use dexter_core::{Point, Rect};
    use dexter_driver::DriverError;
    use serde_json::{json, Value};
    use std::io::{self, Cursor, Read};

    const PID: i32 = 7142;

    fn frame(width: f64, height: f64) -> Rect {
        Rect {
            x: 0.0,
            y: 0.0,
            w: width,
            h: height,
        }
    }

    fn node(name: &str, rect_x: i64, rect_y: i64, width: i64, height: i64) -> Value {
        json!({
            "pid": PID,
            "name": name,
            "shell": "xdg_shell",
            "visible": true,
            "rect": {"x": rect_x, "y": rect_y, "width": width + 4, "height": height + 2},
            "window_rect": {"x": 2, "y": 0, "width": width, "height": height},
            "geometry": {"x": 0, "y": 0},
            "nodes": [],
            "floating_nodes": []
        })
    }

    fn tiled_tree() -> Value {
        json!({
            "nodes": [{
                "nodes": [
                    node("dexter-act-fixture", 0, 58, 1596, 1740),
                    node("dexter-act-fixture-second", 1600, 58, 1596, 1740)
                ],
                "floating_nodes": []
            }],
            "floating_nodes": []
        })
    }

    #[test]
    fn tiled_pair_uses_each_absolute_content_origin() {
        let candidates = native_candidates(&tiled_tree(), PID);
        assert_eq!(
            classify_native("dexter-act-fixture", frame(1596.0, 1740.0), &candidates).unwrap(),
            Point { x: 2.0, y: 58.0 }
        );
        assert_eq!(
            classify_native(
                "dexter-act-fixture-second",
                frame(1596.0, 1740.0),
                &candidates
            )
            .unwrap(),
            Point { x: 1602.0, y: 58.0 }
        );
    }

    #[test]
    fn floating_window_uses_rect_plus_window_rect_origin() {
        let tree = json!({
            "nodes": [],
            "floating_nodes": [node("dexter-act-fixture", 300, 258, 500, 560)]
        });
        let candidates = native_candidates(&tree, PID);
        assert_eq!(
            classify_native("dexter-act-fixture", frame(500.0, 560.0), &candidates).unwrap(),
            Point { x: 302.0, y: 258.0 }
        );
    }

    #[test]
    fn ignores_xwayland_and_invisible_nodes() {
        let mut xwayland = node("xwayland", 0, 0, 100, 100);
        xwayland["shell"] = json!("xwayland");
        let mut invisible = node("invisible", 0, 0, 100, 100);
        invisible["visible"] = json!(false);
        let tree = json!({"nodes": [xwayland], "floating_nodes": [invisible]});
        assert!(native_candidates(&tree, PID).is_empty());
    }

    #[test]
    fn client_geometry_offset_is_unsupported() {
        let mut candidate = node("dexter-act-fixture", 0, 0, 100, 100);
        candidate["geometry"]["x"] = json!(1);
        let candidates = native_candidates(&json!({"nodes": [candidate]}), PID);
        assert!(matches!(
            classify_native("dexter-act-fixture", frame(100.0, 100.0), &candidates),
            Err(DriverError::Unsupported(message))
                if message == "client-side decoration offset unverified"
        ));
    }

    #[test]
    fn duplicate_name_and_size_are_ambiguous() {
        let tree = json!({
            "nodes": [
                node("same", 0, 0, 100, 100),
                node("same", 200, 0, 100, 100)
            ]
        });
        let candidates = native_candidates(&tree, PID);
        assert!(matches!(
            classify_native("same", frame(100.0, 100.0), &candidates),
            Err(DriverError::Ambiguous(_))
        ));
    }

    #[test]
    fn name_match_with_size_mismatch_is_stale() {
        let candidates = native_candidates(&json!({"nodes": [node("same", 0, 0, 100, 100)]}), PID);
        assert!(matches!(
            classify_native("same", frame(500.0, 400.0), &candidates),
            Err(DriverError::StaleReference(message))
                if message.contains("500") && message.contains("100")
        ));
    }

    #[test]
    fn nonzero_frame_origin_is_unsupported() {
        let mut atspi = frame(100.0, 100.0);
        atspi.x = 1.0;
        assert!(matches!(
            classify_native("same", atspi, &[]),
            Err(DriverError::Unsupported(message))
                if message == "frame extents are not window-relative; toolkit coordinate space unverified"
        ));
    }

    #[test]
    fn missing_native_candidate_is_unsupported() {
        assert!(matches!(
            classify_native("same", frame(100.0, 100.0), &[]),
            Err(DriverError::Unsupported(message))
                if message == "no sway xdg_shell window for pid"
        ));
    }

    #[test]
    fn unset_sway_socket_is_unsupported() {
        assert!(matches!(
            placement(None, PID, "same", frame(100.0, 100.0)),
            Err(DriverError::Unsupported(message))
                if message == "native Wayland placement needs sway IPC (SWAYSOCK unset); other compositors are not supported"
        ));
    }

    #[test]
    fn one_integer_scaled_output_is_returned() {
        let outputs = json!([{
            "active": true,
            "scale": 2.0,
            "rect": {"x": 0, "y": 0, "width": 1600, "height": 900}
        }]);
        let (bounds, scale) = single_output(&outputs).unwrap();
        assert_eq!(
            bounds,
            Rect {
                x: 0.0,
                y: 0.0,
                w: 1600.0,
                h: 900.0
            }
        );
        assert_eq!(scale, 2);
    }

    #[test]
    fn rejects_multiple_active_outputs_and_fractional_scale() {
        let one = json!({
            "active": true,
            "scale": 1.0,
            "rect": {"x": 0, "y": 0, "width": 100, "height": 100}
        });
        assert!(matches!(
            single_output(&json!([one.clone(), one])),
            Err(DriverError::Unsupported(_))
        ));
        let fractional = json!([{
            "active": true,
            "scale": 1.5,
            "rect": {"x": 0, "y": 0, "width": 100, "height": 100}
        }]);
        assert!(matches!(
            single_output(&fractional),
            Err(DriverError::Unsupported(_))
        ));
    }

    #[test]
    fn encodes_and_decodes_i3_ipc_framing_across_short_reads() {
        let request = encode_request(GET_TREE);
        assert_eq!(&request[..6], MAGIC);
        assert_eq!(&request[6..10], &[0, 0, 0, 0]);
        assert_eq!(
            u32::from_le_bytes(request[10..14].try_into().unwrap()),
            GET_TREE
        );

        let payload = br#"{"ok":true}"#;
        let mut reply = Vec::from(*MAGIC);
        reply.extend_from_slice(&(payload.len() as u32).to_le_bytes());
        reply.extend_from_slice(&GET_TREE.to_le_bytes());
        reply.extend_from_slice(payload);
        let mut reader = ChunkedReader {
            cursor: Cursor::new(reply),
            max_chunk: 3,
        };
        assert_eq!(read_reply(&mut reader, GET_TREE).unwrap(), payload);
    }

    #[test]
    fn rejects_reply_larger_than_sixteen_mebibytes() {
        let mut header = Vec::from(*MAGIC);
        header.extend_from_slice(&((MAX_REPLY_SIZE as u32) + 1).to_le_bytes());
        header.extend_from_slice(&GET_TREE.to_le_bytes());
        assert!(matches!(
            read_reply(&mut Cursor::new(header), GET_TREE),
            Err(DriverError::Platform(message)) if message.contains("16 MiB")
        ));
        assert_eq!(HEADER_LEN, 14);
    }

    struct ChunkedReader<R> {
        cursor: R,
        max_chunk: usize,
    }

    impl<R: Read> Read for ChunkedReader<R> {
        fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
            let len = buffer.len().min(self.max_chunk);
            self.cursor.read(&mut buffer[..len])
        }
    }
}

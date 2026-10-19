use dexter_driver::DriverError;

pub(crate) fn gdk_scale_for_pid(pid: i32) -> Result<f64, DriverError> {
    let path = format!("/proc/{pid}/environ");
    let environ = std::fs::read(&path).map_err(|_| {
        DriverError::Unsupported(format!(
            "cannot read /proc/{pid}/environ to determine GDK_SCALE"
        ))
    })?;
    parse_gdk_scale(&environ, pid)
}

fn parse_gdk_scale(environ: &[u8], pid: i32) -> Result<f64, DriverError> {
    let Some(value) = environ
        .split(|byte| *byte == 0)
        .find_map(|entry| entry.strip_prefix(b"GDK_SCALE="))
    else {
        return Ok(1.0);
    };
    if value == b"1" {
        return Ok(1.0);
    }
    match std::str::from_utf8(value)
        .ok()
        .and_then(|value| value.parse::<u32>().ok())
        .filter(|value| (2..=4).contains(value))
    {
        Some(scale) => Ok(f64::from(scale)),
        None => Err(DriverError::Unsupported(format!(
            "unsupported GDK_SCALE '{}' for pid {pid}",
            String::from_utf8_lossy(value)
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::parse_gdk_scale;
    use dexter_driver::DriverError;

    #[test]
    fn gdk_scale_parser_reads_nul_separated_environment() {
        assert_eq!(
            parse_gdk_scale(b"PATH=/bin\0GDK_SCALE=2\0HOME=/home/test\0", 41).unwrap(),
            2.0
        );
        assert_eq!(parse_gdk_scale(b"GDK_SCALE=4\0", 42).unwrap(), 4.0);
    }

    #[test]
    fn missing_or_one_gdk_scale_uses_identity() {
        assert_eq!(parse_gdk_scale(b"PATH=/bin\0", 41).unwrap(), 1.0);
        assert_eq!(parse_gdk_scale(b"GDK_SCALE=1\0", 42).unwrap(), 1.0);
    }

    #[test]
    fn unsupported_gdk_scale_values_fail_closed_with_the_pid() {
        for value in ["", "0", "1.5", "bad", "5"] {
            let environ = format!("GDK_SCALE={value}\0");
            let error = parse_gdk_scale(environ.as_bytes(), 1234).unwrap_err();
            assert!(
                matches!(error, DriverError::Unsupported(ref detail)
                    if detail == &format!("unsupported GDK_SCALE '{value}' for pid 1234")),
                "{error:?}"
            );
        }
    }
}

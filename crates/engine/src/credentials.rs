use dexter_core::CredentialField;
use serde::Deserialize;
use std::collections::HashMap;
use std::path::Path;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawStore {
    #[serde(default)]
    credential: Vec<RawCredential>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawCredential {
    name: String,
    origin: String,
    username: Option<String>,
    password_env: Option<String>,
}

struct Credential {
    origin: String,
    username: Option<String>,
    password_env: Option<String>,
}

/// Local metadata for origin-bound browser credentials. Password values are
/// resolved from the environment only when an authorized action runs.
pub struct CredentialStore {
    entries: HashMap<String, Credential>,
}

impl CredentialStore {
    /// Normalize a configured origin before it is stored.
    pub fn normalize_origin(input: &str) -> Result<String, String> {
        self::normalize_origin(input)
    }

    /// Load a credential file after checking that it is a private regular
    /// file, not a symlink.
    pub fn load(path: &Path) -> Result<Self, String> {
        let metadata = std::fs::symlink_metadata(path).map_err(|error| {
            format!(
                "cannot inspect credentials file '{}': {error}",
                path.display()
            )
        })?;
        if metadata.file_type().is_symlink() {
            return Err(format!(
                "credentials file '{}' must not be a symlink",
                path.display()
            ));
        }
        if !metadata.is_file() {
            return Err(format!(
                "credentials path '{}' is not a regular file",
                path.display()
            ));
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if metadata.permissions().mode() & 0o022 != 0 {
                return Err(format!(
                    "credentials file '{}' is group/world-writable",
                    path.display()
                ));
            }
        }
        let text = std::fs::read_to_string(path).map_err(|error| {
            format!("cannot read credentials file '{}': {error}", path.display())
        })?;
        Self::from_toml_str(&text)
    }

    /// Parse credential metadata from TOML.
    pub fn from_toml_str(text: &str) -> Result<Self, String> {
        let raw: RawStore =
            toml::from_str(text).map_err(|error| format!("credentials TOML: {error}"))?;
        let mut entries = HashMap::new();
        for credential in raw.credential {
            if credential.name.is_empty() {
                return Err("credential name must not be empty".into());
            }
            if credential.username.is_none() && credential.password_env.is_none() {
                return Err(format!(
                    "credential '{}' must define username or password_env",
                    credential.name
                ));
            }
            if credential
                .password_env
                .as_deref()
                .is_some_and(str::is_empty)
            {
                return Err(format!(
                    "credential '{}' has an empty password_env",
                    credential.name
                ));
            }
            let origin = normalize_origin(&credential.origin)?;
            let name = credential.name;
            if entries
                .insert(
                    name.clone(),
                    Credential {
                        origin,
                        username: credential.username,
                        password_env: credential.password_env,
                    },
                )
                .is_some()
            {
                return Err(format!("duplicate credential name '{name}'"));
            }
        }
        Ok(Self { entries })
    }

    /// Resolve the configured origin and one field's value. Secret values
    /// are never included in errors.
    pub fn resolve(&self, name: &str, field: CredentialField) -> Result<(String, String), String> {
        let credential = self
            .entries
            .get(name)
            .ok_or_else(|| format!("unknown credential '{name}'"))?;
        let secret = match field {
            CredentialField::Username => credential
                .username
                .clone()
                .ok_or_else(|| format!("credential '{name}' has no username configured"))?,
            CredentialField::Password => {
                let env_name = credential
                    .password_env
                    .as_deref()
                    .ok_or_else(|| format!("credential '{name}' has no password_env configured"))?;
                let value = std::env::var(env_name).map_err(|_| {
                    format!("credential '{name}' requires environment variable '{env_name}'")
                })?;
                if value.is_empty() {
                    return Err(format!(
                        "credential '{name}' requires non-empty environment variable '{env_name}'"
                    ));
                }
                value
            }
        };
        Ok((credential.origin.clone(), secret))
    }
}

/// Normalize an origin to the serialized form used by `location.origin`.
pub fn normalize_origin(input: &str) -> Result<String, String> {
    let (raw_scheme, remainder) = input
        .split_once("://")
        .ok_or_else(|| "origin must include a scheme and authority".to_string())?;
    let scheme = raw_scheme.to_ascii_lowercase();
    if scheme != "https" && scheme != "http" {
        return Err("origin scheme must be https (or local http)".into());
    }
    if remainder.contains('?') || remainder.contains('#') {
        return Err("origin must not include a query or fragment".into());
    }
    let (authority, path) = match remainder.split_once('/') {
        Some((authority, path)) => (authority, Some(path)),
        None => (remainder, None),
    };
    if path.is_some_and(|path| !path.is_empty()) {
        return Err("origin path must be empty or '/'".into());
    }
    if authority.is_empty() || authority.contains('@') || authority.chars().any(char::is_whitespace)
    {
        return Err("origin authority is invalid or contains userinfo".into());
    }

    let (host, port) = if let Some(after_open) = authority.strip_prefix('[') {
        let close = after_open
            .find(']')
            .ok_or_else(|| "origin has an invalid IPv6 host".to_string())?;
        let address = after_open[..close]
            .parse::<std::net::Ipv6Addr>()
            .map_err(|_| "origin has an invalid IPv6 host".to_string())?;
        let suffix = &after_open[close + 1..];
        let port = if suffix.is_empty() {
            None
        } else if let Some(port) = suffix.strip_prefix(':') {
            Some(parse_port(port)?)
        } else {
            return Err("origin has an invalid IPv6 authority".into());
        };
        (format!("[{address}]"), port)
    } else {
        let (host, port) = match authority.rsplit_once(':') {
            Some((host, port)) => {
                if host.contains(':') {
                    return Err("IPv6 origins must use brackets".into());
                }
                (host, Some(parse_port(port)?))
            }
            None => (authority, None),
        };
        if host.is_empty()
            || !host
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-'))
        {
            return Err("origin has an invalid host".into());
        }
        (host.to_ascii_lowercase(), port)
    };

    if scheme == "http" && !matches!(host.as_str(), "localhost" | "127.0.0.1" | "[::1]") {
        return Err("HTTP origins are allowed only for localhost".into());
    }
    let port = match (scheme.as_str(), port) {
        ("https", Some(443)) | ("http", Some(80)) => None,
        (_, port) => port,
    };
    Ok(match port {
        Some(port) => format!("{scheme}://{host}:{port}"),
        None => format!("{scheme}://{host}"),
    })
}

fn parse_port(value: &str) -> Result<u16, String> {
    if value.is_empty() {
        return Err("origin port must be a number".into());
    }
    value
        .parse()
        .map_err(|_| "origin port must be a number from 0 to 65535".into())
}

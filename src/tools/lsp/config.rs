use std::{
    collections::BTreeSet,
    ffi::OsString,
    path::{Path, PathBuf},
};

use thiserror::Error;

use crate::model::JsonValue;

use super::super::plugin::{PluginConfigError, PluginProgram, read_private_config};

const CONFIG_VERSION: u64 = 1;
const MAX_SERVERS: usize = 8;
const MAX_EXTENSIONS_PER_SERVER: usize = 16;
const MAX_TOTAL_EXTENSIONS: usize = 32;
const MAX_EXTENSION_BYTES: usize = 32;
const MAX_LANGUAGE_ID_BYTES: usize = 64;
const MAX_ENVIRONMENT_OVERRIDES: usize = 8;
const MAX_ENVIRONMENT_BYTES: usize = 16 * 1024;
const DEFAULT_TOOL_TIMEOUT_MS: u64 = 60_000;
const MIN_TOOL_TIMEOUT_MS: u64 = 100;
const MAX_TOOL_TIMEOUT_MS: u64 = 295_000;

#[derive(Debug, Error)]
pub(crate) enum LspConfigError {
    #[error("LSP configuration could not be opened")]
    PathUnavailable,
    #[error("LSP configuration file is not private and regular")]
    UnsafeConfigFile,
    #[error("LSP configuration exceeds its size limit")]
    ConfigTooLarge,
    #[error("LSP configuration changed while it was being read")]
    ConfigChanged,
    #[error("LSP configuration JSON is invalid")]
    InvalidJson,
    #[error("LSP configuration version is unsupported")]
    UnsupportedVersion,
    #[error("LSP configuration must contain one to eight valid servers")]
    InvalidServers,
    #[error("LSP server {server_id} configuration is invalid")]
    InvalidServer { server_id: String },
    #[error("LSP server {server_id} program is not a safe executable")]
    InvalidProgram { server_id: String },
    #[error("LSP extension {extension} is configured more than once")]
    DuplicateExtension { extension: String },
    #[error("LSP configuration declares too many file extensions")]
    TooManyExtensions,
}

pub(crate) struct LspConfig {
    servers: Box<[LspServerConfig]>,
    tool_timeout: std::time::Duration,
}

impl std::fmt::Debug for LspConfig {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("LspConfig")
            .field("server_count", &self.servers.len())
            .finish()
    }
}

impl LspConfig {
    pub(crate) fn load(
        startup_directory: &Path,
        configured_path: &Path,
    ) -> Result<Self, LspConfigError> {
        let path = if configured_path.is_absolute() {
            configured_path.to_path_buf()
        } else {
            startup_directory.join(configured_path)
        };
        let value = read_private_config(&path).map_err(map_private_config_error)?;
        let root = value
            .as_value()
            .as_object()
            .ok_or(LspConfigError::InvalidJson)?;
        if root
            .keys()
            .any(|key| !matches!(key.as_str(), "version" | "servers" | "toolTimeoutMs"))
            || !root.contains_key("version")
            || !root.contains_key("servers")
        {
            return Err(LspConfigError::InvalidJson);
        }
        if root.get("version").and_then(serde_json::Value::as_u64) != Some(CONFIG_VERSION) {
            return Err(LspConfigError::UnsupportedVersion);
        }
        let raw_servers = root
            .get("servers")
            .and_then(serde_json::Value::as_object)
            .ok_or(LspConfigError::InvalidServers)?;
        if raw_servers.is_empty() || raw_servers.len() > MAX_SERVERS {
            return Err(LspConfigError::InvalidServers);
        }

        let mut routes = BTreeSet::new();
        let mut servers = Vec::new();
        servers
            .try_reserve_exact(raw_servers.len())
            .map_err(|_| LspConfigError::InvalidServers)?;
        for (server_id, raw) in raw_servers {
            let server = LspServerConfig::parse(server_id, raw)?;
            for (extension, _) in server.extensions() {
                if !routes.insert(extension.clone()) {
                    return Err(LspConfigError::DuplicateExtension {
                        extension: extension.clone(),
                    });
                }
                if routes.len() > MAX_TOTAL_EXTENSIONS {
                    return Err(LspConfigError::TooManyExtensions);
                }
            }
            servers.push(server);
        }
        let timeout_ms = root
            .get("toolTimeoutMs")
            .map_or(Some(DEFAULT_TOOL_TIMEOUT_MS), serde_json::Value::as_u64)
            .filter(|value| (*value >= MIN_TOOL_TIMEOUT_MS) && (*value <= MAX_TOOL_TIMEOUT_MS))
            .ok_or(LspConfigError::InvalidJson)?;
        Ok(Self {
            servers: servers.into_boxed_slice(),
            tool_timeout: std::time::Duration::from_millis(timeout_ms),
        })
    }

    pub(crate) fn into_parts(self) -> (Box<[LspServerConfig]>, std::time::Duration) {
        (self.servers, self.tool_timeout)
    }
}

pub(crate) struct LspServerConfig {
    program: PluginProgram,
    extensions: Box<[(String, String)]>,
    environment: Box<[(OsString, OsString)]>,
    initialization_options: JsonValue,
    configuration: JsonValue,
}

impl std::fmt::Debug for LspServerConfig {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("LspServerConfig")
            .field("server_id", &self.program.id())
            .field("extension_count", &self.extensions.len())
            .field("environment_entries", &self.environment.len())
            .finish_non_exhaustive()
    }
}

impl LspServerConfig {
    fn parse(server_id: &str, raw: &serde_json::Value) -> Result<Self, LspConfigError> {
        let invalid = || LspConfigError::InvalidServer {
            server_id: server_id.to_owned(),
        };
        let fields = raw.as_object().ok_or_else(invalid)?;
        if fields.keys().any(|key| {
            !matches!(
                key.as_str(),
                "command"
                    | "args"
                    | "extensionToLanguage"
                    | "env"
                    | "initializationOptions"
                    | "configuration"
            )
        }) {
            return Err(invalid());
        }
        let command = fields
            .get("command")
            .and_then(serde_json::Value::as_str)
            .filter(|value| !value.contains('\0'))
            .map(PathBuf::from)
            .ok_or_else(invalid)?;
        let arguments = parse_arguments(fields.get("args"), &invalid)?;
        let program =
            PluginProgram::from_parts(server_id.to_owned(), command, arguments).map_err(|_| {
                LspConfigError::InvalidProgram {
                    server_id: server_id.to_owned(),
                }
            })?;
        let extensions = parse_extensions(fields.get("extensionToLanguage"), &invalid)?;
        let environment = parse_environment(fields.get("env"), &invalid)?;
        let initialization_options = JsonValue::new(
            fields
                .get("initializationOptions")
                .cloned()
                .unwrap_or(serde_json::Value::Null),
        )
        .map_err(|_| invalid())?;
        let configuration = JsonValue::new(
            fields
                .get("configuration")
                .cloned()
                .unwrap_or(serde_json::Value::Null),
        )
        .map_err(|_| invalid())?;
        Ok(Self {
            program,
            extensions,
            environment,
            initialization_options,
            configuration,
        })
    }

    pub(crate) fn id(&self) -> &str {
        self.program.id()
    }

    pub(crate) fn program(&self) -> &PluginProgram {
        &self.program
    }

    pub(crate) fn extensions(&self) -> &[(String, String)] {
        &self.extensions
    }

    pub(crate) fn environment(&self) -> &[(OsString, OsString)] {
        &self.environment
    }

    pub(crate) fn initialization_options(&self) -> &JsonValue {
        &self.initialization_options
    }

    pub(crate) fn configuration(&self) -> &JsonValue {
        &self.configuration
    }
}

fn parse_arguments(
    raw: Option<&serde_json::Value>,
    invalid: &impl Fn() -> LspConfigError,
) -> Result<Vec<String>, LspConfigError> {
    let Some(raw) = raw else {
        return Ok(Vec::new());
    };
    let values = raw.as_array().ok_or_else(invalid)?;
    values
        .iter()
        .map(|value| {
            value
                .as_str()
                .filter(|value| !value.contains('\0'))
                .map(str::to_owned)
                .ok_or_else(invalid)
        })
        .collect()
}

fn parse_extensions(
    raw: Option<&serde_json::Value>,
    invalid: &impl Fn() -> LspConfigError,
) -> Result<Box<[(String, String)]>, LspConfigError> {
    let values = raw
        .and_then(serde_json::Value::as_object)
        .ok_or_else(invalid)?;
    if values.is_empty() || values.len() > MAX_EXTENSIONS_PER_SERVER {
        return Err(invalid());
    }
    let mut normalized = BTreeSet::new();
    let mut extensions = Vec::new();
    extensions
        .try_reserve_exact(values.len())
        .map_err(|_| invalid())?;
    for (raw_extension, raw_language) in values {
        let extension = normalize_extension(raw_extension).ok_or_else(invalid)?;
        if !normalized.insert(extension.clone()) {
            return Err(invalid());
        }
        let language = raw_language
            .as_str()
            .map(str::trim)
            .filter(|value| {
                !value.is_empty()
                    && value.len() <= MAX_LANGUAGE_ID_BYTES
                    && !value.chars().any(char::is_control)
            })
            .ok_or_else(invalid)?
            .to_owned();
        extensions.push((extension, language));
    }
    Ok(extensions.into_boxed_slice())
}

fn normalize_extension(raw: &str) -> Option<String> {
    let extension = if raw.starts_with('.') {
        raw.to_ascii_lowercase()
    } else {
        format!(".{}", raw.to_ascii_lowercase())
    };
    (extension.len() >= 2
        && extension.len() <= MAX_EXTENSION_BYTES
        && extension.is_ascii()
        && extension[1..]
            .bytes()
            .all(|byte| !matches!(byte, b'.' | b'/' | b'\\') && !byte.is_ascii_control()))
    .then_some(extension)
}

fn parse_environment(
    raw: Option<&serde_json::Value>,
    invalid: &impl Fn() -> LspConfigError,
) -> Result<Box<[(OsString, OsString)]>, LspConfigError> {
    let Some(raw) = raw else {
        return Ok(Box::new([]));
    };
    let values = raw.as_object().ok_or_else(invalid)?;
    if values.len() > MAX_ENVIRONMENT_OVERRIDES {
        return Err(invalid());
    }
    let mut bytes = 0_usize;
    let mut environment = Vec::new();
    environment
        .try_reserve_exact(values.len())
        .map_err(|_| invalid())?;
    for (name, raw_value) in values {
        if !valid_environment_name(name) {
            return Err(invalid());
        }
        let value = raw_value
            .as_str()
            .filter(|value| !value.contains('\0'))
            .ok_or_else(invalid)?;
        bytes = bytes
            .checked_add(name.len())
            .and_then(|total| total.checked_add(value.len()))
            .ok_or_else(invalid)?;
        if bytes > MAX_ENVIRONMENT_BYTES {
            return Err(invalid());
        }
        environment.push((OsString::from(name), OsString::from(value)));
    }
    Ok(environment.into_boxed_slice())
}

fn valid_environment_name(name: &str) -> bool {
    let mut bytes = name.bytes();
    matches!(bytes.next(), Some(b'A'..=b'Z' | b'a'..=b'z' | b'_'))
        && name.len() <= 128
        && bytes.all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
}

fn map_private_config_error(error: PluginConfigError) -> LspConfigError {
    match error {
        PluginConfigError::PathUnavailable => LspConfigError::PathUnavailable,
        PluginConfigError::UnsafeConfigFile => LspConfigError::UnsafeConfigFile,
        PluginConfigError::ConfigTooLarge => LspConfigError::ConfigTooLarge,
        PluginConfigError::ConfigChanged => LspConfigError::ConfigChanged,
        PluginConfigError::InvalidJson => LspConfigError::InvalidJson,
        PluginConfigError::UnsupportedVersion
        | PluginConfigError::TooManyPlugins
        | PluginConfigError::InvalidEntry
        | PluginConfigError::InvalidPluginId
        | PluginConfigError::InvalidProgram
        | PluginConfigError::Plugin { .. } => LspConfigError::InvalidJson,
    }
}

#[cfg(test)]
mod tests {
    use std::{fs, os::unix::fs::PermissionsExt as _};

    use super::{LspConfig, LspConfigError};

    #[test]
    fn private_config_normalizes_routes_and_redacts_debug() {
        let root = temporary_root("valid");
        fs::create_dir(&root).unwrap();
        let root = fs::canonicalize(root).unwrap();
        let program = root.join("server");
        fs::write(&program, b"#!/bin/sh\nexit 0\n").unwrap();
        fs::set_permissions(&program, fs::Permissions::from_mode(0o700)).unwrap();
        let config = root.join("lsp.json");
        fs::write(
            &config,
            format!(
                r#"{{"version":1,"servers":{{"rust":{{"command":{},"args":[],"extensionToLanguage":{{"RS":"rust"}},"env":{{"TOKEN":"secret"}},"initializationOptions":null,"configuration":null}}}}}}"#,
                serde_json::to_string(program.to_str().unwrap()).unwrap()
            ),
        )
        .unwrap();
        fs::set_permissions(&config, fs::Permissions::from_mode(0o600)).unwrap();

        let config = LspConfig::load(&root, &config).unwrap();
        let debug = format!("{config:?}");
        assert_eq!(
            config.servers[0].extensions()[0],
            (".rs".to_owned(), "rust".to_owned())
        );
        assert!(!debug.contains("secret"));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn config_rejects_duplicate_routes_unknown_fields_and_unsafe_permissions() {
        let root = temporary_root("invalid");
        fs::create_dir(&root).unwrap();
        let root = fs::canonicalize(root).unwrap();
        let program = root.join("server");
        fs::write(&program, b"#!/bin/sh\nexit 0\n").unwrap();
        fs::set_permissions(&program, fs::Permissions::from_mode(0o700)).unwrap();
        let encoded = serde_json::to_string(program.to_str().unwrap()).unwrap();
        let config = root.join("lsp.json");
        fs::write(
            &config,
            format!(
                r#"{{"version":1,"servers":{{"a":{{"command":{encoded},"extensionToLanguage":{{"rs":"rust"}}}},"b":{{"command":{encoded},"extensionToLanguage":{{".RS":"other"}}}}}}}}"#
            ),
        )
        .unwrap();
        fs::set_permissions(&config, fs::Permissions::from_mode(0o600)).unwrap();
        assert!(matches!(
            LspConfig::load(&root, &config),
            Err(LspConfigError::DuplicateExtension { .. })
        ));

        fs::set_permissions(&config, fs::Permissions::from_mode(0o644)).unwrap();
        assert!(matches!(
            LspConfig::load(&root, &config),
            Err(LspConfigError::UnsafeConfigFile)
        ));
        fs::remove_dir_all(root).unwrap();
    }

    fn temporary_root(label: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!("dsh-lsp-config-{label}-{}", uuid::Uuid::new_v4()))
    }
}

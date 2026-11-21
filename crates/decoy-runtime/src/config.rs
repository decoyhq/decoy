use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use serde::Deserialize;
use thiserror::Error;

use crate::time::parse_duration;

pub const DEFAULT_PORT: u16 = 8080;
/// Larger request bodies get `413` (gRPC: `RESOURCE_EXHAUSTED`) instead of being buffered.
pub const DEFAULT_MAX_BODY_BYTES: usize = 32 * 1024 * 1024;
/// Binds every loopback address `localhost` resolves to (`127.0.0.1` and `::1`).
pub const DEFAULT_HOST: &str = "localhost";

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ServeCliOptions {
    pub config_file: Option<PathBuf>,
    pub routes: Option<PathBuf>,
    pub collections: Option<PathBuf>,
    pub port: Option<u16>,
    pub host: Option<String>,
    pub collection: Option<String>,
    pub passthrough_base_url: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServeConfig {
    pub routes: PathBuf,
    pub collections: PathBuf,
    pub port: Listener,
    /// Extra named listeners (e.g. `grpc: 50051`) serving the same Routes and Sessions as `port`.
    pub listeners: BTreeMap<String, Listener>,
    /// Certificate served by every listener with `tls` on; paths are absolute.
    pub tls: Option<TlsSource>,
    pub collection: Option<String>,
    pub passthrough_base_url: Option<String>,
    pub protos: ProtoSources,
    pub max_body_bytes: usize,
    /// Transformers configured with a persistent worker.
    pub plugins: BTreeMap<String, PluginConfig>,
    /// Named step lists a Route uses with `pipeline: <name>`.
    pub pipelines: BTreeMap<String, PipelineConfig>,
    pub commands: BTreeMap<String, CommandsConfig>,
}

/// One plugin process (`docs/design/plugin-protocol.md`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginConfig {
    pub command: Vec<String>,
    /// Directory containing the Decoy config file.
    pub dir: PathBuf,
    /// Longest a call may take.
    pub timeout: std::time::Duration,
}

/// A plugin call's timeout when its entry sets none.
pub const DEFAULT_PLUGIN_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PipelineConfig {
    #[serde(default)]
    pub input: Vec<String>,
    #[serde(default)]
    pub output: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandsConfig {
    pub input: Option<Vec<String>>,
    pub output: Option<Vec<String>>,
    pub dir: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(untagged)]
enum TransformerFile {
    Worker(PluginFile),
    Commands(CommandFile),
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
struct CommandFile {
    input: Option<Vec<String>>,
    output: Option<Vec<String>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
struct PluginFile {
    worker: Vec<String>,
    timeout: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Listener {
    pub port: u16,
    /// An IP, or a name whose every resolved address is bound.
    pub host: String,
    pub tls: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TlsFiles {
    pub cert: PathBuf,
    pub key: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TlsSource {
    Files(TlsFiles),
    /// Generate a cert at startup, signed by a local CA kept (and reused) in `dir`.
    Auto {
        dir: PathBuf,
    },
}

/// `tls` in the config file: `auto`, or `{ cert, key }`.
#[derive(Debug, Clone, PartialEq, Eq)]
enum TlsFile {
    Files(TlsFiles),
    Auto,
}

impl<'de> Deserialize<'de> for TlsFile {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        use serde::de::Error;
        match serde_norway::Value::deserialize(deserializer)? {
            serde_norway::Value::String(value) if value == "auto" => Ok(Self::Auto),
            value @ serde_norway::Value::Mapping(_) => serde_norway::from_value(value)
                .map(Self::Files)
                .map_err(D::Error::custom),
            _ => Err(D::Error::custom("expected `auto` or `{ cert, key }`")),
        }
    }
}

/// Where `tls: auto` keeps its CA, relative to the config file.
pub const AUTO_TLS_DIR: &str = ".decoy";

/// A listener in the config file: a bare port, or an object for per-listener settings.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
struct ListenerFile {
    port: u16,
    host: Option<String>,
    tls: Option<bool>,
}

fn listener_file<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<ListenerFile, D::Error> {
    use serde::de::Error;
    match serde_norway::Value::deserialize(deserializer)? {
        serde_norway::Value::Mapping(mapping) => {
            serde_norway::from_value(serde_norway::Value::Mapping(mapping))
                .map_err(D::Error::custom)
        }
        value => u16::deserialize(value)
            .map(|port| ListenerFile {
                port,
                host: None,
                tls: None,
            })
            .map_err(D::Error::custom),
    }
}

fn optional_listener_file<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<ListenerFile>, D::Error> {
    match serde_norway::Value::deserialize(deserializer)? {
        serde_norway::Value::Null => Ok(None),
        value => listener_file(value)
            .map(Some)
            .map_err(serde::de::Error::custom),
    }
}

fn listener_files<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<BTreeMap<String, ListenerFile>, D::Error> {
    #[derive(Deserialize)]
    struct Entry(#[serde(deserialize_with = "listener_file")] ListenerFile);
    Ok(BTreeMap::<String, Entry>::deserialize(deserializer)?
        .into_iter()
        .map(|(name, Entry(listener))| (name, listener))
        .collect())
}

/// Where gRPC message types come from; paths are absolute after config resolution.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Default)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ProtoSources {
    #[serde(default)]
    pub roots: Vec<PathBuf>,
    #[serde(default)]
    pub descriptor_sets: Vec<PathBuf>,
    /// Glob patterns picking the entry files under the roots; empty compiles every `.proto`.
    #[serde(default)]
    pub files: Vec<PathBuf>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Default)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct NativeConfigFile {
    #[serde(alias = "routesDir")]
    routes: Option<PathBuf>,
    #[serde(alias = "collectionsFile")]
    collections: Option<PathBuf>,
    #[serde(default, deserialize_with = "optional_listener_file")]
    port: Option<ListenerFile>,
    host: Option<String>,
    #[serde(default, deserialize_with = "listener_files")]
    listeners: BTreeMap<String, ListenerFile>,
    tls: Option<TlsFile>,
    #[serde(alias = "startupCollection")]
    collection: Option<String>,
    #[serde(alias = "passthroughBaseUrl")]
    passthrough_base_url: Option<String>,
    protos: Option<ProtoSources>,
    max_body_bytes: Option<usize>,
    #[serde(default)]
    transformers: BTreeMap<String, TransformerFile>,
    #[serde(default)]
    pipelines: BTreeMap<String, PipelineConfig>,
}

impl ServeConfig {
    pub fn from_options(options: ServeCliOptions, cwd: &Path) -> Result<Self, ConfigError> {
        let (file_config, config_base) = match &options.config_file {
            Some(path) => {
                let path = absolutize(cwd, path);
                (
                    Some(read_config_file(&path)?),
                    path.parent().unwrap_or(cwd).to_path_buf(),
                )
            }
            None => (None, cwd.to_path_buf()),
        };

        let routes = resolve_required_path(
            cwd,
            &config_base,
            options.routes,
            file_config
                .as_ref()
                .and_then(|config| config.routes.clone()),
            "routes",
        )?;
        let collections = resolve_required_path(
            cwd,
            &config_base,
            options.collections,
            file_config
                .as_ref()
                .and_then(|config| config.collections.clone()),
            "collections",
        )?;
        let tls = file_config
            .as_ref()
            .and_then(|config| config.tls.clone())
            .map(|tls| match tls {
                TlsFile::Files(files) => TlsSource::Files(TlsFiles {
                    cert: absolutize(&config_base, &files.cert),
                    key: absolutize(&config_base, &files.key),
                }),
                TlsFile::Auto => TlsSource::Auto {
                    dir: config_base.join(AUTO_TLS_DIR),
                },
            });
        // `--host` replaces the file's default host and the main listener's own.
        let host = options
            .host
            .clone()
            .or_else(|| file_config.as_ref().and_then(|config| config.host.clone()))
            .unwrap_or_else(|| DEFAULT_HOST.to_owned());
        let resolve = |name: &str, listener: ListenerFile| match (listener.tls, &tls) {
            (Some(true), None) => Err(ConfigError::TlsWithoutCert {
                name: name.to_owned(),
            }),
            (on, tls) => Ok(Listener {
                port: listener.port,
                host: listener.host.unwrap_or_else(|| host.clone()),
                tls: tls.is_some() && on != Some(false),
            }),
        };
        let file_port = file_config.as_ref().and_then(|config| config.port.clone());
        let mut port = resolve(
            "port",
            file_port.unwrap_or(ListenerFile {
                port: DEFAULT_PORT,
                host: None,
                tls: None,
            }),
        )?;
        if let Some(cli_port) = options.port {
            port.port = cli_port;
        }
        if let Some(cli_host) = &options.host {
            port.host = cli_host.clone();
        }
        let listeners = file_config
            .as_ref()
            .map(|config| config.listeners.clone())
            .unwrap_or_default()
            .into_iter()
            .map(|(name, listener)| {
                Ok((
                    name.clone(),
                    resolve(&format!("listeners.{name}"), listener)?,
                ))
            })
            .collect::<Result<BTreeMap<_, _>, ConfigError>>()?;

        let main = if options.port.is_some() {
            "--port"
        } else {
            "port"
        };
        let mut taken = BTreeMap::from([(port.port, main.to_owned())]);
        // Port 0 asks the OS for a free port, so several 0s never collide.
        for (name, extra) in listeners.iter().filter(|(_, extra)| extra.port != 0) {
            if let Some(first) = taken.insert(extra.port, format!("listeners.{name}")) {
                return Err(ConfigError::PortConflict {
                    port: extra.port,
                    first,
                    second: format!("listeners.{name}"),
                });
            }
        }
        let collection = options.collection.or_else(|| {
            file_config
                .as_ref()
                .and_then(|config| config.collection.clone())
        });
        let passthrough_name = if options.passthrough_base_url.is_some() {
            "--passthrough-base-url"
        } else {
            "passthroughBaseUrl"
        };
        let passthrough_base_url = options.passthrough_base_url.or_else(|| {
            file_config
                .as_ref()
                .and_then(|config| config.passthrough_base_url.clone())
        });
        if let Some(url) = &passthrough_base_url
            && !url::Url::parse(url).is_ok_and(|parsed| matches!(parsed.scheme(), "http" | "https"))
        {
            return Err(ConfigError::InvalidPassthroughBaseUrl {
                name: passthrough_name,
                url: url.clone(),
            });
        }
        let max_body_bytes = file_config
            .as_ref()
            .and_then(|config| config.max_body_bytes)
            .unwrap_or(DEFAULT_MAX_BODY_BYTES);
        let mut plugins = BTreeMap::new();
        let mut commands = BTreeMap::new();
        for (name, transformer) in file_config
            .as_ref()
            .map(|config| config.transformers.clone())
            .unwrap_or_default()
        {
            match transformer {
                TransformerFile::Worker(worker) => {
                    let worker = plugin_config(&name, worker, &config_base)?;
                    plugins.insert(name, worker);
                }
                TransformerFile::Commands(command) => {
                    commands.insert(
                        name,
                        CommandsConfig {
                            input: command.input,
                            output: command.output,
                            dir: config_base.clone(),
                        },
                    );
                }
            }
        }
        let pipelines = file_config
            .as_ref()
            .map(|config| config.pipelines.clone())
            .unwrap_or_default();
        let protos = file_config
            .and_then(|config| config.protos)
            .map(|protos| ProtoSources {
                roots: protos
                    .roots
                    .iter()
                    .map(|path| absolutize(&config_base, path))
                    .collect(),
                descriptor_sets: protos
                    .descriptor_sets
                    .iter()
                    .map(|path| absolutize(&config_base, path))
                    .collect(),
                files: protos
                    .files
                    .iter()
                    .map(|path| absolutize(&config_base, path))
                    .collect(),
            })
            .unwrap_or_default();

        Ok(Self {
            routes,
            collections,
            port,
            listeners,
            tls,
            collection,
            passthrough_base_url,
            protos,
            max_body_bytes,
            plugins,
            pipelines,
            commands,
        })
    }
}

/// Validates the configuration for a persistent worker.
fn plugin_config(key: &str, plugin: PluginFile, dir: &Path) -> Result<PluginConfig, ConfigError> {
    let invalid = |reason: String| ConfigError::InvalidPlugin {
        key: key.to_owned(),
        reason,
    };
    if key.is_empty()
        || !key
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        return Err(invalid(
            "a key takes only letters, digits, `-` and `_`".to_owned(),
        ));
    }
    if plugin.worker.first().is_none_or(String::is_empty) {
        return Err(invalid("`worker` needs an executable".to_owned()));
    }
    let timeout = match plugin.timeout {
        None => DEFAULT_PLUGIN_TIMEOUT,
        Some(text) => parse_duration(&text)
            .filter(|timeout| !timeout.is_zero())
            .ok_or_else(|| invalid(format!("`timeout: {text}` is not a duration like `5s`")))?,
    };
    Ok(PluginConfig {
        command: plugin.worker,
        dir: dir.to_path_buf(),
        timeout,
    })
}

fn read_config_file(path: &Path) -> Result<NativeConfigFile, ConfigError> {
    let input = fs::read_to_string(path).map_err(|source| ConfigError::ReadConfig {
        path: path.to_path_buf(),
        source,
    })?;
    serde_norway::from_str(&input).map_err(|source| ConfigError::ParseConfig {
        path: path.to_path_buf(),
        source,
    })
}

fn resolve_required_path(
    cwd: &Path,
    config_base: &Path,
    cli_value: Option<PathBuf>,
    config_value: Option<PathBuf>,
    name: &'static str,
) -> Result<PathBuf, ConfigError> {
    if let Some(path) = cli_value {
        return Ok(absolutize(cwd, &path));
    }

    if let Some(path) = config_value {
        return Ok(absolutize(config_base, &path));
    }

    Err(ConfigError::MissingValue { name })
}

fn absolutize(base: &Path, path: &Path) -> PathBuf {
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        base.join(path)
    }
}

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("failed to read config `{}`: {source}", path.display())]
    ReadConfig {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("failed to parse config `{}`: {source}", path.display())]
    ParseConfig {
        path: PathBuf,
        source: serde_norway::Error,
    },
    #[error("`{second}` uses port {port}, already taken by `{first}`")]
    PortConflict {
        port: u16,
        first: String,
        second: String,
    },
    #[error("`{name}` sets `tls: true`, but no top-level `tls` cert is configured")]
    TlsWithoutCert { name: String },
    #[error("`{name}` `{url}` must be an absolute http or https URL")]
    InvalidPassthroughBaseUrl { name: &'static str, url: String },
    #[error("missing required serve {name}; pass --{name} or set it in --config")]
    MissingValue { name: &'static str },
    #[error("plugin `{key}`: {reason}")]
    InvalidPlugin { key: String, reason: String },
}

#[cfg(test)]
mod tests {
    use pretty_assertions::assert_eq;
    use tempfile::tempdir;

    use super::*;

    #[test]
    fn config_file_supplies_serve_inputs_relative_to_config_file() {
        let dir = tempdir().unwrap();
        let base = dir.path();
        fs::create_dir_all(base.join("nested")).unwrap();
        fs::write(
            base.join("nested/decoy.yaml"),
            r#"
routes: config/routes
collections: config/collections.yaml
port: 4100
startupCollection: local
"#,
        )
        .unwrap();

        let config = ServeConfig::from_options(
            ServeCliOptions {
                config_file: Some(base.join("nested/decoy.yaml")),
                ..ServeCliOptions::default()
            },
            base,
        )
        .unwrap();

        assert_eq!(config.routes, base.join("nested/config/routes"));
        assert_eq!(
            config.collections,
            base.join("nested/config/collections.yaml")
        );
        assert_eq!(
            config.port,
            Listener {
                port: 4100,
                host: "localhost".to_owned(),
                tls: false
            }
        );
        assert_eq!(config.collection.as_deref(), Some("local"));
        assert_eq!(config.protos, ProtoSources::default());
    }

    #[test]
    fn config_file_proto_paths_resolve_relative_to_config_file() {
        let dir = tempdir().unwrap();
        let base = dir.path();
        fs::create_dir_all(base.join("nested")).unwrap();
        fs::write(
            base.join("nested/decoy.yaml"),
            r#"
routes: routes
collections: collections.yaml
protos:
  roots: [../proto]
  descriptorSets: [build/api.binpb]
  files: [../proto/services/**/*.proto]
"#,
        )
        .unwrap();

        let config = ServeConfig::from_options(
            ServeCliOptions {
                config_file: Some(base.join("nested/decoy.yaml")),
                ..ServeCliOptions::default()
            },
            base,
        )
        .unwrap();

        assert_eq!(
            config.protos,
            ProtoSources {
                roots: vec![base.join("nested/../proto")],
                descriptor_sets: vec![base.join("nested/build/api.binpb")],
                files: vec![base.join("nested/../proto/services/**/*.proto")],
            }
        );
    }

    #[test]
    fn config_file_accepts_descriptive_path_aliases() {
        let dir = tempdir().unwrap();
        let base = dir.path();
        fs::write(
            base.join("decoy.yaml"),
            r#"
routesDir: config/routes
collectionsFile: config/collections.yaml
"#,
        )
        .unwrap();

        let config = ServeConfig::from_options(
            ServeCliOptions {
                config_file: Some(base.join("decoy.yaml")),
                ..ServeCliOptions::default()
            },
            base,
        )
        .unwrap();

        assert_eq!(config.routes, base.join("config/routes"));
        assert_eq!(config.collections, base.join("config/collections.yaml"));
    }

    #[test]
    fn explicit_cli_flags_override_config_values() {
        let dir = tempdir().unwrap();
        let base = dir.path();
        fs::write(
            base.join("decoy.yaml"),
            r#"
routes: config/routes
collections: config/collections.yaml
port: 4100
collection: config-collection
"#,
        )
        .unwrap();

        let config = ServeConfig::from_options(
            ServeCliOptions {
                config_file: Some(base.join("decoy.yaml")),
                routes: Some(PathBuf::from("flag/routes")),
                collections: Some(PathBuf::from("flag/collections.yaml")),
                port: Some(4200),
                host: Some("127.0.0.1".to_owned()),
                collection: Some("flag-collection".to_owned()),
                passthrough_base_url: Some("http://flag.example".to_owned()),
            },
            base,
        )
        .unwrap();

        assert_eq!(config.routes, base.join("flag/routes"));
        assert_eq!(config.collections, base.join("flag/collections.yaml"));
        assert_eq!(
            config.port,
            Listener {
                port: 4200,
                host: "127.0.0.1".to_owned(),
                tls: false
            }
        );
        assert_eq!(config.collection.as_deref(), Some("flag-collection"));
        assert_eq!(
            config.passthrough_base_url.as_deref(),
            Some("http://flag.example")
        );
    }

    fn load_raw(base: &Path) -> Result<ServeConfig, ConfigError> {
        ServeConfig::from_options(
            ServeCliOptions {
                config_file: Some(base.join("decoy.yaml")),
                ..ServeCliOptions::default()
            },
            base,
        )
    }

    #[test]
    fn passthrough_base_url_must_be_an_http_url() {
        let dir = tempdir().unwrap();
        let base = dir.path();
        let load = |url: &str| {
            fs::write(
                base.join("decoy.yaml"),
                format!("routes: r\ncollections: c.yaml\npassthroughBaseUrl: \"{url}\"\n"),
            )
            .unwrap();
            load_raw(base)
        };

        assert!(load("https://api.example.test/v1").is_ok());
        for url in ["localhost:3000", "ftp://files.example.test", "http://"] {
            assert_eq!(
                load(url).unwrap_err().to_string(),
                format!("`passthroughBaseUrl` `{url}` must be an absolute http or https URL"),
                "{url}"
            );
        }
    }

    #[test]
    fn extra_ports_are_named_and_must_not_collide() {
        let dir = tempdir().unwrap();
        let base = dir.path();
        let load = |ports: &str, port: Option<u16>| {
            fs::write(
                base.join("decoy.yaml"),
                format!("routes: r\ncollections: c.yaml\nport: 4100\nlisteners: {ports}\n"),
            )
            .unwrap();
            ServeConfig::from_options(
                ServeCliOptions {
                    config_file: Some(base.join("decoy.yaml")),
                    port,
                    ..ServeCliOptions::default()
                },
                base,
            )
        };

        let config = load("{ grpc: 50051 }", None).unwrap();
        assert_eq!(
            config.listeners,
            BTreeMap::from([(
                "grpc".to_owned(),
                Listener {
                    port: 50051,
                    host: "localhost".to_owned(),
                    tls: false
                }
            )])
        );

        assert_eq!(
            load("{ grpc: 4100 }", None).unwrap_err().to_string(),
            "`listeners.grpc` uses port 4100, already taken by `port`"
        );
        assert_eq!(
            load("{ a: 5000, b: 5000 }", None).unwrap_err().to_string(),
            "`listeners.b` uses port 5000, already taken by `listeners.a`"
        );
        assert_eq!(
            load("{ grpc: 4200 }", Some(4200)).unwrap_err().to_string(),
            "`listeners.grpc` uses port 4200, already taken by `--port`"
        );
        fs::write(
            base.join("decoy.yaml"),
            "routes: r\ncollections: c.yaml\nport: 0\nlisteners: { a: 0, b: 0 }\n",
        )
        .unwrap();
        assert!(load_raw(base).is_ok());
    }

    #[test]
    fn tls_applies_to_every_listener_unless_it_opts_out() {
        let dir = tempdir().unwrap();
        let base = dir.path();
        fs::write(
            base.join("decoy.yaml"),
            r#"
routes: r
collections: c.yaml
tls: { cert: certs/dev.pem, key: certs/dev-key.pem }
port: 8443
listeners:
  payments: 9443
  legacy: { port: 8080, tls: false }
"#,
        )
        .unwrap();

        let config = load_raw(base).unwrap();

        assert_eq!(
            config.tls,
            Some(TlsSource::Files(TlsFiles {
                cert: base.join("certs/dev.pem"),
                key: base.join("certs/dev-key.pem"),
            }))
        );
        assert_eq!(
            config.port,
            Listener {
                port: 8443,
                host: "localhost".to_owned(),
                tls: true
            }
        );
        assert_eq!(
            config.listeners,
            BTreeMap::from([
                (
                    "legacy".to_owned(),
                    Listener {
                        port: 8080,
                        host: "localhost".to_owned(),
                        tls: false
                    }
                ),
                (
                    "payments".to_owned(),
                    Listener {
                        port: 9443,
                        host: "localhost".to_owned(),
                        tls: true
                    }
                ),
            ])
        );
    }

    #[test]
    fn main_port_object_keeps_its_tls_choice_under_a_cli_port() {
        let dir = tempdir().unwrap();
        let base = dir.path();
        fs::write(
            base.join("decoy.yaml"),
            "routes: r\ncollections: c.yaml\ntls: { cert: c.pem, key: k.pem }\nport: { port: 8080, tls: false }\n",
        )
        .unwrap();

        let config = ServeConfig::from_options(
            ServeCliOptions {
                config_file: Some(base.join("decoy.yaml")),
                port: Some(4200),
                ..ServeCliOptions::default()
            },
            base,
        )
        .unwrap();

        assert_eq!(
            config.port,
            Listener {
                port: 4200,
                host: "localhost".to_owned(),
                tls: false
            }
        );
    }

    #[test]
    fn listener_tls_without_a_cert_and_unknown_listener_keys_fail() {
        let dir = tempdir().unwrap();
        let base = dir.path();
        let load = |yaml: &str| {
            fs::write(
                base.join("decoy.yaml"),
                format!("routes: r\ncollections: c.yaml\n{yaml}\n"),
            )
            .unwrap();
            load_raw(base)
        };

        assert_eq!(
            load("listeners: { a: { port: 9000, tls: true } }")
                .unwrap_err()
                .to_string(),
            "`listeners.a` sets `tls: true`, but no top-level `tls` cert is configured"
        );
        assert_eq!(
            load("port: { port: 9000, tls: false }").unwrap().port,
            Listener {
                port: 9000,
                host: "localhost".to_owned(),
                tls: false
            }
        );
        assert_eq!(load("port: ~").unwrap().port.port, DEFAULT_PORT);
        let error = load("listeners: { a: { port: 9000, bind: x } }")
            .unwrap_err()
            .to_string();
        assert!(error.contains("unknown field `bind`"), "{error}");
        // `ports` was renamed before any release.
        let error = load("ports: { a: 9000 }").unwrap_err().to_string();
        assert!(error.contains("unknown field `ports`"), "{error}");
        let error = load("tls: { cert: c.pem }").unwrap_err().to_string();
        assert!(error.contains("missing field `key`"), "{error}");
        let error = load("tls: yes-please").unwrap_err().to_string();
        assert!(
            error.contains("expected `auto` or `{ cert, key }`"),
            "{error}"
        );
        assert_eq!(
            load("tls: auto").unwrap().tls,
            Some(TlsSource::Auto {
                dir: base.join(".decoy")
            })
        );
    }

    /// Pins the YAML parse errors users see (message and location) across YAML library swaps.
    #[test]
    fn config_parse_errors_keep_their_messages_and_locations() {
        let dir = tempdir().unwrap();
        let base = dir.path();
        let load = |yaml: &str| {
            fs::write(base.join("decoy.yaml"), yaml).unwrap();
            let error = load_raw(base).unwrap_err().to_string();
            error.replace(&base.display().to_string(), "<dir>")
        };

        assert_eq!(
            load("routes: r\ncollections: c.yaml\nport: [9000\n"),
            "failed to parse config `<dir>/decoy.yaml`: did not find expected ',' or ']' at line 4 column 1, while parsing a flow sequence at line 3 column 7"
        );
        assert_eq!(
            load("routes: r\ncollections: c.yaml\nlisteners: { a: { port: 9000, bind: x } }\n"),
            "failed to parse config `<dir>/decoy.yaml`: listeners: unknown field `bind`, expected one of `port`, `host`, `tls` at line 3 column 12"
        );
        assert_eq!(
            load("routes: r\ncollections: c.yaml\nport: { port: high }\n"),
            "failed to parse config `<dir>/decoy.yaml`: invalid type: string \"high\", expected u16"
        );
        assert_eq!(
            load("routes: r\ncollections: c.yaml\ntls: yes-please\n"),
            "failed to parse config `<dir>/decoy.yaml`: expected `auto` or `{ cert, key }`"
        );
    }

    #[test]
    fn host_defaults_to_localhost_and_cli_host_replaces_the_default() {
        let dir = tempdir().unwrap();
        let base = dir.path();
        fs::write(
            base.join("decoy.yaml"),
            "routes: r\ncollections: c.yaml\nhost: 127.0.0.1\nport: { port: 8080, host: '::1' }\nlisteners:\n  plain: 9000\n  docker: { port: 9001, host: 0.0.0.0 }\n",
        )
        .unwrap();
        let load = |host: Option<&str>| {
            ServeConfig::from_options(
                ServeCliOptions {
                    config_file: Some(base.join("decoy.yaml")),
                    host: host.map(str::to_owned),
                    ..ServeCliOptions::default()
                },
                base,
            )
            .unwrap()
        };
        let hosts = |config: &ServeConfig| {
            (
                config.port.host.clone(),
                config.listeners["plain"].host.clone(),
                config.listeners["docker"].host.clone(),
            )
        };

        assert_eq!(
            hosts(&load(None)),
            ("::1".into(), "127.0.0.1".into(), "0.0.0.0".into())
        );
        assert_eq!(
            hosts(&load(Some("localhost"))),
            ("localhost".into(), "localhost".into(), "0.0.0.0".into())
        );
        fs::write(base.join("decoy.yaml"), "routes: r\ncollections: c.yaml\n").unwrap();
        assert_eq!(load(None).port.host, DEFAULT_HOST);
    }

    #[test]
    fn plugin_timeouts_use_milliseconds_or_seconds() {
        let dir = tempdir().unwrap();
        let base = dir.path();
        let path = base.join("decoy.yaml");
        let load = |timeout| {
            fs::write(
                &path,
                format!(
                    "routes: r\ncollections: c.yaml\ntransformers:\n  demo:\n    worker: [node, worker.js]\n    timeout: {timeout}\n"
                ),
            )
            .unwrap();
            ServeConfig::from_options(
                ServeCliOptions {
                    config_file: Some(path.clone()),
                    ..ServeCliOptions::default()
                },
                base,
            )
        };

        for (value, millis) in [("500ms", 500), ("1.5s", 1_500)] {
            assert_eq!(
                load(value).unwrap().plugins["demo"].timeout,
                std::time::Duration::from_millis(millis)
            );
        }
        for value in ["1m", "0s"] {
            assert!(matches!(
                load(value),
                Err(ConfigError::InvalidPlugin { .. })
            ));
        }
    }

    #[test]
    fn missing_required_paths_fail_before_startup() {
        let dir = tempdir().unwrap();
        let error = ServeConfig::from_options(ServeCliOptions::default(), dir.path()).unwrap_err();

        assert_eq!(
            error.to_string(),
            "missing required serve routes; pass --routes or set it in --config"
        );
    }
}

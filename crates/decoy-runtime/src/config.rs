use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use serde::Deserialize;
use thiserror::Error;

pub const DEFAULT_PORT: u16 = 8080;

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ServeCliOptions {
    pub config_file: Option<PathBuf>,
    pub routes: Option<PathBuf>,
    pub collections: Option<PathBuf>,
    pub port: Option<u16>,
    pub collection: Option<String>,
    pub passthrough_base_url: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServeConfig {
    pub routes: PathBuf,
    pub collections: PathBuf,
    pub port: Listener,
    /// Extra named listeners (e.g. `grpc: 50051`) serving the same Routes and Sessions as `port`.
    pub ports: BTreeMap<String, Listener>,
    /// Certificate served by every listener with `tls` on; paths are absolute.
    pub tls: Option<TlsSource>,
    pub collection: Option<String>,
    pub passthrough_base_url: Option<String>,
    pub protos: ProtoSources,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Listener {
    pub port: u16,
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
        match serde_yaml::Value::deserialize(deserializer)? {
            serde_yaml::Value::String(value) if value == "auto" => Ok(Self::Auto),
            value @ serde_yaml::Value::Mapping(_) => serde_yaml::from_value(value)
                .map(Self::Files)
                .map_err(D::Error::custom),
            _ => Err(D::Error::custom("expected `auto` or `{ cert, key }`")),
        }
    }
}

/// Where `tls: auto` keeps its CA, relative to the config file.
pub const AUTO_TLS_DIR: &str = ".decoy";

/// A listener in the config file: a bare port, or an object for per-listener settings.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
struct ListenerFile {
    port: u16,
    tls: Option<bool>,
}

fn listener_file<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<ListenerFile, D::Error> {
    use serde::de::Error;
    match serde_yaml::Value::deserialize(deserializer)? {
        serde_yaml::Value::Mapping(mapping) => {
            serde_yaml::from_value(serde_yaml::Value::Mapping(mapping)).map_err(D::Error::custom)
        }
        value => u16::deserialize(value)
            .map(|port| ListenerFile { port, tls: None })
            .map_err(D::Error::custom),
    }
}

fn optional_listener_file<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<ListenerFile>, D::Error> {
    match serde_yaml::Value::deserialize(deserializer)? {
        serde_yaml::Value::Null => Ok(None),
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
    #[serde(default, deserialize_with = "listener_files")]
    ports: BTreeMap<String, ListenerFile>,
    tls: Option<TlsFile>,
    #[serde(alias = "startupCollection")]
    collection: Option<String>,
    #[serde(alias = "passthroughBaseUrl")]
    passthrough_base_url: Option<String>,
    protos: Option<ProtoSources>,
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
        let resolve = |name: &str, listener: ListenerFile| match (listener.tls, &tls) {
            (Some(true), None) => Err(ConfigError::TlsWithoutCert {
                name: name.to_owned(),
            }),
            (on, tls) => Ok(Listener {
                port: listener.port,
                tls: tls.is_some() && on != Some(false),
            }),
        };
        let file_port = file_config.as_ref().and_then(|config| config.port);
        let mut port = resolve(
            "port",
            file_port.unwrap_or(ListenerFile {
                port: DEFAULT_PORT,
                tls: None,
            }),
        )?;
        if let Some(cli_port) = options.port {
            port.port = cli_port;
        }
        let ports = file_config
            .as_ref()
            .map(|config| config.ports.clone())
            .unwrap_or_default()
            .into_iter()
            .map(|(name, listener)| {
                Ok((name.clone(), resolve(&format!("ports.{name}"), listener)?))
            })
            .collect::<Result<BTreeMap<_, _>, ConfigError>>()?;
        let main = if options.port.is_some() {
            "--port"
        } else {
            "port"
        };
        let mut taken = BTreeMap::from([(port.port, main.to_owned())]);
        // Port 0 asks the OS for a free port, so several 0s never collide.
        for (name, extra) in ports.iter().filter(|(_, extra)| extra.port != 0) {
            if let Some(first) = taken.insert(extra.port, format!("ports.{name}")) {
                return Err(ConfigError::PortConflict {
                    port: extra.port,
                    first,
                    second: format!("ports.{name}"),
                });
            }
        }
        let collection = options.collection.or_else(|| {
            file_config
                .as_ref()
                .and_then(|config| config.collection.clone())
        });
        let passthrough_base_url = options.passthrough_base_url.or_else(|| {
            file_config
                .as_ref()
                .and_then(|config| config.passthrough_base_url.clone())
        });
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
            })
            .unwrap_or_default();

        Ok(Self {
            routes,
            collections,
            port,
            ports,
            tls,
            collection,
            passthrough_base_url,
            protos,
        })
    }
}

fn read_config_file(path: &Path) -> Result<NativeConfigFile, ConfigError> {
    let input = fs::read_to_string(path).map_err(|source| ConfigError::ReadConfig {
        path: path.to_path_buf(),
        source,
    })?;
    serde_yaml::from_str(&input).map_err(|source| ConfigError::ParseConfig {
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
        source: serde_yaml::Error,
    },
    #[error("`{second}` uses port {port}, already taken by `{first}`")]
    PortConflict {
        port: u16,
        first: String,
        second: String,
    },
    #[error("`{name}` sets `tls: true`, but no top-level `tls` cert is configured")]
    TlsWithoutCert { name: String },
    #[error("missing required serve {name}; pass --{name} or set it in --config")]
    MissingValue { name: &'static str },
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
    fn extra_ports_are_named_and_must_not_collide() {
        let dir = tempdir().unwrap();
        let base = dir.path();
        let load = |ports: &str, port: Option<u16>| {
            fs::write(
                base.join("decoy.yaml"),
                format!("routes: r\ncollections: c.yaml\nport: 4100\nports: {ports}\n"),
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
            config.ports,
            BTreeMap::from([(
                "grpc".to_owned(),
                Listener {
                    port: 50051,
                    tls: false
                }
            )])
        );

        assert_eq!(
            load("{ grpc: 4100 }", None).unwrap_err().to_string(),
            "`ports.grpc` uses port 4100, already taken by `port`"
        );
        assert_eq!(
            load("{ a: 5000, b: 5000 }", None).unwrap_err().to_string(),
            "`ports.b` uses port 5000, already taken by `ports.a`"
        );
        assert_eq!(
            load("{ grpc: 4200 }", Some(4200)).unwrap_err().to_string(),
            "`ports.grpc` uses port 4200, already taken by `--port`"
        );
        fs::write(
            base.join("decoy.yaml"),
            "routes: r\ncollections: c.yaml\nport: 0\nports: { a: 0, b: 0 }\n",
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
ports:
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
                tls: true
            }
        );
        assert_eq!(
            config.ports,
            BTreeMap::from([
                (
                    "legacy".to_owned(),
                    Listener {
                        port: 8080,
                        tls: false
                    }
                ),
                (
                    "payments".to_owned(),
                    Listener {
                        port: 9443,
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
            load("ports: { a: { port: 9000, tls: true } }")
                .unwrap_err()
                .to_string(),
            "`ports.a` sets `tls: true`, but no top-level `tls` cert is configured"
        );
        assert_eq!(
            load("port: { port: 9000, tls: false }").unwrap().port,
            Listener {
                port: 9000,
                tls: false
            }
        );
        assert_eq!(load("port: ~").unwrap().port.port, DEFAULT_PORT);
        let error = load("ports: { a: { port: 9000, host: x } }")
            .unwrap_err()
            .to_string();
        assert!(error.contains("unknown field `host`"), "{error}");
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

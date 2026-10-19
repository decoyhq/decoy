use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};

use serde_norway::Value as YamlValue;
use thiserror::Error;

use crate::collections::{Collection, CollectionError, CollectionsFile};
use crate::engine::Catalog;
use crate::http::RuntimeConfig;
use crate::schema::{Route, ValidationError};

#[derive(Debug, Clone, PartialEq)]
pub struct Startup {
    pub catalog: Catalog,
    pub default_collection: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceLocation {
    pub path: PathBuf,
    pub line: Option<usize>,
    pub value_path: Option<String>,
}

impl SourceLocation {
    fn path(path: impl Into<PathBuf>) -> Self {
        Self {
            path: path.into(),
            line: None,
            value_path: None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StartupDiagnosticKind {
    Io,
    Parse,
    Schema,
    CrossReference,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StartupDiagnostic {
    pub kind: StartupDiagnosticKind,
    pub source: SourceLocation,
    pub message: String,
}

#[derive(Debug, Error, Clone, PartialEq, Eq)]
#[error("{diagnostic}")]
pub struct StartupError {
    pub diagnostic: StartupDiagnostic,
}

impl fmt::Display for StartupDiagnostic {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}", self.source.path.display())?;
        if let Some(line) = self.source.line {
            write!(formatter, ":{line}")?;
        }
        if let Some(value_path) = &self.source.value_path {
            write!(formatter, " {value_path}")?;
        }
        write!(formatter, ": {}", self.message)
    }
}

impl StartupError {
    pub(crate) fn new(
        kind: StartupDiagnosticKind,
        path: impl Into<PathBuf>,
        message: impl Into<String>,
    ) -> Self {
        Self::with_source(kind, SourceLocation::path(path), message)
    }

    fn with_source(
        kind: StartupDiagnosticKind,
        source: SourceLocation,
        message: impl Into<String>,
    ) -> Self {
        Self {
            diagnostic: StartupDiagnostic {
                kind,
                source,
                message: message.into(),
            },
        }
    }
}

pub fn load_catalog_from_files(
    routes_dir: impl AsRef<Path>,
    collections_file: impl AsRef<Path>,
    runtime: RuntimeConfig,
    default_collection: Option<&str>,
) -> Result<Startup, StartupError> {
    let routes_dir = routes_dir.as_ref();
    let collections_file = collections_file.as_ref();

    let mut routes = Vec::new();
    let mut seen_route_paths = BTreeMap::new();
    for route_path in discover_route_files(routes_dir)? {
        let input = fs::read_to_string(&route_path).map_err(|error| {
            StartupError::new(StartupDiagnosticKind::Io, &route_path, error.to_string())
        })?;
        let route = decode_route(&route_path, &input)?;
        crate::grpc::schema::check_route(&route, &runtime.protos).map_err(|reason| {
            StartupError::new(StartupDiagnosticKind::Schema, &route_path, reason)
        })?;
        if let Some(first_path) = seen_route_paths.insert(route.id.clone(), route_path.clone()) {
            return Err(StartupError::new(
                StartupDiagnosticKind::Schema,
                &route_path,
                format!(
                    "duplicate route id `{}` in `{}` and `{}`",
                    route.id,
                    normalize_for_sort(routes_dir, &first_path),
                    normalize_for_sort(routes_dir, &route_path)
                ),
            ));
        }
        routes.push(route);
    }

    let collections_input = fs::read_to_string(collections_file).map_err(|error| {
        StartupError::new(
            StartupDiagnosticKind::Io,
            collections_file,
            error.to_string(),
        )
    })?;
    let collection_vec = decode_collections(collections_file, &collections_input)?;
    let first_authored_collection = collection_vec
        .first()
        .map(|collection| collection.id.clone());
    let collections = CollectionsFile::new(collection_vec)
        .map_err(|error| collection_error(collections_file, error))?;

    let default_collection = match default_collection {
        Some(id) => {
            if collections.get(id).is_none() {
                return Err(StartupError::new(
                    StartupDiagnosticKind::CrossReference,
                    collections_file,
                    format!("unknown startup collection `{id}`"),
                ));
            }
            id.to_owned()
        }
        None => first_authored_collection.ok_or_else(|| {
            StartupError::new(
                StartupDiagnosticKind::Schema,
                collections_file,
                "collections file must define at least one collection",
            )
        })?,
    };

    validate_collection_references(&routes, &collections, collections_file)?;
    let catalog = Catalog::from_checked(routes, collections, runtime).map_err(|error| {
        StartupError::new(
            StartupDiagnosticKind::Schema,
            collections_file,
            error.to_string(),
        )
    })?;

    Ok(Startup {
        catalog,
        default_collection,
    })
}

fn discover_route_files(routes_dir: &Path) -> Result<Vec<PathBuf>, StartupError> {
    let mut paths = Vec::new();
    discover_route_files_inner(routes_dir, &mut paths, &mut BTreeSet::new())?;
    paths.sort_by(|left, right| {
        normalize_for_sort(routes_dir, left).cmp(&normalize_for_sort(routes_dir, right))
    });
    Ok(paths)
}

/// Follows symlinks; `visited` holds the folders walked so far, so a folder linking back to an
/// ancestor isn't walked again.
fn discover_route_files_inner(
    dir: &Path,
    paths: &mut Vec<PathBuf>,
    visited: &mut BTreeSet<PathBuf>,
) -> Result<(), StartupError> {
    let canonical = dir
        .canonicalize()
        .map_err(|error| StartupError::new(StartupDiagnosticKind::Io, dir, error.to_string()))?;
    if !visited.insert(canonical) {
        return Ok(());
    }
    for entry in fs::read_dir(dir)
        .map_err(|error| StartupError::new(StartupDiagnosticKind::Io, dir, error.to_string()))?
    {
        let entry = entry.map_err(|error| {
            StartupError::new(StartupDiagnosticKind::Io, dir, error.to_string())
        })?;
        let path = entry.path();
        let file_type = match fs::metadata(&path) {
            Ok(metadata) => metadata.file_type(),
            // A dangling symlink, skipped as before links were followed.
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => {
                return Err(StartupError::new(
                    StartupDiagnosticKind::Io,
                    &path,
                    error.to_string(),
                ));
            }
        };
        if file_type.is_dir() {
            discover_route_files_inner(&path, paths, visited)?;
        } else if file_type.is_file() && is_yaml_path(&path) {
            paths.push(path);
        }
    }
    Ok(())
}

fn is_yaml_path(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| matches!(extension, "yaml" | "yml"))
}

fn normalize_for_sort(routes_dir: &Path, path: &Path) -> String {
    path.strip_prefix(routes_dir)
        .unwrap_or(path)
        .components()
        .map(|component| component.as_os_str().to_string_lossy())
        .collect::<Vec<_>>()
        .join("/")
}

fn decode_route(path: &Path, input: &str) -> Result<Route, StartupError> {
    parse_yaml(path, input)?;
    // Decode from text, not the parsed value, so schema errors keep their line.
    let route = serde_norway::from_str::<Route>(input).map_err(|error| {
        StartupError::with_source(
            StartupDiagnosticKind::Schema,
            source_from_yaml_error(path, &error),
            format!("failed to decode route schema: {error}"),
        )
    })?;
    route.validate().map_err(|error| route_error(path, error))?;
    Ok(route)
}

fn decode_collections(path: &Path, input: &str) -> Result<Vec<Collection>, StartupError> {
    parse_yaml(path, input)?;
    serde_norway::from_str::<Vec<Collection>>(input).map_err(|error| {
        StartupError::with_source(
            StartupDiagnosticKind::Schema,
            source_from_yaml_error(path, &error),
            format!("failed to decode collections schema: {error}"),
        )
    })
}

fn parse_yaml(path: &Path, input: &str) -> Result<YamlValue, StartupError> {
    serde_norway::from_str::<YamlValue>(input).map_err(|error| {
        StartupError::with_source(
            StartupDiagnosticKind::Parse,
            source_from_yaml_error(path, &error),
            error.to_string(),
        )
    })
}

fn route_error(path: &Path, error: ValidationError) -> StartupError {
    StartupError::with_source(
        StartupDiagnosticKind::Schema,
        source_from_validation_error(path, &error),
        error.to_string(),
    )
}

fn collection_error(path: &Path, error: CollectionError) -> StartupError {
    let source = match &error {
        CollectionError::Yaml(error) => source_from_yaml_error(path, error),
        CollectionError::Validation(error) => source_from_validation_error(path, error),
        _ => SourceLocation::path(path),
    };
    StartupError::with_source(StartupDiagnosticKind::Schema, source, error.to_string())
}

fn source_from_validation_error(path: &Path, error: &ValidationError) -> SourceLocation {
    match error {
        ValidationError::Yaml(error) => source_from_yaml_error(path, error),
        _ => SourceLocation::path(path),
    }
}

fn source_from_yaml_error(path: &Path, error: &serde_norway::Error) -> SourceLocation {
    let mut source = SourceLocation::path(path);
    source.line = error.location().map(|location| location.line());
    source
}

fn validate_collection_references(
    routes: &[Route],
    collections: &CollectionsFile,
    collections_file: &Path,
) -> Result<(), StartupError> {
    let routes_by_id = routes
        .iter()
        .map(|route| (route.id.as_str(), route))
        .collect::<BTreeMap<_, _>>();

    for collection_id in collections.ids() {
        let activations = collections.resolve(collection_id).map_err(|error| {
            StartupError::new(
                StartupDiagnosticKind::CrossReference,
                collections_file,
                error.to_string(),
            )
        })?;

        for activation in activations {
            let route = routes_by_id.get(activation.route.as_str()).ok_or_else(|| {
                StartupError::new(
                    StartupDiagnosticKind::CrossReference,
                    collections_file,
                    format!(
                        "collection `{collection_id}` references unknown route in `{}`",
                        activation.address()
                    ),
                )
            })?;
            let case = route.cases.get(&activation.case).ok_or_else(|| {
                StartupError::new(
                    StartupDiagnosticKind::CrossReference,
                    collections_file,
                    format!(
                        "collection `{collection_id}` references unknown case in `{}`",
                        activation.address()
                    ),
                )
            })?;
            if !case.behaviors.contains_key(&activation.behavior) {
                return Err(StartupError::new(
                    StartupDiagnosticKind::CrossReference,
                    collections_file,
                    format!(
                        "collection `{collection_id}` references unknown behavior in `{}`",
                        activation.address()
                    ),
                ));
            }
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use std::fs;

    use pretty_assertions::assert_eq;

    use crate::http::RuntimeConfig;
    use crate::schema::PassthroughTarget;
    use crate::startup::{StartupDiagnosticKind, load_catalog_from_files};

    fn write(path: &std::path::Path, contents: &str) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, contents).unwrap();
    }

    fn route_yaml(id: &str) -> String {
        format!(
            r#"
id: {id}
transport: http
match:
  method: GET
  path: /users/{{id}}
cases:
  user-123:
    match:
      pathParams:
        id: "123"
    behaviors:
      success:
        status: 200
        body:
          id: "123"
"#
        )
    }

    fn collections_yaml(address: &str) -> String {
        format!(
            r#"
- id: local
  routes:
    - {address}
"#
        )
    }

    /// Pins the YAML diagnostics users see (message and line) across YAML library swaps.
    #[test]
    fn startup_yaml_diagnostics_keep_their_messages_and_lines() {
        let diagnose = |route: &str, collections: &str| {
            let temp = tempfile::tempdir().unwrap();
            write(&temp.path().join("routes/r.yaml"), route);
            write(&temp.path().join("collections.yaml"), collections);
            let error = load_catalog_from_files(
                temp.path().join("routes"),
                temp.path().join("collections.yaml"),
                RuntimeConfig::default(),
                None,
            )
            .unwrap_err();
            (error.diagnostic.source.line, error.diagnostic.message)
        };
        let collections = collections_yaml("get-user:user-123:success");

        assert_eq!(
            diagnose("id: get-user\nmatch: [unterminated\n", &collections),
            (Some(3), "did not find expected ',' or ']' at line 3 column 1, while parsing a flow sequence at line 2 column 8".to_owned())
        );
        assert_eq!(
            diagnose("id: get-user\ntransport: http\ncases: 7\n", &collections),
            (
                Some(3),
                "failed to decode route schema: cases: invalid type: integer `7`, expected a map at line 3 column 8"
                    .to_owned()
            )
        );
        assert_eq!(
            diagnose(&route_yaml("get-user"), "- id: local\n  routes: oops\n"),
            (Some(2), "failed to decode collections schema: .[0].routes: invalid type: string \"oops\", expected a sequence at line 2 column 11".to_owned())
        );
        assert_eq!(
            diagnose(&route_yaml("get-user"), "- id: local\n  routes:\n    - a\n\t- b\n"),
            (Some(4), "found a tab character that violates indentation at line 4 column 1, while scanning a plain scalar at line 3 column 7".to_owned())
        );
    }

    #[test]
    fn startup_route_parse_failures_name_the_source_path() {
        let temp = tempfile::tempdir().unwrap();
        let route_path = temp.path().join("routes/bad.yaml");
        let collections_path = temp.path().join("collections.yaml");
        write(&route_path, "id: [unterminated\n");
        write(
            &collections_path,
            &collections_yaml("get-user:user-123:success"),
        );

        let error = load_catalog_from_files(
            temp.path().join("routes"),
            &collections_path,
            RuntimeConfig::default(),
            None,
        )
        .unwrap_err();

        assert_eq!(error.diagnostic.kind, StartupDiagnosticKind::Parse);
        assert_eq!(error.diagnostic.source.path, route_path);
        assert!(error.to_string().contains("routes/bad.yaml"));
    }

    #[test]
    fn startup_route_schema_failures_name_the_source_path() {
        let temp = tempfile::tempdir().unwrap();
        let route_path = temp.path().join("routes/get-user.yaml");
        let collections_path = temp.path().join("collections.yaml");
        write(
            &route_path,
            r#"
id: get-user
transport: http
match:
  method: GET
  path: /users/{id}
"#,
        );
        write(
            &collections_path,
            &collections_yaml("get-user:user-123:success"),
        );

        let error = load_catalog_from_files(
            temp.path().join("routes"),
            &collections_path,
            RuntimeConfig::default(),
            None,
        )
        .unwrap_err();

        assert_eq!(error.diagnostic.kind, StartupDiagnosticKind::Schema);
        assert_eq!(error.diagnostic.source.path, route_path);
        assert!(error.to_string().contains("get-user.yaml"));
        assert!(error.to_string().contains("must define at least one case"));
    }

    #[test]
    fn startup_route_decode_failures_are_schema_diagnostics() {
        let temp = tempfile::tempdir().unwrap();
        let route_path = temp.path().join("routes/get-user.yaml");
        let collections_path = temp.path().join("collections.yaml");
        write(
            &route_path,
            r#"
id: get-user
transport: http
match:
  method: GET
  path: [bad]
cases:
  user-123:
    behaviors:
      success:
        status: 200
"#,
        );
        write(
            &collections_path,
            &collections_yaml("get-user:user-123:success"),
        );

        let error = load_catalog_from_files(
            temp.path().join("routes"),
            &collections_path,
            RuntimeConfig::default(),
            None,
        )
        .unwrap_err();

        assert_eq!(error.diagnostic.kind, StartupDiagnosticKind::Schema);
        assert_eq!(error.diagnostic.source.path, route_path);
        assert!(error.to_string().contains("get-user.yaml"));
    }

    #[test]
    fn startup_collection_parse_failures_name_the_source_path() {
        let temp = tempfile::tempdir().unwrap();
        let route_path = temp.path().join("routes/get-user.yaml");
        let collections_path = temp.path().join("collections.yaml");
        write(&route_path, &route_yaml("get-user"));
        write(&collections_path, "- id: [unterminated\n");

        let error = load_catalog_from_files(
            temp.path().join("routes"),
            &collections_path,
            RuntimeConfig::default(),
            None,
        )
        .unwrap_err();

        assert_eq!(error.diagnostic.kind, StartupDiagnosticKind::Parse);
        assert_eq!(error.diagnostic.source.path, collections_path);
        assert!(error.diagnostic.source.line.is_some());
        assert!(error.to_string().contains("collections.yaml"));
    }

    #[test]
    fn startup_collection_schema_failures_name_the_source_path() {
        let temp = tempfile::tempdir().unwrap();
        let route_path = temp.path().join("routes/get-user.yaml");
        let collections_path = temp.path().join("collections.yaml");
        write(&route_path, &route_yaml("get-user"));
        write(&collections_path, &collections_yaml("get-user:user-123"));

        let error = load_catalog_from_files(
            temp.path().join("routes"),
            &collections_path,
            RuntimeConfig::default(),
            None,
        )
        .unwrap_err();

        assert_eq!(error.diagnostic.kind, StartupDiagnosticKind::Schema);
        assert_eq!(error.diagnostic.source.path, collections_path);
        assert!(error.to_string().contains("collections.yaml"));
        assert!(error.to_string().contains("route:case:behavior"));
    }

    #[test]
    fn startup_collection_decode_failures_are_schema_diagnostics() {
        let temp = tempfile::tempdir().unwrap();
        let route_path = temp.path().join("routes/get-user.yaml");
        let collections_path = temp.path().join("collections.yaml");
        write(&route_path, &route_yaml("get-user"));
        write(
            &collections_path,
            r#"
- id: [bad]
  routes:
    - get-user:user-123:success
"#,
        );

        let error = load_catalog_from_files(
            temp.path().join("routes"),
            &collections_path,
            RuntimeConfig::default(),
            None,
        )
        .unwrap_err();

        assert_eq!(error.diagnostic.kind, StartupDiagnosticKind::Schema);
        assert_eq!(error.diagnostic.source.path, collections_path);
        assert!(error.to_string().contains("collections.yaml"));
    }

    #[test]
    fn startup_cross_reference_failures_name_the_collections_source_path() {
        let temp = tempfile::tempdir().unwrap();
        let route_path = temp.path().join("routes/get-user.yaml");
        let collections_path = temp.path().join("collections.yaml");
        write(&route_path, &route_yaml("get-user"));
        write(
            &collections_path,
            &collections_yaml("get-user:user-123:missing"),
        );

        let error = load_catalog_from_files(
            temp.path().join("routes"),
            &collections_path,
            RuntimeConfig::default(),
            None,
        )
        .unwrap_err();

        assert_eq!(error.diagnostic.kind, StartupDiagnosticKind::CrossReference);
        assert_eq!(error.diagnostic.source.path, collections_path);
        assert!(error.to_string().contains("get-user:user-123:missing"));
        assert!(error.to_string().contains("unknown behavior"));
    }

    #[test]
    fn invalid_route_files_fail_startup_instead_of_being_skipped() {
        let temp = tempfile::tempdir().unwrap();
        let routes_dir = temp.path().join("routes");
        let good_path = routes_dir.join("a-good.yaml");
        let bad_path = routes_dir.join("z-bad.yaml");
        let collections_path = temp.path().join("collections.yaml");
        write(&good_path, &route_yaml("get-user"));
        write(&bad_path, "id: [unterminated\n");
        write(
            &collections_path,
            &collections_yaml("get-user:user-123:success"),
        );

        let error = load_catalog_from_files(
            &routes_dir,
            &collections_path,
            RuntimeConfig::default(),
            None,
        )
        .unwrap_err();

        assert_eq!(error.diagnostic.source.path, bad_path);
    }

    #[test]
    fn startup_loader_keeps_happy_path_catalog_behavior_unchanged() {
        let temp = tempfile::tempdir().unwrap();
        let routes_dir = temp.path().join("routes");
        let route_path = routes_dir.join("get-user.yaml");
        let collections_path = temp.path().join("collections.yaml");
        write(&route_path, &route_yaml("get-user"));
        write(
            &collections_path,
            &collections_yaml("get-user:user-123:success"),
        );

        let startup = load_catalog_from_files(
            &routes_dir,
            &collections_path,
            RuntimeConfig {
                passthrough: Some(PassthroughTarget {
                    base_url: "https://example.test".to_owned(),
                }),
                ..RuntimeConfig::default()
            },
            None,
        )
        .unwrap();

        assert_eq!(startup.default_collection, "local");
    }

    #[cfg(unix)]
    #[test]
    fn startup_follows_symlinked_route_files_and_survives_a_folder_loop() {
        let temp = tempfile::tempdir().unwrap();
        let routes_dir = temp.path().join("routes");
        let collections_path = temp.path().join("collections.yaml");
        write(
            &temp.path().join("shared/get-user.yaml"),
            &route_yaml("get-user"),
        );
        fs::create_dir(&routes_dir).unwrap();
        std::os::unix::fs::symlink("../shared/get-user.yaml", routes_dir.join("get-user.yaml"))
            .unwrap();
        std::os::unix::fs::symlink(".", routes_dir.join("loop")).unwrap();
        write(
            &collections_path,
            &collections_yaml("get-user:user-123:success"),
        );

        let startup = load_catalog_from_files(
            &routes_dir,
            &collections_path,
            RuntimeConfig::default(),
            None,
        )
        .unwrap();

        assert_eq!(startup.default_collection, "local");
    }

    #[test]
    fn startup_unknown_keys_name_the_file_and_line() {
        let temp = tempfile::tempdir().unwrap();
        let route_path = temp.path().join("routes/get-user.yaml");
        let collections_path = temp.path().join("collections.yaml");
        write(
            &route_path,
            &route_yaml("get-user").replace("    match:", "    matches:"),
        );
        write(
            &collections_path,
            &collections_yaml("get-user:user-123:success"),
        );

        let error = load_catalog_from_files(
            temp.path().join("routes"),
            &collections_path,
            RuntimeConfig::default(),
            None,
        )
        .unwrap_err();

        assert_eq!(error.diagnostic.kind, StartupDiagnosticKind::Schema);
        assert_eq!(error.diagnostic.source.path, route_path);
        assert_eq!(error.diagnostic.source.line, Some(9));
        assert!(
            error.diagnostic.message.contains("unknown field `matches`"),
            "{}",
            error.diagnostic.message
        );
    }
}

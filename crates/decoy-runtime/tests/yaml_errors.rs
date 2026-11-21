use decoy_runtime::collections::CollectionError;
use decoy_runtime::config::{ConfigError, ServeCliOptions, ServeConfig};
use decoy_runtime::{CollectionsFile, Route, ValidationError};

#[test]
fn ordinary_yaml_errors_keep_the_public_parser_type_and_location() {
    fn line(error: serde_norway::Error) -> usize {
        error.location().unwrap().line()
    }

    let ValidationError::Yaml(route) = Route::from_yaml("id: r\nbogus: 1").unwrap_err() else {
        panic!("expected a Route YAML error")
    };
    assert_eq!(line(route), 2);
    let CollectionError::Yaml(collection) =
        CollectionsFile::from_yaml("- id: c\n  bogus: 1").unwrap_err()
    else {
        panic!("expected a Collection YAML error")
    };
    assert_eq!(line(collection), 2);

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("decoy.yaml");
    std::fs::write(&path, "routes: routes\nbogus: 1").unwrap();
    let options = ServeCliOptions {
        config_file: Some(path),
        ..Default::default()
    };
    let ConfigError::ParseConfig { source, .. } =
        ServeConfig::from_options(options, dir.path()).unwrap_err()
    else {
        panic!("expected a config YAML error")
    };
    assert_eq!(line(source), 2);
}

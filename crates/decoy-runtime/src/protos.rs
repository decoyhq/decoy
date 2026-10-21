//! Proto loading: compile configured proto roots and descriptor sets into one
//! `DescriptorPool` at startup, so gRPC Routes can resolve message types.

use std::fs;
use std::path::{Component, Path, PathBuf};

use globset::{GlobBuilder, GlobMatcher};
use prost_reflect::DescriptorPool;
use walkdir::WalkDir;

use crate::config::ProtoSources;
use crate::startup::{StartupDiagnosticKind, StartupError};

/// Compiles every `.proto` under the roots (roots double as import paths), or only those that
/// `files` patterns pick, then adds each descriptor set. Empty sources yield an empty pool.
pub fn load_descriptor_pool(sources: &ProtoSources) -> Result<DescriptorPool, StartupError> {
    for root in &sources.roots {
        if !root.is_dir() {
            return Err(StartupError::new(
                StartupDiagnosticKind::Io,
                root,
                "proto root is not a directory",
            ));
        }
    }
    let files = if sources.files.is_empty() {
        discover_proto_files(&sources.roots)?
    } else {
        find_entry_files(&sources.roots, &sources.files)?
    };
    let mut pool = if files.is_empty() {
        DescriptorPool::new()
    } else {
        let mut compiler =
            protox::Compiler::new(&sources.roots).expect("protox include resolver never fails");
        compiler
            .include_imports(true)
            .open_files(&files)
            .map_err(|error| proto_error(&sources.roots, error))?;
        compiler.descriptor_pool()
    };

    for path in &sources.descriptor_sets {
        let bytes = fs::read(path).map_err(|error| {
            StartupError::new(StartupDiagnosticKind::Io, path, error.to_string())
        })?;
        pool.decode_file_descriptor_set(bytes.as_slice())
            .map_err(|error| {
                StartupError::new(StartupDiagnosticKind::Parse, path, error.to_string())
            })?;
    }

    Ok(pool)
}

fn discover_proto_files(roots: &[PathBuf]) -> Result<Vec<PathBuf>, StartupError> {
    let mut files = Vec::new();
    for root in roots {
        // A root nested in another root is walked on its own, so each file gets one name.
        let walk = WalkDir::new(root)
            .sort_by_file_name()
            .into_iter()
            .filter_entry(|entry| entry.path() == root || !roots.iter().any(|r| r == entry.path()));
        for entry in walk {
            let entry = entry.map_err(|error| {
                StartupError::new(StartupDiagnosticKind::Io, root, error.to_string())
            })?;
            if is_proto(&entry) {
                // Open by import name so protox resolves it like an `import`, not by first root.
                let name = entry.path().strip_prefix(root).unwrap_or(entry.path());
                files.push(name.to_path_buf());
            }
        }
    }
    Ok(files)
}

/// Import names of the files the patterns match. Each pattern walks only the folder before its
/// first wildcard, so a large root costs nothing beyond the picked subtree. A file is named by
/// the innermost root holding it, as [`discover_proto_files`] names it. A pattern that matches
/// nothing fails, so a typo doesn't quietly drop its services.
fn find_entry_files(roots: &[PathBuf], patterns: &[PathBuf]) -> Result<Vec<PathBuf>, StartupError> {
    let roots: Vec<PathBuf> = roots.iter().map(|root| normalize(root)).collect();
    let mut files = Vec::new();
    for pattern in patterns {
        let matcher = entry_matcher(pattern)?;
        let mut matched = false;
        for entry in WalkDir::new(literal_base(&normalize(pattern))).sort_by_file_name() {
            let Ok(entry) = entry else { continue };
            if !is_proto(&entry) || !matcher.is_match(entry.path()) {
                continue;
            }
            matched = true;
            let name = roots
                .iter()
                .filter_map(|root| entry.path().strip_prefix(root).ok())
                .min_by_key(|name| name.components().count())
                .ok_or_else(|| {
                    StartupError::new(
                        StartupDiagnosticKind::Io,
                        entry.path(),
                        "proto file is not under any proto root",
                    )
                })?;
            if !files.iter().any(|file| file == name) {
                files.push(name.to_path_buf());
            }
        }
        if !matched {
            return Err(StartupError::new(
                StartupDiagnosticKind::Io,
                pattern,
                "proto files pattern matches no .proto file",
            ));
        }
    }
    Ok(files)
}

/// What `--watch` follows when `files` is set: the folders the patterns walk, so a new entry file
/// counts, and the picked files with everything they import. `None` when `files` is unset or the
/// protos don't compile right now; the whole roots are watched then, so fixing an import reloads.
pub fn watched_entry_paths(sources: &ProtoSources) -> Option<(Vec<PathBuf>, Vec<PathBuf>)> {
    if sources.files.is_empty() {
        return None;
    }
    let entries = find_entry_files(&sources.roots, &sources.files).ok()?;
    let mut compiler = protox::Compiler::new(&sources.roots).ok()?;
    compiler.include_imports(true).open_files(&entries).ok()?;
    let folders = sources
        .files
        .iter()
        .map(|pattern| literal_base(normalize(pattern).parent().unwrap_or(pattern)))
        .collect();
    // Well-known types ship inside protox and have no path.
    let files = compiler
        .files()
        .filter_map(|file| file.path().map(Path::to_path_buf))
        .collect();
    Some((folders, files))
}

fn is_proto(entry: &walkdir::DirEntry) -> bool {
    entry.file_type().is_file() && entry.path().extension().is_some_and(|ext| ext == "proto")
}

/// The leading folders of a pattern that hold no wildcard.
fn literal_base(pattern: &Path) -> PathBuf {
    pattern
        .components()
        .take_while(|component| {
            !component
                .as_os_str()
                .to_string_lossy()
                .contains(['*', '?', '[', '{'])
        })
        .collect()
}

/// `*` stays within one folder and `**` crosses folders, as in shell globs.
fn entry_matcher(pattern: &Path) -> Result<GlobMatcher, StartupError> {
    let text = normalize(pattern).to_string_lossy().replace('\\', "/");
    GlobBuilder::new(&text)
        .literal_separator(true)
        .build()
        .map(|glob| glob.compile_matcher())
        .map_err(|error| {
            StartupError::new(StartupDiagnosticKind::Parse, pattern, error.to_string())
        })
}

/// Drops `.` and folds `..` without touching the disk, so `../proto/**` in the config matches
/// files found under a root written as `../proto`.
fn normalize(path: &Path) -> PathBuf {
    let mut normal = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                normal.pop();
            }
            component => normal.push(component),
        }
    }
    normal
}

/// Points the diagnostic at the failing file when protox names one.
fn proto_error(roots: &[PathBuf], error: protox::Error) -> StartupError {
    let path = error
        .file()
        .and_then(|name| resolve_proto_name(roots, name))
        .unwrap_or_else(|| roots[0].clone());
    StartupError::new(StartupDiagnosticKind::Parse, path, error.to_string())
}

fn resolve_proto_name(roots: &[PathBuf], name: &str) -> Option<PathBuf> {
    let name = Path::new(name);
    if name.is_absolute() {
        return Some(name.to_path_buf());
    }
    roots
        .iter()
        .map(|root| root.join(name))
        .find(|path| path.is_file())
}

#[cfg(test)]
mod tests {
    use tempfile::tempdir;

    use super::*;

    fn fixture_root() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/proto")
    }

    #[test]
    fn compiles_every_proto_under_the_roots() {
        let pool = load_descriptor_pool(&ProtoSources {
            roots: vec![fixture_root()],
            descriptor_sets: vec![],
            files: vec![],
        })
        .unwrap();

        let service = pool.get_service_by_name("approvals.v1.Approvals").unwrap();
        assert!(
            service
                .methods()
                .any(|method| method.name() == "GetApproval")
        );
        assert!(pool.get_message_by_name("approvals.v1.Approval").is_some());
    }

    fn write_versioned_layout(proto: &Path, import: impl Fn(&str, &str) -> String) {
        let files = [
            ("v1/keyboards", "keyboards", "keyboards.v1", "Keyboard"),
            ("v1/monitors", "monitors", "monitors.v1", "Monitor"),
            ("v2/my-entity", "my-entity", "my_entity.v2", "MyEntity"),
        ];
        fs::write(
            proto.join("common.proto"),
            "syntax = \"proto3\";\npackage common;\nmessage Money { int64 units = 1; }\n",
        )
        .unwrap();
        for (dir, file, package, message) in files {
            fs::create_dir_all(proto.join(dir)).unwrap();
            fs::write(
                proto.join(dir).join(format!("{file}.proto")),
                format!(
                    "syntax = \"proto3\";\npackage {package};\nimport \"common.proto\";\n\
                     message {message} {{ string id = 1; common.Money price = 2; }}\n"
                ),
            )
            .unwrap();
            fs::write(
                proto.join(dir).join(format!("{file}_service.proto")),
                format!(
                    "syntax = \"proto3\";\npackage {package};\nimport \"{}\";\n\
                     message Get{message}Request {{ string id = 1; }}\n\
                     service {message}Service {{ rpc Get{message}(Get{message}Request) returns ({message}); }}\n",
                    import(dir, file)
                ),
            )
            .unwrap();
        }
    }

    const VERSIONED_SERVICES: [&str; 3] = [
        "keyboards.v1.KeyboardService",
        "monitors.v1.MonitorService",
        "my_entity.v2.MyEntityService",
    ];

    #[test]
    fn one_root_resolves_imports_written_from_it() {
        let dir = tempdir().unwrap();
        write_versioned_layout(dir.path(), |dir, file| format!("{dir}/{file}.proto"));

        let pool = load_descriptor_pool(&ProtoSources {
            roots: vec![dir.path().to_path_buf()],
            descriptor_sets: vec![],
            files: vec![],
        })
        .unwrap();

        for service in VERSIONED_SERVICES {
            assert!(pool.get_service_by_name(service).is_some(), "{service}");
        }
    }

    #[test]
    fn root_per_folder_resolves_bare_imports() {
        let dir = tempdir().unwrap();
        write_versioned_layout(dir.path(), |_, file| format!("{file}.proto"));

        let mut roots = vec![dir.path().to_path_buf()];
        roots.extend(
            ["v1/keyboards", "v1/monitors", "v2/my-entity"].map(|folder| dir.path().join(folder)),
        );
        let pool = load_descriptor_pool(&ProtoSources {
            roots,
            descriptor_sets: vec![],
            files: vec![],
        })
        .unwrap();

        for service in VERSIONED_SERVICES {
            assert!(pool.get_service_by_name(service).is_some(), "{service}");
        }
    }

    #[test]
    fn nested_roots_load_each_file_once_in_any_order() {
        let dir = tempdir().unwrap();
        let proto = dir.path().join("proto");
        let vendored = proto.join("third_party");
        fs::create_dir_all(vendored.join("ext")).unwrap();
        fs::write(
            vendored.join("ext/common.proto"),
            "syntax = \"proto3\";\npackage ext;\nmessage Money { int64 units = 1; }\n",
        )
        .unwrap();
        fs::write(
            proto.join("api.proto"),
            "syntax = \"proto3\";\nimport \"ext/common.proto\";\nmessage Price { ext.Money amount = 1; }\n",
        )
        .unwrap();

        for roots in [
            vec![proto.clone(), vendored.clone()],
            vec![vendored.clone(), proto.clone()],
        ] {
            let pool = load_descriptor_pool(&ProtoSources {
                roots,
                descriptor_sets: vec![],
                files: vec![],
            })
            .unwrap();
            assert!(pool.get_message_by_name("Price").is_some());
        }
    }

    /// One service, the shared type it imports, and an unrelated proto that doesn't compile.
    fn write_monorepo_layout(proto: &Path) {
        for (file, text) in [
            (
                "shared/v1/money.proto",
                "syntax = \"proto3\";\npackage shared.v1;\nmessage Money { int64 units = 1; }\n",
            ),
            (
                "services/catalog/v1/catalog_service.proto",
                "syntax = \"proto3\";\npackage catalog.v1;\nimport \"shared/v1/money.proto\";\n\
                 message GetPriceRequest { string id = 1; }\n\
                 service CatalogService { rpc GetPrice(GetPriceRequest) returns (shared.v1.Money); }\n",
            ),
            (
                "legacy/broken.proto",
                "syntax = \"proto3\";\nmessage Broken { string id = 1 }\n",
            ),
        ] {
            let path = proto.join(file);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, text).unwrap();
        }
    }

    #[test]
    fn files_pick_entry_files_and_imports_resolve_through_roots() {
        let dir = tempdir().unwrap();
        let proto = dir.path().join("proto");
        write_monorepo_layout(&proto);
        fs::create_dir_all(dir.path().join("config")).unwrap();

        let pool = load_descriptor_pool(&ProtoSources {
            roots: vec![dir.path().join("config/../proto")],
            descriptor_sets: vec![],
            files: vec![
                dir.path()
                    .join("config/../proto/services/catalog/**/*.proto"),
            ],
        })
        .unwrap();

        assert!(
            pool.get_service_by_name("catalog.v1.CatalogService")
                .is_some()
        );
        assert!(pool.get_message_by_name("shared.v1.Money").is_some());
        assert!(pool.get_message_by_name("Broken").is_none());
    }

    #[test]
    fn files_star_stays_within_one_folder() {
        let dir = tempdir().unwrap();
        write_monorepo_layout(dir.path());

        let error = load_descriptor_pool(&ProtoSources {
            roots: vec![dir.path().to_path_buf()],
            descriptor_sets: vec![],
            files: vec![dir.path().join("services/*.proto")],
        })
        .unwrap_err();

        assert_eq!(
            error.diagnostic.source.path,
            dir.path().join("services/*.proto")
        );
        assert!(error.to_string().contains("matches no .proto file"));
    }

    #[test]
    fn files_pattern_outside_the_roots_fails() {
        let dir = tempdir().unwrap();
        write_monorepo_layout(&dir.path().join("proto"));
        fs::create_dir_all(dir.path().join("elsewhere")).unwrap();
        fs::write(
            dir.path().join("elsewhere/api.proto"),
            "syntax = \"proto3\";\nmessage Api {}\n",
        )
        .unwrap();

        let error = load_descriptor_pool(&ProtoSources {
            roots: vec![dir.path().join("proto/services")],
            descriptor_sets: vec![],
            files: vec![dir.path().join("elsewhere/*.proto")],
        })
        .unwrap_err();

        assert_eq!(
            error.diagnostic.source.path,
            dir.path().join("elsewhere/api.proto")
        );
        assert!(error.to_string().contains("not under any proto root"));
    }

    #[test]
    fn invalid_files_pattern_fails() {
        let dir = tempdir().unwrap();
        write_monorepo_layout(dir.path());

        let error = load_descriptor_pool(&ProtoSources {
            roots: vec![dir.path().to_path_buf()],
            descriptor_sets: vec![],
            files: vec![dir.path().join("services/[.proto")],
        })
        .unwrap_err();

        assert_eq!(error.diagnostic.kind, StartupDiagnosticKind::Parse);
        assert_eq!(
            error.diagnostic.source.path,
            dir.path().join("services/[.proto")
        );
    }

    #[test]
    fn empty_sources_yield_an_empty_pool() {
        let pool = load_descriptor_pool(&ProtoSources::default()).unwrap();
        assert_eq!(pool.services().count(), 0);
    }

    #[test]
    fn accepts_descriptor_sets() {
        let dir = tempdir().unwrap();
        let set = dir.path().join("api.binpb");
        let mut compiler = protox::Compiler::new([fixture_root()]).unwrap();
        compiler
            .include_imports(true)
            .open_file("approvals/v1/approvals.proto")
            .unwrap();
        fs::write(&set, compiler.encode_file_descriptor_set()).unwrap();

        let pool = load_descriptor_pool(&ProtoSources {
            roots: vec![],
            descriptor_sets: vec![set],
            files: vec![],
        })
        .unwrap();

        assert!(pool.get_service_by_name("approvals.v1.Approvals").is_some());
    }

    #[test]
    fn broken_proto_names_the_file() {
        let dir = tempdir().unwrap();
        fs::create_dir_all(dir.path().join("bad")).unwrap();
        fs::write(
            dir.path().join("bad/broken.proto"),
            "syntax = \"proto3\";\nmessage Broken { string id = 1 }\n",
        )
        .unwrap();

        let error = load_descriptor_pool(&ProtoSources {
            roots: vec![dir.path().to_path_buf()],
            descriptor_sets: vec![],
            files: vec![],
        })
        .unwrap_err();

        assert_eq!(error.diagnostic.kind, StartupDiagnosticKind::Parse);
        assert_eq!(
            error.diagnostic.source.path,
            dir.path().join("bad/broken.proto")
        );
    }

    #[test]
    fn missing_root_fails() {
        let dir = tempdir().unwrap();
        let missing = dir.path().join("nope");

        let error = load_descriptor_pool(&ProtoSources {
            roots: vec![missing.clone()],
            descriptor_sets: vec![],
            files: vec![],
        })
        .unwrap_err();

        assert_eq!(error.diagnostic.source.path, missing);
        assert!(error.to_string().contains("proto root is not a directory"));
    }

    #[test]
    fn unreadable_descriptor_set_fails() {
        let dir = tempdir().unwrap();
        let set = dir.path().join("api.binpb");
        fs::write(&set, b"not a descriptor set").unwrap();

        let error = load_descriptor_pool(&ProtoSources {
            roots: vec![],
            descriptor_sets: vec![set.clone()],
            files: vec![],
        })
        .unwrap_err();

        assert_eq!(error.diagnostic.kind, StartupDiagnosticKind::Parse);
        assert_eq!(error.diagnostic.source.path, set);
    }
}

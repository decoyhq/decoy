//! Proto loading: compile configured proto roots and descriptor sets into one
//! `DescriptorPool` at startup, so gRPC Routes can resolve message types.

use std::fs;
use std::path::{Path, PathBuf};

use prost_reflect::DescriptorPool;
use walkdir::WalkDir;

use crate::config::ProtoSources;
use crate::startup::{StartupDiagnosticKind, StartupError};

/// Compiles every `.proto` under the roots (roots double as import paths), then
/// adds each descriptor set. Empty sources yield an empty pool.
pub fn load_descriptor_pool(sources: &ProtoSources) -> Result<DescriptorPool, StartupError> {
    let files = discover_proto_files(&sources.roots)?;
    let mut pool = if files.is_empty() {
        DescriptorPool::new()
    } else {
        let mut compiler = protox::Compiler::new(&sources.roots).map_err(|error| {
            StartupError::new(
                StartupDiagnosticKind::Io,
                &sources.roots[0],
                error.to_string(),
            )
        })?;
        compiler
            .include_imports(true)
            .open_files(&files)
            .map_err(|error| proto_error(&sources.roots, &files, error))?;
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
        if !root.is_dir() {
            return Err(StartupError::new(
                StartupDiagnosticKind::Io,
                root,
                "proto root is not a directory",
            ));
        }
        for entry in WalkDir::new(root).sort_by_file_name() {
            let entry = entry.map_err(|error| {
                StartupError::new(StartupDiagnosticKind::Io, root, error.to_string())
            })?;
            if entry.file_type().is_file()
                && entry.path().extension().is_some_and(|ext| ext == "proto")
            {
                files.push(entry.into_path());
            }
        }
    }
    Ok(files)
}

/// Points the diagnostic at the failing file when protox names one.
fn proto_error(roots: &[PathBuf], files: &[PathBuf], error: protox::Error) -> StartupError {
    let path = error
        .file()
        .and_then(|name| resolve_proto_name(roots, files, name))
        .unwrap_or_else(|| roots[0].clone());
    StartupError::new(StartupDiagnosticKind::Parse, path, error.to_string())
}

fn resolve_proto_name(roots: &[PathBuf], files: &[PathBuf], name: &str) -> Option<PathBuf> {
    let name = Path::new(name);
    if name.is_absolute() {
        return Some(name.to_path_buf());
    }
    roots
        .iter()
        .map(|root| root.join(name))
        .find(|path| files.contains(path) || path.is_file())
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
        })
        .unwrap_err();

        assert_eq!(error.diagnostic.kind, StartupDiagnosticKind::Parse);
        assert_eq!(error.diagnostic.source.path, set);
    }
}

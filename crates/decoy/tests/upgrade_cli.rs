//! Exercise `decoy upgrade` without downloading or replacing binaries.
#![cfg(unix)]

use std::fs;
use std::os::unix::fs::{PermissionsExt, symlink};
use std::path::Path;
use std::process::{Command, Output};

fn upgrade(latest: &str, download: &str) -> Output {
    let bin = tempfile::tempdir().unwrap();
    symlink("/bin/sh", bin.path().join("sh")).unwrap();
    let curl = bin.path().join("curl");
    fs::write(
        &curl,
        r#"#!/bin/sh
url=
write_redirect=no
for arg do
    if [ "$url" = -w ] && [ "$arg" = '%{redirect_url}' ]; then
        write_redirect=yes
    fi
    url=$arg
done
case "$url" in
    */releases/latest)
        if [ "$write_redirect" = yes ]; then
            printf '%s/v%s' 'https://github.com/decoyhq/decoy/releases/tag' "$TEST_LATEST"
        fi ;;
    */install.sh)
        [ "$TEST_DOWNLOAD" = ok ] || exit 22
        printf '%s\n' 'printf "%s\n%s\n" "$DECOY_VERSION" "$DECOY_INSTALL_DIR"' ;;
    *) exit 2 ;;
esac
"#,
    )
    .unwrap();
    fs::set_permissions(curl, fs::Permissions::from_mode(0o700)).unwrap();
    // Only the fixture's curl and sh are on PATH; the real installer cannot run.
    Command::new(env!("CARGO_BIN_EXE_decoy"))
        .arg("upgrade")
        .env("PATH", bin.path())
        .env("TEST_LATEST", latest)
        .env("TEST_DOWNLOAD", download)
        .output()
        .unwrap()
}

#[test]
fn a_newer_release_installs_the_version_that_was_compared() {
    let output = upgrade("999.0.0", "ok");
    assert!(output.status.success(), "{output:?}");
    let exe = Path::new(env!("CARGO_BIN_EXE_decoy"))
        .canonicalize()
        .unwrap();
    let expected = format!("999.0.0\n{}\n", exe.parent().unwrap().display());
    assert_eq!(String::from_utf8(output.stdout).unwrap(), expected);
}

#[test]
fn the_current_release_does_not_download_an_installer() {
    let output = upgrade(env!("CARGO_PKG_VERSION"), "fail");
    assert!(output.status.success(), "{output:?}");
    assert!(
        String::from_utf8(output.stdout)
            .unwrap()
            .contains("up to date")
    );
}

#[test]
fn a_failed_download_is_not_a_successful_upgrade() {
    let output = upgrade("999.0.0", "fail");
    assert!(!output.status.success(), "{output:?}");
    assert!(
        String::from_utf8(output.stderr)
            .unwrap()
            .contains("upgrade failed")
    );
}

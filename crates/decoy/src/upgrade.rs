//! `decoy upgrade`: replace the running binary by re-running the install script into its
//! directory, so script installs and upgrades share one download/verify path.

use semver::Version;
use std::path::Path;
use std::process::Command;

// Redirects to the Latest release's tag page.
const LATEST: &str = "https://github.com/decoyhq/decoy/releases/latest";

/// Why `exe` must not upgrade itself, if it is owned by a package manager.
pub fn refusal(exe: &Path) -> Option<&'static str> {
    exe.components()
        .any(|part| part.as_os_str() == "node_modules")
        .then_some("decoy was installed with npm; upgrade with `npm i -D @decoyhq/cli@latest`")
}

/// The Latest release's version, from the tag `LATEST` redirects to.
fn latest_version() -> Result<Version, Box<dyn std::error::Error>> {
    let null = if cfg!(windows) { "NUL" } else { "/dev/null" };
    let output = Command::new("curl")
        .args(["-fsS", "-o", null, "-w", "%{redirect_url}", LATEST])
        .output()?;
    if !output.status.success() {
        return Err("cannot resolve the Latest release".into());
    }
    tag_version(&String::from_utf8_lossy(&output.stdout))
        .ok_or_else(|| "cannot read the Latest release's version".into())
}

/// The version in a `…/releases/tag/v<version>` URL.
fn tag_version(location: &str) -> Option<Version> {
    let (_, tag) = location.trim().rsplit_once("/tag/")?;
    Version::parse(tag.trim_start_matches('v')).ok()
}

/// Whether moving from `current` to `latest` goes forward; a pre-release ahead of Latest stays.
fn is_upgrade(latest: &Version, current: &Version) -> bool {
    latest > current
}

/// Upgrade the running binary to the release marked Latest, if that is newer.
pub fn run() -> Result<(), Box<dyn std::error::Error>> {
    let exe = std::env::current_exe()?.canonicalize()?;
    if let Some(reason) = refusal(&exe) {
        return Err(reason.into());
    }
    let current = Version::parse(env!("CARGO_PKG_VERSION"))?;
    let latest = latest_version()?;
    if !is_upgrade(&latest, &current) {
        println!("decoy {current} is up to date (Latest is {latest})");
        return Ok(());
    }
    let dir = exe
        .parent()
        .ok_or("cannot locate the decoy binary directory")?;

    let mut command = installer();
    command.env("DECOY_INSTALL_DIR", dir);
    // Install the version just compared, even if Latest moves meanwhile.
    command.env("DECOY_VERSION", latest.to_string());

    if !command.status()?.success() {
        return Err("upgrade failed".into());
    }
    Ok(())
}

#[cfg(windows)]
fn installer() -> Command {
    let mut command = Command::new("powershell");
    command.args([
        "-NoProfile",
        "-ExecutionPolicy",
        "Bypass",
        "-Command",
        "irm https://github.com/decoyhq/decoy/releases/latest/download/install.ps1 | iex",
    ]);
    command
}

#[cfg(not(windows))]
fn installer() -> Command {
    const SCRIPT: &str = "https://github.com/decoyhq/decoy/releases/latest/download/install.sh";
    let mut command = Command::new("sh");
    // Not `curl | sh`: a failed download would pipe nothing into sh, which exits 0.
    let script = format!("s=\"$(curl -fsSL {SCRIPT})\" && printf '%s\\n' \"$s\" | sh");
    command.args(["-c", &script]);
    command
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn refuses_npm_installs() {
        let npm = Path::new("/p/node_modules/@decoyhq/cli-linux-x64/bin/decoy");
        assert!(refusal(npm).is_some());
        assert!(refusal(Path::new("/home/u/.decoy/bin/decoy")).is_none());
    }

    #[test]
    fn reads_the_version_from_the_latest_redirect() {
        let location = "https://github.com/decoyhq/decoy/releases/tag/v0.2.0";
        assert_eq!(tag_version(location), Some(Version::new(0, 2, 0)));
        assert_eq!(
            tag_version("https://github.com/decoyhq/decoy/releases"),
            None
        );
    }

    #[test]
    fn upgrades_only_to_a_newer_release() {
        let v = |s| Version::parse(s).unwrap();
        assert!(is_upgrade(&v("0.3.0"), &v("0.2.0")));
        assert!(is_upgrade(&v("0.3.0"), &v("0.3.0-beta.1")));
        assert!(!is_upgrade(&v("0.2.0"), &v("0.2.0")));
        // A pre-release ahead of Latest stays put.
        assert!(!is_upgrade(&v("0.2.0"), &v("0.3.0-beta.1")));
    }
}

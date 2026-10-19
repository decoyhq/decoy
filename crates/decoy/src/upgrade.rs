//! `decoy upgrade`: replace the running binary by re-running the install script into its
//! directory, so script installs and upgrades share one download/verify path.

use std::path::Path;
use std::process::Command;

/// The release copy of `script`: from that version's release when pinned, so an old version is
/// installed by the script it shipped with, else from the latest release.
fn install_script_url(script: &str, version: Option<&str>) -> String {
    let releases = "https://github.com/decoyhq/decoy/releases";
    match version {
        Some(version) => format!(
            "{releases}/download/v{}/{script}",
            version.trim_start_matches('v')
        ),
        None => format!("{releases}/latest/download/{script}"),
    }
}

/// Why `exe` must not upgrade itself, if it is owned by a package manager.
pub fn refusal(exe: &Path) -> Option<&'static str> {
    exe.components()
        .any(|part| part.as_os_str() == "node_modules")
        .then_some("decoy was installed with npm; upgrade with `npm i -D @decoyhq/cli@latest`")
}

/// Upgrade the running binary to `version` (`None` = newest release).
pub fn run(version: Option<&str>) -> Result<(), Box<dyn std::error::Error>> {
    let exe = std::env::current_exe()?.canonicalize()?;
    if let Some(reason) = refusal(&exe) {
        return Err(reason.into());
    }
    let dir = exe
        .parent()
        .ok_or("cannot locate the decoy binary directory")?;

    let mut command = if cfg!(windows) {
        let mut command = Command::new("powershell");
        command.args([
            "-NoProfile",
            "-ExecutionPolicy",
            "Bypass",
            "-Command",
            &format!("irm {} | iex", install_script_url("install.ps1", version)),
        ]);
        command
    } else {
        let mut command = Command::new("sh");
        // Not `curl | sh`: a failed download would pipe nothing into sh, which exits 0.
        let url = install_script_url("install.sh", version);
        let script = format!("s=\"$(curl -fsSL {url})\" && printf '%s\\n' \"$s\" | sh");
        command.args(["-c", &script]);
        command
    };
    command.env("DECOY_INSTALL_DIR", dir);
    if let Some(version) = version {
        command.env("DECOY_VERSION", version);
    }

    if !command.status()?.success() {
        return Err("upgrade failed".into());
    }
    Ok(())
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
    fn pinned_version_fetches_that_releases_install_script() {
        assert_eq!(
            install_script_url("install.sh", Some("0.1.0-alpha.6")),
            "https://github.com/decoyhq/decoy/releases/download/v0.1.0-alpha.6/install.sh"
        );
        assert_eq!(
            install_script_url("install.ps1", Some("v0.2.0")),
            "https://github.com/decoyhq/decoy/releases/download/v0.2.0/install.ps1"
        );
        assert_eq!(
            install_script_url("install.sh", None),
            "https://github.com/decoyhq/decoy/releases/latest/download/install.sh"
        );
    }
}

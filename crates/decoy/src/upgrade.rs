//! `decoy upgrade`: replace the running binary by re-running the install script into its
//! directory, so script installs and upgrades share one download/verify path.

use std::path::Path;
use std::process::Command;

// The Latest release's copies install that release's version, unless DECOY_VERSION overrides it.
const INSTALL_SH: &str = "https://github.com/decoyhq/decoy/releases/latest/download/install.sh";
const INSTALL_PS1: &str = "https://github.com/decoyhq/decoy/releases/latest/download/install.ps1";

/// Why `exe` must not upgrade itself, if it is owned by a package manager.
pub fn refusal(exe: &Path) -> Option<&'static str> {
    exe.components()
        .any(|part| part.as_os_str() == "node_modules")
        .then_some("decoy was installed with npm; upgrade with `npm i -D @decoyhq/cli@latest`")
}

/// Upgrade the running binary to the release marked Latest.
pub fn run() -> Result<(), Box<dyn std::error::Error>> {
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
            &format!("irm {INSTALL_PS1} | iex"),
        ]);
        command
    } else {
        let mut command = Command::new("sh");
        // Not `curl | sh`: a failed download would pipe nothing into sh, which exits 0.
        let script = format!("s=\"$(curl -fsSL {INSTALL_SH})\" && printf '%s\\n' \"$s\" | sh");
        command.args(["-c", &script]);
        command
    };
    command.env("DECOY_INSTALL_DIR", dir);
    // An inherited DECOY_VERSION would pin the script; upgrade always means Latest.
    command.env_remove("DECOY_VERSION");

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
}

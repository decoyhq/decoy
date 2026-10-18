//! Watching the config files for `--watch`: one signal per burst of changes, so a caller runs
//! the same reload as `/reload` once per save.

use std::path::{Path, PathBuf};
use std::time::Duration;

use notify::{EventKind, RecursiveMode};
use tokio::sync::mpsc;

use crate::config::ServeConfig;

/// Quiet time after the last change before a reload; editors write in several steps.
pub const DEBOUNCE: Duration = Duration::from_millis(300);
/// How often the polling fallback looks at the files.
pub const POLL_INTERVAL: Duration = Duration::from_millis(500);

/// Watches until dropped.
pub struct ConfigWatcher {
    _watcher: Box<dyn notify::Watcher + Send>,
    /// The polling fallback is on, forced or detected.
    pub polling: bool,
}

/// What `--watch` looks at: the config file, routes, collections and proto roots.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WatchedPaths {
    /// Files whose own changes count.
    files: Vec<PathBuf>,
    /// Directories whose whole tree counts.
    trees: Vec<PathBuf>,
}

impl WatchedPaths {
    pub fn of(config: &ServeConfig, config_file: Option<&Path>) -> Self {
        let mut files: Vec<PathBuf> = config_file.into_iter().map(canonical).collect();
        files.push(canonical(&config.collections));
        let mut trees = Vec::new();
        if config.routes.is_dir() {
            trees.push(canonical(&config.routes));
        } else {
            files.push(canonical(&config.routes));
        }
        trees.extend(config.protos.roots.iter().map(|root| canonical(root)));
        Self { files, trees }
    }

    fn matches(&self, path: &Path) -> bool {
        self.files.iter().any(|file| file == path)
            || self.trees.iter().any(|tree| path.starts_with(tree))
    }

    /// Each file's parent directory (editors replace files by renaming over them, which a watch
    /// on the file itself misses) and each tree.
    fn watches(&self) -> Vec<(PathBuf, RecursiveMode)> {
        let mut watches: Vec<(PathBuf, RecursiveMode)> = self
            .files
            .iter()
            .filter_map(|file| file.parent())
            .map(|dir| (dir.to_path_buf(), RecursiveMode::NonRecursive))
            .collect();
        watches.extend(
            self.trees
                .iter()
                .map(|tree| (tree.clone(), RecursiveMode::Recursive)),
        );
        watches.sort_by(|a, b| a.0.cmp(&b.0));
        watches.dedup_by(|a, b| a.0 == b.0 && a.1 == b.1);
        watches
    }
}

/// `path` with symlinks resolved, as file events report it; a missing file keeps its name under
/// its resolved parent.
fn canonical(path: &Path) -> PathBuf {
    path.canonicalize()
        .unwrap_or_else(|_| match (path.parent(), path.file_name()) {
            (Some(parent), Some(name)) => parent
                .canonicalize()
                .map_or_else(|_| path.to_path_buf(), |parent| parent.join(name)),
            _ => path.to_path_buf(),
        })
}

/// Starts watching `paths`; the receiver gets one `()` per burst of relevant changes, after
/// [`DEBOUNCE`]. `poll` forces the polling fallback, which is also used where native events
/// don't arrive (see [`needs_polling`]). Call within a Tokio runtime.
pub fn watch(
    paths: WatchedPaths,
    poll: bool,
) -> Result<(ConfigWatcher, mpsc::Receiver<()>), notify::Error> {
    let watches = paths.watches();
    let polling = poll || watches.iter().any(|(dir, _)| needs_polling(dir));
    let (raw_tx, mut raw_rx) = mpsc::unbounded_channel::<()>();
    let handler = move |event: notify::Result<notify::Event>| {
        let Ok(event) = event else { return };
        if matches!(event.kind, EventKind::Access(_)) {
            return;
        }
        if event.paths.iter().any(|path| paths.matches(path)) {
            let _ = raw_tx.send(());
        }
    };
    let mut watcher: Box<dyn notify::Watcher + Send> = if polling {
        Box::new(notify::PollWatcher::new(
            handler,
            // Polling compares mtimes in whole seconds; hashing the (small) config files also
            // catches a save in the same second as the previous look.
            notify::Config::default()
                .with_poll_interval(POLL_INTERVAL)
                .with_compare_contents(true),
        )?)
    } else {
        Box::new(notify::recommended_watcher(handler)?)
    };
    for (dir, mode) in &watches {
        watcher.watch(dir, *mode)?;
    }

    let (tx, rx) = mpsc::channel(1);
    tokio::spawn(async move {
        while raw_rx.recv().await.is_some() {
            // Wait until the files have been quiet for DEBOUNCE.
            while let Ok(Some(())) = tokio::time::timeout(DEBOUNCE, raw_rx.recv()).await {}
            // A reload still running already covers this change once it re-reads the files.
            if let Err(mpsc::error::TrySendError::Closed(())) = tx.try_send(()) {
                return;
            }
        }
    });
    Ok((
        ConfigWatcher {
            _watcher: watcher,
            polling,
        },
        rx,
    ))
}

/// True where native file events don't arrive: network filesystems, and Windows drives seen
/// from WSL2 (`/mnt/c`), which inotify can't watch.
pub fn needs_polling(path: &Path) -> bool {
    #[cfg(target_os = "linux")]
    {
        let path = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
        let mounts = std::fs::read_to_string("/proc/mounts").unwrap_or_default();
        let wsl = std::fs::read_to_string("/proc/sys/kernel/osrelease")
            .is_ok_and(|release| release.to_ascii_lowercase().contains("microsoft"));
        polled(&path, &mounts, wsl)
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = path;
        false
    }
}

/// [`needs_polling`] given the `/proc/mounts` text and whether this is WSL.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn polled(path: &Path, mounts: &str, wsl: bool) -> bool {
    const POLLED: &[&str] = &[
        "nfs",
        "nfs4",
        "cifs",
        "smb3",
        "smbfs",
        "9p",
        "drvfs",
        "fuse.sshfs",
    ];
    if wsl && path.starts_with("/mnt") && path.components().count() > 2 {
        return true;
    }
    // The mount with the longest mount point that contains `path` decides.
    mounts
        .lines()
        .filter_map(|line| {
            let mut fields = line.split_whitespace();
            let (_, point, kind) = (fields.next()?, fields.next()?, fields.next()?);
            // Spaces in mount points are octal-escaped.
            let point = point.replace("\\040", " ");
            path.starts_with(&point).then_some((point.len(), kind))
        })
        .max_by_key(|(len, _)| *len)
        .is_some_and(|(_, kind)| POLLED.contains(&kind))
}

#[cfg(test)]
mod tests {
    use super::*;

    const MOUNTS: &str = "/dev/sdc / ext4 rw 0 0\n\
        C:\\134 /mnt/c 9p rw,dirsync 0 0\n\
        server:/export /home/me/nfs nfs4 rw 0 0\n\
        /dev/sdd /home/me/nfs/local ext4 rw 0 0\n";

    #[test]
    fn network_and_windows_drives_poll_local_disks_do_not() {
        assert!(!polled(Path::new("/home/me/mocks"), MOUNTS, false));
        assert!(polled(Path::new("/mnt/c/Users/me/mocks"), MOUNTS, false));
        assert!(polled(Path::new("/home/me/nfs/mocks"), MOUNTS, false));
        assert!(!polled(
            Path::new("/home/me/nfs/local/mocks"),
            MOUNTS,
            false
        ));
    }

    #[test]
    fn wsl_polls_under_mnt_whatever_the_mount_table_says() {
        assert!(polled(
            Path::new("/mnt/d/mocks"),
            "/dev/sdc / ext4 rw 0 0\n",
            true
        ));
        assert!(!polled(
            Path::new("/mnt/d/mocks"),
            "/dev/sdc / ext4 rw 0 0\n",
            false
        ));
        assert!(!polled(
            Path::new("/home/me"),
            "/dev/sdc / ext4 rw 0 0\n",
            true
        ));
    }
}

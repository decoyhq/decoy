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

/// Reads the config again and says what to watch now; `None` keeps the current paths (e.g. the
/// config doesn't parse right now).
pub type ReadPaths = Box<dyn Fn() -> Option<WatchedPaths> + Send + Sync>;

/// What `--watch` looks at: the config file, routes, collections, proto roots and descriptor sets.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WatchedPaths {
    /// Files whose own changes count.
    files: Vec<PathBuf>,
    /// Directories whose files with these extensions count (and the directories themselves).
    trees: Vec<(PathBuf, &'static [&'static str])>,
}

impl WatchedPaths {
    pub fn of(config: &ServeConfig, config_file: Option<&Path>) -> Self {
        let mut files: Vec<PathBuf> = config_file.into_iter().map(canonical).collect();
        files.push(canonical(&config.collections));
        files.extend(
            config
                .protos
                .descriptor_sets
                .iter()
                .map(|set| canonical(set)),
        );
        let mut trees: Vec<(PathBuf, &[&str])> = Vec::new();
        if config.routes.is_dir() {
            trees.push((canonical(&config.routes), &["yaml", "yml"]));
        } else {
            files.push(canonical(&config.routes));
        }
        trees.extend(
            config
                .protos
                .roots
                .iter()
                .map(|root| (canonical(root), &["proto"][..])),
        );
        Self { files, trees }
    }

    /// Editor swap and backup files (`.users.yaml.swp`, `users.yaml~`) don't count, so editing a
    /// broken file doesn't print its error again on every autosave.
    fn matches(&self, path: &Path) -> bool {
        self.files.iter().any(|file| file == path)
            || self.trees.iter().any(|(tree, extensions)| {
                path.starts_with(tree)
                    && path.extension().is_none_or(|extension| {
                        extensions.iter().any(|wanted| extension == *wanted)
                    })
            })
    }

    /// Natively, each file's parent directory (editors save by renaming a temp file over the
    /// original, which a watch on the file misses) and each tree. Polling sees a renamed-over
    /// file anyway, and must not hash the rest of the config's directory, so it takes the file.
    fn watches(&self, polling: bool) -> Vec<(PathBuf, RecursiveMode)> {
        let mut watches: Vec<(PathBuf, RecursiveMode)> = self
            .files
            .iter()
            .filter_map(|file| match file.parent() {
                Some(_) if polling && file.exists() => Some(file.clone()),
                parent => parent.map(Path::to_path_buf),
            })
            .map(|path| (path, RecursiveMode::NonRecursive))
            .collect();
        watches.extend(
            self.trees
                .iter()
                .map(|(tree, _)| (tree.clone(), RecursiveMode::Recursive)),
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

/// Watches the config files until dropped. [`ConfigWatcher::changed`] resolves once per burst of
/// relevant changes, after [`DEBOUNCE`].
pub struct ConfigWatcher {
    watcher: Box<dyn notify::Watcher + Send>,
    paths: WatchedPaths,
    read_paths: ReadPaths,
    force_poll: bool,
    polling: bool,
    events: mpsc::UnboundedSender<()>,
    changes: mpsc::Receiver<()>,
}

impl ConfigWatcher {
    /// Starts watching `paths`. `poll` forces the polling fallback, which is also used where
    /// native events don't arrive (see [`needs_polling`]) or can't be set up. Call within a
    /// Tokio runtime.
    pub fn new(
        paths: WatchedPaths,
        read_paths: ReadPaths,
        poll: bool,
    ) -> Result<Self, notify::Error> {
        let (events, mut raw) = mpsc::unbounded_channel::<()>();
        let (tx, changes) = mpsc::channel(1);
        tokio::spawn(async move {
            while raw.recv().await.is_some() {
                // Wait until the files have been quiet for DEBOUNCE.
                while let Ok(Some(())) = tokio::time::timeout(DEBOUNCE, raw.recv()).await {}
                // A signal already waiting covers this change: its reload re-reads the files.
                if let Err(mpsc::error::TrySendError::Closed(())) = tx.try_send(()) {
                    return;
                }
            }
        });
        let (watcher, polling) = arm(&paths, poll, &events)?;
        Ok(Self {
            watcher,
            paths,
            read_paths,
            force_poll: poll,
            polling,
            events,
            changes,
        })
    }

    /// `watching the config files`, plus how.
    pub fn describe(&self) -> String {
        let polling = if self.polling { " (polling)" } else { "" };
        format!("watching the config files{polling}")
    }

    /// The next burst of changes. Before it returns, the watches are set up again from the
    /// config as it is now, so a recreated directory or a config that points elsewhere keeps
    /// being watched.
    pub async fn changed(&mut self) -> Option<()> {
        let changed = self.changes.recv().await;
        if let Some(paths) = (self.read_paths)() {
            self.paths = paths;
        }
        if let Ok((watcher, polling)) = arm(&self.paths, self.force_poll, &self.events) {
            self.watcher = watcher;
            self.polling = polling;
        }
        changed
    }
}

/// A watcher on `paths` that sends `()` to `events` per relevant file event; polling when forced,
/// needed, or when native watching fails (e.g. the inotify watch limit).
fn arm(
    paths: &WatchedPaths,
    force_poll: bool,
    events: &mpsc::UnboundedSender<()>,
) -> Result<(Box<dyn notify::Watcher + Send>, bool), notify::Error> {
    let handler = {
        let paths = paths.clone();
        let events = events.clone();
        move |event: notify::Result<notify::Event>| {
            let Ok(event) = event else { return };
            if matches!(event.kind, EventKind::Access(_)) {
                return;
            }
            if event.paths.iter().any(|path| paths.matches(path)) {
                let _ = events.send(());
            }
        }
    };
    let poll = force_poll
        || paths
            .watches(false)
            .iter()
            .any(|(path, _)| needs_polling(path));
    if !poll {
        let native = notify::recommended_watcher(handler.clone()).and_then(|mut watcher| {
            for (path, mode) in paths.watches(false) {
                notify::Watcher::watch(&mut watcher, &path, mode)?;
            }
            Ok(watcher)
        });
        if let Ok(watcher) = native {
            return Ok((Box::new(watcher), false));
        }
    }
    let mut watcher = notify::PollWatcher::new(
        handler,
        // Polling compares mtimes in whole seconds; hashing the (small) config files also
        // catches a save in the same second as the previous look.
        notify::Config::default()
            .with_poll_interval(POLL_INTERVAL)
            .with_compare_contents(true),
    )?;
    for (path, mode) in paths.watches(true) {
        notify::Watcher::watch(&mut watcher, &path, mode)?;
    }
    Ok((Box::new(watcher), true))
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

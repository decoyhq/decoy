//! Watching the config files for `--watch`: one signal per burst of changes, so a caller runs
//! the same reload as `/reload` once per save.

use std::path::{Path, PathBuf};
use std::time::Duration;

use notify::{EventKind, RecursiveMode};
use tokio::sync::mpsc;

use crate::config::ServeConfig;
use crate::protos;

/// Quiet time after the last change before a reload; editors write in several steps.
pub const DEBOUNCE: Duration = Duration::from_millis(300);
/// How often the polling fallback looks at the files.
pub const POLL_INTERVAL: Duration = Duration::from_millis(500);

/// Reads the config again and says what to watch now; `None` keeps the current paths (e.g. the
/// config doesn't parse right now).
pub type ReadPaths = Box<dyn Fn() -> Option<WatchedPaths> + Send + Sync>;

/// What `--watch` looks at: the config file, routes, collections, proto roots (or with `files`, the
/// picked protos and their imports) and descriptor sets.
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
        let proto_roots = match protos::watched_entry_paths(&config.protos) {
            Some((folders, proto_files)) => {
                files.extend(proto_files.iter().map(|file| canonical(file)));
                folders
            }
            None => config.protos.roots.clone(),
        };
        trees.extend(
            proto_roots
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
    /// A path that's missing right now is watched through its nearest existing ancestor, and
    /// comes back paired with that ancestor in the second list.
    #[allow(clippy::type_complexity)]
    fn watches(&self, polling: bool) -> (Vec<(PathBuf, RecursiveMode)>, Vec<(PathBuf, PathBuf)>) {
        let files = self.files.iter().filter_map(|file| match file.parent() {
            Some(_) if polling && file.exists() => {
                Some((file.clone(), RecursiveMode::NonRecursive))
            }
            parent => parent.map(|parent| (parent.to_path_buf(), RecursiveMode::NonRecursive)),
        });
        let trees = self
            .trees
            .iter()
            .map(|(tree, _)| (tree.clone(), RecursiveMode::Recursive));
        let mut missing = Vec::new();
        let mut watches: Vec<(PathBuf, RecursiveMode)> = files
            .chain(trees)
            .filter_map(|(path, mode)| {
                if path.exists() {
                    return Some((path, mode));
                }
                let ancestor = path
                    .ancestors()
                    .skip(1)
                    .find(|dir| dir.exists())?
                    .to_path_buf();
                missing.push((path, ancestor.clone()));
                Some((ancestor, RecursiveMode::NonRecursive))
            })
            .collect();
        // A path watched both ways keeps the recursive watch.
        watches.sort_by_key(|(path, mode)| (path.clone(), *mode == RecursiveMode::NonRecursive));
        watches.dedup_by(|a, b| a.0 == b.0);
        (watches, missing)
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
    _watchers: Vec<Box<dyn notify::Watcher + Send>>,
    paths: WatchedPaths,
    read_paths: ReadPaths,
    force_poll: bool,
    polling: bool,
    /// Some path was missing when the watches were set up, so they're set up again on every
    /// change until it's back.
    missing: bool,
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
        let (watchers, polling, missing) = arm(&paths, poll, &events)?;
        Ok(Self {
            _watchers: watchers,
            paths,
            read_paths,
            force_poll: poll,
            polling,
            missing,
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
        let paths = (self.read_paths)().unwrap_or_else(|| self.paths.clone());
        // A poller walks its roots again on every look, so it already sees a recreated
        // directory; starting a new one would hash every file on this thread for nothing.
        if self.polling && !self.missing && paths == self.paths {
            return changed;
        }
        self.paths = paths;
        if let Ok((watchers, polling, missing)) = arm(&self.paths, self.force_poll, &self.events) {
            self._watchers = watchers;
            self.polling = polling;
            self.missing = missing;
        }
        changed
    }
}

/// Watchers on `paths` that send `()` to `events` per relevant file event; polling when forced,
/// needed, or when native watching fails (e.g. the inotify watch limit). Also says whether it
/// polls and whether some path is missing.
#[allow(clippy::type_complexity)]
fn arm(
    paths: &WatchedPaths,
    force_poll: bool,
    events: &mpsc::UnboundedSender<()>,
) -> Result<(Vec<Box<dyn notify::Watcher + Send>>, bool, bool), notify::Error> {
    let handler = |missing: Vec<(PathBuf, PathBuf)>| {
        let paths = paths.clone();
        let events = events.clone();
        move |event: notify::Result<notify::Event>| {
            let Ok(event) = event else { return };
            if matches!(event.kind, EventKind::Access(_)) {
                return;
            }
            // Under the ancestor standing in for a missing path, a step back towards it counts.
            let towards_missing = |path: &Path| {
                missing
                    .iter()
                    .any(|(gone, ancestor)| gone.starts_with(path) && path != ancestor)
            };
            if event
                .paths
                .iter()
                .any(|path| paths.matches(path) || towards_missing(path))
            {
                let _ = events.send(());
            }
        }
    };
    let (native, missing) = paths.watches(false);
    let poll = force_poll || native.iter().any(|(path, _)| needs_polling(path));
    if !poll {
        let armed =
            notify::recommended_watcher(handler(missing.clone())).and_then(|mut watcher| {
                for (path, mode) in &native {
                    notify::Watcher::watch(&mut watcher, path, *mode)?;
                }
                Ok(watcher)
            });
        if let Ok(watcher) = armed {
            let watcher: Box<dyn notify::Watcher + Send> = Box::new(watcher);
            return Ok((vec![watcher], false, !missing.is_empty()));
        }
    }
    let (polled, missing) = paths.watches(true);
    let (files, dirs): (Vec<_>, Vec<_>) = polled.into_iter().partition(|(path, _)| path.is_file());
    let config = notify::Config::default().with_poll_interval(POLL_INTERVAL);
    // Polling compares mtimes in whole seconds; hashing the (small) config files also catches a
    // save in the same second as the previous look.
    // shortcut: trees compare mtimes only, so a second save of a route or proto file within the
    // same second is missed; hash them too if that shows up.
    let mut watchers: Vec<Box<dyn notify::Watcher + Send>> = Vec::new();
    for (watches, config) in [(files, config.with_compare_contents(true)), (dirs, config)] {
        let mut watcher = notify::PollWatcher::new(handler(missing.clone()), config)?;
        for (path, mode) in watches {
            notify::Watcher::watch(&mut watcher, &path, mode)?;
        }
        watchers.push(Box::new(watcher));
    }
    Ok((watchers, true, !missing.is_empty()))
}

/// True where native file events don't arrive: network filesystems, and Windows drives seen
/// from WSL (`/mnt/c`, a `9p` or `drvfs` mount), which inotify can't watch.
pub fn needs_polling(path: &Path) -> bool {
    #[cfg(target_os = "linux")]
    {
        let path = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
        let mounts = std::fs::read_to_string("/proc/mounts").unwrap_or_default();
        polled(&path, &mounts)
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = path;
        false
    }
}

/// [`needs_polling`] given the `/proc/mounts` text.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn polled(path: &Path, mounts: &str) -> bool {
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
        assert!(!polled(Path::new("/home/me/mocks"), MOUNTS));
        assert!(polled(Path::new("/mnt/c/Users/me/mocks"), MOUNTS));
        assert!(polled(Path::new("/home/me/nfs/mocks"), MOUNTS));
        assert!(!polled(Path::new("/home/me/nfs/local/mocks"), MOUNTS));
    }

    #[test]
    fn linux_disks_under_mnt_do_not_poll() {
        let wsl = "/dev/sdc / ext4 rw 0 0\n\
            C:\\134 /mnt/c 9p rw,aname=drvfs 0 0\n\
            /dev/sdd /mnt/wslg/distro ext4 ro 0 0\n";
        assert!(polled(Path::new("/mnt/c/Users/me/mocks"), wsl));
        assert!(!polled(Path::new("/mnt/wslg/distro/mocks"), wsl));
        assert!(!polled(Path::new("/mnt/data/mocks"), wsl));
    }
}

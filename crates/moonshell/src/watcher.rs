//! The inotify watcher behind hot reload and `shell.watch_file`.
//!
//! One inotify instance, inserted into calloop as a `Generic` source
//! (via a dup'd fd — see [`Watcher::loop_fd`]); [`Watcher::handle_events`]
//! drains it on readiness. Two kinds of interest share it:
//!
//! - **The config tree**: every directory under the config root is
//!   watched; a change to any `.lua` file reports "reload wanted" to
//!   the caller (who debounces and re-execs). New subdirectories are
//!   picked up from their `CREATE` events.
//! - **`shell.watch_file` targets**: the file's *parent directory* is
//!   watched (a direct file watch dies on the editor rename-replace
//!   dance) and events are matched by full path. Callbacks whose VM is
//!   gone remove themselves; hot reload clears the whole map
//!   ([`Watcher::clear_file_watches`]) before the old VM drops.
//!
//! Directory watches are never removed — a watch on a dir that no
//! longer interests anyone costs a few bytes in the kernel and
//! delivers events into an empty map. Bounded by the set of distinct
//! directories ever watched; revisit if a config tree churns dirs.

use std::collections::HashMap;
use std::ffi::OsString;
use std::io;
use std::os::fd::{AsRawFd as _, BorrowedFd, OwnedFd};
use std::path::{Path, PathBuf};

use inotify::{EventMask, Inotify, WatchDescriptor, WatchMask};
use moonshell_runtime::WatchCallback;

/// One mask for every directory watch — a dir can serve both the
/// config tree and `watch_file`, and inotify replaces (not merges)
/// masks on re-add.
fn dir_mask() -> WatchMask {
    WatchMask::CLOSE_WRITE
        | WatchMask::MOVED_TO
        | WatchMask::MOVED_FROM
        | WatchMask::CREATE
        | WatchMask::DELETE
}

struct DirWatch {
    path: PathBuf,
    /// Changes to `.lua` files here trigger a config reload.
    in_config_tree: bool,
}

pub struct Watcher {
    inotify: Inotify,
    dirs: HashMap<WatchDescriptor, DirWatch>,
    /// `watch_file` callbacks by full (parent-canonicalized) path.
    files: HashMap<PathBuf, Vec<WatchCallback>>,
}

impl Watcher {
    /// Watch `config_root` (the config file's directory) recursively.
    pub fn new(config_root: &Path) -> io::Result<Self> {
        let inotify = Inotify::init()?;
        let mut w = Self {
            inotify,
            dirs: HashMap::new(),
            files: HashMap::new(),
        };
        w.add_config_tree(config_root.to_path_buf())?;
        Ok(w)
    }

    /// A dup of the inotify fd for the calloop `Generic` source. An
    /// independent `OwnedFd`, so source teardown order can never
    /// invalidate the watcher (or vice versa).
    pub fn loop_fd(&self) -> io::Result<OwnedFd> {
        // SAFETY: the raw fd is valid for the borrow's lifetime — it
        // belongs to `self.inotify`, alive across this call.
        unsafe { BorrowedFd::borrow_raw(self.inotify.as_raw_fd()) }.try_clone_to_owned()
    }

    fn add_dir(&mut self, path: PathBuf, in_config_tree: bool) -> io::Result<()> {
        let wd = self.inotify.watches().add(&path, dir_mask())?;
        // Same path twice → same descriptor; keep the stronger flag.
        let entry = self.dirs.entry(wd).or_insert(DirWatch {
            path,
            in_config_tree: false,
        });
        entry.in_config_tree |= in_config_tree;
        Ok(())
    }

    /// Watch a directory and everything under it as config tree. The
    /// root must watch; unreadable/racing children only warn.
    fn add_config_tree(&mut self, root: PathBuf) -> io::Result<()> {
        self.add_dir(root.clone(), true)?;
        let Ok(entries) = std::fs::read_dir(&root) else {
            return Ok(());
        };
        for entry in entries.flatten() {
            // Skip hidden dirs (.git — a commit is not a config edit)
            // and symlinks (cycle risk; file_type() doesn't follow).
            let hidden = entry.file_name().as_encoded_bytes().starts_with(b".");
            if hidden || !entry.file_type().is_ok_and(|t| t.is_dir()) {
                continue;
            }
            if let Err(e) = self.add_config_tree(entry.path()) {
                tracing::warn!("watching {}: {e}", entry.path().display());
            }
        }
        Ok(())
    }

    /// Register a `shell.watch_file` target.
    pub fn watch_file(&mut self, path: &Path, callback: WatchCallback) -> io::Result<()> {
        let name = path.file_name().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("watch_file: no file name in {}", path.display()),
            )
        })?;
        let parent = match path.parent() {
            Some(p) if !p.as_os_str().is_empty() => p,
            _ => Path::new("."),
        };
        // Canonicalize so the event's dir.join(name) matches the key
        // regardless of how the config spelled the path.
        let parent = parent.canonicalize()?;
        let key = parent.join(name);
        self.add_dir(parent, false)?;
        self.files.entry(key).or_default().push(callback);
        Ok(())
    }

    /// Drop every `watch_file` callback — called before hot reload
    /// drops the VM they point into.
    pub fn clear_file_watches(&mut self) {
        self.files.clear();
    }

    /// Drain all queued events. Fires `watch_file` callbacks inline
    /// (loop thread — Lua is safe here); returns `true` when a `.lua`
    /// file in the config tree changed and a reload should be
    /// scheduled.
    pub fn handle_events(&mut self) -> io::Result<bool> {
        let mut reload = false;
        let mut buffer = [0u8; 4096];
        loop {
            // Collect owned copies first: processing needs `&mut self`
            // (new-dir watches), which the events iterator holds.
            let batch: Vec<(WatchDescriptor, EventMask, Option<OsString>)> =
                match self.inotify.read_events(&mut buffer) {
                    Ok(events) => events
                        .map(|e| (e.wd, e.mask, e.name.map(OsString::from)))
                        .collect(),
                    Err(e) if e.kind() == io::ErrorKind::WouldBlock => return Ok(reload),
                    Err(e) => return Err(e),
                };
            for (wd, mask, name) in batch {
                reload |= self.process(wd, mask, name);
            }
        }
    }

    fn process(&mut self, wd: WatchDescriptor, mask: EventMask, name: Option<OsString>) -> bool {
        if mask.contains(EventMask::Q_OVERFLOW) {
            // Events were lost; a config change may be among them.
            tracing::warn!("inotify queue overflow — forcing a reload");
            return true;
        }
        if mask.contains(EventMask::IGNORED) {
            // Kernel dropped the watch (dir deleted/unmounted).
            self.dirs.remove(&wd);
            return false;
        }
        let Some(dir) = self.dirs.get(&wd) else {
            return false;
        };
        let Some(name) = name else {
            return false; // event on the dir itself
        };
        let full = dir.path.join(&name);
        let in_config_tree = dir.in_config_tree;
        let mut reload = false;
        if in_config_tree {
            if mask.contains(EventMask::ISDIR) {
                if mask.intersects(EventMask::CREATE | EventMask::MOVED_TO) {
                    // New subtree — it may already contain files.
                    if let Err(e) = self.add_config_tree(full.clone()) {
                        tracing::warn!("watching new dir {}: {e}", full.display());
                    }
                }
            } else if full.extension().is_some_and(|e| e == "lua") {
                reload = true;
            }
        }
        if !mask.contains(EventMask::ISDIR)
            && mask.intersects(EventMask::CLOSE_WRITE | EventMask::MOVED_TO)
        {
            if let Some(cbs) = self.files.get_mut(&full) {
                let content = std::fs::read_to_string(&full).unwrap_or_default();
                cbs.retain(|cb| cb.call(&content));
            }
        }
        reload
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use moonshell_runtime::{ShellCtx, Vm};

    fn temp_dir(tag: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("moonshell-watcher-{}-{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn lua_watch(vm: &Vm, ctx: &std::rc::Rc<ShellCtx>, path: &Path, global: &str) -> WatchCallback {
        vm.exec(
            &format!(
                "shell.watch_file({:?}, function(c) {global} = c end)",
                path.to_str().unwrap()
            ),
            "t.lua",
        )
        .unwrap();
        ctx.take_watches().pop().unwrap().callback
    }

    #[test]
    fn lua_change_in_config_tree_requests_reload() {
        let root = temp_dir("reload");
        std::fs::write(root.join("init.lua"), "-- v1").unwrap();
        let mut w = Watcher::new(&root).unwrap();
        assert!(!w.handle_events().unwrap(), "startup writes drained");
        std::fs::write(root.join("init.lua"), "-- v2").unwrap();
        assert!(w.handle_events().unwrap());
        assert!(!w.handle_events().unwrap(), "events drain");
    }

    #[test]
    fn non_lua_change_is_ignored() {
        let root = temp_dir("nonlua");
        let mut w = Watcher::new(&root).unwrap();
        std::fs::write(root.join("notes.txt"), "x").unwrap();
        assert!(!w.handle_events().unwrap());
    }

    #[test]
    fn new_subdirectory_is_picked_up() {
        let root = temp_dir("subdir");
        let mut w = Watcher::new(&root).unwrap();
        std::fs::create_dir(root.join("widgets")).unwrap();
        assert!(!w.handle_events().unwrap(), "mkdir alone is no reload");
        std::fs::write(root.join("widgets/clock.lua"), "x").unwrap();
        assert!(w.handle_events().unwrap());
    }

    #[test]
    fn watch_file_fires_with_content_and_clears() {
        let root = temp_dir("watchfile");
        let target = root.join("data.conf");
        std::fs::write(&target, "v1").unwrap();

        let vm = Vm::new().unwrap();
        let ctx = ShellCtx::new();
        vm.install_shell(&ctx).unwrap();

        // Watch a tree elsewhere so the target dir is watch_file-only.
        let other = temp_dir("watchfile-tree");
        let mut w = Watcher::new(&other).unwrap();
        let cb = lua_watch(&vm, &ctx, &target, "got");
        w.watch_file(&target, cb).unwrap();

        std::fs::write(&target, "v2").unwrap();
        assert!(!w.handle_events().unwrap(), "conf change is no reload");
        assert_eq!(vm.lua().globals().get::<String>("got").unwrap(), "v2");

        w.clear_file_watches();
        std::fs::write(&target, "v3").unwrap();
        w.handle_events().unwrap();
        assert_eq!(
            vm.lua().globals().get::<String>("got").unwrap(),
            "v2",
            "cleared watches must not fire"
        );
    }

    #[test]
    fn dead_vm_watch_is_dropped_silently() {
        let root = temp_dir("deadvm");
        let target = root.join("data.conf");
        std::fs::write(&target, "v1").unwrap();

        let vm = Vm::new().unwrap();
        let ctx = ShellCtx::new();
        vm.install_shell(&ctx).unwrap();

        let other = temp_dir("deadvm-tree");
        let mut w = Watcher::new(&other).unwrap();
        let cb = lua_watch(&vm, &ctx, &target, "got");
        w.watch_file(&target, cb).unwrap();

        drop(vm);
        std::fs::write(&target, "v2").unwrap();
        assert!(!w.handle_events().unwrap());
        assert!(w.files.values().all(|v| v.is_empty()), "dead cb removed");
    }
}

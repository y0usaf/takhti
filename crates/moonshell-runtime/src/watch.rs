//! `shell.watch_file` — file-change callbacks queued for the binary's
//! inotify watcher.
//!
//! Lua can't own the inotify instance (it lives with the event loop),
//! so `shell.watch_file(path, fn)` pushes a [`PendingWatch`] onto the
//! [`crate::ShellCtx`] queue; the binary's drain registers it with the
//! shared watcher, which calls [`WatchCallback::call`] with the file's
//! new content on the loop thread. Same discipline as timers: the
//! callback is `WeakLua` + `LuaRegistryKey`, so a watch registered by
//! a hot-reloaded-away VM can never keep it alive — a dead weak ref
//! removes the watch.

use std::path::PathBuf;

use mlua::prelude::*;

/// A `shell.watch_file` call waiting for the binary to register it
/// with the inotify watcher.
pub struct PendingWatch {
    pub path: PathBuf,
    pub callback: WatchCallback,
}

/// The loop-thread half of a file watch: what to call when the file
/// changes.
pub struct WatchCallback {
    weak: WeakLua,
    key: LuaRegistryKey,
}

impl WatchCallback {
    pub fn new(lua: &Lua, f: LuaFunction) -> LuaResult<Self> {
        Ok(Self {
            weak: lua.weak(),
            key: lua.create_registry_value(f)?,
        })
    }

    /// Call with the file's new content (nur's contract). Returns
    /// `false` when the watch should be dropped: the VM is gone or the
    /// registry key no longer resolves. Callback *errors* are logged
    /// and keep the watch — a transient failure must not kill it.
    pub fn call(&self, content: &str) -> bool {
        let Some(lua) = self.weak.try_upgrade() else {
            return false;
        };
        match lua.registry_value::<LuaFunction>(&self.key) {
            Ok(f) => {
                if let Err(e) = f.call::<()>(content) {
                    tracing::error!("watch_file callback error: {e}");
                }
                true
            }
            Err(e) => {
                tracing::error!("watch_file registry lookup failed: {e}");
                false
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::{ShellCtx, Vm};

    #[test]
    fn watch_callback_receives_content() {
        let vm = Vm::new().unwrap();
        let ctx = ShellCtx::new();
        vm.install_shell(&ctx).unwrap();
        vm.exec(
            "shell.watch_file('/tmp/x.conf', function(c) got = c end)",
            "t.lua",
        )
        .unwrap();
        let w = ctx.take_watches().pop().unwrap();
        assert_eq!(w.path, std::path::Path::new("/tmp/x.conf"));
        assert!(w.callback.call("hello"));
        assert_eq!(vm.lua().globals().get::<String>("got").unwrap(), "hello");
    }

    #[test]
    fn watch_callback_error_keeps_the_watch() {
        let vm = Vm::new().unwrap();
        let ctx = ShellCtx::new();
        vm.install_shell(&ctx).unwrap();
        vm.exec(
            "shell.watch_file('x', function() error('boom') end)",
            "t.lua",
        )
        .unwrap();
        let w = ctx.take_watches().pop().unwrap();
        assert!(w.callback.call(""), "a transient error must not drop it");
    }

    #[test]
    fn watch_callback_after_vm_drop_asks_for_removal() {
        let vm = Vm::new().unwrap();
        let ctx = ShellCtx::new();
        vm.install_shell(&ctx).unwrap();
        vm.exec("shell.watch_file('x', function() end)", "t.lua")
            .unwrap();
        let w = ctx.take_watches().pop().unwrap();
        drop(vm);
        assert!(!w.callback.call(""), "a dead VM must drop its watches");
    }
}

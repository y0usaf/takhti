//! Native, event-driven service backends (M3). No subprocess polling,
//! no runtime threads: every backend is a calloop source (sockets,
//! zbus, sysfs/inotify) pushing plain state snapshots through a
//! `notify` callback.
//!
//! No Lua here — the bridge into `shell.services.*` lives in `runtime`
//! (the one-way dependency arrow: `runtime` sees `services`, nothing
//! sees `runtime`).
//!
//! Doctrine 05 shape, per service: one `start(handle, notify)` entry,
//! one plain state struct, snapshots pushed on change. Backends never
//! expose their wire types.

pub mod compositor;

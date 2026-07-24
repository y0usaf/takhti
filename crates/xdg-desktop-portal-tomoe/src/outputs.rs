//! wl_output enumeration over a short-lived Wayland connection.
//!
//! Each `SelectSources` call gets a fresh connection that is torn down when
//! the function returns — outputs can hotplug between calls, so caching
//! would only go stale.

use std::collections::HashMap;

use wayland_client::protocol::{wl_output, wl_registry};
use wayland_client::{Connection, Dispatch, EventQueue, Proxy, QueueHandle};

#[derive(Debug, Clone)]
pub struct OutputInfo {
    pub name: String,
    pub description: String,
    pub width: i32,
    pub height: i32,
    pub refresh_mhz: i32,
}

#[derive(Default)]
struct Enumerator {
    /// Per-proxy accumulation keyed by protocol id; moved to `done` on the
    /// output's `Done` event.
    pending: HashMap<u32, OutputInfo>,
    done: Vec<OutputInfo>,
}

impl Dispatch<wl_registry::WlRegistry, ()> for Enumerator {
    fn event(
        _: &mut Self,
        registry: &wl_registry::WlRegistry,
        event: wl_registry::Event,
        _: &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        if let wl_registry::Event::Global {
            name,
            interface,
            version,
        } = event
        {
            if interface == "wl_output" {
                registry.bind::<wl_output::WlOutput, _, _>(name, version.min(4), qh, ());
            }
        }
    }
}

impl Dispatch<wl_output::WlOutput, ()> for Enumerator {
    fn event(
        state: &mut Self,
        output: &wl_output::WlOutput,
        event: wl_output::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        let id = output.id().protocol_id();
        let cur = state.pending.entry(id).or_insert_with(|| OutputInfo {
            name: String::new(),
            description: String::new(),
            width: 0,
            height: 0,
            refresh_mhz: 0,
        });
        match event {
            wl_output::Event::Name { name } => cur.name = name,
            wl_output::Event::Description { description } => cur.description = description,
            wl_output::Event::Mode {
                flags,
                width,
                height,
                refresh,
            } if flags
                .into_result()
                .map(|f| f.contains(wl_output::Mode::Current))
                .unwrap_or(false) =>
            {
                cur.width = width;
                cur.height = height;
                cur.refresh_mhz = refresh;
            }
            wl_output::Event::Done => {
                if let Some(info) = state.pending.remove(&id) {
                    state.done.push(info);
                }
            }
            _ => {}
        }
    }
}

pub fn enumerate() -> Result<Vec<OutputInfo>, Box<dyn std::error::Error + Send + Sync>> {
    let conn = Connection::connect_to_env()?;
    let mut event_queue: EventQueue<Enumerator> = conn.new_event_queue();
    let qh = event_queue.handle();
    let _registry = conn.display().get_registry(&qh, ());

    let mut state = Enumerator::default();
    // Round 1 binds the globals, round 2 collects the wl_output events.
    event_queue.roundtrip(&mut state)?;
    event_queue.roundtrip(&mut state)?;

    state.done.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(state.done)
}

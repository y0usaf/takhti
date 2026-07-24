//! StatusNotifierItem tray host (FUSION F3): the compositor owns
//! org.kde.StatusNotifierWatcher and acts as its own host, so tray
//! apps register directly with the process that renders them. Item
//! state (id, title, status, icon name) publishes as a snapshot for
//! Lua tray widgets; menus (com.canonical.dbusmenu) are consumed at
//! F4 with the rest of pointer interactivity.
//!
//! Same discipline as the other services: one session-bus connection,
//! bounded blocking setup, then the fd rides calloop and everything —
//! registrations, property replies, name-owner changes — drains
//! nonblocking. If another watcher owns the name (a running tray in a
//! nested session), we degrade to nothing.
//!
//! First cut is icon-*name* only: pixmap arrays (`a(iiay)`) are
//! skipped — moonshell's icon path is theme-vector (`ui.icon`), and a
//! name covers the common tray population. Recorded gap: pixmap-only
//! apps (some Electron trays) show a placeholder until it itches.

use std::cell::RefCell;
use std::collections::HashMap;
use std::os::fd::{AsRawFd, BorrowedFd, OwnedFd};
use std::rc::Rc;

use calloop::generic::Generic;
use calloop::{Interest, LoopHandle, Mode, PostAction};
use rustbus::message_builder::MarshalledMessage;
use rustbus::wire::unmarshal::traits::Variant;
use rustbus::{MessageBuilder, RpcConn};

use crate::dbus::{self, DbusError, SETUP};

const WATCHER_NAME: &str = "org.kde.StatusNotifierWatcher";
const WATCHER_PATH: &str = "/StatusNotifierWatcher";
const WATCHER_IFACE: &str = "org.kde.StatusNotifierWatcher";
const ITEM_IFACE: &str = "org.kde.StatusNotifierItem";

/// One registered tray item, as Lua sees it.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct TrayItem {
    /// The bus name that owns the item (also the removal key).
    pub service: String,
    /// Item object path (usually /StatusNotifierItem).
    pub path: String,
    pub id: String,
    pub title: String,
    /// "Active" | "Passive" | "NeedsAttention" (empty until known).
    pub status: String,
    pub icon_name: String,
}

/// The registered set, registration order.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct TrayState {
    pub items: Vec<TrayItem>,
}

type Notify<D> = Rc<RefCell<Box<dyn FnMut(&mut D, &TrayState)>>>;

struct Host<D> {
    rpc: RpcConn,
    items: Vec<TrayItem>,
    /// GetAll serials in flight → item service name.
    pending: HashMap<u32, String>,
    notify: Notify<D>,
}

/// Split a RegisterStatusNotifierItem argument into (service, path).
/// Spec practice: a bare bus name means /StatusNotifierItem; some apps
/// pass ":1.23/CustomPath".
fn split_service(arg: &str, sender: Option<&str>) -> (String, String) {
    if let Some(idx) = arg.find('/') {
        let (name, path) = arg.split_at(idx);
        let name = if name.is_empty() {
            sender.unwrap_or_default()
        } else {
            name
        };
        (name.to_string(), path.to_string())
    } else {
        let name = if arg.is_empty() {
            sender.unwrap_or_default().to_string()
        } else {
            arg.to_string()
        };
        (name, "/StatusNotifierItem".to_string())
    }
}

impl<D> Host<D> {
    fn snapshot(&self) -> TrayState {
        TrayState {
            items: self.items.clone(),
        }
    }

    fn signal(&mut self, member: &str, arg: &str) {
        let mut sig = MessageBuilder::new()
            .signal(WATCHER_IFACE, member, WATCHER_PATH)
            .build();
        if sig.body.push_param(arg).is_ok() {
            if let Err(e) = dbus::send(&mut self.rpc, &mut sig) {
                tracing::debug!("tray: {member} signal: {e}");
            }
        }
    }

    /// Ask for the item's properties; the reply lands via `pending`.
    fn query_item(&mut self, service: &str, path: &str) {
        match dbus::get_all(service, path, ITEM_IFACE) {
            Ok(mut call) => match dbus::send(&mut self.rpc, &mut call) {
                Ok(serial) => {
                    self.pending.insert(serial, service.to_string());
                }
                Err(e) => tracing::debug!("tray: GetAll send to {service}: {e}"),
            },
            Err(e) => tracing::debug!("tray: GetAll build: {e}"),
        }
    }

    fn apply_props(&mut self, service: &str, reply: &MarshalledMessage) {
        let Ok(props) = reply.body.parser().get::<HashMap<String, Variant>>() else {
            return;
        };
        let Some(item) = self.items.iter_mut().find(|i| i.service == service) else {
            return;
        };
        let get_s =
            |key: &str| -> Option<String> { props.get(key).and_then(|v| v.get::<String>().ok()) };
        if let Some(v) = get_s("Id") {
            item.id = v;
        }
        if let Some(v) = get_s("Title") {
            item.title = v;
        }
        if let Some(v) = get_s("Status") {
            item.status = v;
        }
        if let Some(v) = get_s("IconName") {
            item.icon_name = v;
        }
    }

    /// True when the set changed (drop + signal).
    fn drop_service(&mut self, service: &str) -> bool {
        let before = self.items.len();
        self.items.retain(|i| i.service != service);
        if self.items.len() == before {
            return false;
        }
        self.signal("StatusNotifierItemUnregistered", service);
        true
    }

    fn handle_call(&mut self, call: &MarshalledMessage) -> bool {
        let member = call.dynheader.member.as_deref().unwrap_or("");
        let sender = call.dynheader.sender.as_deref();
        let mut changed = false;
        let mut reply = call.dynheader.make_response();
        match member {
            "RegisterStatusNotifierItem" => {
                let arg: String = call.body.parser().get().unwrap_or_default();
                let (service, path) = split_service(&arg, sender);
                if !service.is_empty() && !self.items.iter().any(|i| i.service == service) {
                    self.items.push(TrayItem {
                        service: service.clone(),
                        path: path.clone(),
                        ..TrayItem::default()
                    });
                    self.query_item(&service, &path);
                    self.signal("StatusNotifierItemRegistered", &service);
                    changed = true;
                }
            }
            "RegisterStatusNotifierHost" => {
                // We are the host; external hosts are welcome to listen
                // to the signals but we don't track them.
                self.signal("StatusNotifierHostRegistered", "");
            }
            // Watcher properties, asked over org.freedesktop.DBus.Properties.
            "Get" | "GetAll" => {
                // Minimal: hosts mostly probe IsStatusNotifierHostRegistered.
                // Serve GetAll with the three properties as basic types is
                // involved with rustbus variants; answer Get for the
                // common boolean and let GetAll fall through empty.
                let mut p = call.body.parser();
                let _iface: String = p.get().unwrap_or_default();
                let prop: String = p.get().unwrap_or_default();
                if member == "Get" && prop == "IsStatusNotifierHostRegistered" {
                    use rustbus::params::{Base, Container, Param, Variant as ParamVariant};
                    let value = Param::Base(Base::Boolean(true));
                    let variant = Param::Container(Container::Variant(Box::new(ParamVariant {
                        sig: value.sig(),
                        value,
                    })));
                    let _ = reply.body.push_old_param(&variant);
                }
            }
            other => {
                tracing::debug!("tray: unhandled method {other:?}");
            }
        }
        if let Err(e) = dbus::send(&mut self.rpc, &mut reply) {
            tracing::debug!("tray: reply failed: {e}");
        }
        changed
    }

    /// A broadcast signal arrived: item property updates, or an item's
    /// owner left the bus. True when state changed.
    fn handle_signal(&mut self, sig: &MarshalledMessage) -> bool {
        let member = sig.dynheader.member.as_deref().unwrap_or("");
        match member {
            "NameOwnerChanged" => {
                let mut p = sig.body.parser();
                let name: String = p.get().unwrap_or_default();
                let _old: String = p.get::<String>().unwrap_or_default();
                let new: String = p.get::<String>().unwrap_or_default();
                if new.is_empty() {
                    return self.drop_service(&name);
                }
                false
            }
            "NewIcon" | "NewTitle" | "NewStatus" | "NewToolTip" => {
                let sender = sig.dynheader.sender.clone().unwrap_or_default();
                let path = self
                    .items
                    .iter()
                    .find(|i| i.service == sender)
                    .map(|i| i.path.clone());
                if let Some(path) = path {
                    self.query_item(&sender, &path);
                }
                false
            }
            _ => false,
        }
    }
}

/// Start the watcher+host. Fails (gracefully, in the caller) when the
/// bus is unreachable or another watcher owns the name.
pub fn start<D: 'static>(
    handle: &LoopHandle<'static, D>,
    notify: impl FnMut(&mut D, &TrayState) + 'static,
) -> Result<(), DbusError> {
    let mut rpc = RpcConn::session_conn(SETUP)?;
    let mut req = rustbus::standard_messages::request_name(
        WATCHER_NAME,
        rustbus::standard_messages::DBUS_NAME_FLAG_DO_NOT_QUEUE,
    );
    let serial = dbus::send(&mut rpc, &mut req)?;
    let reply = rpc.wait_response(serial, SETUP)?;
    dbus::reply_ok(&reply)?;
    let code: u32 = reply.body.parser().get().unwrap_or(0);
    if code != rustbus::standard_messages::DBUS_REQUEST_NAME_REPLY_PRIMARY_OWNER {
        return Err(DbusError::Reply(format!(
            "{WATCHER_NAME} already owned (reply code {code})"
        )));
    }

    // Owner changes (item exit) and item update signals.
    for rule in [
        "type='signal',sender='org.freedesktop.DBus',member='NameOwnerChanged'".to_string(),
        format!("type='signal',interface='{ITEM_IFACE}'"),
    ] {
        let mut add = rustbus::standard_messages::add_match(&rule);
        let serial = dbus::send(&mut rpc, &mut add)?;
        dbus::reply_ok(&rpc.wait_response(serial, SETUP)?)?;
    }

    let raw = rpc.conn().as_raw_fd();
    let fd: OwnedFd = unsafe { BorrowedFd::borrow_raw(raw) }.try_clone_to_owned()?;

    let host = Rc::new(RefCell::new(Host {
        rpc,
        items: Vec::new(),
        pending: HashMap::new(),
        notify: Rc::new(RefCell::new(Box::new(notify))),
    }));

    let inserted = handle.insert_source(
        Generic::new(fd, Interest::READ, Mode::Level),
        move |_, _, data: &mut D| {
            let h = host.clone();
            let mut changed = false;
            {
                let host = &mut *h.borrow_mut();
                match host.rpc.refill_all() {
                    Ok(_) => {}
                    Err(e) => {
                        tracing::warn!("tray: bus read failed ({e}); host off");
                        return Ok(PostAction::Remove);
                    }
                }
                while let Some(call) = host.rpc.try_get_call() {
                    changed |= host.handle_call(&call);
                }
                while let Some(sig) = host.rpc.try_get_signal() {
                    changed |= host.handle_signal(&sig);
                }
                let serials: Vec<u32> = host.pending.keys().copied().collect();
                for serial in serials {
                    if let Some(reply) = host.rpc.try_get_response(serial) {
                        let service = host.pending.remove(&serial).unwrap_or_default();
                        host.apply_props(&service, &reply);
                        changed = true;
                    }
                }
            }
            if changed {
                let (state, notify) = {
                    let host = h.borrow();
                    (host.snapshot(), host.notify.clone())
                };
                (notify.borrow_mut())(data, &state);
            }
            Ok(PostAction::Continue)
        },
    );
    inserted.map_err(|e| DbusError::Loop(e.to_string()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn service_splitting() {
        assert_eq!(
            split_service(":1.42", None),
            (":1.42".into(), "/StatusNotifierItem".into())
        );
        assert_eq!(
            split_service(":1.42/Custom/Path", None),
            (":1.42".into(), "/Custom/Path".into())
        );
        assert_eq!(
            split_service("/StatusNotifierItem", Some(":1.7")),
            (":1.7".into(), "/StatusNotifierItem".into())
        );
        assert_eq!(
            split_service("", Some(":1.7")),
            (":1.7".into(), "/StatusNotifierItem".into())
        );
    }
}

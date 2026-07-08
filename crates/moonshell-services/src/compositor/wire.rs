//! Shared socket plumbing for compositor backends: nonblocking
//! drains, line splitting, and the reconnect timer. Backends own
//! their protocol; this module owns the bytes.

use std::io::{ErrorKind, Read};
use std::os::unix::net::UnixStream;
use std::rc::Rc;
use std::time::Duration;

use calloop::timer::{TimeoutAction, Timer};
use calloop::LoopHandle;

/// Reconnect cadence while a compositor socket is unreachable — the
/// only periodic wakeup any backend owns, and only while disconnected.
pub(super) const RETRY: Duration = Duration::from_secs(2);

/// Drain everything currently readable from a nonblocking socket into
/// `buf`. Returns true when the connection is dead (EOF or a real
/// error). Callers holding a calloop `NoIoDrop<UnixStream>` pass
/// `&**stream` — `NoIoDrop` has no `DerefMut`, but `&UnixStream` is
/// `Read`.
pub(super) fn read_available(mut stream: &UnixStream, buf: &mut Vec<u8>) -> bool {
    loop {
        let mut chunk = [0u8; 4096];
        match stream.read(&mut chunk) {
            Ok(0) => return true,
            Ok(n) => buf.extend_from_slice(&chunk[..n]),
            Err(e) if e.kind() == ErrorKind::WouldBlock => return false,
            Err(e) if e.kind() == ErrorKind::Interrupted => continue,
            Err(e) => {
                tracing::warn!("compositor IPC read: {e}");
                return true;
            }
        }
    }
}

/// Split one `\n`-terminated line off the front of `buf`.
pub(super) fn take_line(buf: &mut Vec<u8>) -> Option<Vec<u8>> {
    let pos = buf.iter().position(|&b| b == b'\n')?;
    let rest = buf.split_off(pos + 1);
    let mut line = std::mem::replace(buf, rest);
    line.pop(); // the newline
    Some(line)
}

/// Arm the reconnect timer: try `connect` every [`RETRY`] until it
/// succeeds, then drop the timer.
pub(super) fn arm_retry<D: 'static>(
    handle: &LoopHandle<'static, D>,
    what: &'static str,
    connect: Rc<dyn Fn() -> std::io::Result<()>>,
) {
    let armed = handle.insert_source(Timer::from_duration(RETRY), move |_, _, _| {
        match connect() {
            Ok(()) => {
                tracing::info!("{what} reconnected");
                TimeoutAction::Drop
            }
            Err(_) => TimeoutAction::ToDuration(RETRY),
        }
    });
    if let Err(e) = armed {
        tracing::error!("{what}: arming retry timer: {e} — reconnect disabled");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn take_line_splits_and_keeps_tail() {
        let mut buf = b"one\ntwo\nthr".to_vec();
        assert_eq!(take_line(&mut buf).as_deref(), Some(&b"one"[..]));
        assert_eq!(take_line(&mut buf).as_deref(), Some(&b"two"[..]));
        assert_eq!(take_line(&mut buf), None);
        assert_eq!(buf, b"thr");
    }
}

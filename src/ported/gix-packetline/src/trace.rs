//! Packet tracing, a port of `packet_trace()` and its helpers (pkt-line.c:11-86).
//!
//! Every pkt-line written or read is reported to `GIT_TRACE_PACKET` as
//! `packet: <prog>> <data>` or `packet: <prog>< <data>`, and the bytes of a pack
//! that arrives over pkt-lines go to `GIT_TRACE_PACKFILE` instead. The trace keys
//! themselves, their destinations and the timestamp prefix are `trace.c`'s, which
//! the embedding program provides through [`set_sink()`]; without one nothing is
//! traced.

use std::cell::Cell;
use std::sync::{Mutex, OnceLock};

/// The `trace.c` half of packet tracing, provided by the embedding program.
#[derive(Clone, Copy)]
pub struct Sink {
    /// `trace_want(&trace_packet)`: whether `GIT_TRACE_PACKET` traces anywhere.
    pub want_packet: fn() -> bool,
    /// `trace_want(&trace_pack)`: whether `GIT_TRACE_PACKFILE` traces anywhere.
    pub want_pack: fn() -> bool,
    /// `trace_strbuf(&trace_packet, &out)` at pkt-line.c:85, which prefixes the line.
    pub packet: fn(&[u8]),
    /// `trace_verbatim(&trace_pack, buf, len)`.
    pub pack: fn(&[u8]),
}

static SINK: OnceLock<Sink> = OnceLock::new();

/// `packet_trace_prefix`, `"git"` until [`packet_trace_identity()`] names the program.
static PREFIX: Mutex<Option<String>> = Mutex::new(None);

/// `packet_trace()`'s `static int in_pack, sideband;`, shared by every thread as
/// the C statics are shared with the async demultiplexer.
static PACK_STATE: Mutex<(bool, bool)> = Mutex::new((false, false));

thread_local! {
    /// [`in_async()`].
    static IN_ASYNC: Cell<bool> = const { Cell::new(false) };
}

/// Install the `trace.c` half. Only the first call takes effect.
pub fn set_sink(sink: Sink) {
    let _ = SINK.set(sink);
}

/// `packet_trace_identity()` (pkt-line.c:15-18).
pub fn packet_trace_identity(prog: &str) {
    *PREFIX.lock().unwrap_or_else(|e| e.into_inner()) = Some(prog.to_owned());
}

/// Run `f` as git runs `sideband_demux()`: on the async thread, where
/// `get_trace_prefix()` answers `"sideband"` (pkt-line.c:20-23, run-command.c:1086-1091).
pub fn as_async<R>(f: impl FnOnce() -> R) -> R {
    let was = IN_ASYNC.with(|c| c.replace(true));
    let out = f();
    IN_ASYNC.with(|c| c.set(was));
    out
}

/// `in_async()` (run-command.c:1086-1091), see [`as_async()`].
fn in_async() -> bool {
    IN_ASYNC.with(Cell::get)
}

/// `get_trace_prefix()` (pkt-line.c:20-23).
fn get_trace_prefix() -> String {
    if in_async() {
        return "sideband".into();
    }
    PREFIX
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clone()
        .unwrap_or_else(|| "git".into())
}

/// `packet_trace_pack()` (pkt-line.c:25-38).
fn packet_trace_pack(sink: &Sink, buf: &[u8], sideband: bool) -> bool {
    if !sideband {
        if (sink.want_pack)() {
            (sink.pack)(buf);
        }
        true
    } else if buf.first() == Some(&1) {
        if (sink.want_pack)() {
            (sink.pack)(&buf[1..]);
        }
        true
    } else {
        // it's another non-pack sideband
        false
    }
}

/// The `!trace_want(&trace_packet) && !trace_want(&trace_pack)` early return of
/// [`packet_trace()`], for callers that would otherwise build its argument for nothing.
pub fn wanted() -> bool {
    SINK.get().is_some_and(|sink| (sink.want_packet)() || (sink.want_pack)())
}

/// `packet_trace()` (pkt-line.c:40-86): report one packet's payload, `buf` without
/// its length header, or the literal `0000`/`0001`/`0002` of a special packet.
pub fn packet_trace(buf: &[u8], write: bool) {
    let Some(sink) = SINK.get() else {
        return;
    };
    if !(sink.want_packet)() && !(sink.want_pack)() {
        return;
    }

    let mut buf = buf;
    {
        let mut state = PACK_STATE.lock().unwrap_or_else(|e| e.into_inner());
        let (in_pack, sideband) = &mut *state;
        if *in_pack {
            if packet_trace_pack(sink, buf, *sideband) {
                return;
            }
        } else if buf.starts_with(b"PACK") || buf.starts_with(b"\x01PACK") {
            *in_pack = true;
            *sideband = buf[0] == 1;
            packet_trace_pack(sink, buf, *sideband);

            // Make a note in the human-readable trace that the pack data started.
            buf = b"PACK ...";
        }
    }

    if !(sink.want_packet)() {
        return;
    }

    let mut out = Vec::with_capacity(buf.len() + 32);
    out.extend_from_slice(format!("packet: {:>12}{} ", get_trace_prefix(), if write { '>' } else { '<' }).as_bytes());
    // XXX we should really handle printable utf8
    for &b in buf {
        // suppress newlines
        if b == b'\n' {
            continue;
        }
        if (0x20..=0x7e).contains(&b) {
            out.push(b);
        } else {
            // `strbuf_addf(&out, "\\%o", buf[i])` promotes a `char`, whose
            // signedness is the platform's, to `int` and prints it as unsigned.
            out.extend_from_slice(format!("\\{:o}", i32::from(b as std::ffi::c_char) as u32).as_bytes());
        }
    }
    out.push(b'\n');
    (sink.packet)(&out);
}

/// Return the identity and the pack state to a fresh process's, for a program
/// that runs several commands in one process.
pub fn reset() {
    *PREFIX.lock().unwrap_or_else(|e| e.into_inner()) = None;
    *PACK_STATE.lock().unwrap_or_else(|e| e.into_inner()) = (false, false);
}

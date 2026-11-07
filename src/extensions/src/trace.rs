//! `GIT_TRACE_PACKET` and `GIT_TRACE_PACKFILE`, a port of the trace.c keys
//! `packet_trace()` (pkt-line.c) writes to.
//!
//! The pkt-line half lives in `gix_packetline::trace`; this is the half that
//! decides where a key traces to (`get_trace_fd()`, trace.c:37-75), prefixes each
//! line with the time and the C call site (`prepare_trace_line()`, trace.c:98-124),
//! and writes it (`trace_write()`, trace.c:126-133).

use std::ffi::CString;
use std::sync::Mutex;

/// One `struct trace_key` (trace.h:82-87).
struct TraceKey {
    key: &'static str,
    state: Mutex<KeyState>,
}

#[derive(Clone, Copy)]
struct KeyState {
    initialized: bool,
    fd: i32,
    need_close: bool,
}

impl TraceKey {
    const fn new(key: &'static str) -> Self {
        TraceKey {
            key,
            state: Mutex::new(KeyState { initialized: false, fd: 0, need_close: false }),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, KeyState> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// `get_trace_fd()` (trace.c:37-75).
    fn fd(&self) -> i32 {
        let mut state = self.lock();
        // don't open twice
        if state.initialized {
            return state.fd;
        }
        let trace = std::env::var_os(self.key).map(|v| v.to_string_lossy().into_owned());
        let trace = trace.as_deref().unwrap_or("");
        state.fd = if trace.is_empty() || trace == "0" || trace.eq_ignore_ascii_case("false") {
            0
        } else if trace == "1" || trace.eq_ignore_ascii_case("true") {
            libc::STDERR_FILENO
        } else if trace.len() == 1 && trace.as_bytes()[0].is_ascii_digit() {
            i32::from(trace.as_bytes()[0] - b'0')
        } else if trace.starts_with('/') {
            let path = CString::new(trace).unwrap_or_default();
            // Safety: `path` is NUL-terminated and outlives the call.
            let fd = unsafe { libc::open(path.as_ptr(), libc::O_WRONLY | libc::O_APPEND | libc::O_CREAT, 0o666) };
            if fd == -1 {
                let err = std::io::Error::last_os_error();
                eprintln!("warning: could not open '{trace}' for tracing: {}", crate::external::strerror(&err));
                0
            } else {
                state.need_close = true;
                fd
            }
        } else {
            eprintln!(
                "warning: unknown trace value for '{key}': {trace}\n         If you want to trace into a file, then please set {key}\n         to an absolute pathname (starting with /)",
                key = self.key
            );
            0
        };
        state.initialized = true;
        state.fd
    }

    /// Forget the destination, closing a file `get_trace_fd()` opened, so the next
    /// use reads the environment again.
    fn reset(&self) {
        let mut state = self.lock();
        if state.need_close {
            // Safety: `fd` is the descriptor `get_trace_fd()` opened for this key.
            unsafe { libc::close(state.fd) };
        }
        *state = KeyState { initialized: false, fd: 0, need_close: false };
    }

    /// `trace_want()` (trace.c:325-328).
    fn want(&self) -> bool {
        self.fd() != 0
    }

    /// `trace_write()` (trace.c:126-133): a failed write disables the key.
    fn write(&self, buf: &[u8]) {
        let fd = self.fd();
        if let Err(err) = write_in_full(fd, buf) {
            eprintln!("warning: unable to write trace for {}: {}", self.key, crate::external::strerror(&err));
            // `trace_disable()` (trace.c:89-96)
            let mut state = self.lock();
            if state.need_close {
                // Safety: `fd` is the descriptor `get_trace_fd()` opened for this key.
                unsafe { libc::close(fd) };
            }
            *state = KeyState { initialized: true, fd: 0, need_close: false };
        }
    }
}

static TRACE_PACKET: TraceKey = TraceKey::new("GIT_TRACE_PACKET");
static TRACE_PACK: TraceKey = TraceKey::new("GIT_TRACE_PACKFILE");
static TRACE_BARE: TraceKey = TraceKey::new("GIT_TRACE_BARE");

/// `write_in_full()` (wrapper.c).
fn write_in_full(fd: i32, mut buf: &[u8]) -> std::io::Result<()> {
    while !buf.is_empty() {
        // Safety: `buf` is a live slice of `buf.len()` bytes.
        let n = unsafe { libc::write(fd, buf.as_ptr().cast(), buf.len()) };
        if n < 0 {
            let err = std::io::Error::last_os_error();
            if err.kind() == std::io::ErrorKind::Interrupted {
                continue;
            }
            return Err(err);
        }
        buf = &buf[n as usize..];
    }
    Ok(())
}

/// `prepare_trace_line()` (trace.c:98-124) for `file:line`.
fn prepare_trace_line(file: &str, line: u32, buf: &mut Vec<u8>) {
    // unit tests may want to disable additional trace output
    if TRACE_BARE.want() {
        return;
    }
    let mut tv = libc::timeval { tv_sec: 0, tv_usec: 0 };
    // Safety: a zeroed `tm` is a valid out-parameter, and both pointers are live locals.
    let tm = unsafe {
        libc::gettimeofday(&mut tv, std::ptr::null_mut());
        let mut tm: libc::tm = std::mem::zeroed();
        libc::localtime_r(&tv.tv_sec, &mut tm);
        tm
    };
    buf.extend_from_slice(
        format!(
            "{:02}:{:02}:{:02}.{:06} {file}:{line}",
            tm.tm_hour, tm.tm_min, tm.tm_sec, tv.tv_usec
        )
        .as_bytes(),
    );
    // align trace output (column 40 catches most files names in git)
    while buf.len() < 40 {
        buf.push(b' ');
    }
}

/// `trace_strbuf(&trace_packet, &out)` at pkt-line.c:85, through `trace_strbuf_fl()`
/// and `print_trace_line()` (trace.c:142-146, :177-188).
fn trace_packet_line(data: &[u8]) {
    let mut buf = Vec::with_capacity(data.len() + 40);
    prepare_trace_line("pkt-line.c", 85, &mut buf);
    buf.extend_from_slice(data);
    // `strbuf_complete_line()`
    if buf.last().is_some_and(|&b| b != b'\n') {
        buf.push(b'\n');
    }
    TRACE_PACKET.write(&buf);
}

/// `trace_verbatim(&trace_pack, ...)` (trace.c:135-140).
fn trace_pack_bytes(data: &[u8]) {
    TRACE_PACK.write(data);
}

/// Hand the pkt-line layer its trace keys, as a fresh process holds them: called
/// at the start of every command, which a host may run several of in one process.
pub fn install() {
    for key in [&TRACE_PACKET, &TRACE_PACK, &TRACE_BARE] {
        key.reset();
    }
    gix_transport::packetline::trace::reset();
    gix_transport::packetline::trace::set_sink(gix_transport::packetline::trace::Sink {
        want_packet: || TRACE_PACKET.want(),
        want_pack: || TRACE_PACK.want(),
        packet: trace_packet_line,
        pack: trace_pack_bytes,
    });
}

/// `packet_trace_identity()` as each builtin calls it at its start: `fetch`
/// (builtin/fetch.c:2636), `upload-pack` (builtin/upload-pack.c:46), `push`
/// (builtin/push.c:746), `fetch-pack` (builtin/fetch-pack.c:70), `ls-remote`
/// (builtin/ls-remote.c:117), `bundle` (builtin/bundle.c:244), `clone`
/// (builtin/clone.c:1010) and `receive-pack` (builtin/receive-pack.c:2514).
/// Every other command keeps the default `git`.
pub fn identity_for(cmd: &str) {
    if matches!(
        cmd,
        "fetch" | "upload-pack" | "push" | "fetch-pack" | "ls-remote" | "bundle" | "clone" | "receive-pack"
    ) {
        packet_trace_identity(cmd);
    }
}

pub use gix_transport::packetline::trace::{as_async, packet_trace, packet_trace_identity};

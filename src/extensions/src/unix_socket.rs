//! `unix-socket.c`: Unix stream sockets named by paths longer than `sun_path`.
//!
//! `unix_sockaddr_init()` (unix-socket.c:35-78) fits an over-long path into the
//! address by `chdir`ing to its directory and using the basename, and
//! `unix_sockaddr_cleanup()` (unix-socket.c:21-33) moves back afterwards. Both
//! halves of the credential cache — the client's `unix_stream_connect()` and the
//! daemon's `unix_stream_listen()` — go through it, so a socket under a deep
//! `$XDG_CACHE_HOME` works on macOS, whose `sun_path` holds only 104 bytes.

use std::io;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::Path;

/// `sizeof(sa->sun_path)` for this platform.
fn sun_path_len() -> usize {
    // SAFETY: `sockaddr_un` is plain old data; all-zero is a valid value.
    let sa: libc::sockaddr_un = unsafe { std::mem::zeroed() };
    sa.sun_path.len()
}

/// Run `op` on the name `unix_sockaddr_init()` would put in `sun_path`: `path`
/// itself when it fits, otherwise its basename with the process standing in its
/// directory, restoring the original directory before returning.
fn with_sockaddr<T>(path: &Path, op: impl FnOnce(&Path) -> io::Result<T>) -> io::Result<T> {
    let bytes = path.as_os_str().as_bytes();
    if bytes.len() < sun_path_len() {
        return op(path);
    }
    let too_long = || io::Error::from_raw_os_error(libc::ENAMETOOLONG);
    let slash = bytes.iter().rposition(|&b| b == b'/').ok_or_else(too_long)?;
    let base = Path::new(std::ffi::OsStr::from_bytes(&bytes[slash + 1..]));
    if base.as_os_str().len() >= sun_path_len() {
        return Err(too_long());
    }
    let orig_dir = std::env::current_dir()?;
    // `chdir_len(dir, slash - dir)`: a leading-slash path chdirs to "", which
    // fails with ENOENT exactly as it does in C.
    std::env::set_current_dir(std::ffi::OsStr::from_bytes(&bytes[..slash]))?;
    let result = op(base);
    // "we have moved the cwd of the whole process ... We are better off to just
    // die" (unix-socket.c:26-31).
    if std::env::set_current_dir(&orig_dir).is_err() {
        eprintln!("fatal: unable to restore original working directory");
        std::process::exit(128);
    }
    result
}

/// `unix_stream_connect(path, 0)` (unix-socket.c:80-104).
pub fn connect(path: &Path) -> io::Result<UnixStream> {
    with_sockaddr(path, |name| UnixStream::connect(name))
}

/// `unix_stream_listen(path, opts)` with `disallow_chdir = 0`
/// (unix-socket.c:106-141): the stale socket is unlinked first, so a killed
/// daemon's leftover does not make the bind fail.
pub fn listen(path: &Path) -> io::Result<UnixListener> {
    let _ = std::fs::remove_file(path);
    with_sockaddr(path, |name| UnixListener::bind(name))
}

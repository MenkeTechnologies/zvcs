//! `git credential-cache--daemon` — the in-memory credential cache server.
//!
//! A faithful port of `builtin/credential-cache--daemon.c` together with the
//! parts of `credential.c` (`credential_read`, `credential_match`,
//! `credential_from_url`) and `unix-socket.c` (`unix_stream_listen`) that it
//! drives. The daemon binds a Unix stream socket, speaks the credential-helper
//! key/value protocol to `git credential-cache` clients, holds each credential
//! for the timeout the client asked for, and exits once nothing is held.
//!
//! ### Covered — verified byte-for-byte against stock git 2.55.0
//!
//! * Option parsing as `parse_options` does it for the single `OPT_BOOL`
//!   `--debug`: `--debug` / `--no-debug`, unique-prefix abbreviation (`--deb`),
//!   `--` as an option terminator, options permuting after the positional, and
//!   the four failure shapes —
//!   ``error: unknown option `NAME'`` + usage (129),
//!   ``error: unknown switch `C'`` + usage (129),
//!   ``error: option `NAME' takes no value`` with **no** usage block (129),
//!   and a missing `<socket-path>` → usage on stderr (129).
//!   `-h` prints the same usage block to **stdout** and exits 129.
//! * `fatal: socket directory must be an absolute path` (128).
//! * `init_socket_directory`: POSIX `dirname` of the socket path, the
//!   loose-permissions refusal (`st_mode & 077`) with git's four-line advice
//!   ending in a tab-indented `chmod 0700 <dir>`, otherwise creating the
//!   directory mode 0700 (`unable to create directories for '<dir>'` /
//!   `unable to mkdir '<dir>'`, both 128), then a best-effort `chdir` into it.
//! * `serve_cache`: unlink-then-bind, `ok\n` on stdout, closing stdout so the
//!   client's `read_in_full` sees EOF, and pointing stderr at `/dev/null`
//!   unless `--debug` — so every `warning:`/`error:` below is visible only
//!   under `--debug`, exactly as upstream.
//! * `check_expirations`: the 30-second initial grace, the 30-second
//!   re-arm whenever an entry is reaped, and the swap-remove reaping order
//!   (which decides *which* duplicate a later `get` finds).
//! * `read_request` + `credential_read`: `action=` / `timeout=` on the first two
//!   lines (LF-stripped only), then `key=value` lines (CRLF-stripped) until a
//!   blank line or EOF, `capability[]=authtype|state`, `password_expiry_utc`,
//!   `oauth_refresh_token`, `authtype`, `credential`, `ephemeral`, `url=`, and
//!   the two rejections `error: client sent bogus action line: <l>` /
//!   `error: client sent bogus timeout line: <l>` and
//!   `warning: invalid credential line: <l>`.
//! * `credential_from_url` on a `url=` line, including that it *clears* the
//!   credential first (so a `capability[]=authtype` sent before it is lost),
//!   its percent-decoding, its "skip the part before the first colon" quirk in
//!   `url_decode_mem`, and its failure pair
//!   `warning: url has no scheme: <u>` + `fatal: credential url cannot be
//!   parsed: <u>` which kills the daemon with exit 128.
//! * The four actions. `get` emits, in this order and only when present:
//!   `capability[]=authtype`, `username=`, `password=`, then `authtype=` and
//!   `credential=` only when the client announced `capability[]=authtype`,
//!   then `password_expiry_utc=` when it is not `TIME_MAX`, then
//!   `oauth_refresh_token=`. `store` carries git's three warnings
//!   (`didn't specify a timeout`, `gave us a partial credential`,
//!   `not storing ephemeral credential`). `erase` matches with the password.
//!   `exit` removes the socket and exits 0. Anything else is
//!   `warning: cache client sent unknown action: <a>`.
//!
//! ### Not covered — and why
//!
//! * **`credentialcache.ignoreSIGHUP`.** Honouring it needs `signal(2)`; this
//!   crate depends on `gix` and `anyhow` only, with no `libc`, and `std` exposes
//!   no signal API. The config is read, and a true value `bail!`s rather than
//!   being silently dropped. The default (die on SIGHUP) is what a signal-less
//!   process already does, so unset/false behaves correctly.
//! * **Socket removal on a fatal signal.** Upstream's `register_tempfile`
//!   installs `sigchain` handlers so SIGINT/SIGTERM still unlink the socket.
//!   Without `libc` those handlers cannot be installed; the socket is removed on
//!   every ordinary exit path (`exit` action, expiry, the `url=` die) but a
//!   killed daemon leaves a stale socket behind. Stock git's client unlinks
//!   before binding anyway, so this self-heals on the next start.
//! * **`git_config_bool` on a non-numeric `ephemeral=` / `continue=` / `quit=`
//!   value.** Upstream dies with `bad numeric config value`; here such a value
//!   reads as false. Only reachable from a malformed client.
//! * **Non-UTF-8 bytes in a protocol line.** Upstream keeps credential values
//!   as raw bytes; here each line is read as bytes and then lossily converted,
//!   so an invalid sequence in a username or password becomes U+FFFD. Git's own
//!   clients send text, so this is reachable only from a hand-rolled client.
//! * **`poll(2)` versus a timed channel receive.** The wait is implemented with
//!   a dedicated accepting thread and `recv_timeout`, since `std` has no `poll`.
//!   Clients are still served strictly one at a time, as upstream does.

use anyhow::Result;
use std::fs;
use std::io::{self, BufRead, BufReader, Write};
use std::os::unix::fs::{DirBuilderExt, MetadataExt};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::Path;
use std::process::ExitCode;
use std::sync::mpsc::{self, RecvTimeoutError};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// `timestamp_t` is `uintmax_t` in git, so `TIME_MAX` is `UINTMAX_MAX`. It is
/// the sentinel meaning "this credential never expires by wall clock".
const TIME_MAX: u64 = u64::MAX;

/// The usage block `parse_options` renders for this command, trailing blank
/// line included (git's `usage_with_options` always ends with one).
const USAGE: &str = "usage: git credential-cache--daemon [--debug] <socket-path>\n\n    \
                     --[no-]debug          print debugging messages to stderr\n\n";

pub fn credential_cache__daemon(args: &[String]) -> Result<ExitCode> {
    // `args[0]` is the subcommand itself when dispatched; tolerate its absence.
    let rest = match args.first() {
        Some(a) if a == "credential-cache--daemon" => &args[1..],
        _ => args,
    };

    // Upstream reads the config before `parse_options`, so a true value is
    // rejected even for an otherwise unusable command line.
    if ignore_sighup_configured() {
        anyhow::bail!(
            "credentialcache.ignoreSIGHUP=true is not supported: honouring it needs signal(2) \
             and this crate has no libc dependency (ported: everything else)"
        );
    }

    let (debug, positionals) = match parse_options(rest) {
        Ok(v) => v,
        Err(ParseFailure::Help) => {
            // `-h` renders usage on stdout; the exit code is still 129.
            print!("{USAGE}");
            let _ = io::stdout().flush();
            return Ok(ExitCode::from(129));
        }
        Err(ParseFailure::Message(msg)) => {
            eprint!("{msg}");
            return Ok(ExitCode::from(129));
        }
    };

    let Some(socket_path) = positionals.first() else {
        eprint!("{USAGE}");
        return Ok(ExitCode::from(129));
    };

    if !socket_path.starts_with('/') {
        eprintln!("fatal: socket directory must be an absolute path");
        return Ok(ExitCode::from(128));
    }

    if let Err(code) = init_socket_directory(socket_path) {
        return Ok(code);
    }

    serve_cache(socket_path, debug)
}

// ---------------------------------------------------------------------------
// option parsing
// ---------------------------------------------------------------------------

/// Why `parse_options` stopped short of returning a command line.
enum ParseFailure {
    /// `-h`: the caller must render usage on stdout.
    Help,
    /// Text to write verbatim to stderr before exiting 129.
    Message(String),
}

/// Mirror `parse_options()` for this command's single `OPT_BOOL("debug")`, with
/// no `PARSE_OPT_*` flags set — so `--` terminates options, non-options permute
/// to the end rather than stopping the scan, and long options may be
/// abbreviated to any unique prefix.
///
/// Returns `(debug, positionals)`.
fn parse_options(args: &[String]) -> Result<(bool, Vec<String>), ParseFailure> {
    let mut debug = false;
    let mut positionals = Vec::new();
    let mut no_more_opts = false;

    for a in args {
        if no_more_opts {
            positionals.push(a.clone());
            continue;
        }
        if a == "--" {
            no_more_opts = true;
            continue;
        }
        if let Some(long) = a.strip_prefix("--") {
            // Split `--name=value` so the "takes no value" diagnostic can name
            // the option exactly as git does (negation included).
            let (name, value) = match long.split_once('=') {
                Some((n, v)) => (n, Some(v)),
                None => (long, None),
            };
            let negated = name.strip_prefix("no-");
            let bare = negated.unwrap_or(name);
            if !bare.is_empty() && "debug".starts_with(bare) {
                if value.is_some() {
                    return Err(ParseFailure::Message(format!(
                        "error: option `{name}' takes no value\n"
                    )));
                }
                debug = negated.is_none();
                continue;
            }
            return Err(ParseFailure::Message(format!(
                "error: unknown option `{long}'\n{USAGE}"
            )));
        }
        // A bare `-` is a positional for git, not a switch. `h` is the only
        // short option this command has, so the first character decides.
        if a.len() > 1 && a.starts_with('-') {
            let c = a[1..].chars().next().expect("length checked above");
            if c == 'h' {
                return Err(ParseFailure::Help);
            }
            return Err(ParseFailure::Message(format!(
                "error: unknown switch `{c}'\n{USAGE}"
            )));
        }
        positionals.push(a.clone());
    }

    Ok((debug, positionals))
}

/// `repo_config_get_bool(the_repository, "credentialcache.ignoresighup")`.
///
/// The builtin runs without repository setup, so a repository here is a
/// convenience: fall back to the system/global files when there is none.
fn ignore_sighup_configured() -> bool {
    let from_repo = gix::discover(".")
        .ok()
        .and_then(|repo| repo.config_snapshot().boolean("credentialcache.ignoreSIGHUP"));
    match from_repo {
        Some(v) => v,
        // `File::boolean` reports a malformed value as an error; git would die
        // there, and reading it as unset is the closest honest fallback.
        None => gix::config::File::from_globals()
            .ok()
            .and_then(|f| f.boolean("credentialcache.ignoreSIGHUP").ok().flatten())
            .unwrap_or(false),
    }
}

// ---------------------------------------------------------------------------
// socket directory
// ---------------------------------------------------------------------------

/// Port of `init_socket_directory()`.
///
/// Refuses a group/other-accessible directory outright; otherwise creates it
/// with mode 0700 in one step (never `mkdir` + `chmod`, which would leave a
/// window in which the socket is world-reachable), then `chdir`s into it so the
/// daemon does not pin the caller's cwd. A failed `chdir` is ignored, as
/// upstream ignores it.
fn init_socket_directory(path: &str) -> Result<(), ExitCode> {
    let dir = dirname(path);

    match fs::metadata(&dir) {
        Ok(st) => {
            if st.mode() & 0o77 != 0 {
                eprintln!(
                    "fatal: The permissions on your socket directory are too loose; other\n\
                     users may be able to read your cached credentials. Consider running:\n\
                     \n\
                     \tchmod 0700 {dir}"
                );
                return Err(ExitCode::from(128));
            }
        }
        Err(_) => {
            if let Some(parent) = Path::new(&dir).parent() {
                if let Err(e) = fs::create_dir_all(parent) {
                    eprintln!(
                        "fatal: unable to create directories for '{dir}': {}",
                        strerror(&e)
                    );
                    return Err(ExitCode::from(128));
                }
            }
            if let Err(e) = fs::DirBuilder::new().mode(0o700).create(&dir) {
                eprintln!("fatal: unable to mkdir '{dir}': {}", strerror(&e));
                return Err(ExitCode::from(128));
            }
        }
    }

    let _ = std::env::set_current_dir(&dir);
    Ok(())
}

/// POSIX `dirname(3)`: trailing slashes are stripped before the last component
/// is removed, a path with no slash is `.`, and the root stays `/`.
fn dirname(path: &str) -> String {
    let trimmed = path.trim_end_matches('/');
    if trimmed.is_empty() {
        return if path.is_empty() { ".".into() } else { "/".into() };
    }
    match trimmed.rfind('/') {
        None => ".".into(),
        Some(0) => "/".into(),
        Some(i) => trimmed[..i].trim_end_matches('/').to_string(),
    }
}

// ---------------------------------------------------------------------------
// the cache itself
// ---------------------------------------------------------------------------

/// One announced capability, tracked per protocol step exactly as
/// `struct credential_capability` does.
#[derive(Clone, Copy, Default)]
struct Capa {
    request_initial: bool,
    request_helper: bool,
}

impl Capa {
    /// `credential_has_capability(capa, CREDENTIAL_OP_RESPONSE)`.
    fn response(self) -> bool {
        self.request_initial && self.request_helper
    }
}

/// `struct credential`, reduced to the fields this daemon reads or writes.
struct Credential {
    protocol: Option<String>,
    host: Option<String>,
    path: Option<String>,
    username: Option<String>,
    password: Option<String>,
    credential: Option<String>,
    authtype: Option<String>,
    oauth_refresh_token: Option<String>,
    password_expiry_utc: u64,
    ephemeral: bool,
    capa_authtype: Capa,
    /// Tracked so `capability[]=state` is parsed as upstream parses it, but the
    /// cache daemon never emits `state[]` lines, so nothing reads it back.
    #[allow(dead_code)]
    capa_state: Capa,
}

impl Default for Credential {
    /// `CREDENTIAL_INIT`.
    fn default() -> Self {
        Credential {
            protocol: None,
            host: None,
            path: None,
            username: None,
            password: None,
            credential: None,
            authtype: None,
            oauth_refresh_token: None,
            password_expiry_utc: TIME_MAX,
            ephemeral: false,
            capa_authtype: Capa::default(),
            capa_state: Capa::default(),
        }
    }
}

/// `credential_match()`. A field the *wanted* credential leaves unset matches
/// anything; a field it sets must be present and equal on the candidate.
fn credential_match(want: &Credential, have: &Credential, match_password: bool) -> bool {
    fn check(want: &Option<String>, have: &Option<String>) -> bool {
        match want {
            None => true,
            Some(w) => have.as_deref() == Some(w.as_str()),
        }
    }
    check(&want.protocol, &have.protocol)
        && check(&want.host, &have.host)
        && check(&want.path, &have.path)
        && check(&want.username, &have.username)
        && (!match_password || check(&want.password, &have.password))
        && (!match_password || check(&want.credential, &have.credential))
}

struct Entry {
    item: Credential,
    expiration: u64,
}

/// The daemon's whole mutable state: the cached entries plus the static
/// `wait_for_entry_until` that `check_expirations()` keeps between calls.
#[derive(Default)]
struct Cache {
    entries: Vec<Entry>,
    wait_for_entry_until: u64,
}

/// Seconds since the epoch, as `time(NULL)`.
fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

impl Cache {
    /// Port of `check_expirations()`: reap everything that has expired and
    /// return how many seconds to wait before looking again, or `0` when the
    /// daemon has nothing left to do and should exit.
    ///
    /// The 30-second grace is armed twice — once so a freshly spawned daemon
    /// gives its client time to store anything at all, and again after each
    /// reap so a credential that was just erased can be replaced by a correct
    /// one without restarting the daemon.
    fn check_expirations(&mut self) -> u64 {
        let now = now();
        let mut next = TIME_MAX;

        if self.wait_for_entry_until == 0 {
            self.wait_for_entry_until = now + 30;
        }

        let mut i = 0;
        while i < self.entries.len() {
            if self.entries[i].expiration <= now {
                // Upstream fills the hole with the last entry and re-examines
                // this slot; that reordering decides which of several matching
                // credentials a later `get` returns, so mirror it exactly.
                self.entries.swap_remove(i);
                self.wait_for_entry_until = now + 30;
            } else {
                if self.entries[i].expiration < next {
                    next = self.entries[i].expiration;
                }
                i += 1;
            }
        }

        if self.entries.is_empty() {
            if self.wait_for_entry_until <= now {
                return 0;
            }
            next = self.wait_for_entry_until;
        }

        next - now
    }

    /// `lookup_credential()` — the first entry matching `c` ignoring secrets.
    fn lookup(&self, c: &Credential) -> Option<&Entry> {
        self.entries.iter().find(|e| credential_match(c, &e.item, false))
    }

    /// `remove_credential()` — expire every match in place, so the next
    /// `check_expirations()` reaps them.
    fn remove(&mut self, c: &Credential, match_password: bool) {
        for e in &mut self.entries {
            if credential_match(c, &e.item, match_password) {
                e.expiration = 0;
            }
        }
    }

    /// `cache_credential()`.
    fn store(&mut self, c: Credential, timeout: i64) {
        let expiration = now().saturating_add(timeout.max(0) as u64);
        self.entries.push(Entry { item: c, expiration });
    }
}

// ---------------------------------------------------------------------------
// serving
// ---------------------------------------------------------------------------

/// Port of `serve_cache()`: bind, announce readiness, detach the standard
/// streams, then run the accept/expire loop until nothing is cached.
fn serve_cache(socket_path: &str, debug: bool) -> Result<ExitCode> {
    // `unix_stream_listen()` unlinks first so a stale socket from a killed
    // daemon does not make the bind fail.
    let _ = fs::remove_file(socket_path);

    let listener = match UnixListener::bind(socket_path) {
        Ok(l) => l,
        Err(e) if e.kind() == io::ErrorKind::InvalidInput => {
            // The path did not fit in `sun_path`. Upstream chdirs to the
            // directory and binds the basename; `init_socket_directory` has
            // already put us there, so binding the basename is that same move.
            let base = basename(socket_path);
            match UnixListener::bind(base) {
                Ok(l) => l,
                Err(_) => {
                    eprintln!(
                        "fatal: unable to bind to '{socket_path}': {}",
                        strerror(&e)
                    );
                    return Ok(ExitCode::from(128));
                }
            }
        }
        Err(e) => {
            eprintln!("fatal: unable to bind to '{socket_path}': {}", strerror(&e));
            return Ok(ExitCode::from(128));
        }
    };

    println!("ok");
    let _ = io::stdout().flush();
    detach_std_streams(debug);

    let code = accept_loop(listener, socket_path);

    // `delete_tempfile()` in upstream's atexit handler.
    let _ = fs::remove_file(socket_path);
    Ok(code)
}

/// Close stdout so the spawning `git credential-cache` sees EOF from its
/// `read_in_full`, and point stderr at `/dev/null` unless `--debug`.
///
/// `std` has no `dup2`, so stderr is redirected the portable way: close fd 2
/// while fd 1 is still open, then open `/dev/null`, which lands on the lowest
/// free descriptor — fd 2. Only then is fd 1 closed. Nothing else runs
/// concurrently at this point, so no other thread can claim the descriptor.
fn detach_std_streams(debug: bool) {
    use std::os::fd::FromRawFd;

    if !debug {
        unsafe { drop(fs::File::from_raw_fd(2)) };
        if let Ok(devnull) = fs::OpenOptions::new().write(true).open("/dev/null") {
            // Deliberately leaked: dropping it would close fd 2 again.
            std::mem::forget(devnull);
        }
    }
    unsafe { drop(fs::File::from_raw_fd(1)) };
}

/// The `while (serve_cache_loop(fd))` body.
///
/// `std` has no `poll`, so the blocking `accept()` runs on its own thread and
/// hands each connection over a channel; the main thread waits with
/// `recv_timeout` for exactly the interval `check_expirations()` asked for. The
/// acceptor waits for an acknowledgement before accepting again, so clients are
/// served strictly one at a time as they are upstream.
fn accept_loop(listener: UnixListener, socket_path: &str) -> ExitCode {
    let (conn_tx, conn_rx) = mpsc::channel::<io::Result<UnixStream>>();
    let (ack_tx, ack_rx) = mpsc::channel::<()>();

    std::thread::spawn(move || loop {
        let accepted = listener.accept().map(|(stream, _)| stream);
        if conn_tx.send(accepted).is_err() || ack_rx.recv().is_err() {
            return;
        }
    });

    let mut cache = Cache::default();
    loop {
        let wakeup = cache.check_expirations();
        if wakeup == 0 {
            return ExitCode::SUCCESS;
        }

        match conn_rx.recv_timeout(Duration::from_secs(wakeup)) {
            Err(RecvTimeoutError::Timeout) => continue,
            Err(RecvTimeoutError::Disconnected) => return ExitCode::SUCCESS,
            Ok(Err(e)) => {
                eprintln!("warning: accept failed: {}", strerror(&e));
                let _ = ack_tx.send(());
            }
            Ok(Ok(stream)) => {
                let outcome = serve_one_client(stream, &mut cache, socket_path);
                if let Some(code) = outcome {
                    return code;
                }
                let _ = ack_tx.send(());
            }
        }
    }
}

/// Port of `serve_one_client()`. Returns `Some(code)` when the exchange ends
/// the daemon — the `exit` action, or the `die()` inside `credential_from_url`.
///
/// Both terminating paths unlink the socket *before* returning, because
/// returning drops the client's connection and the client reads that EOF as
/// "cleanup is finished". Unlinking afterwards would let a client that has
/// already spawned a replacement daemon have the fresh socket deleted out from
/// under it — the race upstream's atexit-then-exit ordering exists to avoid.
fn serve_one_client(stream: UnixStream, cache: &mut Cache, socket_path: &str) -> Option<ExitCode> {
    let mut out = match stream.try_clone() {
        Ok(s) => s,
        Err(e) => {
            eprintln!("warning: dup failed: {}", strerror(&e));
            return None;
        }
    };
    let mut reader = BufReader::new(stream);

    let mut c = Credential::default();
    let mut action = String::new();
    let mut timeout: i64 = -1;

    match read_request(&mut reader, &mut c, &mut action, &mut timeout) {
        // `read_request` already reported; upstream explicitly ignores the error.
        Err(RequestError::Rejected) => return None,
        Err(RequestError::Die(code)) => {
            let _ = fs::remove_file(socket_path);
            return Some(code);
        }
        Ok(()) => {}
    }

    match action.as_str() {
        "get" => {
            let want_authtype = c.capa_authtype.response();
            if let Some(e) = cache.lookup(&c) {
                let item = &e.item;
                let mut buf = String::new();
                buf.push_str("capability[]=authtype\n");
                if let Some(v) = &item.username {
                    buf.push_str(&format!("username={v}\n"));
                }
                if let Some(v) = &item.password {
                    buf.push_str(&format!("password={v}\n"));
                }
                if want_authtype {
                    if let Some(v) = &item.authtype {
                        buf.push_str(&format!("authtype={v}\n"));
                    }
                    if let Some(v) = &item.credential {
                        buf.push_str(&format!("credential={v}\n"));
                    }
                }
                if item.password_expiry_utc != TIME_MAX {
                    buf.push_str(&format!(
                        "password_expiry_utc={}\n",
                        item.password_expiry_utc
                    ));
                }
                if let Some(v) = &item.oauth_refresh_token {
                    buf.push_str(&format!("oauth_refresh_token={v}\n"));
                }
                let _ = out.write_all(buf.as_bytes());
            }
            let _ = out.flush();
        }
        "exit" => {
            // Upstream exits from here rather than returning, so that the
            // atexit handler unlinks the socket *before* the process ends and
            // the client's EOF therefore means "cleanup is done".
            let _ = fs::remove_file(socket_path);
            return Some(ExitCode::SUCCESS);
        }
        "erase" => cache.remove(&c, true),
        "store" => {
            if timeout < 0 {
                eprintln!("warning: cache client didn't specify a timeout");
            } else if (c.username.is_none() || c.password.is_none())
                && c.authtype.is_none()
                && c.credential.is_none()
            {
                eprintln!("warning: cache client gave us a partial credential");
            } else if c.ephemeral {
                eprintln!("warning: not storing ephemeral credential");
            } else {
                cache.remove(&c, false);
                cache.store(c, timeout);
            }
        }
        other => eprintln!("warning: cache client sent unknown action: {other}"),
    }

    None
}

/// Why a request was not acted upon.
enum RequestError {
    /// Reported on stderr and dropped, as upstream drops it.
    Rejected,
    /// `die()` was reached mid-request; the daemon must exit with this code.
    Die(ExitCode),
}

/// Port of `read_request()` followed by `credential_read(…, CREDENTIAL_OP_HELPER)`.
///
/// The first two lines are read with `strbuf_getline_lf` semantics (a stray CR
/// is data), every later line with `strbuf_getline` semantics (CRLF tolerated).
fn read_request(
    reader: &mut BufReader<UnixStream>,
    c: &mut Credential,
    action: &mut String,
    timeout: &mut i64,
) -> Result<(), RequestError> {
    let line = read_line_lf(reader);
    let Some(p) = line.strip_prefix("action=") else {
        eprintln!("error: client sent bogus action line: {line}");
        return Err(RequestError::Rejected);
    };
    action.push_str(p);

    let line = read_line_lf(reader);
    let Some(p) = line.strip_prefix("timeout=") else {
        eprintln!("error: client sent bogus timeout line: {line}");
        return Err(RequestError::Rejected);
    };
    *timeout = atoi(p);

    // `credential_set_all_capabilities(c, CREDENTIAL_OP_INITIAL)`.
    c.capa_authtype.request_initial = true;
    c.capa_state.request_initial = true;

    credential_read(reader, c)
}

/// Port of `credential_read()` for `CREDENTIAL_OP_HELPER`.
///
/// Unknown keys are skipped on purpose: upstream keeps them silent so a newer
/// client can talk to an older daemon without either side breaking.
fn credential_read(
    reader: &mut BufReader<UnixStream>,
    c: &mut Credential,
) -> Result<(), RequestError> {
    loop {
        let Some(line) = read_line_crlf(reader) else {
            return Ok(()); // EOF
        };
        if line.is_empty() {
            return Ok(());
        }
        let Some((key, value)) = line.split_once('=') else {
            eprintln!("warning: invalid credential line: {line}");
            return Err(RequestError::Rejected);
        };

        match key {
            "username" => c.username = Some(value.to_owned()),
            "password" => c.password = Some(value.to_owned()),
            "credential" => c.credential = Some(value.to_owned()),
            "protocol" => c.protocol = Some(value.to_owned()),
            "host" => c.host = Some(value.to_owned()),
            "path" => c.path = Some(value.to_owned()),
            "ephemeral" => c.ephemeral = config_bool(value),
            "authtype" => c.authtype = Some(value.to_owned()),
            "oauth_refresh_token" => c.oauth_refresh_token = Some(value.to_owned()),
            "capability[]" => match value {
                "authtype" => c.capa_authtype.request_helper = true,
                "state" => c.capa_state.request_helper = true,
                _ => {}
            },
            "password_expiry_utc" => c.password_expiry_utc = parse_timestamp(value),
            "url" => {
                if credential_from_url(c, value).is_err() {
                    eprintln!("fatal: credential url cannot be parsed: {value}");
                    return Err(RequestError::Die(ExitCode::from(128)));
                }
            }
            // `wwwauth[]`, `state[]`, `continue`, `quit` and anything else are
            // read by upstream into fields this daemon never consults.
            _ => {}
        }
    }
}

/// `strbuf_getline_lf`: strip only a trailing LF. EOF yields an empty string,
/// which is what upstream then reports as a bogus line.
fn read_line_lf(reader: &mut BufReader<UnixStream>) -> String {
    let mut buf = Vec::new();
    if reader.read_until(b'\n', &mut buf).is_err() {
        return String::new();
    }
    if buf.last() == Some(&b'\n') {
        buf.pop();
    }
    String::from_utf8_lossy(&buf).into_owned()
}

/// `strbuf_getline`: strip a trailing LF and then a trailing CR. Returns `None`
/// at EOF with nothing read, which ends the loop.
fn read_line_crlf(reader: &mut BufReader<UnixStream>) -> Option<String> {
    let mut buf = Vec::new();
    match reader.read_until(b'\n', &mut buf) {
        Ok(0) | Err(_) => return None,
        Ok(_) => {}
    }
    if buf.last() == Some(&b'\n') {
        buf.pop();
        if buf.last() == Some(&b'\r') {
            buf.pop();
        }
    }
    Some(String::from_utf8_lossy(&buf).into_owned())
}

/// `atoi()`: leading whitespace, an optional sign, then as many digits as parse.
/// Anything else yields 0.
fn atoi(s: &str) -> i64 {
    let s = s.trim_start();
    let (neg, digits) = match s.strip_prefix('-') {
        Some(r) => (true, r),
        None => (false, s.strip_prefix('+').unwrap_or(s)),
    };
    let end = digits
        .find(|ch: char| !ch.is_ascii_digit())
        .unwrap_or(digits.len());
    let v: i64 = digits[..end].parse().unwrap_or(0);
    if neg {
        -v
    } else {
        v
    }
}

/// `parse_timestamp(value, NULL, 10)`, i.e. `strtoumax`: leading whitespace,
/// then as many decimal digits as parse. Upstream maps both a zero result and
/// an out-of-range one to `TIME_MAX`, which is the "never expires" sentinel —
/// so garbage and `0` alike mean "no expiry", not "expired long ago".
fn parse_timestamp(value: &str) -> u64 {
    let digits = value.trim_start();
    let end = digits
        .find(|ch: char| !ch.is_ascii_digit())
        .unwrap_or(digits.len());
    match digits[..end].parse::<u64>() {
        Ok(0) | Err(_) => TIME_MAX,
        Ok(v) => v,
    }
}

/// `git_config_bool()` for the handful of boolean protocol keys.
///
/// Deviation: upstream dies on a non-numeric value; here it reads as false.
fn config_bool(value: &str) -> bool {
    match value.to_ascii_lowercase().as_str() {
        "yes" | "on" | "true" => true,
        "no" | "off" | "false" | "" => false,
        other => other.parse::<i64>().is_ok_and(|v| v != 0),
    }
}

// ---------------------------------------------------------------------------
// url parsing
// ---------------------------------------------------------------------------

/// Port of `credential_from_url_1(c, url, allow_partial_url = 0, quiet = 0)`.
///
/// Note the leading `credential_clear()`: a `url=` line discards everything read
/// so far, capabilities included, so a `capability[]=authtype` sent before it
/// does not reach the response.
fn credential_from_url(c: &mut Credential, url: &str) -> Result<(), ()> {
    *c = Credential::default();

    let Some(proto_end) = url.find("://") else {
        eprintln!("warning: url has no scheme: {url}");
        return Err(());
    };
    if proto_end == 0 {
        eprintln!("warning: url has no scheme: {url}");
        return Err(());
    }

    let cp = &url[proto_end + 3..];
    let at = cp.find('@');
    let colon = cp.find(':');
    // A query or fragment marker before the slash also ends the host portion.
    let slash = cp.find(['/', '?', '#']).unwrap_or(cp.len());

    let host_start = match (at, colon) {
        // (1) proto://<host>/…
        (None, _) => 0,
        (Some(at), _) if slash <= at => 0,
        // (2) proto://<user>@<host>/…
        (Some(at), None) => {
            c.username = Some(url_decode_mem(&cp[..at]));
            at + 1
        }
        (Some(at), Some(colon)) if at <= colon => {
            c.username = Some(url_decode_mem(&cp[..at]));
            at + 1
        }
        // (3) proto://<user>:<pass>@<host>/…
        (Some(at), Some(colon)) => {
            c.username = Some(url_decode_mem(&cp[..colon]));
            c.password = Some(url_decode_mem(&cp[colon + 1..at]));
            at + 1
        }
    };

    c.protocol = Some(url[..proto_end].to_owned());
    c.host = Some(url_decode_mem(&cp[host_start..slash]));

    // Trim leading and trailing slashes from the path.
    let rest = cp[slash..].trim_start_matches('/');
    if !rest.is_empty() {
        c.path = Some(url_decode_mem(rest).trim_end_matches('/').to_owned());
    }

    // A newline anywhere would let a value forge extra protocol lines.
    for (name, v) in [
        ("username", &c.username),
        ("password", &c.password),
        ("protocol", &c.protocol),
        ("host", &c.host),
        ("path", &c.path),
    ] {
        if v.as_deref().is_some_and(|s| s.contains('\n')) {
            eprintln!("warning: url contains a newline in its {name} component: {url}");
            return Err(());
        }
    }

    Ok(())
}

/// Port of `url_decode_mem()`.
///
/// Two upstream quirks are load-bearing and reproduced: everything up to the
/// first colon is copied without decoding (it is assumed to be a scheme), and
/// `%00` is left literal because the decoder only accepts a strictly positive
/// byte value.
fn url_decode_mem(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());

    let mut i = match bytes.iter().position(|&b| b == b':') {
        // `url < colon`: only a non-empty prefix is skipped.
        Some(colon) if colon > 0 => {
            out.extend_from_slice(&bytes[..colon]);
            colon
        }
        _ => 0,
    };

    while i < bytes.len() {
        let c = bytes[i];
        if c == 0 {
            break;
        }
        if c == b'%' && bytes.len() - i >= 3 {
            if let Some(v) = hex2chr(&bytes[i + 1..i + 3]) {
                if v > 0 {
                    out.push(v);
                    i += 3;
                    continue;
                }
            }
        }
        out.push(c);
        i += 1;
    }

    String::from_utf8_lossy(&out).into_owned()
}

/// `hex2chr()`: two hex digits to a byte, or `None` if either digit is invalid.
fn hex2chr(pair: &[u8]) -> Option<u8> {
    let hi = (pair[0] as char).to_digit(16)?;
    let lo = (pair[1] as char).to_digit(16)?;
    Some((hi * 16 + lo) as u8)
}

// ---------------------------------------------------------------------------
// helpers
// ---------------------------------------------------------------------------

/// The last path component, for the over-long-`sun_path` bind fallback.
fn basename(path: &str) -> &str {
    let trimmed = path.trim_end_matches('/');
    match trimmed.rfind('/') {
        Some(i) => &trimmed[i + 1..],
        None => trimmed,
    }
}

/// The bare `strerror` text, without Rust's ` (os error N)` suffix, so the
/// `fatal:` lines read exactly as git's `die_errno` ones do.
fn strerror(e: &io::Error) -> String {
    let text = e.to_string();
    match text.rfind(" (os error ") {
        Some(i) => text[..i].to_string(),
        None => text,
    }
}

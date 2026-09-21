use anyhow::Result;
use std::io::{Read, Write};
use std::path::PathBuf;
use std::process::ExitCode;

/// `git http-backend` — the CGI program that serves a repository over HTTP.
///
/// It takes no command-line options (stock `cmd_main` ignores `argc`/`argv`
/// entirely, so extra arguments are ignored here too); every input arrives in
/// the CGI environment: `REQUEST_METHOD`, `PATH_INFO` + `GIT_PROJECT_ROOT` (or
/// `PATH_TRANSLATED`), `QUERY_STRING`, `CONTENT_TYPE`, `REMOTE_USER`,
/// `SERVER_PROTOCOL`, `GIT_HTTP_EXPORT_ALL`. Like stock git it always exits 0 —
/// the HTTP status lives in the `Status:` CGI header, not the exit code.
///
/// Ported faithfully (headers, bodies and stderr text are byte-identical to
/// stock git for these):
///   * URL translation (`getdir`) incl. the `daemon_avoid_alias` rejection, and
///     the three `die` paths that emit `Status: 500` with `fatal:` on stderr.
///   * Route dispatch over git's `services[]` table, `405 Method Not Allowed`
///     (on `SERVER_PROTOCOL: HTTP/1.1`) / `400 Bad Request` otherwise, and the
///     `404` for an unmatched path, a non-repository, or an unexported one.
///   * `http.getanyfile` gating (`403 Forbidden`).
///   * The dumb-HTTP GET routes: `/HEAD`, `/info/refs` (no `service=`),
///     `/objects/info/alternates`, `/objects/info/http-alternates`,
///     `/objects/info/packs`, loose objects, and `pack-*.{pack,idx}`.
///   * Service selection for the smart routes, so the `403` answers for an
///     unknown, disabled, or unauthenticated service are exact, plus the `415
///     Unsupported Media Type` answer for a POST with the wrong `Content-Type`.
///   * The smart-HTTP routes themselves. `GET /info/refs?service=…` writes the
///     `application/x-git-<svc>-advertisement` header, the `# service=git-<svc>`
///     banner and flush that a v0/v1 client expects (and that a v2 one does not
///     get), then runs `<svc> --http-backend-info-refs .`; `POST
///     /git-{upload-pack,upload-archive,receive-pack}` writes the
///     `…-result` header and runs `<svc> --stateless-rpc .`, with the request
///     body fed in by `run_service` — buffered whole for `upload-pack`,
///     streamed for the others, inflated first when the body arrived
///     `Content-Encoding: gzip`, and refused past `http.maxrequestbuffer` /
///     `GIT_HTTP_MAX_REQUEST_BUFFER`. `HTTP_GIT_PROTOCOL` becomes the child's
///     `GIT_PROTOCOL`, so a v2 client is served v2.
///   * The CGI variables git tests for NULL rather than for a value, where a
///     set-but-empty one is not the same as an absent one: `REQUEST_METHOD=`
///     reaches the route table and comes back a `400`, `GIT_HTTP_EXPORT_ALL=`
///     exports, and `GIT_HTTP_MAX_REQUEST_BUFFER=` is a parse failure and so a
///     `500`.
///
/// NOT ported:
///   * A `~user` project root (git's `interpolate_path` in `enter_repo`).
///
/// Known, documented deviations: the repository-ownership check that stock
/// `enter_repo` performs is not applied, and multiple local packs are ordered
/// by `.idx` mtime descending then name — git's `sort_pack` leaves the
/// equal-mtime order up to `readdir`, so it is unspecified there in stock too.
pub fn http_backend(_args: &[String]) -> Result<ExitCode> {
    let mut hdr = Headers::default();

    // cmd_main: REQUEST_METHOD is mandatory; HEAD is served exactly like GET
    // (the web server drops the body).
    // `getenv` is checked for NULL only (http-backend.c:777), so `REQUEST_METHOD=`
    // is a method that matches no route rather than a missing one: the request
    // reaches the route table and comes back out as `bad_request`.
    let Some(mut method) = std::env::var("REQUEST_METHOD").ok() else {
        return Ok(die(&mut hdr, "No REQUEST_METHOD from server"));
    };
    if method == "HEAD" {
        method = "GET".into();
    }

    let dir = match getdir() {
        Ok(d) => d,
        Err(msg) => return Ok(die(&mut hdr, &msg)),
    };

    // Route lookup. Every pattern in git's services[] table is end-anchored and
    // of fixed shape, so a suffix test reproduces `regexec`'s leftmost match.
    let mut matched: Option<(&ServiceCmd, String, String)> = None;
    for cmd in SERVICES {
        let Some(start) = cmd.match_start(&dir) else {
            continue;
        };
        if method != cmd.method {
            return Ok(bad_request(&mut hdr, cmd.method));
        }
        // git keeps the text after the matched '/' as the handler argument and
        // truncates `dir` at the match, leaving the repository path behind.
        matched = Some((cmd, dir[start + 1..].to_string(), dir[..start].to_string()));
        break;
    }
    let Some((cmd, arg, repo_path)) = matched else {
        return Ok(not_found(&mut hdr, &format!("Request not supported: '{dir}'")));
    };

    let Some(git_dir) = enter_repo(&repo_path) else {
        return Ok(not_found(
            &mut hdr,
            &format!("Not a git repository: '{repo_path}'"),
        ));
    };
    // http-backend.c:814 tests `getenv(...)` for NULL, not for a value, so
    // `GIT_HTTP_EXPORT_ALL=` exports the repository just as `=1` does.
    if std::env::var_os("GIT_HTTP_EXPORT_ALL").is_none()
        && !git_dir.join("git-daemon-export-ok").exists()
    {
        return Ok(not_found(
            &mut hdr,
            &format!("Repository not exported: '{repo_path}'"),
        ));
    }
    let mut repo = match gix::open(&git_dir) {
        Ok(r) => r,
        Err(_) => {
            return Ok(not_found(
                &mut hdr,
                &format!("Not a git repository: '{repo_path}'"),
            ))
        }
    };
    // The dumb ref routes are namespaced in git, unlike the ref-listing builtins:
    // `http-backend.c:569` iterates with `.namespace = get_git_namespace()`,
    // `:523` writes each name through `strip_namespace(ref->name)`, and `:604`
    // resolves HEAD via `refs_head_ref_namespaced()` with `:591` stripping the
    // symref target. Installing the namespace on the ref store gives all three,
    // since `gix-ref` prefixes on lookup and strips on the way out. The object and
    // pack routes are plain file serving and are unaffected either way.
    crate::namespace::apply(&mut repo)?;
    let repo = repo;
    let cfg = match HttpConfig::read(&repo) {
        Ok(cfg) => cfg,
        Err(msg) => return Ok(die(&mut hdr, &msg)),
    };

    // http-backend.c:822-824: the `Git-Protocol` request header reaches the
    // service as `GIT_PROTOCOL`. `setenv(..., 0)` does not overwrite, so an
    // environment the server set itself wins.
    if let Some(proto) = std::env::var_os("HTTP_GIT_PROTOCOL") {
        if std::env::var_os("GIT_PROTOCOL").is_none() {
            std::env::set_var("GIT_PROTOCOL", proto);
        }
    }

    match cmd.imp {
        Imp::Head => get_head(&mut hdr, &repo, &cfg),
        Imp::InfoRefs => get_info_refs(&mut hdr, &repo, &cfg, &git_dir),
        Imp::TextFile => get_text_file(&mut hdr, &repo, &cfg, &arg),
        Imp::InfoPacks => get_info_packs(&mut hdr, &repo, &cfg),
        Imp::LooseObject => get_local_file(
            &mut hdr,
            &repo,
            &cfg,
            &arg,
            "application/x-git-loose-object",
            Cache::Forever,
        ),
        Imp::PackFile => get_local_file(
            &mut hdr,
            &repo,
            &cfg,
            &arg,
            "application/x-git-packed-objects",
            Cache::Forever,
        ),
        Imp::IdxFile => get_local_file(
            &mut hdr,
            &repo,
            &cfg,
            &arg,
            "application/x-git-packed-objects-toc",
            Cache::Forever,
        ),
        Imp::ServiceRpc => service_rpc(&mut hdr, &cfg, &arg, &git_dir),
    }
}

// ---------------------------------------------------------------------------
// CGI header accumulation
// ---------------------------------------------------------------------------

/// git buffers every header into one `strbuf` and flushes it in `end_headers`.
/// The ordering quirks that follow from that are observable — e.g. a `403` on
/// `/info/refs?service=…` emits the no-cache block, then `Status:`, then the
/// no-cache block again — so the buffer is modelled the same way here.
#[derive(Default)]
struct Headers(String);

impl Headers {
    fn status(&mut self, code: u16, msg: &str) {
        self.0.push_str(&format!("Status: {code} {msg}\r\n"));
    }

    fn str(&mut self, name: &str, value: &str) {
        self.0.push_str(&format!("{name}: {value}\r\n"));
    }

    fn int(&mut self, name: &str, value: u64) {
        self.0.push_str(&format!("{name}: {value}\r\n"));
    }

    fn date(&mut self, name: &str, when: i64) {
        let value = rfc2822(when);
        self.str(name, &value);
    }

    fn nocache(&mut self) {
        self.str("Expires", "Fri, 01 Jan 1980 00:00:00 GMT");
        self.str("Pragma", "no-cache");
        self.str("Cache-Control", "no-cache, max-age=0, must-revalidate");
    }

    fn cache_forever(&mut self) {
        let now = now_secs();
        self.date("Date", now);
        self.date("Expires", now + 31_536_000);
        self.str("Cache-Control", "public, max-age=31536000");
    }

    /// Terminate and flush the header block, consuming the buffer.
    fn end(&mut self) {
        self.0.push_str("\r\n");
        write_stdout(self.0.as_bytes());
        self.0.clear();
    }
}

/// Which cache-policy header block a file route prepends.
enum Cache {
    None,
    Forever,
}

fn write_stdout(bytes: &[u8]) {
    let mut out = std::io::stdout().lock();
    // git uses write_or_die; a dead pipe is not something we can report anyway.
    let _ = out.write_all(bytes);
    let _ = out.flush();
}

/// `not_found`: `404` + no-cache, message on stderr, exit 0.
fn not_found(hdr: &mut Headers, err: &str) -> ExitCode {
    hdr.status(404, "Not Found");
    hdr.nocache();
    hdr.end();
    eprintln!("{err}");
    ExitCode::SUCCESS
}

/// `forbidden`: `403` + no-cache, message on stderr, exit 0.
fn forbidden(hdr: &mut Headers, err: &str) -> ExitCode {
    hdr.status(403, "Forbidden");
    hdr.nocache();
    hdr.end();
    eprintln!("{err}");
    ExitCode::SUCCESS
}

/// `die_webcgi`: the `fatal:` line on stderr, then `500` + no-cache, exit 0.
fn die(hdr: &mut Headers, err: &str) -> ExitCode {
    eprintln!("fatal: {err}");
    hdr.status(500, "Internal Server Error");
    hdr.nocache();
    hdr.end();
    ExitCode::SUCCESS
}

/// `bad_request`: `405` with an `Allow:` header when the server spoke HTTP/1.1,
/// `400` otherwise.
fn bad_request(hdr: &mut Headers, method: &str) -> ExitCode {
    if env("SERVER_PROTOCOL").as_deref() == Some("HTTP/1.1") {
        hdr.status(405, "Method Not Allowed");
        hdr.str("Allow", if method == "GET" { "GET, HEAD" } else { method });
    } else {
        hdr.status(400, "Bad Request");
    }
    hdr.nocache();
    hdr.end();
    ExitCode::SUCCESS
}

/// `send_strbuf`: `Content-Length` + `Content-Type`, then the body.
fn send_buf(hdr: &mut Headers, content_type: &str, body: &[u8]) -> ExitCode {
    hdr.int("Content-Length", body.len() as u64);
    hdr.str("Content-Type", content_type);
    hdr.end();
    write_stdout(body);
    ExitCode::SUCCESS
}

// ---------------------------------------------------------------------------
// Route table
// ---------------------------------------------------------------------------

enum Imp {
    Head,
    InfoRefs,
    TextFile,
    InfoPacks,
    LooseObject,
    PackFile,
    IdxFile,
    ServiceRpc,
}

/// One row of git's `services[]` table. `shape` mirrors the row's regex; every
/// stock pattern is `…$`-anchored with a fixed length, which `Shape` encodes.
struct ServiceCmd {
    method: &'static str,
    shape: Shape,
    imp: Imp,
}

/// The end-anchored shape a URL path must have for a route to fire.
enum Shape {
    /// A literal suffix, e.g. `/HEAD$`.
    Literal(&'static str),
    /// `/objects/<2 hex>/<n hex>$`.
    Loose(usize),
    /// `/objects/pack/pack-<n hex><ext>$`.
    Pack(usize, &'static str),
}

impl ServiceCmd {
    /// Byte offset in `dir` where this route's pattern matches, if it does.
    /// Every pattern is pure ASCII, so a byte offset that matches is always a
    /// character boundary of the surrounding UTF-8 string.
    fn match_start(&self, dir: &str) -> Option<usize> {
        let b = dir.as_bytes();
        match self.shape {
            Shape::Literal(lit) => {
                let start = b.len().checked_sub(lit.len())?;
                (&b[start..] == lit.as_bytes()).then_some(start)
            }
            Shape::Loose(n) => {
                const PREFIX: &[u8] = b"/objects/";
                let start = b.len().checked_sub(PREFIX.len() + 2 + 1 + n)?;
                let tail = &b[start..];
                (&tail[..PREFIX.len()] == PREFIX
                    && is_hex(&tail[PREFIX.len()..PREFIX.len() + 2])
                    && tail[PREFIX.len() + 2] == b'/'
                    && is_hex(&tail[PREFIX.len() + 3..]))
                .then_some(start)
            }
            Shape::Pack(n, ext) => {
                const PREFIX: &[u8] = b"/objects/pack/pack-";
                let start = b.len().checked_sub(PREFIX.len() + n + ext.len())?;
                let tail = &b[start..];
                (&tail[..PREFIX.len()] == PREFIX
                    && is_hex(&tail[PREFIX.len()..PREFIX.len() + n])
                    && &tail[PREFIX.len() + n..] == ext.as_bytes())
                .then_some(start)
            }
        }
    }
}

/// Lower-case hex only, matching the `[0-9a-f]` character class git uses.
fn is_hex(bytes: &[u8]) -> bool {
    !bytes.is_empty()
        && bytes
            .iter()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(b))
}

/// git's `services[]`, in order. The `{38}`/`{62}` and `{40}`/`{64}` pairs are
/// the SHA-1 and SHA-256 spellings of the same route.
const SERVICES: &[ServiceCmd] = &[
    ServiceCmd { method: "GET", shape: Shape::Literal("/HEAD"), imp: Imp::Head },
    ServiceCmd { method: "GET", shape: Shape::Literal("/info/refs"), imp: Imp::InfoRefs },
    ServiceCmd { method: "GET", shape: Shape::Literal("/objects/info/alternates"), imp: Imp::TextFile },
    ServiceCmd { method: "GET", shape: Shape::Literal("/objects/info/http-alternates"), imp: Imp::TextFile },
    ServiceCmd { method: "GET", shape: Shape::Literal("/objects/info/packs"), imp: Imp::InfoPacks },
    ServiceCmd { method: "GET", shape: Shape::Loose(38), imp: Imp::LooseObject },
    ServiceCmd { method: "GET", shape: Shape::Loose(62), imp: Imp::LooseObject },
    ServiceCmd { method: "GET", shape: Shape::Pack(40, ".pack"), imp: Imp::PackFile },
    ServiceCmd { method: "GET", shape: Shape::Pack(64, ".pack"), imp: Imp::PackFile },
    ServiceCmd { method: "GET", shape: Shape::Pack(40, ".idx"), imp: Imp::IdxFile },
    ServiceCmd { method: "GET", shape: Shape::Pack(64, ".idx"), imp: Imp::IdxFile },
    ServiceCmd { method: "POST", shape: Shape::Literal("/git-upload-pack"), imp: Imp::ServiceRpc },
    ServiceCmd { method: "POST", shape: Shape::Literal("/git-upload-archive"), imp: Imp::ServiceRpc },
    ServiceCmd { method: "POST", shape: Shape::Literal("/git-receive-pack"), imp: Imp::ServiceRpc },
];

// ---------------------------------------------------------------------------
// URL translation and repository entry
// ---------------------------------------------------------------------------

fn env(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|v| !v.is_empty())
}

/// git's `getdir`: `GIT_PROJECT_ROOT` + `PATH_INFO`, else `PATH_TRANSLATED`.
/// The `Err` payload is the exact `die` text.
fn getdir() -> Result<String, String> {
    if let Some(root) = env("GIT_PROJECT_ROOT") {
        let Some(pathinfo) = env("PATH_INFO") else {
            return Err("GIT_PROJECT_ROOT is set but PATH_INFO is not".into());
        };
        if !daemon_avoid_alias(&pathinfo) {
            return Err(format!("'{pathinfo}': aliased"));
        }
        let mut buf = root;
        if !buf.ends_with('/') {
            buf.push('/');
        }
        buf.push_str(pathinfo.strip_prefix('/').unwrap_or(&pathinfo));
        Ok(buf)
    } else if let Some(path) = env("PATH_TRANSLATED") {
        Ok(path)
    } else {
        Err("No GIT_PROJECT_ROOT or PATH_TRANSLATED from server".into())
    }
}

/// Port of `daemon_avoid_alias` (path.c): reject `//`, `/./`, `/../`, `/.` and
/// `/..`, and any path not starting with `/` or `~`. Returns true when safe.
fn daemon_avoid_alias(p: &str) -> bool {
    let bytes = p.as_bytes();
    if bytes.first() != Some(&b'/') && bytes.first() != Some(&b'~') {
        return false;
    }
    // `sl` stays set from a '/' for as long as only dots follow it.
    let (mut sl, mut ndot) = (true, 0usize);
    for i in 1..=bytes.len() {
        let ch = bytes.get(i).copied().unwrap_or(0);
        if sl {
            match ch {
                b'.' => ndot += 1,
                b'/' => {
                    if ndot < 3 {
                        return false;
                    }
                    ndot = 0;
                }
                0 => return !(0 < ndot && ndot < 3),
                _ => {
                    sl = false;
                    ndot = 0;
                }
            }
        } else if ch == 0 {
            return true;
        } else if ch == b'/' {
            sl = true;
            ndot = 0;
        }
    }
    true
}

/// git's `enter_repo(path, 0)`, minus the `~user` interpolation and the
/// ownership check: try each suffix in order and return the resolved git
/// directory (following a `gitdir:` file) for the first candidate that is one.
fn enter_repo(path: &str) -> Option<PathBuf> {
    let mut len = path.len();
    while len > 1 && path.as_bytes()[len - 1] == b'/' {
        len -= 1;
    }
    let base = &path[..len];
    if base.is_empty() || base.starts_with('~') {
        return None;
    }

    for suffix in ["/.git", "", ".git/.git", ".git"] {
        let candidate = PathBuf::from(format!("{base}{suffix}"));
        let Ok(meta) = std::fs::metadata(&candidate) else {
            continue;
        };
        if meta.is_file() {
            // A `gitdir:` pointer file, as used by worktrees and submodules.
            let resolved = gix::discover::path::from_gitdir_file(&candidate).ok()?;
            return gix::discover::is_git(&resolved).is_ok().then_some(resolved);
        }
        if meta.is_dir() && gix::discover::is_git(&candidate).is_ok() {
            return Some(candidate);
        }
    }
    None
}

// ---------------------------------------------------------------------------
// Service configuration and selection
// ---------------------------------------------------------------------------

/// The `http.*` knobs git's `http_config` reads. `None` for a service means
/// "not configured", which defers to the built-in default.
struct HttpConfig {
    getanyfile: bool,
    upload_pack: Option<bool>,
    receive_pack: Option<bool>,
    upload_archive: Option<bool>,
    /// http-backend.c:31's `max_request_buffer`, after `http.maxrequestbuffer`
    /// (:255) and then `GIT_HTTP_MAX_REQUEST_BUFFER` (:820) have had their say.
    max_request_buffer: u64,
}

impl HttpConfig {
    /// `Err` carries a `die` text: both buffer-size readers reject a value they
    /// cannot parse rather than falling back to the default.
    fn read(repo: &gix::Repository) -> Result<Self, String> {
        let cfg = repo.config_snapshot();
        let mut max_request_buffer = 10 * 1024 * 1024;

        // `repo_config_get_ulong` (http-backend.c:255) goes through
        // `git_config_ulong`, so a bad value is `die_bad_number`, naming the file
        // it came from. Last value wins, as for every `repo_config_get_*`.
        if let Some(v) = crate::config::walk_config(repo)
            .into_iter()
            .filter(|v| v.key == "http.maxrequestbuffer")
            .next_back()
        {
            let raw = v.value.as_deref().unwrap_or("");
            max_request_buffer = crate::config::parse_config_ulong(raw).map_err(|reason| {
                format!(
                    "bad numeric config value '{raw}' for 'http.maxrequestbuffer'{}: {reason}",
                    v.origin.bad_number_clause()
                )
            })?;
        }

        // `git_env_ulong` (http-backend.c:820) overrides the config, and dies
        // naming only the variable when the value does not parse — which
        // includes an empty one, since it checks for NULL alone.
        if let Some(raw) = std::env::var("GIT_HTTP_MAX_REQUEST_BUFFER").ok() {
            max_request_buffer = crate::config::parse_config_ulong(&raw)
                .map_err(|_| "failed to parse GIT_HTTP_MAX_REQUEST_BUFFER".to_string())?;
        }

        Ok(HttpConfig {
            getanyfile: cfg.boolean("http.getanyfile").unwrap_or(true),
            upload_pack: cfg.boolean("http.uploadpack"),
            receive_pack: cfg.boolean("http.receivepack"),
            upload_archive: cfg.boolean("http.uploadarchive"),
            max_request_buffer,
        })
    }
}

/// `select_getanyfile`: `Err` carries the ready-made `403`.
fn select_getanyfile(hdr: &mut Headers, cfg: &HttpConfig) -> Result<(), ExitCode> {
    if cfg.getanyfile {
        Ok(())
    } else {
        Err(forbidden(hdr, "Unsupported service: getanyfile"))
    }
}

/// `select_service`: map `git-<name>` to a service and apply its enablement.
/// `upload-pack` is enabled by default; `receive-pack` and `upload-archive`
/// carry git's negative default, meaning "enabled only for an authenticated
/// request" — i.e. only when the server set `REMOTE_USER`.
fn select_service(
    hdr: &mut Headers,
    cfg: &HttpConfig,
    name: &str,
) -> Result<&'static str, ExitCode> {
    let Some(svc_name) = name.strip_prefix("git-") else {
        return Err(forbidden(hdr, &format!("Unsupported service: '{name}'")));
    };
    let (svc, configured, default_on) = match svc_name {
        "upload-pack" => ("upload-pack", cfg.upload_pack, Some(true)),
        "receive-pack" => ("receive-pack", cfg.receive_pack, None),
        "upload-archive" => ("upload-archive", cfg.upload_archive, None),
        _ => return Err(forbidden(hdr, &format!("Unsupported service: '{name}'"))),
    };
    // A negative built-in default means "enabled iff the request is authenticated".
    let enabled = configured.unwrap_or_else(|| default_on.unwrap_or_else(|| env("REMOTE_USER").is_some()));
    if !enabled {
        return Err(forbidden(hdr, &format!("Service not enabled: '{svc}'")));
    }
    Ok(svc)
}

// ---------------------------------------------------------------------------
// Route handlers
// ---------------------------------------------------------------------------

/// `get_head`: `ref: <fully resolved name>` for a symbolic HEAD that resolves,
/// the raw object id for a detached one, and an empty body for an unborn one.
fn get_head(hdr: &mut Headers, repo: &gix::Repository, cfg: &HttpConfig) -> Result<ExitCode> {
    if let Err(code) = select_getanyfile(hdr, cfg) {
        return Ok(code);
    }
    let mut body = String::new();
    if let Ok(head) = repo.find_reference("HEAD") {
        match head.target() {
            gix::refs::TargetRef::Symbolic(_) => {
                // An unborn HEAD resolves to nothing and yields an empty body.
                if let Some(name) = resolve_symref_chain(repo, "HEAD") {
                    body = format!("ref: {name}\n");
                }
            }
            gix::refs::TargetRef::Object(id) => body = format!("{}\n", id.to_hex()),
        }
    }
    Ok(send_buf(hdr, "text/plain", body.as_bytes()))
}

/// Follow a symref chain to the last name that still resolves to an object,
/// mirroring `refs_resolve_ref_unsafe(..., RESOLVE_REF_READING, ...)`.
fn resolve_symref_chain(repo: &gix::Repository, start: &str) -> Option<String> {
    use gix::bstr::ByteSlice;
    let mut name = start.to_owned();
    // git gives up after SYMREF_MAXDEPTH (5) hops.
    for _ in 0..5 {
        let r = repo.find_reference(name.as_str()).ok()?;
        match r.target() {
            gix::refs::TargetRef::Symbolic(target) => {
                name = target.as_bstr().to_str().ok()?.to_owned();
            }
            gix::refs::TargetRef::Object(_) => return Some(name),
        }
    }
    None
}

/// `get_info_refs`: the dumb `<oid>\t<ref>` listing, with a `^{}` line after
/// every tag object. The smart form (`?service=…`) is not ported.
fn get_info_refs(
    hdr: &mut Headers,
    repo: &gix::Repository,
    cfg: &HttpConfig,
    git_dir: &std::path::Path,
) -> Result<ExitCode> {
    hdr.nocache();

    if let Some(service_name) = query_parameter("service") {
        let svc = match select_service(hdr, cfg, &service_name) {
            Ok(s) => s,
            Err(code) => return Ok(code),
        };
        hdr.str(
            "Content-Type",
            &format!("application/x-git-{svc}-advertisement"),
        );
        hdr.end();

        // http-backend.c:559-562: v0 and v1 clients get the `# service=` banner
        // and a flush ahead of the advertisement; a v2 client gets neither,
        // because `serve.c` opens with its own capability list.
        if super::upload_pack::protocol_version_from_env() != 2 {
            let mut banner = Vec::new();
            pkt_line(&mut banner, format!("# service=git-{svc}\n").as_bytes());
            banner.extend_from_slice(b"0000");
            write_stdout(&banner);
        }

        return Ok(run_service(
            &[svc, "--http-backend-info-refs", "."],
            false,
            git_dir,
            cfg.max_request_buffer,
        ));
    }

    if let Err(code) = select_getanyfile(hdr, cfg) {
        return Ok(code);
    }

    use gix::bstr::ByteSlice;
    let mut names: Vec<String> = Vec::new();
    for r in repo.references()?.all()? {
        let Ok(r) = r else { continue };
        if let Ok(name) = r.name().as_bstr().to_str() {
            names.push(name.to_owned());
        }
    }
    names.sort();

    let mut body = String::new();
    for name in names {
        let Ok(mut r) = repo.find_reference(name.as_str()) else {
            continue;
        };
        // A ref whose object is missing is skipped entirely, as `parse_object`
        // returning NULL makes git's callback bail before appending anything.
        let Ok(id) = r.follow_to_object() else { continue };
        let oid = id.detach();
        let Ok(obj) = repo.find_object(oid) else { continue };
        let is_tag = obj.kind == gix::object::Kind::Tag;
        body.push_str(&format!("{}\t{name}\n", oid.to_hex()));
        if is_tag {
            let Ok(peeled) = obj.peel_tags_to_end() else {
                continue;
            };
            body.push_str(&format!("{}\t{name}^{{}}\n", peeled.id.to_hex()));
        }
    }
    Ok(send_buf(hdr, "text/plain", body.as_bytes()))
}

/// `get_info_packs`: one `P <pack-name>` line per local pack, then a blank line.
fn get_info_packs(hdr: &mut Headers, repo: &gix::Repository, cfg: &HttpConfig) -> Result<ExitCode> {
    if let Err(code) = select_getanyfile(hdr, cfg) {
        return Ok(code);
    }

    // git derives the pack list from the `.idx` files present, discarding any
    // whose `.pack` is missing, and orders younger packs first.
    let pack_dir = repo.objects.store_ref().path().join("pack");
    let mut packs: Vec<(std::time::SystemTime, String)> = Vec::new();
    if let Ok(entries) = std::fs::read_dir(&pack_dir) {
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            let Some(stem) = name.strip_suffix(".idx") else {
                continue;
            };
            let pack_name = format!("{stem}.pack");
            if !pack_dir.join(&pack_name).exists() {
                continue;
            }
            let mtime = entry
                .metadata()
                .and_then(|m| m.modified())
                .unwrap_or(std::time::UNIX_EPOCH);
            packs.push((mtime, pack_name));
        }
    }
    packs.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));

    let mut body = String::new();
    for (_, name) in &packs {
        body.push_str(&format!("P {name}\n"));
    }
    body.push('\n');

    hdr.nocache();
    Ok(send_buf(hdr, "text/plain; charset=utf-8", body.as_bytes()))
}

/// `get_text_file`: no-cache, then the raw file as `text/plain`.
fn get_text_file(
    hdr: &mut Headers,
    repo: &gix::Repository,
    cfg: &HttpConfig,
    name: &str,
) -> Result<ExitCode> {
    get_local_file(hdr, repo, cfg, name, "text/plain", Cache::None)
}

/// `get_loose_object` / `get_pack_file` / `get_idx_file` / `get_text_file`, all
/// of which are `select_getanyfile` + a cache block + `send_local_file`.
fn get_local_file(
    hdr: &mut Headers,
    repo: &gix::Repository,
    cfg: &HttpConfig,
    name: &str,
    content_type: &str,
    cache: Cache,
) -> Result<ExitCode> {
    if let Err(code) = select_getanyfile(hdr, cfg) {
        return Ok(code);
    }
    match cache {
        Cache::None => hdr.nocache(),
        Cache::Forever => hdr.cache_forever(),
    }
    Ok(send_local_file(hdr, repo, name, content_type))
}

/// `send_local_file`: `Content-Length`, `Content-Type` and `Last-Modified`,
/// then the bytes. Every route that reaches here names a path under `objects/`,
/// which git's `repo_git_path` resolves against the object directory.
fn send_local_file(
    hdr: &mut Headers,
    repo: &gix::Repository,
    name: &str,
    content_type: &str,
) -> ExitCode {
    let path = git_path(repo, name);
    let mut file = match std::fs::File::open(&path) {
        Ok(f) => f,
        Err(err) => {
            // git reports the repository-relative path, since it has chdir'd
            // into the git directory by this point.
            return not_found(hdr, &format!("Cannot open '{name}': {}", errno_text(&err)));
        }
    };
    let meta = match file.metadata() {
        Ok(m) => m,
        Err(err) => return die(hdr, &format!("Cannot stat '{name}': {}", errno_text(&err))),
    };

    hdr.int("Content-Length", meta.len());
    hdr.str("Content-Type", content_type);
    hdr.date("Last-Modified", mtime_secs(&meta));
    hdr.end();

    let mut buf = vec![0u8; 8192];
    loop {
        match file.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => write_stdout(&buf[..n]),
            Err(err) => return die(hdr, &format!("Cannot read '{name}': {}", errno_text(&err))),
        }
    }
    ExitCode::SUCCESS
}

/// Resolve a `repo_git_path`-style relative name. Only `objects/…` names reach
/// this port, and those live in the object directory (which already accounts
/// for a linked worktree's common directory and `GIT_OBJECT_DIRECTORY`).
fn git_path(repo: &gix::Repository, name: &str) -> PathBuf {
    match name.strip_prefix("objects/") {
        Some(rest) => repo.objects.store_ref().path().join(rest),
        None => repo.common_dir().join(name),
    }
}

/// `service_rpc`: the `403` and `415` answers are exact; the RPC body itself
/// has no substrate to run against.
fn service_rpc(
    hdr: &mut Headers,
    cfg: &HttpConfig,
    service_name: &str,
    git_dir: &std::path::Path,
) -> Result<ExitCode> {
    let svc = match select_service(hdr, cfg, service_name) {
        Ok(s) => s,
        Err(code) => return Ok(code),
    };

    let accepted = format!("application/x-git-{svc}-request");
    let actual = std::env::var("CONTENT_TYPE").unwrap_or_default();
    if actual != accepted {
        hdr.status(415, "Unsupported Media Type");
        hdr.nocache();
        hdr.end();
        write_stdout(
            format!("Expected POST with Content-Type '{accepted}', but received '{actual}' instead.\n")
                .as_bytes(),
        );
        return Ok(ExitCode::SUCCESS);
    }

    hdr.nocache();
    hdr.str("Content-Type", &format!("application/x-git-{svc}-result"));
    hdr.end();

    // http-backend.c:660-663: every service but `upload-archive` is run with
    // `--stateless-rpc`, and the repository is always named `.` — the child is
    // started inside the directory `enter_repo` chose.
    let argv: Vec<&str> = if svc == "upload-archive" {
        vec![svc, "."]
    } else {
        vec![svc, "--stateless-rpc", "."]
    };
    // `rpc_service[]` (http-backend.c:42-46) sets `buffer_input` for
    // `upload-pack` alone: its request is small and has no terminating flush
    // the child can block on, so the whole body is read before the child sees
    // any of it.
    Ok(run_service(
        &argv,
        svc == "upload-pack",
        git_dir,
        cfg.max_request_buffer,
    ))
}

// ---------------------------------------------------------------------------
// Running the service (http-backend.c: run_service and its request feeders)
// ---------------------------------------------------------------------------

/// `run_service`: start the named server command as a child of this process,
/// feed it the request body, and take its exit status.
///
/// git runs it as a `git` child (`cld.git_cmd = 1`), so this spawns the zvcs
/// `git` binary rather than calling in-process: `upload-pack` and
/// `receive-pack` own their stdin and stdout for the length of the exchange,
/// and a child is the only way to hand them fd 0 and fd 1 unencumbered by the
/// header bytes this process has already written.
///
/// git closes its own fd 1 right after `start_command`
/// (http-backend.c:507) so the server sees EOF as soon as the child exits; that
/// is reproduced here, because it is what makes a later `die` emit its `fatal:`
/// line with no `Status: 500` block behind the body.
fn run_service(
    argv: &[&str],
    buffer_input: bool,
    git_dir: &std::path::Path,
    max_request_buffer: u64,
) -> ExitCode {
    let encoding = std::env::var("HTTP_CONTENT_ENCODING").unwrap_or_default();
    let gzipped = encoding == "gzip" || encoding == "x-gzip";
    let user = env("REMOTE_USER").unwrap_or_else(|| "anonymous".into());
    let host = env("REMOTE_ADDR").unwrap_or_else(|| "(none)".into());

    let req_len = match content_length() {
        Ok(len) => len,
        Err(raw) => {
            return die(
                &mut Headers::default(),
                &format!("failed to parse CONTENT_LENGTH: {raw}"),
            )
        }
    };

    let Ok(exe) = crate::hosted::git_exe() else {
        // `start_command` failing is `exit(1)` with nothing on stdout.
        return ExitCode::from(1);
    };
    let mut cld = std::process::Command::new(exe);
    cld.args(argv).current_dir(git_dir);
    // http-backend.c:492-496: the pushing user's identity, for `receive-pack`'s
    // reflog. `getenv` guards, so an identity the server set survives.
    if std::env::var_os("GIT_COMMITTER_NAME").is_none() {
        cld.env("GIT_COMMITTER_NAME", &user);
    }
    if std::env::var_os("GIT_COMMITTER_EMAIL").is_none() {
        cld.env("GIT_COMMITTER_EMAIL", format!("{user}@http.{host}"));
    }

    let feed = buffer_input || gzipped || req_len.is_some();
    cld.stdin(if feed {
        std::process::Stdio::piped()
    } else {
        std::process::Stdio::inherit()
    });
    let mut child = match cld.spawn() {
        Ok(c) => c,
        Err(_) => return ExitCode::from(1),
    };

    // http-backend.c:507's `close(1)`, and it is observable: every `die` from
    // here on is `die_webcgi`, which tries to write `Status: 500` to a fd that
    // is no longer open, so the request body is followed by the `fatal:` line
    // on stderr and nothing more on stdout. The child kept its own copy of fd 1
    // when it was spawned.
    // SAFETY: fd 1 is not touched again by this process — `write_stdout`
    // already discards its errors — and the header bytes were flushed by
    // `Headers::end` before the child started.
    unsafe {
        libc::close(1);
    }

    if feed {
        let mut sink = child.stdin.take().expect("stdin was piped");
        let stdin = std::io::stdin();
        let mut input = stdin.lock();
        let fed = if gzipped {
            inflate_request(
                &mut input,
                &mut sink,
                argv[0],
                buffer_input,
                req_len,
                max_request_buffer,
            )
        } else if buffer_input {
            copy_request(&mut input, &mut sink, argv[0], req_len, max_request_buffer)
        } else {
            pipe_fixed_length(
                &mut input,
                &mut sink,
                argv[0],
                req_len.expect("feed implies a length"),
            )
        };
        // `close(out)`: the child must see EOF before it can answer.
        drop(sink);
        if let Err(err) = fed {
            // git's `clean_on_exit` kills the child from its atexit handler.
            let _ = child.kill();
            let _ = child.wait();
            return die(&mut Headers::default(), &err);
        }
    }

    match child.wait() {
        Ok(status) if status.success() => ExitCode::SUCCESS,
        _ => ExitCode::from(1),
    }
}

/// `get_content_length`: the body length, or `None` for "read to EOF". `Err`
/// carries the unparseable value for git's `die`.
fn content_length() -> Result<Option<u64>, String> {
    let Some(raw) = env("CONTENT_LENGTH") else {
        return Ok(None);
    };
    // `git_parse_ssize_t` is the same grammar as `git_parse_ulong` bounded by
    // `ssize_t`; a negative value fails to parse rather than meaning "to EOF".
    match crate::config::parse_config_ulong(&raw) {
        Ok(v) if v <= i64::MAX as u64 => Ok(Some(v)),
        _ => Err(raw),
    }
}

/// `read_request_eof`: the whole body, refusing to grow past the cap.
fn read_request_eof(input: &mut impl Read, max_request_buffer: u64) -> Result<Vec<u8>, String> {
    let max = max_request_buffer.max(8192);
    let mut alloc = 8192usize;
    let mut buf = vec![0u8; alloc];
    let mut len = 0usize;
    loop {
        len += read_in_full(input, &mut buf[len..alloc])?;
        // A short read out of `read_in_full` is EOF.
        if len < alloc {
            buf.truncate(len);
            return Ok(buf);
        }
        if alloc as u64 == max {
            return Err(format!(
                "request was larger than our maximum size ({max}); \
                 try setting GIT_HTTP_MAX_REQUEST_BUFFER"
            ));
        }
        // git's `alloc_nr(x)`.
        alloc = (alloc + 16) * 3 / 2;
        if alloc as u64 > max {
            alloc = max as usize;
        }
        buf.resize(alloc, 0);
    }
}

/// `read_request_fixed_len`.
fn read_request_fixed_len(
    input: &mut impl Read,
    req_len: u64,
    max_request_buffer: u64,
) -> Result<Vec<u8>, String> {
    if max_request_buffer < req_len {
        return Err(format!(
            "request was larger than our maximum size ({max_request_buffer}): \
             {req_len}; try setting GIT_HTTP_MAX_REQUEST_BUFFER"
        ));
    }
    let mut buf = vec![0u8; req_len as usize];
    let got = read_in_full(input, &mut buf)?;
    buf.truncate(got);
    Ok(buf)
}

/// `read_request`: the fixed-length reader when `CONTENT_LENGTH` said so, the
/// read-to-EOF one otherwise.
fn read_request(
    input: &mut impl Read,
    req_len: Option<u64>,
    max_request_buffer: u64,
) -> Result<Vec<u8>, String> {
    match req_len {
        Some(len) => read_request_fixed_len(input, len, max_request_buffer),
        None => read_request_eof(input, max_request_buffer),
    }
}

/// `copy_request`: read the body whole, then hand it over.
fn copy_request(
    input: &mut impl Read,
    out: &mut impl Write,
    prog_name: &str,
    req_len: Option<u64>,
    max_request_buffer: u64,
) -> Result<(), String> {
    let buf = read_request(input, req_len, max_request_buffer)?;
    write_to_child(out, &buf, prog_name)
}

/// `pipe_fixed_length`: stream exactly `req_len` bytes across, 8 KiB at a time.
fn pipe_fixed_length(
    input: &mut impl Read,
    out: &mut impl Write,
    prog_name: &str,
    req_len: u64,
) -> Result<(), String> {
    let mut buf = [0u8; 8192];
    let mut remaining = req_len;
    while remaining > 0 {
        let chunk = std::cmp::min(remaining, buf.len() as u64) as usize;
        let n = input
            .read(&mut buf[..chunk])
            .map_err(|e| format!("Reading request failed: {}", errno_text(&e)))?;
        if n == 0 {
            break;
        }
        write_to_child(out, &buf[..n], prog_name)?;
        remaining -= n as u64;
    }
    Ok(())
}

/// `inflate_request`: the body arrived `Content-Encoding: gzip`, so it is
/// inflated on the way to the child. git initialises the stream with
/// `git_inflate_init_gzip_only()` — windowBits 15 + 16 — which rejects a bare
/// zlib or raw-deflate body rather than guessing at it.
fn inflate_request(
    input: &mut impl Read,
    out: &mut impl Write,
    prog_name: &str,
    buffer_input: bool,
    req_len: Option<u64>,
    max_request_buffer: u64,
) -> Result<(), String> {
    let mut stream = zlib_rs::Inflate::new(true, 15 + 16);
    let mut in_buf = [0u8; 8192];
    let mut out_buf = [0u8; 8192];
    let mut full_request: Option<Vec<u8>> = None;
    let mut spent = false;
    let mut remaining = req_len;

    loop {
        let chunk: &[u8] = if buffer_input {
            if spent {
                &[]
            } else {
                full_request = Some(read_request(input, req_len, max_request_buffer)?);
                spent = true;
                full_request.as_deref().expect("just filled")
            }
        } else {
            let want = match remaining {
                Some(left) if left <= in_buf.len() as u64 => left as usize,
                _ => in_buf.len(),
            };
            let n = input
                .read(&mut in_buf[..want])
                .map_err(|e| format!("Reading request failed: {}", errno_text(&e)))?;
            if let Some(left) = remaining.as_mut() {
                *left -= n as u64;
            }
            &in_buf[..n]
        };
        if chunk.is_empty() {
            return Err("request ended in the middle of the gzip stream".into());
        }

        let mut consumed = 0usize;
        while consumed < chunk.len() {
            let before_in = stream.total_in();
            let before_out = stream.total_out();
            let status = stream
                .decompress(
                    &chunk[consumed..],
                    &mut out_buf,
                    zlib_rs::InflateFlush::NoFlush,
                )
                .map_err(|e| {
                    format!("zlib error inflating request, result {}", inflate_errno(&e))
                })?;
            consumed += (stream.total_in() - before_in) as usize;
            let produced = (stream.total_out() - before_out) as usize;
            write_to_child(out, &out_buf[..produced], prog_name)?;
            if status == zlib_rs::Status::StreamEnd {
                return Ok(());
            }
            if produced == 0 && stream.total_in() == before_in {
                // No progress in either direction: the stream is wedged, and
                // looping would spin. git's zlib returns an error here.
                return Err("zlib error inflating request, result -5".into());
            }
        }
    }
}

/// zlib's own `Z_*` result codes, which is what git's message quotes.
fn inflate_errno(err: &zlib_rs::InflateError) -> i32 {
    match err {
        zlib_rs::InflateError::NeedDict { .. } => 2,
        zlib_rs::InflateError::StreamError => -2,
        zlib_rs::InflateError::DataError => -3,
        zlib_rs::InflateError::MemError => -4,
    }
}

/// `write_to_child`.
fn write_to_child(out: &mut impl Write, buf: &[u8], prog_name: &str) -> Result<(), String> {
    out.write_all(buf)
        .map_err(|_| format!("unable to write to '{prog_name}'"))
}

/// `read_in_full`: fill `buf`, stopping early only at EOF.
fn read_in_full(input: &mut impl Read, buf: &mut [u8]) -> Result<usize, String> {
    let mut len = 0usize;
    while len < buf.len() {
        match input.read(&mut buf[len..]) {
            Ok(0) => break,
            Ok(n) => len += n,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(format!("error reading request body: {}", errno_text(&e))),
        }
    }
    Ok(len)
}

/// pkt-line framing, for the one packet `http-backend` writes itself.
fn pkt_line(out: &mut Vec<u8>, payload: &[u8]) {
    out.extend_from_slice(format!("{:04x}", payload.len() + 4).as_bytes());
    out.extend_from_slice(payload);
}

// ---------------------------------------------------------------------------
// QUERY_STRING parsing
// ---------------------------------------------------------------------------

/// Look up one `QUERY_STRING` parameter. As in git's `string_list`-backed
/// `get_parameters`, a repeated name keeps the last value.
fn query_parameter(want: &str) -> Option<String> {
    let query = std::env::var("QUERY_STRING").ok()?;
    let mut found = None;
    for pair in query.split('&').filter(|p| !p.is_empty()) {
        let (name, value) = match pair.split_once('=') {
            Some((n, v)) => (n, v),
            None => (pair, ""),
        };
        if url_decode(name) == want {
            found = Some(url_decode(value));
        }
    }
    found
}

/// `url_decode_internal` with `decode_plus`: `%XX` escapes plus `+` as space.
/// A malformed escape is kept verbatim rather than aborting the parse.
fn url_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            b'%' if i + 2 < bytes.len()
                && bytes[i + 1].is_ascii_hexdigit()
                && bytes[i + 2].is_ascii_hexdigit() =>
            {
                let hi = (bytes[i + 1] as char).to_digit(16).unwrap_or(0) as u8;
                let lo = (bytes[i + 2] as char).to_digit(16).unwrap_or(0) as u8;
                out.push((hi << 4) | lo);
                i += 3;
            }
            b => {
                out.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

// ---------------------------------------------------------------------------
// Time and errno formatting
// ---------------------------------------------------------------------------

fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

fn mtime_secs(meta: &std::fs::Metadata) -> i64 {
    let Ok(mtime) = meta.modified() else { return 0 };
    match mtime.duration_since(std::time::UNIX_EPOCH) {
        Ok(d) => d.as_secs() as i64,
        Err(e) => -(e.duration().as_secs() as i64),
    }
}

/// git's `show_date(when, 0, DATE_MODE(RFC2822))`: UTC, English abbreviations,
/// and — matching the `%d` in date.c — a day of month that is *not* zero-padded.
fn rfc2822(secs: i64) -> String {
    const WD: [&str; 7] = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];
    const MO: [&str; 12] = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    let (hh, mm, ss) = (rem / 3600, (rem % 3600) / 60, rem % 60);
    // 1970-01-01 was a Thursday, index 4 in a Sunday-first table.
    let wd = (days + 4).rem_euclid(7) as usize;
    let (y, m, d) = civil_from_days(days);
    format!(
        "{}, {d} {} {y} {hh:02}:{mm:02}:{ss:02} +0000",
        WD[wd],
        MO[(m - 1) as usize]
    )
}

/// Days since the Unix epoch to a proleptic-Gregorian `(year, month, day)`.
fn civil_from_days(z: i64) -> (i64, i64, i64) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (yoe + era * 400 + i64::from(m <= 2), m, d)
}

/// `strerror(errno)` — the plain message, without Rust's `(os error N)` tail.
fn errno_text(err: &std::io::Error) -> String {
    let text = err.to_string();
    match text.rfind(" (os error ") {
        Some(at) => text[..at].to_string(),
        None => text,
    }
}

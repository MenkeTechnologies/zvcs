use anyhow::{bail, Result};
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
///
/// NOT ported — these `bail!` instead of emitting plausible-looking output:
///   * The smart-HTTP payloads themselves: `GET /info/refs?service=…` and
///     `POST /git-{upload-pack,upload-archive,receive-pack}`. Stock git runs
///     `upload-pack --http-backend-info-refs` / `--stateless-rpc` as a child.
///     The vendored gitoxide has no server side at all — `gix-protocol` ships
///     only `fetch`/`handshake`/`ls_refs` clients — so there is nothing to port
///     onto, and shelling out to stock git would defeat the purpose.
///   * `GIT_NAMESPACE` (git's `strip_namespace` over the ref listing).
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
    let Some(mut method) = env("REQUEST_METHOD") else {
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
    if env("GIT_HTTP_EXPORT_ALL").is_none() && !git_dir.join("git-daemon-export-ok").exists() {
        return Ok(not_found(
            &mut hdr,
            &format!("Repository not exported: '{repo_path}'"),
        ));
    }
    let repo = match gix::open(&git_dir) {
        Ok(r) => r,
        Err(_) => {
            return Ok(not_found(
                &mut hdr,
                &format!("Not a git repository: '{repo_path}'"),
            ))
        }
    };
    let cfg = HttpConfig::read(&repo);

    match cmd.imp {
        Imp::Head => get_head(&mut hdr, &repo, &cfg),
        Imp::InfoRefs => get_info_refs(&mut hdr, &repo, &cfg),
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
        Imp::ServiceRpc => service_rpc(&mut hdr, &cfg, &arg),
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
}

impl HttpConfig {
    fn read(repo: &gix::Repository) -> Self {
        let cfg = repo.config_snapshot();
        HttpConfig {
            getanyfile: cfg.boolean("http.getanyfile").unwrap_or(true),
            upload_pack: cfg.boolean("http.uploadpack"),
            receive_pack: cfg.boolean("http.receivepack"),
            upload_archive: cfg.boolean("http.uploadarchive"),
        }
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

/// git's ref-listing routes run through `strip_namespace`, which this port does
/// not implement; refuse rather than serve an unfiltered listing.
fn reject_namespace() -> Result<()> {
    if env("GIT_NAMESPACE").is_some() {
        bail!("GIT_NAMESPACE is not supported");
    }
    Ok(())
}

/// `get_head`: `ref: <fully resolved name>` for a symbolic HEAD that resolves,
/// the raw object id for a detached one, and an empty body for an unborn one.
fn get_head(hdr: &mut Headers, repo: &gix::Repository, cfg: &HttpConfig) -> Result<ExitCode> {
    if let Err(code) = select_getanyfile(hdr, cfg) {
        return Ok(code);
    }
    reject_namespace()?;
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
fn get_info_refs(hdr: &mut Headers, repo: &gix::Repository, cfg: &HttpConfig) -> Result<ExitCode> {
    hdr.nocache();

    if let Some(service_name) = query_parameter("service") {
        let svc = match select_service(hdr, cfg, &service_name) {
            Ok(s) => s,
            Err(code) => return Ok(code),
        };
        bail!(
            "smart HTTP advertisement for {svc:?} needs a server-side {svc} \
             (git runs `{svc} --http-backend-info-refs`); the vendored gitoxide \
             has no server implementation — gix-protocol is client-only"
        );
    }

    if let Err(code) = select_getanyfile(hdr, cfg) {
        return Ok(code);
    }
    reject_namespace()?;

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
fn service_rpc(hdr: &mut Headers, cfg: &HttpConfig, service_name: &str) -> Result<ExitCode> {
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

    bail!(
        "smart HTTP RPC for {svc:?} needs a server-side {svc} \
         (git runs `{svc} --stateless-rpc .` and pipes the request body into it); \
         the vendored gitoxide has no server implementation — gix-protocol is client-only"
    )
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

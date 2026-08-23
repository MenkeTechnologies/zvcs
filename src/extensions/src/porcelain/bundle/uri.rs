//! The bundle-URI client: `git clone --bundle-uri=<uri>` and the
//! `fetch.bundleURI` that a `git fetch` follows.
//!
//! Port of git 2.55.0's `bundle-uri.c` (client half only — the `bundle-uri`
//! protocol-v2 *command* that lets a server advertise a list is not here). The
//! flow, and the names, follow the C one-for-one:
//!
//! `fetch_bundle_uri()` downloads whatever is at the URI into a temporary file.
//! If those bytes parse as a bundle they are queued for unbundling; otherwise
//! they are parsed with the config parser as a **bundle list** — which is what
//! makes `bundle.version`, `bundle.mode`, `bundle.heuristic` and the per-bundle
//! `bundle.<id>.uri` / `.creationToken` live reads. Those keys never come from a
//! repository config file (git-config(1) says so explicitly); they are only ever
//! read out of a downloaded list, which is why the whole key space needs this
//! client to exist at all.
//!
//! A list with `bundle.heuristic=creationToken` is walked by
//! `fetch_bundles_by_token()`: bundles are sorted by `bundle.<id>.creationToken`
//! descending, downloaded from the newest down until one unbundles, then applied
//! back up in increasing order. The largest token that was applied is stored as
//! `fetch.bundleCreationToken` so a later fetch downloads nothing when the list
//! has not moved on. Any other list is downloaded whole (`bundle.mode=all`) or
//! until one URI works (`bundle.mode=any`).
//!
//! Deliberate deviations from the C, each visible in the code below:
//!
//!   * git inserts the downloaded bundle into the global list from *inside*
//!     `fetch_bundle_uri_internal()`; here that insertion is returned to the
//!     caller as [`Downloaded::Bundle`] instead, so one list can be walked and
//!     appended to without aliasing. Same order, same set.
//!   * git's list is a `hashmap` iterated in hash order; this keeps the file
//!     order of the list, so a `bundle.mode=any` list picks its first entry
//!     deterministically instead of whichever one the hash seed put first.
//!   * `bundle.<id>.filter` is parsed but not acted on: git compares it against
//!     the repository's own partial-clone filter to drop bundles that do not
//!     match, and that comparison is not reproduced here. Bundles carrying a
//!     filter are therefore downloaded and applied like any other, which is the
//!     "downloads more than requested" case bundle-uri.adoc calls wasteful but
//!     not an error. It is *not* claimed as a supported key.
//!   * `--fsck-objects` is not forwarded to `index-pack` (git derives it from
//!     `fetch_pack_fsck_objects()`), because `porcelain/index_pack.rs` rejects
//!     that flag rather than running a pass it cannot run.
//!   * git 2.55's `init_bundle_list()` sets `mode = all` and `version = 1` as
//!     implied defaults, which makes its two `BUNDLE_MODE_NONE` checks
//!     unreachable; they are not reproduced.

use std::path::{Path, PathBuf};

use gix::hash::ObjectId;
use gix::refs::transaction::{Change, LogChange, PreviousValue, RefEdit, RefLog};
use gix::refs::Target;

use super::{index_pack, open_bundle, verify_bundle};

/// `max_bundle_uri_depth`.
const MAX_BUNDLE_URI_DEPTH: usize = 4;

/// `enum bundle_list_mode`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Mode {
    All,
    Any,
}

/// `enum bundle_list_heuristic`. The `heuristics[]` table maps exactly one
/// spelling, `creationToken`; anything else is ignored, as git does.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Heuristic {
    None,
    CreationToken,
}

/// `struct remote_bundle_info`.
#[derive(Default, Clone)]
struct RemoteBundleInfo {
    id: String,
    uri: Option<String>,
    /// The temporary file the URI was downloaded to. Non-`None` once a download
    /// was *attempted*, even if it failed — git relies on that to avoid retries.
    file: Option<PathBuf>,
    creation_token: u64,
    unbundled: bool,
}

/// `struct bundle_list`.
struct BundleList {
    version: i64,
    mode: Mode,
    heuristic: Heuristic,
    base_uri: Option<String>,
    bundles: Vec<RemoteBundleInfo>,
}

impl BundleList {
    /// `init_bundle_list()`, implied defaults included.
    fn new() -> Self {
        BundleList {
            version: 1,
            mode: Mode::All,
            heuristic: Heuristic::None,
            base_uri: None,
            bundles: Vec::new(),
        }
    }

    fn index_of(&self, id: &str) -> Option<usize> {
        self.bundles.iter().position(|b| b.id == id)
    }
}

/// Everything the C reaches through `struct repository *r`: the repository the
/// bundles are applied to, plus where temporary downloads live.
struct Ctx {
    repo: gix::Repository,
    git_dir: PathBuf,
    /// `<objects>/bundles`, the directory `find_temp_filename()` mkstemps in.
    tmp_dir: PathBuf,
    next_tmp: std::cell::Cell<u32>,
}

impl Ctx {
    /// `find_temp_filename()`: `odb_mkstemp(…, "bundles/tmp_uri_XXXXXX")`, then
    /// `unlink` — git only wants the name, the download recreates the file.
    fn temp_filename(&self) -> Option<PathBuf> {
        if std::fs::create_dir_all(&self.tmp_dir).is_err() {
            eprintln!("warning: failed to create temporary file");
            return None;
        }
        let n = self.next_tmp.get();
        self.next_tmp.set(n + 1);
        Some(
            self.tmp_dir
                .join(format!("tmp_uri_{}_{n}", std::process::id())),
        )
    }
}

// ---------------------------------------------------------------- URL bits ---

/// `url_is_local_not_ssh()` (`url.c`). The Windows drive-letter arm is not
/// reachable on the platforms this builds for and is left out.
fn url_is_local_not_ssh(url: &str) -> bool {
    let colon = url.find(':');
    let slash = url.find('/');
    match (colon, slash) {
        (None, _) => true,
        (Some(c), Some(s)) => s < c,
        (Some(_), None) => false,
    }
}

/// `chop_last_dir()` (`remote.c`). `Err` is git's `die("cannot strip one
/// component off url '%s'")`, which the caller turns into a failed download
/// rather than an abort.
fn chop_last_dir(url: &mut String, is_relative: bool) -> Result<bool, ()> {
    if let Some(i) = url.rfind('/') {
        url.truncate(i);
        return Ok(false);
    }
    if let Some(i) = url.rfind(':') {
        url.truncate(i);
        return Ok(true);
    }
    if is_relative || url == "." {
        return Err(());
    }
    *url = ".".into();
    Ok(false)
}

/// `relative_url(remote_url, url, NULL)` (`remote.c`), which `bundle_list_update`
/// applies to every `bundle.<id>.uri` against the list's own base URI.
///
/// Note what the C actually does with a leading `/`: `is_absolute_path(url)` is
/// true, so the value is returned verbatim rather than being reattached to the
/// list's domain the way bundle-uri.adoc describes. The code is the spec here.
fn relative_url(remote_url: &str, url: &str) -> Result<String, ()> {
    if !url_is_local_not_ssh(url) || url.starts_with('/') {
        return Ok(url.to_string());
    }
    if remote_url.is_empty() {
        return Err(());
    }

    let mut remoteurl = remote_url.to_string();
    if remoteurl.ends_with('/') {
        remoteurl.pop();
    }

    let is_relative = if !url_is_local_not_ssh(&remoteurl) || remoteurl.starts_with('/') {
        false
    } else {
        if !remoteurl.starts_with("./") && !remoteurl.starts_with("../") {
            remoteurl = format!("./{remoteurl}");
        }
        true
    };

    let mut colonsep = false;
    let mut rest = url;
    loop {
        if let Some(r) = rest.strip_prefix("../") {
            rest = r;
            colonsep |= chop_last_dir(&mut remoteurl, is_relative)?;
        } else if let Some(r) = rest.strip_prefix("./") {
            rest = r;
        } else {
            break;
        }
    }

    let mut out = format!("{remoteurl}{}{rest}", if colonsep { ":" } else { "/" });
    if rest.ends_with('/') {
        out.pop();
    }
    Ok(match out.strip_prefix("./") {
        Some(s) => s.to_string(),
        None => out,
    })
}

/// `strbuf_strip_file_from_path()` (`strbuf.c`): keep everything up to and
/// including the last `/`, or nothing at all when there is none.
fn strip_file_from_path(uri: &str) -> String {
    match uri.rfind('/') {
        Some(i) => uri[..=i].to_string(),
        None => String::new(),
    }
}

// ------------------------------------------------------------- list parsing --

/// `bundle_list_update()`, reached through `config_to_bundle_list()`. `key` is
/// the full dotted config key, exactly what git's config parser hands its
/// callback. `Err` is the C's `return -1`, which aborts the whole list parse.
fn bundle_list_update(key: &str, value: &str, list: &mut BundleList) -> Result<(), ()> {
    // The list-level keys, which git reaches by having `parse_config_key()`
    // strip the `bundle` section and comparing what is left. Matching the whole
    // key is the same test on the same input, and names the key as documented.
    if key.eq_ignore_ascii_case("bundle.version") {
        let version: i64 = value.parse().map_err(|_| ())?;
        if version != 1 {
            return Err(());
        }
        list.version = version;
        return Ok(());
    }
    if key.eq_ignore_ascii_case("bundle.mode") {
        list.mode = match value {
            "all" => Mode::All,
            "any" => Mode::Any,
            _ => return Err(()),
        };
        return Ok(());
    }
    if key.eq_ignore_ascii_case("bundle.heuristic") {
        // The `heuristics[]` table, minus the empty name for `NONE` which no
        // list can spell. Unknown heuristics are ignored, not rejected.
        if value == "creationToken" {
            list.heuristic = Heuristic::CreationToken;
        }
        return Ok(());
    }

    // Everything else is per-bundle: `bundle.<id>.<subkey>`, where `<id>` is the
    // config subsection and keeps its case while the subkey does not. A key with
    // no subsection at all is an unknown global one, which git ignores.
    let Some(rest) = key.strip_prefix("bundle.") else {
        return Err(());
    };
    let Some((id, subkey)) = rest.rsplit_once('.') else {
        return Ok(());
    };

    let idx = match list.index_of(id) {
        Some(i) => i,
        None => {
            list.bundles.push(RemoteBundleInfo {
                id: id.to_string(),
                ..Default::default()
            });
            list.bundles.len() - 1
        }
    };

    if subkey.eq_ignore_ascii_case("uri") {
        if list.bundles[idx].uri.is_some() {
            return Err(());
        }
        let base = list.base_uri.clone().unwrap_or_default();
        list.bundles[idx].uri = Some(relative_url(&base, value)?);
        return Ok(());
    }

    if subkey.eq_ignore_ascii_case("creationToken") {
        match value.parse::<u64>() {
            Ok(t) => list.bundles[idx].creation_token = t,
            Err(_) => eprintln!(
                "warning: could not parse bundle list key creationToken with value '{value}'"
            ),
        }
        return Ok(());
    }

    // Anything else is a hint for a heuristic this client does not know.
    Ok(())
}

/// `bundle_uri_parse_config_format()`: parse the downloaded file as git config
/// and fold every `bundle.*` key into `list`.
fn bundle_uri_parse_config_format(uri: &str, filename: &Path, list: &mut BundleList) -> Result<(), ()> {
    if list.base_uri.is_none() {
        list.base_uri = Some(strip_file_from_path(uri));
    }

    let file = gix::config::File::from_path_no_includes(
        filename.to_path_buf(),
        gix::config::Source::Local,
    )
    .map_err(|_| ())?;

    // git parses the file with `git_config_from_file_with_options(...,
    // CONFIG_ERROR_ERROR)`, so a key the callback rejects is reported as a
    // config syntax error naming the physical line before the parse gives up.
    let raw = std::fs::read_to_string(filename).unwrap_or_default();
    for (section_ord, section) in file.sections().enumerate() {
        let is_bundle = section.header().name() == "bundle";
        let subsection = section.header().subsection_name().map(ToString::to_string);
        for (key_ord, (name, value)) in section.body().into_iter().enumerate() {
            if !is_bundle {
                continue;
            }
            // Reassemble the dotted key the config parser would have handed the
            // callback: section, optional subsection, value name.
            let key = match &subsection {
                Some(id) => format!("bundle.{id}.{name}"),
                None => format!("bundle.{name}"),
            };
            let value = value.to_string();
            if bundle_list_update(&key, &value, list).is_err() {
                if let Some(line) = config_line_of(&raw, section_ord, key_ord) {
                    eprintln!(
                        "error: bad config line {line} in file {}",
                        filename.display()
                    );
                }
                return Err(());
            }
        }
    }

    // Every bundle in the list has to name a URI.
    let mut result = Ok(());
    for bundle in &list.bundles {
        if bundle.uri.is_none() {
            let id = if bundle.id.is_empty() {
                "<unknown>"
            } else {
                &bundle.id
            };
            eprintln!("error: bundle list at '{uri}': bundle '{id}' has no uri");
            result = Err(());
        }
    }
    result
}

// ---------------------------------------------------------------- download ---

/// The 1-based physical line holding the `key_ord`-th value of the
/// `section_ord`-th section, which is git's `cf->linenr` when its config
/// callback rejects a key.
///
/// The file has already parsed cleanly at this point, so a plain scan is enough:
/// a `[` line opens the next section, and any other non-blank, non-comment line
/// carries the next value of the current one.
fn config_line_of(raw: &str, section_ord: usize, key_ord: usize) -> Option<usize> {
    let mut section: isize = -1;
    let mut key: usize = 0;
    for (i, line) in raw.lines().enumerate() {
        let t = line.trim_start();
        if t.is_empty() || t.starts_with('#') || t.starts_with(';') {
            continue;
        }
        if t.starts_with('[') {
            section += 1;
            key = 0;
            continue;
        }
        if section == section_ord as isize {
            if key == key_ord {
                return Some(i + 1);
            }
            key += 1;
        }
    }
    None
}

/// `copy_uri_to_file()`.
fn copy_uri_to_file(filename: &Path, uri: &str) -> Result<(), ()> {
    if uri.starts_with("https:") || uri.starts_with("http:") {
        return download_https_uri_to_file(filename, uri);
    }
    let path = uri.strip_prefix("file://").unwrap_or(uri);
    std::fs::copy(path, filename).map(|_| ()).map_err(|_| ())
}

/// `download_https_uri_to_file()`: drive the `git-remote-https` helper's `get`
/// capability, which is what routes the download through the credential helper
/// and the `http.*` configuration.
///
/// git spawns the helper with the URI as its only argument; this port's helper
/// takes git's two-argument `<remote> <url>` form (`porcelain/remote_http.rs`
/// reads the URL from `argv[2]`), so the URI is passed there and the remote name
/// slot carries the same string.
fn download_https_uri_to_file(filename: &Path, uri: &str) -> Result<(), ()> {
    use std::io::{BufRead, BufReader, Write};

    let Some(file_str) = filename.to_str() else {
        return Err(());
    };
    if uri.contains(' ') || uri.contains('\n') {
        eprintln!("error: bundle-uri: URI is malformed: '{file_str}'");
        return Err(());
    }
    if file_str.contains('\n') {
        eprintln!("error: bundle-uri: filename is malformed: '{file_str}'");
        return Err(());
    }

    let exe = crate::hosted::git_exe().map_err(|_| ())?;
    let mut child = std::process::Command::new(exe)
        .args(["remote-https", uri, uri])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .map_err(|_| ())?;

    let mut child_in = child.stdin.take().ok_or(())?;
    let mut child_out = BufReader::new(child.stdout.take().ok_or(())?);

    let mut found_get = false;
    let mut result = Ok(());
    if child_in.write_all(b"capabilities\n").is_ok() && child_in.flush().is_ok() {
        let mut line = String::new();
        while child_out.read_line(&mut line).map(|n| n > 0).unwrap_or(false) {
            let trimmed = line.trim_end_matches(['\n', '\r']);
            if trimmed.is_empty() {
                break;
            }
            if trimmed == "get" {
                found_get = true;
            }
            line.clear();
        }
    }

    if found_get {
        let _ = write!(child_in, "get {uri} {file_str}\n\n");
        let _ = child_in.flush();
    } else {
        eprintln!("error: insufficient capabilities");
        result = Err(());
    }
    drop(child_in);

    match child.wait() {
        Ok(status) if status.success() => result,
        _ => Err(()),
    }
}

/// What the bytes at a URI turned out to be.
enum Downloaded {
    /// A bundle, now at `bundle.file`. git's caller inserts it into the global
    /// list at this point.
    Bundle,
    /// A bundle list, already recursed into.
    List,
}

/// `fetch_bundle_uri_internal()`.
fn fetch_bundle_uri_internal(
    ctx: &Ctx,
    bundle: &mut RemoteBundleInfo,
    depth: usize,
    global: &mut BundleList,
) -> Result<Downloaded, ()> {
    if depth >= MAX_BUNDLE_URI_DEPTH {
        eprintln!("warning: exceeded bundle URI recursion limit ({MAX_BUNDLE_URI_DEPTH})");
        return Err(());
    }

    let Some(uri) = bundle.uri.clone() else {
        let id = if bundle.id.is_empty() {
            "<unknown>"
        } else {
            &bundle.id
        };
        eprintln!("error: bundle '{id}' has no uri");
        return Err(());
    };

    if bundle.file.is_none() {
        bundle.file = ctx.temp_filename();
        if bundle.file.is_none() {
            return Err(());
        }
    }
    let file = bundle.file.clone().expect("set above");

    if copy_uri_to_file(&file, &uri).is_err() {
        eprintln!("warning: failed to download bundle from URI '{uri}'");
        let _ = std::fs::remove_file(&file);
        return Err(());
    }

    if is_bundle(&file) {
        return Ok(Downloaded::Bundle);
    }

    let result = fetch_bundle_list_in_config_format(ctx, global, bundle, depth);
    if result.is_err() {
        eprintln!("warning: file at URI '{uri}' is not a bundle or bundle list");
        let _ = std::fs::remove_file(&file);
        return Err(());
    }
    Ok(Downloaded::List)
}

/// `is_bundle(path, 1)`: the header parses, so the pack behind it is a bundle.
fn is_bundle(path: &Path) -> bool {
    path.to_str()
        .is_some_and(|p| p != "-" && open_bundle(p).is_ok())
}

/// `fetch_bundle_list_in_config_format()`.
fn fetch_bundle_list_in_config_format(
    ctx: &Ctx,
    global: &mut BundleList,
    bundle: &RemoteBundleInfo,
    depth: usize,
) -> Result<(), ()> {
    let (Some(uri), Some(file)) = (bundle.uri.as_deref(), bundle.file.as_deref()) else {
        return Err(());
    };

    let mut list = BundleList::new();
    bundle_uri_parse_config_format(uri, file, &mut list)?;

    // A list using the creationToken heuristic advertises bundles, never nested
    // lists, so `global` and `depth` do not carry into it.
    if list.heuristic == Heuristic::CreationToken {
        let result = fetch_bundles_by_token(ctx, &mut list);
        global.heuristic = Heuristic::CreationToken;
        // Every bundle this path downloaded has already been applied (or given
        // up on), so its temporary file is dead. git leaks these; the global
        // list it unlinks at the end never learns about a creationToken list.
        unlink_all(&list);
        return result;
    }

    // No unlink here: `download_bundle_list` handed the downloaded files to
    // `global`, which unbundles them once the whole recursion is done.
    download_bundle_list(ctx, &mut list, global, depth)
}

/// `download_bundle_list()` / `download_bundle_to_file()`: walk the nested list,
/// downloading as much as it can. `bundle.mode=any` stops after the first
/// success; `bundle.mode=all` keeps going so every usable bundle is applied.
fn download_bundle_list(
    ctx: &Ctx,
    local: &mut BundleList,
    global: &mut BundleList,
    depth: usize,
) -> Result<(), ()> {
    let mode = local.mode;
    let mut count = 0usize;

    for i in 0..local.bundles.len() {
        if mode == Mode::Any && count > 0 {
            break;
        }
        let mut bundle = std::mem::take(&mut local.bundles[i]);
        let outcome = fetch_bundle_uri_internal(ctx, &mut bundle, depth + 1, global);
        if let Ok(Downloaded::Bundle) = outcome {
            global.bundles.push(RemoteBundleInfo {
                id: bundle.id.clone(),
                file: bundle.file.clone(),
                ..Default::default()
            });
        }
        if outcome.is_ok() {
            count += 1;
        }
        local.bundles[i] = bundle;
    }
    Ok(())
}

/// `fetch_bundles_by_token()`: the `bundle.heuristic=creationToken` walk.
fn fetch_bundles_by_token(ctx: &Ctx, list: &mut BundleList) -> Result<(), ()> {
    if list.bundles.is_empty() {
        return Ok(());
    }

    // `QSORT(..., compare_creation_token_decreasing)`. Indices, not pointers, so
    // the recursion below may still append to `list.bundles`.
    let mut order: Vec<usize> = (0..list.bundles.len()).collect();
    order.sort_by(|a, b| list.bundles[*b].creation_token.cmp(&list.bundles[*a].creation_token));

    // `fetch.bundleCreationToken`: the largest token already applied. When the
    // list advertises nothing newer, download nothing at all.
    let mut max_creation_token: u64 = 0;
    if let Some(stored) = read_config_string(&ctx.repo, "fetch", "bundleCreationToken") {
        max_creation_token = stored.trim().parse().unwrap_or(0);
        if list.bundles[order[0]].creation_token <= max_creation_token {
            return Ok(());
        }
    }
    let mut new_max_creation_token: u64 = 0;

    let mut cur: isize = 0;
    let mut move_direction: isize = 0;
    while cur >= 0 && (cur as usize) < order.len() {
        let idx = order[cur as usize];

        // Digging below the previous creation token means the list is missing or
        // invalid; stop rather than pull down more data.
        if list.bundles[idx].creation_token <= max_creation_token {
            break;
        }

        let mut bundle = std::mem::take(&mut list.bundles[idx]);
        if bundle.file.is_none() {
            let outcome = fetch_bundle_uri_internal(ctx, &mut bundle, 1, list);
            if outcome.is_err() {
                // Mark as unbundled so it is not retried, and look deeper.
                bundle.unbundled = true;
                list.bundles[idx] = bundle;
                cur += 1;
                move_direction = 1;
                continue;
            }
            // creationToken lists are expected to advertise bundles.
            if !bundle.file.as_deref().is_some_and(is_bundle) {
                let uri = bundle.uri.clone().unwrap_or_default();
                eprintln!("warning: file downloaded from '{uri}' is not a bundle");
                list.bundles[idx] = bundle;
                break;
            }
        }

        if bundle.file.is_some() && !bundle.unbundled {
            match unbundle_from_file(ctx, bundle.file.as_deref().expect("checked")) {
                Err(()) => move_direction = 1,
                Ok(()) => {
                    // Applied; retry the bundles that failed before it.
                    move_direction = -1;
                    bundle.unbundled = true;
                    if bundle.creation_token > new_max_creation_token {
                        new_max_creation_token = bundle.creation_token;
                    }
                }
            }
        }

        list.bundles[idx] = bundle;
        cur += move_direction;
    }

    // Walking off the front means every bundle that was needed got applied.
    if cur < 0 {
        if write_config_string(
            &ctx.git_dir,
            "fetch",
            "bundleCreationToken",
            &new_max_creation_token.to_string(),
        )
        .is_err()
        {
            eprintln!("warning: failed to store maximum creation token");
        }
        return Ok(());
    }
    Err(())
}

/// `unbundle_from_file()`: apply one downloaded bundle and record its tips under
/// `refs/bundles/`, so a later fetch can offer them as `have`s.
fn unbundle_from_file(ctx: &Ctx, file: &Path) -> Result<(), ()> {
    let Some(path) = file.to_str() else {
        return Err(());
    };
    let Ok((header, source)) = open_bundle(path) else {
        return Err(());
    };

    // `VERIFY_BUNDLE_QUIET`: a bundle whose prerequisites are not in the object
    // store yet is not an error, it is a bundle to come back to.
    if !verify_bundle(&ctx.repo, &header, true) {
        return Err(());
    }
    if !index_pack(source, &ctx.repo, &[]).unwrap_or(false) {
        return Err(());
    }

    for (oid, name) in &header.refs {
        let Some(branch) = name.strip_prefix(b"refs/") else {
            continue;
        };
        let Ok(branch) = std::str::from_utf8(branch) else {
            continue;
        };
        let full = format!("refs/bundles/{branch}");
        let Ok(ref_name) = gix::refs::FullName::try_from(full.as_str()) else {
            continue;
        };
        // git passes the old value when the ref already exists and NULL — no
        // check at all — when it does not.
        let expected = match ctx.repo.try_find_reference(&full) {
            Ok(Some(existing)) => match existing.target().try_id().map(ObjectId::from) {
                Some(old) => PreviousValue::MustExistAndMatch(Target::Object(old)),
                None => PreviousValue::Any,
            },
            _ => PreviousValue::Any,
        };
        let _ = ctx.repo.edit_reference(RefEdit {
            change: Change::Update {
                log: LogChange {
                    mode: RefLog::AndReference,
                    force_create_reflog: false,
                    message: "fetched bundle".into(),
                },
                expected,
                new: Target::Object(*oid),
            },
            name: ref_name,
            deref: false,
        });
    }
    Ok(())
}

/// `unbundle_all_bundles()`: keep sweeping the list until a full pass applies
/// nothing, which resolves bundles whose prerequisites only arrive with a later
/// one.
fn unbundle_all_bundles(ctx: &Ctx, list: &mut BundleList) {
    loop {
        let mut applied = false;
        for i in 0..list.bundles.len() {
            if list.bundles[i].unbundled {
                continue;
            }
            let Some(file) = list.bundles[i].file.clone() else {
                continue;
            };
            if unbundle_from_file(ctx, &file).is_ok() {
                list.bundles[i].unbundled = true;
                applied = true;
                break;
            }
        }
        if !applied {
            return;
        }
    }
}

/// `unlink_bundle()` over the whole list.
fn unlink_all(list: &BundleList) {
    for bundle in &list.bundles {
        if let Some(file) = &bundle.file {
            let _ = std::fs::remove_file(file);
        }
    }
}

// ------------------------------------------------------------------ config ---

fn read_config_string(repo: &gix::Repository, section: &str, key: &str) -> Option<String> {
    repo.config_snapshot()
        .string(format!("{section}.{key}").as_str())
        .map(|v| v.to_string())
}

/// `repo_config_set_multivar_gently()` against the repository's own config file.
fn write_config_string(git_dir: &Path, section: &str, key: &str, value: &str) -> Result<(), ()> {
    let path = git_dir.join("config");
    let mut file =
        gix::config::File::from_path_no_includes(path.clone(), gix::config::Source::Local)
            .map_err(|_| ())?;
    file.section_mut_or_create_new(section, None)
        .map_err(|_| ())?
        .set(key, value)
        .map_err(|_| ())?;
    std::fs::write(&path, file.to_bstring()).map_err(|_| ())
}

// -------------------------------------------------------------- entry point --

/// `fetch_bundle_uri()`: download from `uri`, apply everything it yields, and
/// report whether the list carried a `bundle.heuristic` (which is what tells
/// `git clone` to persist `fetch.bundleURI` for later fetches).
///
/// Returns `(failed, has_heuristic)`, mirroring the C's return value and its
/// `*has_heuristic` out-parameter — which git fills in regardless of failure.
pub(crate) fn fetch_bundle_uri(repo: &gix::Repository, uri: &str) -> (bool, bool) {
    // An empty bundle URI signals a disabled one; do not fetch it.
    if uri.is_empty() {
        return (false, false);
    }

    let ctx = Ctx {
        repo: repo.clone(),
        // `odb_mkstemp(the_repository->objects, …, "bundles/tmp_uri_XXXXXX")`
        // puts the downloads under the primary object directory.
        tmp_dir: repo.common_dir().join("objects").join("bundles"),
        git_dir: repo.git_dir().to_path_buf(),
        next_tmp: std::cell::Cell::new(0),
    };

    // Anything added to this global list is required.
    let mut list = BundleList::new();
    list.mode = Mode::All;

    let mut bundle = RemoteBundleInfo {
        id: String::new(),
        uri: Some(uri.to_string()),
        ..Default::default()
    };

    let outcome = fetch_bundle_uri_internal(&ctx, &mut bundle, 0, &mut list);
    if let Ok(Downloaded::Bundle) = outcome {
        list.bundles.push(RemoteBundleInfo {
            id: bundle.id.clone(),
            file: bundle.file.clone(),
            ..Default::default()
        });
    }
    let failed = outcome.is_err();
    if !failed {
        unbundle_all_bundles(&ctx, &mut list);
    }

    let has_heuristic = list.heuristic != Heuristic::None;
    unlink_all(&list);
    if let Some(file) = &bundle.file {
        let _ = std::fs::remove_file(file);
    }
    (failed, has_heuristic)
}

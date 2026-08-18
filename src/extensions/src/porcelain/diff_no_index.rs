//! `git diff --no-index <a> <b>` — compare two paths on disk, with no repository
//! and no index in the picture.
//!
//! Ported from `builtin/diff.c`'s `builtin_diff_no_index()` and `diff-no-index.c`.
//! The two operands may be files, directories, or `/dev/null`, and neither has to
//! live inside a repository — the command is git's `diff(1)` for arbitrary paths,
//! which is why it is reachable before repository discovery happens.
//!
//! What it produces is an ordinary diff: the same headers, the same
//! `diffcore`-shaped pairs, and the same emitters. Only three things differ from a
//! tracked diff, all of them from `queue_diff()`:
//!
//!   * the two sides may have *different* names, which the patch header shows as
//!     `a/<lhs> b/<rhs>` and the stat formats as `<lhs> => <rhs>`;
//!   * a directory operand is walked, and each name that exists on one side only
//!     becomes an addition or a deletion;
//!   * `/dev/null` names the empty side of an addition or deletion, and the *other*
//!     operand's name is used for both halves of the header.
//!
//! `--exit-code` is implied (git-diff(1): "this option implies `--exit-code`"), so
//! a difference exits 1.
//!
//! `-R` is the one option that reaches into the pairing rather than the rendering.
//! git spends it in two places and this port follows both: `queue_diff()` swaps
//! `name`/`mode`/`special` at the leaf of the walk (diff-no-index.c:279-283), so
//! the pair order is untouched and only each pair's two sides trade places; and
//! `builtin_diff()` swaps the prefixes it prints with (diff.c:3862-3868), which is
//! why the header reads `diff --git b/<rhs> a/<lhs>` — the *values* are exchanged,
//! so `--src-prefix`/`--dst-prefix` follow along and `--no-prefix` is unaffected.
//!
//! The option table is `add_diff_options(no_index_options, &revs->diffopt)`
//! (diff-no-index.c:372) — the *whole* `diff_opts` table, not a subset — so this is
//! a hand-written implementation of a table `git diff` shares, and a name missing
//! here is a gap rather than a name git rejects. Algorithm selection is on it:
//! `--diff-algorithm=<v>`, the separated `--diff-algorithm <v>`, `--minimal`,
//! `--patience`, `--histogram`, and the `diff.algorithm` default when the
//! comparison happens to be run from inside a repository.

use anyhow::Result;
use gix::bstr::{BString, ByteSlice};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use super::diff_color;

/// `/dev/null` as an operand: git's `DIFF_FILE_VALID` for the side is false, and
/// the pair takes the other side's name.
const DEV_NULL: &str = "/dev/null";

/// `the_hash_algo` for a comparison that has no repository to take one from.
const HASH_KIND: gix::hash::Kind = gix::hash::Kind::Sha1;

/// One side of a pair, as `queue_diff()` resolved it.
#[derive(Clone)]
struct Side {
    /// The name printed for this side, which for `/dev/null` is the peer's name.
    name: BString,
    /// `None` when the side does not exist (a `/dev/null` operand or a name found
    /// only in the other directory).
    file: Option<PathBuf>,
    /// `100644`, `100755` or `120000`, from the file's own metadata.
    mode: u32,
}

impl Side {
    fn absent(name: BString) -> Self {
        Side { name, file: None, mode: 0 }
    }

    /// The name this side carries in the patch header, which for a side that does
    /// not exist is the peer's — `queue_diff()` gives both filespecs one path.
    fn header_name<'a>(&'a self, peer: &'a Side) -> &'a BString {
        if self.file.is_some() { &self.name } else { &peer.name }
    }

    /// The name the stat and raw formats show, where an absent side stays
    /// `/dev/null` rather than borrowing.
    fn display_name(&self) -> &BString {
        &self.name
    }
}

/// `stat()` reduced to git's three blob modes.
fn mode_of(path: &Path) -> Result<u32> {
    let meta = std::fs::symlink_metadata(path)?;
    if meta.file_type().is_symlink() {
        return Ok(0o120000);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        Ok(if meta.permissions().mode() & 0o111 != 0 { 0o100755 } else { 0o100644 })
    }
    #[cfg(not(unix))]
    {
        Ok(0o100644)
    }
}

/// The bytes of a side, empty for one that does not exist. A symlink contributes
/// its target, which is what git stores as the blob.
fn read_side(side: &Side) -> Result<Vec<u8>> {
    let Some(path) = &side.file else { return Ok(Vec::new()) };
    if side.mode == 0o120000 {
        let target = std::fs::read_link(path)?;
        return Ok(gix::path::into_bstr(target).into_owned().into());
    }
    Ok(std::fs::read(path)?)
}

/// `hash_object_file()` over a blob's bytes — the id both `fill_metainfo()` and
/// `hash_filespec()` arrive at for a file no repository has ever stored.
fn blob_id(data: &[u8]) -> gix::ObjectId {
    gix::objs::compute_hash(HASH_KIND, gix::objs::Kind::Blob, data)
        .expect("sha1 of an in-memory blob")
}

/// `diff_populate_filespec()` for a `--no-index` filespec, whose "object" is a
/// path on disk.
///
/// A no-index filespec is identified by the name it prints, which is the path it
/// was found at, so that name is the key. Reads are cached because rename
/// detection asks for the same blob many times and the emitters ask again after.
#[derive(Default)]
struct DiskContent {
    on_disk: std::collections::HashMap<BString, (PathBuf, u32)>,
    cache: std::collections::HashMap<BString, Option<Vec<u8>>>,
}

impl DiskContent {
    /// Remember where a side's blob lives. A side that is not there has nothing
    /// to read and is never asked for.
    fn register(&mut self, side: &Side) {
        if side.file.is_some() {
            self.on_disk
                .insert(side.name.clone(), (side.file.clone().expect("just checked"), side.mode));
        }
    }

    /// The bytes of the named side, or `None` when it cannot be read — git's
    /// `diff_populate_filespec()` returning non-zero.
    fn bytes(&mut self, name: &BString) -> Option<Vec<u8>> {
        if let Some(hit) = self.cache.get(name) {
            return hit.clone();
        }
        let read = self.on_disk.get(name).and_then(|(path, mode)| {
            read_side(&Side { name: name.clone(), file: Some(path.clone()), mode: *mode }).ok()
        });
        self.cache.insert(name.clone(), read.clone());
        read
    }
}

impl super::diffcore_rename::Content for DiskContent {
    fn size(&mut self, spec: &super::diffcore_rename::FileSpec) -> Option<u64> {
        self.bytes(&spec.path).map(|data| data.len() as u64)
    }

    fn data(&mut self, spec: &super::diffcore_rename::FileSpec) -> Option<Vec<u8>> {
        self.bytes(&spec.path)
    }
}

/// The `diff_filespec` a side stands for. A no-index side has no object id until
/// `hash_filespec()` gives it one, so `oid_valid` starts false exactly as
/// `alloc_filespec()` + `fill_filespec(…, 0, mode)` leaves it.
fn spec_of(side: &Side) -> super::diffcore_rename::FileSpec {
    match side.file {
        Some(_) => super::diffcore_rename::FileSpec::new(
            side.name.clone(),
            side.mode,
            gix::ObjectId::null(HASH_KIND),
            false,
        ),
        None => super::diffcore_rename::FileSpec::absent(side.name.clone()),
    }
}

/// The inverse, for reading a pair back out of the queue after rename detection
/// has re-paired the specs across the pairs they arrived in.
fn side_of(spec: &super::diffcore_rename::FileSpec, content: &DiskContent) -> Side {
    match content.on_disk.get(&spec.path) {
        Some((path, _)) if spec.valid() => {
            Side { name: spec.path.clone(), file: Some(path.clone()), mode: spec.mode }
        }
        _ => Side::absent(spec.path.clone()),
    }
}

/// `git diff --no-index`'s two operands, after `/dev/null` and directories are
/// resolved into the pair list `diff_queue` would hold.
fn queue(lhs: &str, rhs: &str) -> Result<Vec<(Side, Side)>, String> {
    let l_null = lhs == DEV_NULL;
    let r_null = rhs == DEV_NULL;
    let l_meta = (!l_null).then(|| std::fs::symlink_metadata(lhs));
    let r_meta = (!r_null).then(|| std::fs::symlink_metadata(rhs));

    // `error("Could not access '%s'", …)` — one message per unreachable operand,
    // and nothing is compared.
    for (name, meta) in [(lhs, &l_meta), (rhs, &r_meta)] {
        if matches!(meta, Some(Err(_))) {
            return Err(format!("Could not access '{name}'"));
        }
    }

    let l_dir = matches!(&l_meta, Some(Ok(m)) if m.is_dir());
    let r_dir = matches!(&r_meta, Some(Ok(m)) if m.is_dir());
    if l_dir || r_dir {
        return Ok(queue_dirs(lhs, rhs, l_dir, r_dir));
    }

    let side = |name: &str, is_null: bool| -> Result<Side, String> {
        if is_null {
            return Ok(Side::absent(BString::from(name)));
        }
        let path = PathBuf::from(name);
        let mode = mode_of(&path).map_err(|_| format!("Could not access '{name}'"))?;
        Ok(Side { name: BString::from(name), file: Some(path), mode })
    };
    Ok(vec![(side(lhs, l_null)?, side(rhs, r_null)?)])
}

/// The directory half of `queue_diff()`: the union of both trees' relative names,
/// in sorted order, each becoming a pair whose missing side is absent.
fn queue_dirs(lhs: &str, rhs: &str, l_dir: bool, r_dir: bool) -> Vec<(Side, Side)> {
    let mut names: Vec<BString> = Vec::new();
    let mut collect = |root: &str, on: bool| {
        if !on {
            return;
        }
        walk(Path::new(root), Path::new(root), &mut names);
    };
    collect(lhs, l_dir);
    collect(rhs, r_dir);
    names.sort();
    names.dedup();

    let make = |root: &str, is_dir: bool, rel: &BString| -> Side {
        let joined = format!("{root}/{rel}");
        if !is_dir {
            // A file compared against a directory keeps its own name on that side.
            let path = PathBuf::from(root);
            let mode = mode_of(&path).unwrap_or(0o100644);
            return Side { name: BString::from(root), file: Some(path), mode };
        }
        let path = PathBuf::from(&joined);
        match mode_of(&path) {
            Ok(mode) if path.exists() => Side { name: BString::from(joined), file: Some(path), mode },
            _ => Side::absent(BString::from(DEV_NULL)),
        }
    };
    names
        .iter()
        .map(|rel| {
            (make(lhs, l_dir, rel), make(rhs, r_dir, rel))
        })
        .collect()
}

/// Every file under `root`, as a path relative to it.
fn walk(root: &Path, dir: &Path, out: &mut Vec<BString>) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for entry in entries.filter_map(Result::ok) {
        let path = entry.path();
        let is_dir = entry.file_type().map(|t| t.is_dir()).unwrap_or(false);
        if is_dir {
            walk(root, &path, out);
        } else if let Ok(rel) = path.strip_prefix(root) {
            out.push(gix::path::into_bstr(rel).into_owned());
        }
    }
}

/// One emitted pair, as the non-patch formats need it.
struct Row {
    /// The pre-image's display name, `/dev/null` when it does not exist.
    a_name: BString,
    /// The post-image's, likewise.
    b_name: BString,
    added: u32,
    deleted: u32,
    binary: bool,
    a_exists: bool,
    b_exists: bool,
    a_mode: u32,
    b_mode: u32,
    /// The pre-image's blob id once `hash_filespec()` has given the filespec one,
    /// which `--raw` prints. `None` while `oid_valid` is false — rename detection
    /// is the only thing that hashes, so a run it declined to start leaves the
    /// null id and `--raw` shows zeros.
    a_oid: Option<gix::ObjectId>,
    /// The post-image's, likewise.
    b_oid: Option<gix::ObjectId>,
    /// `diff_resolve_rename_copy()`'s letter: `A`, `D`, `M`, `T`, `R` or `C`.
    status: u8,
    /// The similarity `R`/`C` was matched at, in `MAX_SCORE` units.
    score: u32,
}

impl Row {
    /// Whether `diff_resolve_rename_copy()` called this pair a rename or a copy,
    /// which is what makes the formats print two names instead of one.
    fn renamed(&self) -> bool {
        matches!(self.status, b'R' | b'C')
    }
}

/// The formats `--no-index` understands, as bits in the same order `diff` uses.
#[derive(Default, Clone, Copy)]
struct Format {
    patch: bool,
    stat: bool,
    numstat: bool,
    shortstat: bool,
    name_only: bool,
    name_status: bool,
    raw: bool,
    summary: bool,
    quiet: bool,
}

impl Format {
    /// `diff_setup_done()`: with nothing selected, the patch is the format.
    fn resolved(self) -> Self {
        if !(self.patch
            || self.stat
            || self.numstat
            || self.shortstat
            || self.name_only
            || self.name_status
            || self.raw
            || self.summary
            || self.quiet)
        {
            Format { patch: true, ..self }
        } else {
            self
        }
    }
}

/// Everything one `--no-index` invocation was told to do.
struct Opts {
    fmt: Format,
    ctx: u32,
    ws: super::diff::Whitespace,
    func_context: bool,
    src_prefix: Vec<u8>,
    dst_prefix: Vec<u8>,
    /// The `index` line's width: `fill_metainfo()`'s `o->abbrev ? o->abbrev :
    /// DEFAULT_ABBREV` (diff.c:4915), so `--no-abbrev`'s zero does not reach it.
    abbrev: usize,
    /// `--raw`'s, which is `opt->abbrev` untouched (diff.c:6477) and therefore
    /// the whole name under `--no-abbrev`.
    raw_abbrev: usize,
    full_index: bool,
    text: bool,
    colors: diff_color::DiffColors,
    /// `options->xdl_opts`' algorithm bits, which `add_diff_options()` exposes to
    /// this parser as fully as to `git diff`'s own.
    algorithm: gix::diff::blob::Algorithm,
}

/// git's `diff_no_index_usage[]`, over the block every `add_diff_options()`
/// caller shares. `usage_with_options()` writes both to stderr and exits 129.
const USAGE_LINE: &str = "usage: git diff --no-index [<options>] <path> <path> [<pathspec>...]\n\n";

/// Print the usage the way `usage_with_options()` does and hand back its code.
fn usage() -> Result<ExitCode> {
    eprint!("{USAGE_LINE}{}", super::diff_pairs::DIFF_OPTIONS);
    Ok(ExitCode::from(129))
}

/// The options `cmd_diff()` consumes itself, before `diff_no_index()` ever runs
/// its own `parse_options()` over `add_diff_options()`. They are ordinary `git
/// diff` options *and* unknown to the no-index parser, which is why `git diff
/// --cached` outside a repository is a parse error rather than a complaint about
/// the missing repository.
const NOT_IN_NO_INDEX: &[&str] = &["--cached", "--staged", "--merge-base"];

/// `git diff --no-index <a> <b>`.
pub(crate) fn run(args: &[String]) -> Result<ExitCode> {
    run_with(args, false)
}

/// The same, entered because `git diff` found no repository: git's
/// `DIFF_NO_INDEX_IMPLICIT`. The comparison itself is identical — only the
/// diagnostic for the wrong number of operands differs, since a user who did not
/// type `--no-index` has to be told why they are being shown its usage.
pub(crate) fn run_implicit(args: &[String]) -> Result<ExitCode> {
    run_with(args, true)
}

fn run_with(args: &[String], implicit: bool) -> Result<ExitCode> {
    let mut fmt = Format::default();
    let mut ctx: u32 = 3;
    let mut ws = super::diff::Whitespace::Keep;
    let mut func_context = false;
    let mut src_prefix = b"a/".to_vec();
    let mut dst_prefix = b"b/".to_vec();
    // `options->abbrev` starts at `DEFAULT_ABBREV`, which is the `core.abbrev`
    // git read at startup; `None` here stands for that deferred value, resolved
    // below once it is known whether a repository was found.
    let mut abbrev: Option<usize> = None;
    let mut no_abbrev = false;
    let mut full_index = false;
    let mut text = false;
    let mut color_when: Option<diff_color::ColorWhen> = None;
    let mut operands: Vec<String> = Vec::new();
    let mut after_dashdash = false;
    // `OPT_BOOL('R', NULL, &options->flags.reverse_diff, …)` (diff.c:6254). The
    // NULL long name is why there is no `--no-R` to turn it back off, and why a
    // repeated `-R` re-sets rather than toggles.
    let mut reverse = false;
    // `options->detect_rename`, `rename_score` and `flags.find_copies_harder`
    // (diff.c:6162-6189). `None` leaves `diff_detect_rename_default`, which is
    // `DIFF_DETECT_RENAME` unless `diff.renames` says otherwise (diff.c:284/399).
    let mut detect_rename: Option<u8> = None;
    let mut rename_score: u32 = 0;
    let mut find_copies_harder = false;
    // `add_diff_options()` (diff-no-index.c:372) hands the no-index parser the
    // *whole* `diff_opts` table, algorithm selection included, so every spelling
    // `git diff` takes is taken here too. `None` leaves the `diff.algorithm`
    // default resolved below.
    let mut algorithm: Option<gix::diff::blob::Algorithm> = None;
    // `--diff-algorithm` is an `OPT_CALLBACK_F` without `PARSE_OPT_OPTARG`, so
    // parse-options consumes the next argv entry as its value before that entry is
    // examined for anything else — a `--`, an operand, or another option.
    let mut want_algorithm_value = false;

    for a in args {
        if std::mem::take(&mut want_algorithm_value) {
            match super::diff_optval::parse_algorithm_value(a) {
                Some(alg) => algorithm = Some(alg),
                None => {
                    eprintln!("{}", super::diff_optval::DIFF_ALGORITHM_ERR);
                    return Ok(ExitCode::from(129));
                }
            }
            continue;
        }
        if a == "--diff-algorithm" {
            want_algorithm_value = true;
            continue;
        }
        if after_dashdash || !a.starts_with('-') || a == "-" {
            operands.push(a.clone());
            continue;
        }
        match a.as_str() {
            "--" => after_dashdash = true,
            "--no-index" => {}
            "-p" | "-u" | "--patch" => fmt.patch = true,
            "--stat" => fmt.stat = true,
            "--numstat" => fmt.numstat = true,
            "--shortstat" => fmt.shortstat = true,
            "--name-only" => fmt.name_only = true,
            "--name-status" => fmt.name_status = true,
            "--raw" => fmt.raw = true,
            "--summary" => fmt.summary = true,
            "-s" | "--no-patch" => fmt = Format::default(),
            "--quiet" => fmt.quiet = true,
            "--exit-code" => {}
            "-w" | "--ignore-all-space" => ws = super::diff::Whitespace::IgnoreAll,
            "-b" | "--ignore-space-change" => ws = super::diff::Whitespace::IgnoreChange,
            "--ignore-space-at-eol" => ws = super::diff::Whitespace::IgnoreAtEol,
            "-R" => reverse = true,
            // `diff_opt_find_renames()` (diff.c:5742) and `diff_opt_find_copies()`
            // (diff.c:5722). Both take an optional score; anything left over after
            // `parse_rename_score()` is `error: invalid argument to <long-name>`
            // followed by the usage block. A second `-C` is what turns copy
            // detection into `--find-copies-harder`.
            s if s == "-M" || s == "--find-renames" || s.starts_with("-M") || s.starts_with("--find-renames=") => {
                let arg = s
                    .strip_prefix("--find-renames=")
                    .unwrap_or_else(|| s.strip_prefix("-M").unwrap_or(""));
                let (score, rest) = super::diffcore_rename::parse_rename_score(arg);
                if !rest.is_empty() {
                    // `parse_options()` turns a callback's `PARSE_OPT_ERROR` into
                    // a bare `exit(129)` (parse-options.c:1200), so the message
                    // stands alone — no usage block follows it.
                    eprintln!("error: invalid argument to find-renames");
                    return Ok(ExitCode::from(129));
                }
                rename_score = score;
                detect_rename = Some(super::diffcore_rename::DETECT_RENAME);
            }
            s if s == "-C" || s == "--find-copies" || s.starts_with("-C") || s.starts_with("--find-copies=") => {
                let arg = s
                    .strip_prefix("--find-copies=")
                    .unwrap_or_else(|| s.strip_prefix("-C").unwrap_or(""));
                let (score, rest) = super::diffcore_rename::parse_rename_score(arg);
                if !rest.is_empty() {
                    eprintln!("error: invalid argument to find-copies");
                    return Ok(ExitCode::from(129));
                }
                rename_score = score;
                if detect_rename == Some(super::diffcore_rename::DETECT_COPY) {
                    find_copies_harder = true;
                } else {
                    detect_rename = Some(super::diffcore_rename::DETECT_COPY);
                }
            }
            "--find-copies-harder" => find_copies_harder = true,
            "--no-find-copies-harder" => find_copies_harder = false,
            "--no-renames" => detect_rename = Some(0),
            // The algorithm aliases, all of which `parse_algorithm_value()` also
            // spells: `--minimal` is `XDF_NEED_MINIMAL`, `--patience` is
            // `XDF_PATIENCE_DIFF`, `--histogram` is `XDF_HISTOGRAM_DIFF`, and
            // `--myers` clears back to 0. They are `OPT_BIT`s on the same
            // `xdl_opts` word, so the last one on the line wins.
            "--minimal" => algorithm = Some(gix::diff::blob::Algorithm::MyersMinimal),
            "--myers" => algorithm = Some(gix::diff::blob::Algorithm::Myers),
            "--patience" => algorithm = Some(gix::diff::blob::Algorithm::Patience),
            "--histogram" => algorithm = Some(gix::diff::blob::Algorithm::Histogram),
            // `diff_opt_diff_algorithm()`'s glued form, matched case-insensitively
            // by `parse_algorithm_value()`'s `strcasecmp` — so `--diff-algorithm=MYERS`
            // is Myers and not the `error()` below.
            s if s.starts_with("--diff-algorithm=") => {
                match super::diff_optval::parse_algorithm_value(&s["--diff-algorithm=".len()..]) {
                    Some(alg) => algorithm = Some(alg),
                    None => {
                        eprintln!("{}", super::diff_optval::DIFF_ALGORITHM_ERR);
                        return Ok(ExitCode::from(129));
                    }
                }
            }
            "-W" | "--function-context" => func_context = true,
            "--no-function-context" => func_context = false,
            "--full-index" => full_index = true,
            "--text" | "-a" => text = true,
            "--no-prefix" => {
                src_prefix.clear();
                dst_prefix.clear();
            }
            "--default-prefix" => {
                src_prefix = b"a/".to_vec();
                dst_prefix = b"b/".to_vec();
            }
            "--color" => color_when = Some(diff_color::ColorWhen::Always),
            "--no-color" => color_when = Some(diff_color::ColorWhen::Never),
            s if s.starts_with("--color=") => {
                match diff_color::parse_color_when(&s["--color=".len()..]) {
                    Some(w) => color_when = Some(w),
                    None => {
                        eprintln!("error: option `color' expects \"always\", \"auto\", or \"never\"");
                        return Ok(ExitCode::from(129));
                    }
                }
            }
            s if s.starts_with("--src-prefix=") => {
                src_prefix = s.as_bytes()["--src-prefix=".len()..].to_vec();
            }
            s if s.starts_with("--dst-prefix=") => {
                dst_prefix = s.as_bytes()["--dst-prefix=".len()..].to_vec();
            }
            s if s.starts_with("-U") || s.starts_with("--unified=") => {
                let val = s.strip_prefix("--unified=").unwrap_or(&s[2..]);
                match val.parse::<u32>() {
                    Ok(n) => ctx = n,
                    Err(_) => {
                        eprintln!("error: --unified expects a numerical value");
                        return Ok(ExitCode::from(129));
                    }
                }
            }
            // `OPT__ABBREV(&options->abbrev)` (diff.c:6128), whose
            // `parse_opt_abbrev_cb()` takes the argument as optional: bare
            // `--abbrev` restores `DEFAULT_ABBREV` and `--no-abbrev` is a zero,
            // which the two consumers read differently.
            "--abbrev" => {
                abbrev = None;
                no_abbrev = false;
            }
            "--no-abbrev" => {
                abbrev = None;
                no_abbrev = true;
            }
            s if s.starts_with("--abbrev=") => {
                let Some(v) = super::super::abbrev::parse_opt_abbrev_value(&s["--abbrev=".len()..])
                else {
                    // `parse_options()` turns a callback's `PARSE_OPT_ERROR` into
                    // a bare `exit(129)` (parse-options.c:1200), so the message
                    // stands alone — no usage block follows it.
                    eprintln!("error: option `abbrev' expects a numerical value");
                    return Ok(ExitCode::from(129));
                };
                // `if (v && v < MINIMUM_ABBREV) v = MINIMUM_ABBREV; else if (v >
                // hexsz) v = hexsz;` — a zero survives both arms and means the
                // whole name, exactly as `--no-abbrev` does.
                let hexsz = HASH_KIND.len_in_hex() as i32;
                let v = match v {
                    0 => 0,
                    v if v < super::super::abbrev::MINIMUM_ABBREV as i32 => {
                        super::super::abbrev::MINIMUM_ABBREV as i32
                    }
                    v if v > hexsz => hexsz,
                    v => v,
                };
                abbrev = (v != 0).then_some(v as usize);
                no_abbrev = v == 0;
            }
            // `parse_options()` rejects these outright: they belong to
            // `cmd_diff()`, not to the no-index parser, and never reach it.
            s if NOT_IN_NO_INDEX.contains(&s) => {
                eprintln!("error: unknown option `{}'", s.trim_start_matches('-'));
                return usage();
            }
            s => anyhow::bail!("unsupported option {s:?}"),
        }
    }
    // A required-argument option standing at the end of the line: parse-options
    // reports it and exits 129 before the operand count is looked at.
    if want_algorithm_value {
        eprintln!("error: option `diff-algorithm' requires a value");
        return Ok(ExitCode::from(129));
    }

    // `if (argc < 2)`: too few operands is where the implicit form explains
    // itself, since the user asked for `git diff` and is being shown the usage
    // of a command they did not name.
    if operands.len() < 2 {
        if implicit {
            eprintln!(
                "warning: Not a git repository. Use --no-index to compare two paths outside a working tree"
            );
        }
        return usage();
    }
    // `else if (argc > 2)`: extra operands are a pathspec, and `fixup_paths()`
    // accepts one only when both sides are directories. A pair that is not two
    // directories is git's refusal; a pair that is has no pathspec support here
    // yet, and says so in this port's own voice rather than borrowing git's.
    if operands.len() > 2 {
        let both_dirs = operands[..2].iter().all(|p| Path::new(p).is_dir());
        if both_dirs {
            anyhow::bail!("--no-index pathspec limiting is not supported");
        }
        eprintln!(
            "warning: Limiting comparison with pathspecs is only supported if both paths are directories."
        );
        return usage();
    }

    // Colour needs a repository to read `color.diff.*` out of. `--no-index` may run
    // without one, in which case the output is left uncoloured — the piped case,
    // which is the one parity is measured on, is uncoloured either way.
    let want_color = match color_when {
        Some(diff_color::ColorWhen::Always) => true,
        Some(diff_color::ColorWhen::Never) => false,
        _ => std::io::IsTerminal::is_terminal(&std::io::stdout()),
    };
    // git's `startup_info->have_repository`, which `diff_abbrev_oid()` branches
    // on: `--no-index` runs either way, but an id is only sized against an object
    // database when there is one.
    let repo = gix::discover(".").ok();
    let colors = match (want_color, &repo) {
        (true, Some(repo)) => diff_color::DiffColors::resolve(repo, true),
        _ => diff_color::DiffColors::disabled(),
    };
    let hexsz = HASH_KIND.len_in_hex();
    let default_abbrev = match &repo {
        Some(repo) => super::super::abbrev::configured_abbrev(repo, hexsz),
        None => super::super::abbrev::global_abbrev(hexsz),
    };
    // `--no-abbrev`'s zero reaches `--raw` as the whole name, while the `index`
    // line's `o->abbrev ? o->abbrev : DEFAULT_ABBREV` turns it back into the
    // configured width.
    let (abbrev, raw_abbrev) = match (no_abbrev, abbrev) {
        (true, _) => (default_abbrev, hexsz),
        (false, explicit) => {
            let v = explicit.unwrap_or(default_abbrev);
            (v, v)
        }
    };
    // `builtin_diff()`: `a_prefix = o->b_prefix; b_prefix = o->a_prefix` under
    // `-R` (diff.c:3862-3868). The exchange is of whatever the two prefixes ended
    // up as, so an explicit `--src-prefix`/`--dst-prefix` pair swaps with them and
    // `--no-prefix`'s two empty strings swap to no visible effect.
    if reverse {
        std::mem::swap(&mut src_prefix, &mut dst_prefix);
    }
    // `git_diff_ui_config()`'s `diff.algorithm` arm runs before `diff_no_index()`
    // does, so the key reaches a no-index comparison exactly as it reaches a
    // tracked one — and a command-line flag still wins, because
    // `diff_opt_diff_algorithm()` writes `options->xdl_opts` after
    // `diff_setup()` copied the configured default in (diff.c:5163). Without a
    // repository there is no config to read and the default is git's own:
    // `static long diff_algorithm;` is zero, which is Myers (diff.c:78).
    let algorithm = match (algorithm, &repo) {
        (Some(alg), _) => alg,
        (None, Some(repo)) => match repo.config_snapshot().string("diff.algorithm") {
            Some(name) => super::diff::parse_config_algorithm(name.as_ref())?,
            None => gix::diff::blob::Algorithm::Myers,
        },
        (None, None) => gix::diff::blob::Algorithm::Myers,
    };
    let opts = Opts {
        fmt: fmt.resolved(),
        ctx,
        ws,
        func_context,
        src_prefix,
        dst_prefix,
        abbrev,
        raw_abbrev,
        full_index,
        text,
        colors,
        algorithm,
    };

    // `diff_setup_done()`: `--find-copies-harder` implies copy detection
    // (diff.c:5288-5289), and `--quiet` — git's `flags.quick` — turns detection
    // off entirely along with it (diff.c:5348-5352). Without a command-line
    // opinion the default is `diff_detect_rename_default`, which `git_diff_ui_config`
    // sets from `diff.renames` and which is `DIFF_DETECT_RENAME` when that is unset.
    let mut rename_detection = detect_rename.unwrap_or_else(|| match &repo {
        Some(repo) => {
            let snapshot = repo.config_snapshot();
            let configured = snapshot.string("diff.renames");
            super::diffcore_rename::config_rename(configured.as_ref().map(|v| v.as_ref()))
        }
        None => super::diffcore_rename::DETECT_RENAME,
    });
    if find_copies_harder {
        rename_detection = super::diffcore_rename::DETECT_COPY;
    }
    if opts.fmt.quiet {
        rename_detection = 0;
        find_copies_harder = false;
    }

    let rename_opts = super::diffcore_rename::Options {
        detect_rename: rename_detection,
        rename_score,
        rename_limit: -1,
        find_copies_harder,
        rename_empty: true,
        break_opt: -1,
        hash_kind: HASH_KIND,
    };

    let (out, changed) = match compare(&operands[0], &operands[1], reverse, &opts, &rename_opts) {
        Ok(result) => result,
        Err(message) => {
            eprintln!("error: {message}");
            return Ok(ExitCode::from(1));
        }
    };

    if !opts.fmt.quiet {
        let painted = diff_color::colorize_patch(
            &out,
            &opts.colors,
            &diff_color::PaintOptions::default(),
            &[],
            diff_color::FilePaint::new(0),
        );
        use std::io::Write;
        let mut stdout = std::io::stdout().lock();
        stdout.write_all(&painted)?;
        stdout.flush()?;
    }
    // git-diff(1): "this option implies --exit-code".
    Ok(if changed { ExitCode::from(1) } else { ExitCode::SUCCESS })
}

/// The comparison itself: `queue_diff()`, then `diffcore_std()`'s passes, then
/// `diff_flush()`. Returns the uncoloured output and git's `has_changes`.
fn compare(
    lhs: &str,
    rhs: &str,
    reverse: bool,
    opts: &Opts,
    rename_opts: &super::diffcore_rename::Options,
) -> std::result::Result<(Vec<u8>, bool), String> {
    let mut pairs = queue(lhs, rhs)?;
    // `queue_diff()`'s `SWAP(mode1, mode2); SWAP(name1, name2); SWAP(special1,
    // special2)` (diff-no-index.c:279-283). It sits at the leaf of the walk, past
    // the directory recursion that decided which names pair with which, so the
    // queue keeps its order and each pair simply trades sides.
    if reverse {
        for (a, b) in &mut pairs {
            std::mem::swap(a, b);
        }
    }

    // Every blob the rest of this function reads, from one cache: `diffcore_rename`
    // asks for the same file repeatedly and the emitters ask again afterwards.
    let mut content = DiskContent::default();
    for (a, b) in &pairs {
        content.register(a);
        content.register(b);
    }

    // `diffcore_skip_stat_unmatch()` (diff.c:7396), which `--no-index` arms by
    // setting `skip_stat_unmatch = 1` (diff-no-index.c:407). Neither side of a
    // no-index pair carries an object id, so `diff_filespec_check_stat_unmatch()`
    // reduces to "both sides present, same mode, same bytes" — and this has to
    // happen *before* rename detection, because a pair the two directories share
    // by name must be gone by the time a deletion and an addition are considered
    // for pairing with each other.
    pairs.retain(|(a, b)| {
        !(a.file.is_some()
            && b.file.is_some()
            && a.mode == b.mode
            && content.bytes(&a.name) == content.bytes(&b.name))
    });

    // `diffcore_std()`'s rename slice (diff.c:7507-7516) over the queue
    // `queue_diff()` left, then `diff_resolve_rename_copy()` (diff.c:7525) to turn
    // each surviving pair into its status letter. `-B` is not accepted here, so
    // `break_opt` stays `-1` and the break/merge-broken passes are no-ops.
    let mut q = super::diffcore_rename::Queue::default();
    for (a, b) in &pairs {
        let one = q.add_spec(spec_of(a));
        let two = q.add_spec(spec_of(b));
        q.add_pair(one, two);
    }
    super::diffcore_rename::run(&mut q, rename_opts, &mut content).emit("diff.renameLimit");
    super::diffcore_rename::resolve_rename_copy(&mut q);

    let mut out: Vec<u8> = Vec::new();
    let mut changed = false;
    let mut stats: Vec<Row> = Vec::new();
    for pi in 0..q.pairs.len() {
        let pair = q.pairs[pi].clone();
        let a = side_of(&q.specs[pair.one], &content);
        let b = side_of(&q.specs[pair.two], &content);
        let (a, b) = (&a, &b);
        let old_data = content.bytes(&a.name).filter(|_| a.file.is_some()).unwrap_or_default();
        let new_data = content.bytes(&b.name).filter(|_| b.file.is_some()).unwrap_or_default();
        // A pair that reaches this point and still has identical content is a
        // rename or copy the detection just made; the stat-unmatch pass above
        // already dropped every same-name pair whose content never changed.
        let same_content = a.file.is_some() == b.file.is_some() && old_data == new_data;
        changed = true;
        let binary = !opts.text
            && (super::diff::looks_binary(&old_data) || super::diff::looks_binary(&new_data));
        let (added, deleted, body) =
        super::diff::no_index_body(
            &old_data,
            &new_data,
            &opts.ctx_geometry(),
            opts.ws,
            binary,
            opts.algorithm,
        );
        // `hash_filespec()` (diffcore-rename.c) is what leaves an object id on a
        // filespec `queue_diff()` created with the null one, and it runs only
        // inside rename detection. `--raw` therefore prints real ids exactly when
        // detection got far enough to hash — which is what `oid_valid` records.
        let spec_oid = |i: usize| -> Option<gix::ObjectId> {
            q.specs[i].oid_valid.then(|| q.specs[i].oid)
        };
        stats.push(Row {
            a_name: a.display_name().clone(),
            b_name: b.display_name().clone(),
            added,
            deleted,
            binary,
            a_exists: a.file.is_some(),
            b_exists: b.file.is_some(),
            a_mode: a.mode,
            b_mode: b.mode,
            a_oid: spec_oid(pair.one),
            b_oid: spec_oid(pair.two),
            status: pair.status,
            score: pair.score,
        });
        if opts.fmt.patch {
            emit_header(&mut out, a, b, &old_data, &new_data, &opts, same_content, binary, &pair);
            // `builtin_diff()`'s binary arm stops at the header when the two sides
            // hold the same object (diff.c):
            //
            // ```c
            // if (oideq(&one->oid, &two->oid)) {
            //         if (must_show_header)
            //                 fprintf(o->file, "%s", header.buf);
            //         goto free_ab_and_return;
            // }
            // ```
            //
            // The only pair that gets here unchanged is a rename or copy the
            // detection just made, and a 100%-similar *binary* rename was picking up
            // a `Binary files … differ` line for content that is identical. The text
            // side needs no such test: its body is empty when nothing changed.
            if binary && !same_content {
                out.extend_from_slice(b"Binary files ");
                push_name(&mut out, &opts.src_prefix, a.header_name(b), a.file.is_some());
                out.extend_from_slice(b" and ");
                push_name(&mut out, &opts.dst_prefix, b.header_name(a), b.file.is_some());
                out.extend_from_slice(b" differ\n");
            } else {
                out.extend_from_slice(&body);
            }
        }
    }

    if !opts.fmt.patch && !stats.is_empty() {
        render_non_patch(&mut out, &stats, opts);
    }
    Ok((out, changed))
}

impl Opts {
    fn ctx_geometry(&self) -> super::diff_pairs::EmitGeometry {
        super::diff_pairs::EmitGeometry {
            ctx: self.ctx as usize,
            inter_hunk_ctx: 0,
            func_context: self.func_context,
        }
    }
}

/// `name_a += (*name_a == '/')` (diff.c:1899-1900, and again at diff.c:3899-3900
/// where the `diff --git` names are built): exactly *one* leading slash is dropped
/// before the `a/` / `b/` prefix goes on, so an absolute operand reads
/// `a/private/tmp/x` rather than `a//private/tmp/x`. The increment is a boolean, so
/// a name that really does start with two slashes keeps the second one.
fn strip_one_leading_slash(name: &BString) -> &[u8] {
    let bytes = name.as_bytes();
    match bytes.first() {
        Some(b'/') => &bytes[1..],
        _ => bytes,
    }
}

/// `<prefix><name>`, with `/dev/null` written bare for a side that is not there.
fn push_name(out: &mut Vec<u8>, prefix: &[u8], name: &BString, exists: bool) {
    if !exists {
        out.extend_from_slice(b"/dev/null");
        return;
    }
    out.extend_from_slice(prefix);
    out.extend_from_slice(strip_one_leading_slash(name));
}

/// `fill_metainfo()` + `emit_diff_symbol(DIFF_SYMBOL_HEADER)`: the `diff --git`
/// line, the mode and index lines, and the two file markers.
#[allow(clippy::too_many_arguments)]
fn emit_header(
    out: &mut Vec<u8>,
    a: &Side,
    b: &Side,
    old_data: &[u8],
    new_data: &[u8],
    opts: &Opts,
    same_content: bool,
    binary: bool,
    pair: &super::diffcore_rename::Pair,
) {
    out.extend_from_slice(b"diff --git ");
    out.extend_from_slice(&opts.src_prefix);
    out.extend_from_slice(strip_one_leading_slash(a.header_name(b)));
    out.push(b' ');
    out.extend_from_slice(&opts.dst_prefix);
    out.extend_from_slice(strip_one_leading_slash(b.header_name(a)));
    out.push(b'\n');

    let hash = |data: &[u8], exists: bool| -> String {
        if !exists {
            return "0".repeat(opts.abbrev);
        }
        let hex = blob_id(data).to_hex().to_string();
        let len = if opts.full_index { hex.len() } else { opts.abbrev.min(hex.len()) };
        hex[..len].to_string()
    };

    match (a.file.is_some(), b.file.is_some()) {
        (false, true) => {
            out.extend_from_slice(format!("new file mode {:o}\n", b.mode).as_bytes());
        }
        (true, false) => {
            out.extend_from_slice(format!("deleted file mode {:o}\n", a.mode).as_bytes());
        }
        (true, true) if a.mode != b.mode => {
            out.extend_from_slice(format!("old mode {:o}\n", a.mode).as_bytes());
            out.extend_from_slice(format!("new mode {:o}\n", b.mode).as_bytes());
        }
        _ => {}
    }

    // `fill_metainfo()`'s `xfrm_msg` for a rename or copy, between the mode lines
    // and the `index` line. The two names go in raw — no `a/`/`b/` prefix and, as
    // stock confirms for an absolute operand, no leading-slash strip either.
    if matches!(pair.status, b'R' | b'C') {
        let verb = if pair.status == b'C' { "copy" } else { "rename" };
        out.extend_from_slice(
            format!(
                "similarity index {}%\n",
                super::diffcore_rename::similarity_index(pair.score)
            )
            .as_bytes(),
        );
        out.extend_from_slice(format!("{verb} from ").as_bytes());
        out.extend_from_slice(a.name.as_bytes());
        out.extend_from_slice(format!("\n{verb} to ").as_bytes());
        out.extend_from_slice(b.name.as_bytes());
        out.push(b'\n');
    }

    // A pure mode change — and a rename or copy that moved the content unaltered —
    // has nothing to describe, so no `index` line and no file markers follow.
    if same_content {
        return;
    }

    out.extend_from_slice(b"index ");
    out.extend_from_slice(hash(old_data, a.file.is_some()).as_bytes());
    out.extend_from_slice(b"..");
    out.extend_from_slice(hash(new_data, b.file.is_some()).as_bytes());
    // The mode is repeated on the index line only when both sides share it.
    if a.file.is_some() && b.file.is_some() && a.mode == b.mode {
        out.extend_from_slice(format!(" {:o}", a.mode).as_bytes());
    }
    out.push(b'\n');

    // `emit_diff_symbol(DIFF_SYMBOL_FILEPAIR_*)` is skipped for a binary pair:
    // there are no line markers to introduce.
    if binary {
        return;
    }
    out.extend_from_slice(b"--- ");
    push_name(out, &opts.src_prefix, a.header_name(b), a.file.is_some());
    out.push(b'\n');
    out.extend_from_slice(b"+++ ");
    push_name(out, &opts.dst_prefix, b.header_name(a), b.file.is_some());
    out.push(b'\n');
}

/// `--stat` / `--numstat` / `--shortstat` / `--name-only` / `--name-status` /
/// `--raw` / `--summary`, in `diff_flush()`'s order.
fn render_non_patch(out: &mut Vec<u8>, rows: &[Row], opts: &Opts) {
    // `diff_flush_name()` prints the post-image name; `diff_flush_raw()` and
    // `--name-status` print the pre-image one, which for an addition is the only
    // name there is.
    if opts.fmt.name_only {
        for row in rows {
            out.extend_from_slice(row.b_name.as_bytes());
            out.push(b'\n');
        }
        return;
    }
    if opts.fmt.name_status || opts.fmt.raw {
        for row in rows {
            if opts.fmt.raw {
                let side = |oid: Option<gix::ObjectId>| match oid {
                    Some(id) => {
                        let hex = id.to_hex().to_string();
                        hex[..opts.raw_abbrev.min(hex.len())].to_string()
                    }
                    None => "0".repeat(opts.raw_abbrev),
                };
                out.extend_from_slice(
                    format!(
                        ":{:06o} {:06o} {} {} ",
                        row.a_mode,
                        row.b_mode,
                        side(row.a_oid),
                        side(row.b_oid),
                    )
                    .as_bytes(),
                );
            }
            out.push(row.status);
            // `diff_flush_raw()`/`--name-status`: a rename or copy carries the
            // similarity as three digits and then *both* names, TAB-separated.
            if row.renamed() {
                out.extend_from_slice(
                    format!("{:03}", super::diffcore_rename::similarity_index(row.score)).as_bytes(),
                );
                out.push(b'\t');
                out.extend_from_slice(row.a_name.as_bytes());
                out.push(b'\t');
                out.extend_from_slice(row.b_name.as_bytes());
            } else {
                out.push(b'\t');
                let name = if row.a_exists { &row.a_name } else { &row.b_name };
                out.extend_from_slice(name.as_bytes());
            }
            out.push(b'\n');
        }
        return;
    }
    let stat_rows: Vec<(BString, BString, u32, u32, bool)> = rows
        .iter()
        .map(|r| (r.a_name.clone(), r.b_name.clone(), r.added, r.deleted, r.binary))
        .collect();
    if opts.fmt.numstat {
        super::diff::render_rows_numstat(out, &stat_rows, false);
    }
    if opts.fmt.stat {
        super::diff::render_rows_stat(out, &stat_rows, &opts.colors);
    }
    if opts.fmt.shortstat {
        let (adds, dels) = rows
            .iter()
            .filter(|r| !r.binary)
            .fold((0u32, 0u32), |(a, d), r| (a + r.added, d + r.deleted));
        super::diffstat::print_stat_summary(out, rows.len() as u64, u64::from(adds), u64::from(dels));
    }
    if opts.fmt.summary {
        // `show_file_mode_name(opt, "create", p->two)` and `(…, "delete", p->one)`
        // (diff.c): each takes the mode *and* the path off its own side of the
        // pair, so a deletion names the file that was there — not the `/dev/null`
        // that replaced it, whose mode is zero.
        for row in rows {
            // `show_rename_copy()` (diff.c): the `pprint_rename`d name pair and the
            // similarity, which is the only summary line a two-sided pair produces.
            if row.renamed() {
                let verb = if row.status == b'C' { "copy" } else { "rename" };
                out.extend_from_slice(format!(" {verb} ").as_bytes());
                out.extend_from_slice(&super::diff::pprint_rename(&row.a_name, &row.b_name));
                out.extend_from_slice(
                    format!(
                        " ({}%)\n",
                        super::diffcore_rename::similarity_index(row.score)
                    )
                    .as_bytes(),
                );
                // `show_rename_copy()` closes with `show_mode_change(opt, p, 0)`: the
                // moved file may also have changed mode, and the name is left off
                // because the line above just gave both spellings of it.
                show_mode_change(out, row, false);
                continue;
            }
            match (row.a_exists, row.b_exists) {
                (false, true) => {
                    out.extend_from_slice(format!(" create mode {:06o} ", row.b_mode).as_bytes());
                    out.extend_from_slice(&super::diff::quoted_name(&row.b_name));
                    out.push(b'\n');
                }
                (true, false) => {
                    out.extend_from_slice(format!(" delete mode {:06o} ", row.a_mode).as_bytes());
                    out.extend_from_slice(&super::diff::quoted_name(&row.a_name));
                    out.push(b'\n');
                }
                // `diff_summary()`'s `default:` arm — a pair that is on both sides
                // reports only the mode, and only when it moved.
                _ => show_mode_change(out, row, true),
            }
        }
    }
}

/// `show_mode_change()` (diff.c): the `--summary` line for a file whose mode moved.
/// Both sides must have a mode — a create or delete has already said everything
/// there is to say — and `show_name` is what separates the standalone line from the
/// one that trails a rename, whose name was printed by the rename line itself.
fn show_mode_change(out: &mut Vec<u8>, row: &Row, show_name: bool) {
    if row.a_mode == 0 || row.b_mode == 0 || row.a_mode == row.b_mode {
        return;
    }
    out.extend_from_slice(
        format!(" mode change {:06o} => {:06o}", row.a_mode, row.b_mode).as_bytes(),
    );
    if show_name {
        out.push(b' ');
        out.extend_from_slice(&super::diff::quoted_name(&row.b_name));
    }
    out.push(b'\n');
}

#[cfg(test)]
mod tests {
    use super::*;

    fn present(name: &str) -> Side {
        Side { name: BString::from(name), file: Some(PathBuf::from(name)), mode: 0o100644 }
    }

    /// A pair that exists on one side only, as `queue_dirs()` builds it: the
    /// missing half is named `/dev/null` and has no mode. `oid` is the id
    /// `hash_filespec()` left on the side that is there, or `None` when rename
    /// detection never ran far enough to hash it.
    fn one_sided(name: &str, exists_on_left: bool, oid: Option<gix::ObjectId>) -> Row {
        let (a_name, b_name) = match exists_on_left {
            true => (BString::from(name), BString::from(DEV_NULL)),
            false => (BString::from(DEV_NULL), BString::from(name)),
        };
        Row {
            a_name,
            b_name,
            added: u32::from(!exists_on_left),
            deleted: u32::from(exists_on_left),
            binary: false,
            a_exists: exists_on_left,
            b_exists: !exists_on_left,
            a_mode: if exists_on_left { 0o100644 } else { 0 },
            b_mode: if exists_on_left { 0 } else { 0o100644 },
            a_oid: exists_on_left.then_some(oid).flatten(),
            b_oid: (!exists_on_left).then_some(oid).flatten(),
            status: if exists_on_left { b'D' } else { b'A' },
            score: 0,
        }
    }

    /// A scratch directory that removes itself, so the end-to-end tests can build
    /// real trees for [`compare`] to walk without leaving anything behind.
    struct Scratch(PathBuf);

    impl Scratch {
        fn new(tag: &str) -> Scratch {
            let dir = std::env::temp_dir().join(format!(
                "zvcs-no-index-{tag}-{}-{:?}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .expect("clock after the epoch")
                    .as_nanos()
            ));
            std::fs::create_dir_all(&dir).expect("scratch directory");
            Scratch(dir)
        }

        fn write(&self, rela: &str, body: &str) {
            let path = self.0.join(rela);
            std::fs::create_dir_all(path.parent().expect("a parent")).expect("scratch subdirectory");
            std::fs::write(path, body).expect("scratch file");
        }

        fn at(&self, rela: &str) -> String {
            self.0.join(rela).to_string_lossy().into_owned()
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// The `--no-index` default: rename detection on, at the default score.
    fn renames_on() -> super::super::diffcore_rename::Options {
        super::super::diffcore_rename::Options {
            detect_rename: super::super::diffcore_rename::DETECT_RENAME,
            hash_kind: HASH_KIND,
            ..Default::default()
        }
    }

    fn compared(s: &Scratch, fmt: Format, rename: &super::super::diffcore_rename::Options) -> String {
        let o = opts(fmt.resolved(), 7);
        let (out, _) = compare(&s.at("A"), &s.at("B"), false, &o, rename).expect("readable operands");
        String::from_utf8(out).expect("ascii fixtures")
    }

    fn opts(fmt: Format, raw_abbrev: usize) -> Opts {
        Opts {
            fmt,
            ctx: 3,
            ws: super::super::diff::Whitespace::Keep,
            func_context: false,
            src_prefix: b"a/".to_vec(),
            dst_prefix: b"b/".to_vec(),
            abbrev: 7,
            raw_abbrev,
            full_index: false,
            text: false,
            // git's default: `static long diff_algorithm` (diff.c) is zero, i.e.
            // Myers, and `--no-index` reaches it through the same `diff_opts`
            // table as every other verb.
            algorithm: gix::diff::blob::Algorithm::Myers,
            colors: diff_color::DiffColors::disabled(),
        }
    }

    fn rendered(rows: &[Row], o: &Opts) -> String {
        let mut out = Vec::new();
        render_non_patch(&mut out, rows, o);
        String::from_utf8(out).expect("ascii fixtures")
    }

    /// `--summary`'s deletion line takes both its mode and its name from the
    /// *pre*-image (`show_file_mode_name(opt, "delete", p->one)`), not from the
    /// `/dev/null` the pair's other half became. Stock git 2.55.0 over a directory
    /// pair where `only_a.txt` exists on the left and `only_b.txt` on the right:
    ///
    /// ```text
    /// $ git diff --no-index --summary da db
    ///  delete mode 100644 da/only_a.txt
    ///  create mode 100644 db/only_b.txt
    /// ```
    ///
    /// Reading the post-image for both lines — which is the shape this port
    /// shipped — turns the first into ` delete mode 000000 /dev/null`, because
    /// that side has neither a name nor a mode of its own.
    #[test]
    fn summary_names_the_side_each_line_is_about() {
        let rows =
            [one_sided("da/only_a.txt", true, None), one_sided("db/only_b.txt", false, None)];
        let o = opts(Format { summary: true, ..Format::default() }, 7);
        assert_eq!(
            rendered(&rows, &o),
            " delete mode 100644 da/only_a.txt\n create mode 100644 db/only_b.txt\n"
        );
    }

    /// `diff_summary()`'s `default:` arm is `show_mode_change(opt, p, !p->score)`, so
    /// a pair present on both sides reports its mode and nothing else — and only when
    /// the mode actually moved. Stock git 2.55.0 over two directories whose `m`
    /// differs only in its executable bit:
    ///
    /// ```text
    /// $ git diff --no-index --summary x y
    ///  mode change 100644 => 100755 y/m
    /// ```
    ///
    /// Dropping the both-sides case — which is the shape this port shipped — prints
    /// nothing at all for that pair.
    #[test]
    fn summary_reports_a_mode_change_on_a_pair_that_stayed() {
        let rows = [Row {
            a_name: BString::from("x/m"),
            b_name: BString::from("y/m"),
            added: 0,
            deleted: 0,
            binary: false,
            a_exists: true,
            b_exists: true,
            a_mode: 0o100644,
            b_mode: 0o100755,
            a_oid: None,
            b_oid: None,
            status: b'M',
            score: 0,
        }];
        let o = opts(Format { summary: true, ..Format::default() }, 7);
        assert_eq!(rendered(&rows, &o), " mode change 100644 => 100755 y/m\n");
    }

    /// The same pair with an unchanged mode says nothing: `show_mode_change()` returns
    /// before it writes when `p->one->mode == p->two->mode`.
    #[test]
    fn summary_is_silent_when_only_the_content_moved() {
        let rows = [Row {
            a_name: BString::from("x/m"),
            b_name: BString::from("y/m"),
            added: 1,
            deleted: 1,
            binary: false,
            a_exists: true,
            b_exists: true,
            a_mode: 0o100644,
            b_mode: 0o100644,
            a_oid: None,
            b_oid: None,
            status: b'M',
            score: 0,
        }];
        let o = opts(Format { summary: true, ..Format::default() }, 7);
        assert_eq!(rendered(&rows, &o), "");
    }

    /// `show_file_mode_name()` writes the path through `quote_c_style()`, so a name
    /// carrying a `"` comes out double-quoted and escaped. Stock git 2.55.0:
    ///
    /// ```text
    ///  delete mode 100644 "x/we ird\"n.txt"
    /// ```
    #[test]
    fn summary_c_quotes_a_name_that_needs_it() {
        let rows = [one_sided("x/we ird\"n.txt", true, None)];
        let o = opts(Format { summary: true, ..Format::default() }, 7);
        assert_eq!(rendered(&rows, &o), " delete mode 100644 \"x/we ird\\\"n.txt\"\n");
    }

    /// `--raw` shows a real blob id exactly when rename detection got far enough
    /// to compute one. `diffcore_rename()` gives up before its exact-match pass
    /// unless the queue holds *both* a destination and a source
    /// (diffcore-rename.c:1461), and that pass — `hash_filespec()` — is the only
    /// thing that ever replaces the null id `queue_diff()` created the filespecs
    /// with. Stock git 2.55.0, over a directory pair where `only_a.txt` exists on
    /// the left, `only_b.txt` on the right and `common.txt` differs on both:
    ///
    /// ```text
    /// $ git diff --no-index --raw da db
    /// :100644 100644 0000000 0000000 M	da/common.txt
    /// :100644 000000 f8779b8 0000000 D	da/only_a.txt
    /// :000000 100644 0000000 5709f57 A	db/only_b.txt
    /// $ git diff --no-index --raw addonly_a addonly_b     # nothing deleted
    /// :000000 100644 0000000 0000000 A	addonly_b/n1.txt
    /// ```
    ///
    /// The modified pair keeps two null ids in both runs: it is neither a rename
    /// source nor a destination, so nothing hashes it. The whole pipeline is run
    /// here rather than the renderer alone, because it is the detection pass that
    /// decides — the row only reports what the filespec ended up holding.
    #[test]
    fn raw_prints_ids_only_when_rename_detection_would_have_hashed_them() {
        let s = Scratch::new("raw-gate");
        s.write("A/common.txt", "left\n");
        s.write("B/common.txt", "right\n");
        s.write("A/only_a.txt", "gone\n");
        s.write("B/only_b.txt", "fresh\n");
        let gone = blob_id(b"gone\n").to_hex().to_string();
        let fresh = blob_id(b"fresh\n").to_hex().to_string();
        assert_eq!(
            compared(&s, Format { raw: true, ..Format::default() }, &renames_on()),
            format!(
                ":100644 100644 0000000 0000000 M\t{}\n\
                 :100644 000000 {} 0000000 D\t{}\n\
                 :000000 100644 0000000 {} A\t{}\n",
                s.at("A/common.txt"),
                &gone[..7],
                s.at("A/only_a.txt"),
                &fresh[..7],
                s.at("B/only_b.txt"),
            )
        );

        // A queue with no deletion never reaches the hashing pass, so both sides
        // of the addition stay null.
        let adds = Scratch::new("raw-gate-adds");
        adds.write("A/keep.txt", "same\n");
        adds.write("B/keep.txt", "same\n");
        adds.write("B/only_b.txt", "fresh\n");
        assert_eq!(
            compared(&adds, Format { raw: true, ..Format::default() }, &renames_on()),
            format!(":000000 100644 0000000 0000000 A\t{}\n", adds.at("B/only_b.txt"))
        );

        // `--no-renames` skips the pass outright, so even a queue with both a
        // source and a destination prints zeros on every side.
        let no_renames = super::super::diffcore_rename::Options {
            hash_kind: HASH_KIND,
            ..Default::default()
        };
        assert_eq!(
            compared(&s, Format { raw: true, ..Format::default() }, &no_renames),
            format!(
                ":100644 100644 0000000 0000000 M\t{}\n\
                 :100644 000000 0000000 0000000 D\t{}\n\
                 :000000 100644 0000000 0000000 A\t{}\n",
                s.at("A/common.txt"),
                s.at("A/only_a.txt"),
                s.at("B/only_b.txt"),
            )
        );
    }

    /// Identical content that moved between the two directories is a rename, and
    /// every format says so. `diffcore_std()` runs its rename pass over the
    /// `--no-index` queue exactly as it does over a tracked one
    /// (diff-no-index.c:426), which is what makes the delete/create pair collapse.
    /// Stock git 2.55.0, with `ra/x.txt` and `rb/y.txt` holding the same bytes:
    ///
    /// ```text
    /// $ git diff --no-index ra rb
    /// diff --git a/ra/x.txt b/rb/y.txt
    /// similarity index 100%
    /// rename from ra/x.txt
    /// rename to rb/y.txt
    /// $ git diff --no-index --raw ra rb
    /// :100644 100644 7a28df3 7a28df3 R100	ra/x.txt	rb/y.txt
    /// $ git diff --no-index --summary ra rb
    ///  rename ra/x.txt => rb/y.txt (100%)
    /// $ git diff --no-index --stat ra rb
    ///  ra/x.txt => rb/y.txt | 0
    ///  1 file changed, 0 insertions(+), 0 deletions(-)
    /// ```
    ///
    /// The patch carries no `index` line and no `---`/`+++` pair: the content is
    /// unchanged, so there is nothing to describe past the rename itself.
    #[test]
    fn an_exact_move_between_directories_is_one_rename() {
        let s = Scratch::new("exact-rename");
        let body = "alpha\nbeta\ngamma\ndelta\n";
        s.write("A/x.txt", body);
        s.write("B/y.txt", body);
        let (src, dst) = (s.at("A/x.txt"), s.at("B/y.txt"));
        let id = blob_id(body.as_bytes()).to_hex().to_string();

        assert_eq!(
            compared(&s, Format::default(), &renames_on()),
            format!(
                "diff --git a{src} b{dst}\n\
                 similarity index 100%\n\
                 rename from {src}\n\
                 rename to {dst}\n"
            )
        );
        assert_eq!(
            compared(&s, Format { raw: true, ..Format::default() }, &renames_on()),
            format!(":100644 100644 {0} {0} R100\t{src}\t{dst}\n", &id[..7])
        );
        assert_eq!(
            compared(&s, Format { name_status: true, ..Format::default() }, &renames_on()),
            format!("R100\t{src}\t{dst}\n")
        );
        assert_eq!(
            compared(&s, Format { summary: true, ..Format::default() }, &renames_on()),
            format!(
                " rename {} (100%)\n",
                String::from_utf8(super::super::diff::pprint_rename(src.as_bytes(), dst.as_bytes()))
                    .expect("ascii fixtures")
            )
        );

        // Without detection the same tree is a delete plus a create, which is what
        // this port produced for every `--no-index` run before the pass was wired
        // in.
        let off = super::super::diffcore_rename::Options {
            hash_kind: HASH_KIND,
            ..Default::default()
        };
        let plain = compared(&s, Format { name_status: true, ..Format::default() }, &off);
        assert_eq!(plain, format!("D\t{src}\nA\t{dst}\n"));
    }

    /// A move that also edited the file is a rename below 100%, and then the patch
    /// *does* carry an `index` line and a body. Stock git 2.55.0, over a 60-line
    /// file with one line changed:
    ///
    /// ```text
    /// diff --git a/A/near.txt b/B/near_moved.txt
    /// similarity index 98%
    /// rename from A/near.txt
    /// rename to B/near_moved.txt
    /// index 51ed5c4870..4e295424cc 100644
    /// --- a/A/near.txt
    /// +++ b/B/near_moved.txt
    /// ```
    #[test]
    fn a_move_with_an_edit_keeps_its_index_line_and_body() {
        let s = Scratch::new("near-rename");
        let before: String = (0..60).map(|i| format!("line {i}\n")).collect();
        let after = before.replace("line 3\n", "CHANGED\n");
        s.write("A/near.txt", &before);
        s.write("B/moved.txt", &after);
        let (src, dst) = (s.at("A/near.txt"), s.at("B/moved.txt"));

        let patch = compared(&s, Format::default(), &renames_on());
        assert!(patch.starts_with(&format!("diff --git a{src} b{dst}\n")), "{patch}");
        assert!(patch.contains("similarity index 98%\n"), "{patch}");
        assert!(patch.contains(&format!("rename from {src}\nrename to {dst}\n")), "{patch}");
        assert!(patch.contains("\nindex "), "{patch}");
        assert!(patch.contains(&format!("\n--- a{src}\n+++ b{dst}\n")), "{patch}");
        assert!(patch.contains("\n-line 3\n+CHANGED\n"), "{patch}");

        assert_eq!(
            compared(&s, Format { name_status: true, ..Format::default() }, &renames_on()),
            format!("R098\t{src}\t{dst}\n")
        );
    }

    /// `--no-abbrev` widens `--raw` to the whole name (`diff_flush_raw()` passes
    /// `opt->abbrev` straight through, and zero means "all of it"), and the null
    /// id widens with it. Stock git 2.55.0:
    ///
    /// ```text
    /// $ git diff --no-index --no-abbrev --raw da db
    /// :100644 000000 f8779b82a49aeea68c066a40cc5828aa69af10e6 0000000000000000000000000000000000000000 D	da/only_a.txt
    /// ```
    #[test]
    fn raw_widths_follow_the_requested_abbreviation() {
        let deleted = gix::ObjectId::from_hex(b"f8779b82a49aeea68c066a40cc5828aa69af10e6").unwrap();
        let added = gix::ObjectId::from_hex(b"5709f57480157adcd6e54fdea37b43fbec0598bc").unwrap();
        let rows = [
            one_sided("da/only_a.txt", true, Some(deleted)),
            one_sided("db/only_b.txt", false, Some(added)),
        ];
        let wide = rendered(&rows, &opts(Format { raw: true, ..Format::default() }, 40));
        assert!(
            wide.starts_with(
                ":100644 000000 f8779b82a49aeea68c066a40cc5828aa69af10e6 \
                 0000000000000000000000000000000000000000 D\tda/only_a.txt\n"
            ),
            "{wide}"
        );
        // The configured width, which is what `core.abbrev = 10` produces.
        let ten = rendered(&rows, &opts(Format { raw: true, ..Format::default() }, 10));
        assert!(
            ten.starts_with(":100644 000000 f8779b82a4 0000000000 D\tda/only_a.txt\n"),
            "{ten}"
        );
    }

    /// `-R` and the `/dev/null` naming rule, which interact.
    ///
    /// `queue_diff()` swaps the two sides (diff-no-index.c:279-283) while
    /// `header_name()` makes an absent side borrow its peer's name, so a swapped
    /// addition has to end up printing the *same* name on both halves of the
    /// header — with the prefixes exchanged. Stock git 2.55.0, on a directory pair
    /// where `da/only_a.txt` exists only on the left:
    ///
    /// ```text
    /// $ git diff --no-index da db          # forward
    /// diff --git a/da/only_a.txt b/da/only_a.txt
    /// --- a/da/only_a.txt
    /// +++ /dev/null
    /// $ git diff --no-index -R da db       # reversed
    /// diff --git b/da/only_a.txt a/da/only_a.txt
    /// --- /dev/null
    /// +++ a/da/only_a.txt
    /// ```
    ///
    /// The `/dev/null` marker moves to the other half and the borrowed name stays
    /// put. A swap that forgot the borrow would print `/dev/null` as a name.
    #[test]
    fn reversing_a_one_sided_pair_moves_dev_null_and_keeps_the_borrowed_name() {
        let (mut a, mut b) = (present("da/only_a.txt"), Side::absent(BString::from("da/only_a.txt")));
        assert_eq!(a.header_name(&b), "da/only_a.txt");
        assert_eq!(b.header_name(&a), "da/only_a.txt");

        std::mem::swap(&mut a, &mut b);
        // The deletion has become an addition: the absent side is now the first.
        assert!(a.file.is_none() && b.file.is_some());
        // Both halves still name the file that exists, never `/dev/null`.
        assert_eq!(a.header_name(&b), "da/only_a.txt");
        assert_eq!(b.header_name(&a), "da/only_a.txt");

        // `push_name` is what turns the absent side into the literal marker, and
        // it ignores the prefix when it does.
        let mut out = Vec::new();
        push_name(&mut out, b"b/", a.header_name(&b), a.file.is_some());
        out.push(b' ');
        push_name(&mut out, b"a/", b.header_name(&a), b.file.is_some());
        assert_eq!(out.as_bstr(), "/dev/null a/da/only_a.txt");
    }

    /// A two-sided pair reversed: names trade places and the prefixes trade with
    /// them, which is `diff --git b/<rhs> a/<lhs>` in stock's output.
    ///
    /// ```text
    /// $ git diff --no-index -R d1/a.txt d1/b.txt
    /// diff --git b/d1/b.txt a/d1/a.txt
    /// $ git diff --no-index -R --src-prefix=S/ --dst-prefix=D/ a.txt b.txt
    /// diff --git D/b.txt S/a.txt
    /// ```
    ///
    /// The second line is the one worth pinning: git exchanges the prefix
    /// *values* (diff.c:3862-3868), so a custom pair follows the swap rather than
    /// staying pinned to its side of the diff.
    #[test]
    fn reversing_a_two_sided_pair_swaps_names_and_prefix_values() {
        let (mut a, mut b) = (present("d1/a.txt"), present("d1/b.txt"));
        let (mut src, mut dst) = (b"a/".to_vec(), b"b/".to_vec());
        std::mem::swap(&mut a, &mut b);
        std::mem::swap(&mut src, &mut dst);

        let mut out = Vec::new();
        push_name(&mut out, &src, a.header_name(&b), true);
        out.push(b' ');
        push_name(&mut out, &dst, b.header_name(&a), true);
        assert_eq!(out.as_bstr(), "b/d1/b.txt a/d1/a.txt");

        let (mut src, mut dst) = (b"S/".to_vec(), b"D/".to_vec());
        std::mem::swap(&mut src, &mut dst);
        let mut out = Vec::new();
        push_name(&mut out, &src, a.header_name(&b), true);
        out.push(b' ');
        push_name(&mut out, &dst, b.header_name(&a), true);
        assert_eq!(out.as_bstr(), "D/d1/b.txt S/d1/a.txt");
    }
}

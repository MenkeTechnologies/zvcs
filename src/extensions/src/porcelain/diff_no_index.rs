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
//! here is a gap rather than a name git rejects. The exceptions are collected in
//! [`NOT_IN_NO_INDEX`]: `--cached`/`--staged`/`--merge-base` belong to `cmd_diff()`
//! and `--expand-tabs`/`--no-expand-tabs` to `builtin/log.c`, so all five are
//! `unknown option` here. Algorithm selection is on it:
//! `--diff-algorithm=<v>`, the separated `--diff-algorithm <v>`, `--minimal`,
//! `--patience`, `--histogram`, and the `diff.algorithm` default when the
//! comparison happens to be run from inside a repository. So are
//! `--inter-hunk-context=<n>`, `-D`/`--irreversible-delete`, `--binary`,
//! `--output=<file>` and `--skip-to=<p>`/`--rotate-to=<p>` — the last pair anchored
//! on the *post-image* name (`R/x`, not `x`) and, because
//! `builtin_diff_no_index()` never raises `rotate_to_strict`, silent rather than
//! fatal when the target names no pair.
//!
//! `diff_setup_done()`'s two output-format rules apply here as well:
//! `--name-only`/`--name-status` clear every other format bit (so `--name-only -p`
//! prints names and no patch), `-s` *assigns* `DIFF_FORMAT_NO_OUTPUT` where it
//! stands, more than one of the four exclusive formats is
//! `fatal: options '--name-only', '--name-status', '--check', and '-s' cannot be
//! used together`, and whatever survives is written before the patch with
//! `DIFF_SYMBOL_SEPARATOR`'s blank line between the two blocks.

use anyhow::Result;
use gix::bstr::{BString, ByteSlice};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use super::diff_color;

/// `/dev/null` as an operand: git's `DIFF_FILE_VALID` for the side is false, and
/// the pair takes the other side's name.
const DEV_NULL: &str = "/dev/null";

/// `file_from_standard_input` (diff-no-index.c:264-271): the operand `-` names
/// standard input, and a real path spelled `-` has to be written `./-`.
const STDIN_NAME: &str = "-";

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
    // `get_mode()` answers for standard input without touching the filesystem:
    // `create_ce_mode(0666)`, which is `100644`.
    let l_in = lhs == STDIN_NAME;
    let r_in = rhs == STDIN_NAME;
    let l_meta = (!l_null && !l_in).then(|| std::fs::symlink_metadata(lhs));
    let r_meta = (!r_null && !r_in).then(|| std::fs::symlink_metadata(rhs));

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

    // `queue_diff()`'s `if (!mode1 && !mode2) return 0;` — two operands that are
    // both `/dev/null` name nothing to compare, so the queue stays empty and the
    // command exits 0.
    if l_null && r_null {
        return Ok(Vec::new());
    }

    let side = |name: &str, is_null: bool, is_stdin: bool| -> Result<Side, String> {
        if is_null {
            return Ok(Side::absent(BString::from(name)));
        }
        if is_stdin {
            return Ok(Side {
                name: BString::from(name),
                file: Some(PathBuf::from(name)),
                mode: 0o100644,
            });
        }
        let path = PathBuf::from(name);
        let mode = mode_of(&path).map_err(|_| format!("Could not access '{name}'"))?;
        Ok(Side { name: BString::from(name), file: Some(path), mode })
    };
    Ok(vec![(side(lhs, l_null, l_in)?, side(rhs, r_null, r_in)?)])
}

/// `fixup_paths()` (diff-no-index.c): when exactly one operand is a directory, the
/// other's basename is appended to it, so `git diff --no-index dir file` compares
/// `dir/file` against `file` — and says `Could not access 'dir/file'` when that
/// name does not exist. Neither operand may be standard input, which the caller
/// has already excluded.
fn fixup_paths(paths: &mut [String; 2]) {
    let isdir =
        |p: &str| std::fs::symlink_metadata(p).map(|m| m.is_dir()).unwrap_or(false);
    let (d0, d1) = (isdir(&paths[0]), isdir(&paths[1]));
    if d0 == d1 {
        return;
    }
    let (dir_i, file_i) = if d0 { (0, 1) } else { (1, 0) };
    // `append_basename()`: everything past the last `/` of the file operand, and
    // no doubled separator when the directory operand already ends in one.
    let file = paths[file_i].clone();
    let base = file.rsplit_once('/').map(|(_, b)| b).unwrap_or(file.as_str());
    let dir = paths[dir_i].clone();
    let sep = if dir.ends_with('/') { "" } else { "/" };
    paths[dir_i] = format!("{dir}{sep}{base}");
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
    /// `show_dirstat()`'s content damage for the pair: the pre-image bytes that did
    /// not survive plus the bytes the post-image gained. Zero unless `--dirstat`
    /// asked for it, since it costs a second content pass.
    damage: u64,
    /// `builtin_diff()`'s `must_show_header`, widened by "the comparison produced a
    /// body": whether this pair has anything at all to say. Under
    /// `diff_from_contents` it is also what `diff_flush_patch_quietly()` answers,
    /// so a pair whose only difference the ignore rules swallowed drops out of the
    /// raw and name listings and out of the exit status.
    shown: bool,
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
    /// `--quiet`, git's `flags.quick`: `diff_setup_done()` turns it into
    /// `DIFF_FORMAT_NO_OUTPUT` at the *end* of the scan, so it beats every format on
    /// the line and also switches rename detection off.
    quiet: bool,
    /// `-s`/`--no-patch`, git's `DIFF_FORMAT_NO_OUTPUT` bit: an assignment made where
    /// it stands, so a format flag after it survives.
    no_output: bool,
    /// `DIFF_FORMAT_DIRSTAT`, set by `--dirstat[=<p>]`, `--dirstat-by-file[=<p>]`,
    /// `--cumulative` and `-X` (`diff_opt_dirstat()`).
    dirstat: bool,
    /// `options->flags.stat_with_summary` (`--compact-summary`): not a format bit
    /// of its own — it turns `--stat` on and annotates its names — so the
    /// exclusive-format clearing reaches it through `stat`.
    compact_summary: bool,
    /// `DIFF_FORMAT_CHECKDIFF` (`--check`): one of the four exclusive formats.
    check: bool,
}

impl Format {
    /// `diff_setup_done()` (diff.c:4899), in two steps.
    ///
    /// First the exclusive formats win outright:
    ///
    /// ```c
    /// if (options->output_format & (DIFF_FORMAT_NAME |
    ///                               DIFF_FORMAT_NAME_STATUS |
    ///                               DIFF_FORMAT_CHECKDIFF |
    ///                               DIFF_FORMAT_NO_OUTPUT))
    ///         options->output_format &= ~(DIFF_FORMAT_RAW |
    ///                                     DIFF_FORMAT_NUMSTAT |
    ///                                     DIFF_FORMAT_DIFFSTAT |
    ///                                     DIFF_FORMAT_SHORTSTAT |
    ///                                     DIFF_FORMAT_DIRSTAT |
    ///                                     DIFF_FORMAT_SUMMARY |
    ///                                     DIFF_FORMAT_PATCH);
    /// ```
    ///
    /// which is why `git diff --no-index --name-only -p` prints names and no patch,
    /// measured against 2.55.0. Only then, with nothing selected at all, does the
    /// patch become the format.
    fn resolved(self) -> Self {
        let mut me = self;
        // `flags.quick`'s own arm runs at the very end of `diff_setup_done()` and
        // *assigns* `DIFF_FORMAT_NO_OUTPUT`, so `--quiet` beats every format on the
        // line however late it stands.
        if me.quiet {
            me = Format { quiet: true, no_output: true, ..Format::default() };
        }
        // `-s` is not in this list: it is an assignment made where it stands, so by
        // the time the clearing runs there is nothing of its own left to clear and a
        // format that came after it survives.
        if me.name_only || me.name_status || me.check {
            me.raw = false;
            me.numstat = false;
            me.stat = false;
            me.shortstat = false;
            me.summary = false;
            me.patch = false;
            me.dirstat = false;
        }
        me.defaulted()
    }

    /// `HAS_MULTI_BITS(...)`: the four exclusive formats, one of which must stand
    /// alone.
    fn exclusive_conflict(&self) -> bool {
        u32::from(self.name_only)
            + u32::from(self.name_status)
            + u32::from(self.check)
            + u32::from(self.no_output)
            > 1
    }

    /// The second step on its own: `if (!options->output_format) ... DIFF_FORMAT_PATCH`.
    fn defaulted(self) -> Self {
        if !(self.patch
            || self.stat
            || self.numstat
            || self.shortstat
            || self.name_only
            || self.name_status
            || self.raw
            || self.summary
            || self.dirstat
            || self.check
            || self.quiet
            || self.no_output)
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
    /// `--inter-hunk-context=<n>`: `xecfg.interhunkctxlen`, fed straight to the
    /// `xdl_emit_diff` port this command already renders through.
    inter_hunk_ctx: usize,
    /// `-D`/`--irreversible-delete`: a deletion emits its header and stops
    /// (`builtin_diff()`, diff.c:3596).
    irreversible_delete: bool,
    /// `--binary` (`diff_opt_binary()`, diff.c:5613): a binary pair gets a
    /// `GIT binary patch` payload instead of the `Binary files ... differ` line, its
    /// `index` line widens to full object names, and the patch format is turned on.
    binary: bool,
    /// `--skip-to=<p>` / `--rotate-to=<p>`: `(is_skip, target)`, matched against the
    /// pair.s post-image name. `None` leaves the queue in `queue_diff()`.s order.
    skip_or_rotate: Option<(bool, BString)>,
    /// `zlib_compression_level` for that payload: `core.looseCompression`, else
    /// `core.compression`, else `Z_BEST_SPEED`. A comparison run outside any
    /// repository has no config to read and takes the default.
    compression_level: i32,
    /// `options->stat_width` / `stat_name_width` / `stat_graph_width` /
    /// `stat_count`, in [`super::diffstat::StatWidths`]' sentinel encoding.
    /// `builtin_diff_no_index()` runs `init_diffstat_widths()` like every other
    /// `builtin/diff.c` entry point, so the default is the terminal-scaled one.
    stat_widths: super::diffstat::StatWidths,
    /// `-z`: `options->line_termination = 0`, which the raw, name and numstat
    /// formats — and `DIFF_SYMBOL_SEPARATOR` — write instead of a newline.
    z: bool,
    /// `--line-prefix=<s>`: `diff_line_prefix()`, prepended to every emitted line.
    line_prefix: Vec<u8>,
    /// `--dirstat`'s parameter block, shared with the tracked path so the two
    /// cannot disagree about `changes`/`lines`/`files` or the cut-off permille.
    dirstat: super::diff_files::DirStat,
    /// `--ignore-blank-lines`: `XDF_IGNORE_BLANK_LINES`, which
    /// `xdl_mark_ignorable_lines()` turns into an `ignore` bit on an all-blank
    /// change group. Not one of `XDF_WHITESPACE_FLAGS`, so it stacks with `-w`
    /// rather than replacing it.
    ignore_blank_lines: bool,
    /// `--diff-filter=<v>`: `diffcore_apply_filter()`'s letter set.
    filter: super::diff_filter::Filter,
    /// `o->ws_error_highlight` and `o->output_indicators[]`, for the re-emission
    /// pass that paints the assembled patch.
    paint: diff_color::PaintOptions,
    /// `o->word_diff` / `o->word_regex` / `o->color_moved`, likewise.
    extra: diff_color::ExtraPaint,
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
const NOT_IN_NO_INDEX: &[&str] = &[
    "--cached",
    "--staged",
    "--merge-base",
    // `--expand-tabs` / `--no-expand-tabs` / `--expand-tabs=<n>` are `builtin/log.c`
    // options (`OPT_EXPAND_TABS`), not entries on the `add_diff_options()` table, so
    // the no-index parser has never heard of them. Verified against 2.55.0:
    // `git diff --no-index --expand-tabs a b` is `error: unknown option
    // `expand-tabs'` followed by the usage block, exit 129.
    "--expand-tabs",
    "--no-expand-tabs",
];

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
    // `options->a_prefix` / `options->b_prefix` as the *command line* left them.
    // git fills these in `diff_setup()` before `parse_options()` runs and the four
    // prefix flags then overwrite them; this port parses the command line before it
    // can read config, so the flag's opinion is kept apart here and merged over the
    // configured value below, which restores git's precedence.
    let mut flag_src_prefix: Option<Vec<u8>> = None;
    let mut flag_dst_prefix: Option<Vec<u8>> = None;
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
    // `options->break_opt`, `-1` when `-B` was not given.
    let mut break_opt: i64 = -1;
    // `add_diff_options()` (diff-no-index.c:372) hands the no-index parser the
    // *whole* `diff_opts` table, algorithm selection included, so every spelling
    // `git diff` takes is taken here too. `None` leaves the `diff.algorithm`
    // default resolved below.
    let mut algorithm: Option<gix::diff::blob::Algorithm> = None;
    // `diff_options.anchors` — the repeatable `--anchored=<text>` list, which pins the
    // algorithm to patience and which a later `--patience` clears (`diff.c:5544-5556`
    // and `diff.c:5839-5858`).
    let mut anchors: Vec<String> = Vec::new();
    let mut want_anchor_value = false;
    // `--diff-algorithm` is an `OPT_CALLBACK_F` without `PARSE_OPT_OPTARG`, so
    // parse-options consumes the next argv entry as its value before that entry is
    // examined for anything else — a `--`, an operand, or another option.
    let mut want_algorithm_value = false;
    // `--inter-hunk-context=<n>`, `-D`, `--binary` and `--skip-to`/`--rotate-to`,
    // all of them from the same `add_diff_options()` table.
    let mut inter_hunk_ctx = 0usize;
    let mut irreversible_delete = false;
    let mut binary = false;
    // `(is_skip, target)`; the last one on the line wins, as `diff_opt_rotate_to`
    // simply reassigns `options->rotate_to`.
    let mut skip_or_rotate: Option<(bool, BString)> = None;
    // `--output=<file>`: `diff_opt_output`'s `xfopen(arg, "w")`, which runs during
    // the option scan.
    let mut output_file: Option<std::fs::File> = None;
    // `options->stat_*_width`, `-z`, `--line-prefix`, `--dirstat`'s parameters,
    // `--ignore-blank-lines`, `--diff-filter` and the two paint families, all of
    // them entries on the same `add_diff_options()` table.
    let mut stat_widths = super::diffstat::StatWidths::default();
    let mut z = false;
    let mut line_prefix: Vec<u8> = Vec::new();
    let mut dirstat = super::diff_files::DirStat::default();
    let mut ignore_blank_lines = false;
    let mut filter = super::diff_filter::Filter::default();
    let mut ws_error_highlight: u32 = diff_color::WSEH_NEW;
    let mut move_word = diff_color::MoveWordOpts::default();
    // The other `OPT_STRING`/`OPT_INTEGER` entries whose value may stand as the next
    // argument instead of being glued on with `=`.
    let mut pending: Option<String> = None;

    for a in args {
        if let Some(flag) = pending.take() {
            match flag.as_str() {
                "--skip-to" | "--rotate-to" => {
                    skip_or_rotate = Some((flag == "--skip-to", a.as_str().into()));
                }
                "--output" => match super::diff::open_output_file(a) {
                    Ok(f) => output_file = Some(f),
                    Err(code) => return Ok(code),
                },
                "--line-prefix" => line_prefix = a.as_bytes().to_vec(),
                "--diff-filter" => {
                    if let Err(bad) = filter.accumulate(a) {
                        eprintln!("error: unknown change class '{bad}' in --diff-filter={a}");
                        return Ok(ExitCode::from(129));
                    }
                }
                "--ws-error-highlight" => match diff_color::parse_ws_error_highlight(a) {
                    Ok(v) => ws_error_highlight = v,
                    Err(accepted) => {
                        eprintln!(
                            "error: unknown value after ws-error-highlight={}",
                            &a[..accepted]
                        );
                        return Ok(ExitCode::from(129));
                    }
                },
                "--color-moved-ws" | "--word-diff-regex" => {
                    let glued = format!("{flag}={a}");
                    if let Some(Err(msg)) = move_word.parse_flag(&glued, &mut color_when) {
                        eprintln!("{msg}");
                        return Ok(ExitCode::from(129));
                    }
                }
                f if super::diff::is_stat_width_flag(f) => {
                    let slot = super::diff::stat_width_slot_of(&mut stat_widths, f)
                        .expect("matched above");
                    match a.parse::<i64>() {
                        Ok(n) => *slot = n,
                        Err(_) => {
                            eprintln!("error: {} expects a numerical value", &f[2..]);
                            return Ok(ExitCode::from(129));
                        }
                    }
                }
                _ => match super::diff::parse_inter_hunk_context(a) {
                    Ok(n) => inter_hunk_ctx = n,
                    Err(msg) => {
                        eprintln!("error: {msg}");
                        return Ok(ExitCode::from(129));
                    }
                },
            }
            continue;
        }
        if matches!(
            a.as_str(),
            "--skip-to"
                | "--rotate-to"
                | "--output"
                | "--inter-hunk-context"
                | "--line-prefix"
                | "--diff-filter"
                | "--ws-error-highlight"
        ) || diff_color::needs_separate_value(a)
            || super::diff::is_stat_width_flag(a)
        {
            pending = Some(a.clone());
            continue;
        }
        if std::mem::take(&mut want_anchor_value) {
            algorithm = Some(gix::diff::blob::Algorithm::Patience);
            anchors.push(a.clone());
            continue;
        }
        if a == "--anchored" {
            want_anchor_value = true;
            continue;
        }
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
        // `--color-words[=<re>]`, `--word-diff[=<mode>]`, `--word-diff-regex=<re>`
        // and the `--color-moved` family are all on the shared `add_diff_options()`
        // table, so the no-index parser takes them through the same reader `git
        // diff` uses. The two that imply colour set `use_color = GIT_COLOR_ALWAYS`
        // by writing `color_when`, which is why it is threaded in.
        match move_word.parse_flag(a, &mut color_when) {
            Some(Ok(())) => continue,
            Some(Err(msg)) => {
                eprintln!("{msg}");
                return Ok(ExitCode::from(129));
            }
            None => {}
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
            // `OPT_BIT_F(0, "patch-with-raw", …, DIFF_FORMAT_PATCH | DIFF_FORMAT_RAW)`
            // and its `--patch-with-stat` neighbour (diff.c:6021-6031): one flag,
            // two format bits.
            "--patch-with-raw" => {
                fmt.patch = true;
                fmt.raw = true;
            }
            "--patch-with-stat" => {
                fmt.patch = true;
                fmt.stat = true;
            }
            // `diff_opt_compact_summary()` (diff.c:5602): the summary flag *and*
            // `DIFF_FORMAT_DIFFSTAT`; `--no-compact-summary` clears only the flag.
            "--compact-summary" => {
                fmt.compact_summary = true;
                fmt.stat = true;
            }
            "--no-compact-summary" => fmt.compact_summary = false,
            // `OPT_SET_INT('z', NULL, &options->line_termination, …, 0)`: not a
            // format bit, so it never satisfies the "nothing selected" default.
            "-z" => z = true,
            // `diff_opt_dirstat()` (diff.c:5490), in its four spellings.
            "--dirstat" | "-X" => fmt.dirstat = true,
            "--dirstat-by-file" => {
                fmt.dirstat = true;
                dirstat.by_file = true;
            }
            "--cumulative" => {
                fmt.dirstat = true;
                dirstat.cumulative = true;
            }
            s if s.starts_with("--dirstat=")
                || s.starts_with("--dirstat-by-file=")
                || s.starts_with("-X") =>
            {
                let by_file = s.starts_with("--dirstat-by-file=");
                let params = s
                    .strip_prefix("--dirstat-by-file=")
                    .or_else(|| s.strip_prefix("--dirstat="))
                    .unwrap_or(&s[2..]);
                let errors = super::diff_files::parse_dirstat_params(params, &mut dirstat);
                if !errors.is_empty() {
                    eprint!("fatal: Failed to parse --dirstat/-X option parameter:\n{errors}\n");
                    return Ok(ExitCode::from(128));
                }
                if by_file {
                    dirstat.by_file = true;
                }
                fmt.dirstat = true;
            }
            // `diff_opt_stat()` (diff.c:5636): the geometry ride-along on `--stat`.
            s if s.starts_with("--stat=") => {
                fmt.stat = true;
                super::diffstat::parse_stat_geometry(&mut stat_widths, &s["--stat=".len()..]);
            }
            s if s.split_once('=').is_some_and(|(k, _)| super::diff::is_stat_width_flag(k)) => {
                fmt.stat = true;
                let (k, v) = s.split_once('=').expect("matched above");
                match v.parse::<i64>() {
                    Ok(n) => {
                        *super::diff::stat_width_slot_of(&mut stat_widths, k)
                            .expect("matched above") = n
                    }
                    Err(_) => {
                        eprintln!("error: {} expects a numerical value", &k[2..]);
                        return Ok(ExitCode::from(129));
                    }
                }
            }
            // `--line-prefix=<s>` (`OPT_STRING_F(0, "line-prefix", …)`).
            s if s.starts_with("--line-prefix=") => {
                line_prefix = s.as_bytes()["--line-prefix=".len()..].to_vec();
            }
            // `diff_opt_diff_filter()` (diff.c:5581): the letters accumulate across
            // repeats and an unknown one is a parse error.
            s if s.starts_with("--diff-filter=") => {
                let v = &s["--diff-filter=".len()..];
                if let Err(bad) = filter.accumulate(v) {
                    eprintln!("error: unknown change class '{bad}' in --diff-filter={v}");
                    return Ok(ExitCode::from(129));
                }
            }
            s if s.starts_with("--ws-error-highlight=") => {
                match diff_color::parse_ws_error_highlight(&s["--ws-error-highlight=".len()..]) {
                    Ok(v) => ws_error_highlight = v,
                    Err(accepted) => {
                        eprintln!(
                            "error: unknown value after ws-error-highlight={}",
                            &s["--ws-error-highlight=".len()..][..accepted]
                        );
                        return Ok(ExitCode::from(129));
                    }
                }
            }
            // `--ignore-blank-lines` is `XDF_IGNORE_BLANK_LINES`, which is outside
            // `XDF_WHITESPACE_FLAGS` and therefore stacks with `-w`/`-b` instead of
            // replacing them.
            "--check" => fmt.check = true,
            "--ignore-blank-lines" => ignore_blank_lines = true,
            "--ignore-cr-at-eol" => ws = super::diff::Whitespace::IgnoreCrAtEol,
            // `OPT_SET_INT_F('s', "no-patch", ..., DIFF_FORMAT_NO_OUTPUT)`: an
            // assignment, so it wipes the formats already chosen and a later one still
            // counts. Measured against 2.55.0: `--stat -s` prints nothing, `-s --stat`
            // prints the stat block.
            "-s" | "--no-patch" => fmt = Format { no_output: true, quiet: fmt.quiet, ..Format::default() },
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
            // `diff_opt_break_rewrites()`: `-B[<n>][/<m>]`, packed as `n | (m << 16)`.
            // `diffcore_std()` runs `diffcore_break()` ahead of rename detection and
            // `diffcore_merge_broken()` after it, which [`diffcore_rename::run`]
            // already does for both callers.
            "-B" | "--break-rewrites" => break_opt = 0,
            s if s.starts_with("--break-rewrites=") || (s.starts_with("-B") && s.len() > 2) => {
                let raw = s.strip_prefix("--break-rewrites=").unwrap_or(&s[2..]);
                match super::diffcore_rename::parse_break_opt(raw) {
                    Ok(v) => break_opt = v,
                    Err(()) => {
                        eprintln!("error: break-rewrites expects <n>/<m> form");
                        return Ok(ExitCode::from(129));
                    }
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
            "--patience" => {
                algorithm = Some(gix::diff::blob::Algorithm::Patience);
                // `diff_opt_patience()` frees every anchor named before it.
                anchors.clear();
            }
            "--histogram" => algorithm = Some(gix::diff::blob::Algorithm::Histogram),
            s if s.starts_with("--anchored=") => {
                algorithm = Some(gix::diff::blob::Algorithm::Patience);
                anchors.push(s["--anchored=".len()..].to_string());
            }
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
            // `diff_opt_binary()` calls `enable_patch_output()` first, so the flag
            // turns the patch on as well as widening the `index` line.
            "--binary" => {
                binary = true;
                fmt.patch = true;
                fmt.no_output = false;
            }
            // `diff_opt_irreversible_delete`.
            "-D" | "--irreversible-delete" => irreversible_delete = true,
            s if s.starts_with("--inter-hunk-context=") => {
                match super::diff::parse_inter_hunk_context(&s["--inter-hunk-context=".len()..]) {
                    Ok(n) => inter_hunk_ctx = n,
                    Err(msg) => {
                        eprintln!("error: {msg}");
                        return Ok(ExitCode::from(129));
                    }
                }
            }
            // `diffcore_rotate()`. `builtin_diff_no_index()` never sets
            // `rotate_to_strict` — only `cmd_diff()`'s tracked path does — so a
            // target that names no pair is silently ignored here, where `git diff
            // --skip-to=<missing>` dies. Measured against 2.55.0: `git diff
            // --no-index --skip-to=zzz L R` prints the whole diff and exits 1.
            s if s.starts_with("--skip-to=") => {
                skip_or_rotate = Some((true, s["--skip-to=".len()..].into()));
            }
            s if s.starts_with("--rotate-to=") => {
                skip_or_rotate = Some((false, s["--rotate-to=".len()..].into()));
            }
            s if s.starts_with("--output=") => {
                match super::diff::open_output_file(&s["--output=".len()..]) {
                    Ok(f) => output_file = Some(f),
                    Err(code) => return Ok(code),
                }
            }
            // `diff_set_noprefix()` (diff.c:3728-3731): both slots become the empty
            // string, which is an assignment — so `--no-prefix` also shuts out the
            // mnemonic fill below.
            "--no-prefix" => {
                flag_src_prefix = Some(Vec::new());
                flag_dst_prefix = Some(Vec::new());
            }
            // `diff_opt_default_prefix()` (diff.c:5785-5796) frees the configured
            // prefixes before installing `a/`/`b/`, so it ignores `diff.srcPrefix`
            // and `diff.dstPrefix` as well as `diff.mnemonicPrefix`.
            "--default-prefix" => {
                flag_src_prefix = Some(b"a/".to_vec());
                flag_dst_prefix = Some(b"b/".to_vec());
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
            // `OPT_STRING_F(0, "src-prefix", &options->a_prefix, …)` (diff.c:6106-6110):
            // one slot each, so the other side is still free for the mnemonic fill.
            s if s.starts_with("--src-prefix=") => {
                flag_src_prefix = Some(s.as_bytes()["--src-prefix=".len()..].to_vec());
            }
            s if s.starts_with("--dst-prefix=") => {
                flag_dst_prefix = Some(s.as_bytes()["--dst-prefix=".len()..].to_vec());
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
    if want_anchor_value {
        eprintln!("error: option `anchored' requires a value");
        return Ok(ExitCode::from(129));
    }
    // The anchor list is final: `--patience` may have cleared it and a later
    // `--anchored` refilled it. See [`super::diff_pairs::set_anchor_texts`].
    super::diff_pairs::set_anchor_texts(anchors);
    // The same for the other value-taking options: parse-options reports the missing
    // value and exits 129 before the operand count is looked at.
    if let Some(flag) = pending {
        eprintln!("error: {}", diff_color::missing_value(&flag));
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

    // `diff_no_index()` (diff-no-index.c:264-271) rewrites a bare `-` operand to
    // its standard-input marker before `fixup_paths()` runs, and `fixup_paths()`
    // returns untouched when either side is that marker.
    let mut pair: [String; 2] = [operands[0].clone(), operands[1].clone()];
    let l_stdin = pair[0] == STDIN_NAME;
    let r_stdin = pair[1] == STDIN_NAME;
    if l_stdin || r_stdin {
        // `queue_diff()` refuses to walk a directory against a stream.
        let other = if l_stdin { &pair[1] } else { &pair[0] };
        if std::fs::symlink_metadata(other).map(|m| m.is_dir()).unwrap_or(false) {
            eprintln!("fatal: cannot compare stdin to a directory");
            return Ok(ExitCode::from(128));
        }
    } else {
        fixup_paths(&mut pair);
    }
    // `populate_from_stdin()`: the stream is read once, whichever side names it.
    let stdin_data = if l_stdin || r_stdin {
        let mut buf = Vec::new();
        use std::io::Read as _;
        std::io::stdin().lock().read_to_end(&mut buf)?;
        Some(buf)
    } else {
        None
    };

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
    let repo = crate::setup::discover().ok();
    // `git_diff_ui_config()` has already run whether or not discovery found a
    // repository, so `color.diff.*` reaches a no-index comparison from the system
    // + `~/.gitconfig` + `GIT_CONFIG_*` cascade even outside one. Reading only the
    // repository's snapshot left `git diff --no-index --color=always a b`
    // uncoloured, which stock 2.55.0 colours.
    let colors = match &repo {
        Some(repo) => diff_color::DiffColors::resolve(repo, want_color),
        None => diff_color::DiffColors::resolve_config(&crate::config::global_config(), want_color),
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
    // `diff_setup()`'s prefix decision (diff.c:5149-5153), then the command line on
    // top of it, then `builtin_diff_no_index()`'s own
    // `diff_set_mnemonic_prefix(&revs->diffopt, "1/", "2/")` (diff-no-index.c:425),
    // and finally `builtin_diff()`'s `a/`/`b/` (diff.c:3838).
    //
    // The keys reach a no-index comparison the same way `diff.algorithm` does:
    // `git_diff_ui_config()` has already run by the time `diff_no_index()` is
    // called. Without a repository there is no config and the statics keep their
    // zero values, which is the plain `a/`/`b/` pair.
    //
    // Read back from stock git 2.55.0 on a repository with the key set:
    // `diff --no-index a b` is `1/a` vs `2/b`; `--src-prefix=S/` makes it `S/a` vs
    // `2/b`; `-R` prints `2/b` against `1/a`; and `diff.noPrefix` still wins.
    let (mut src_prefix, mut dst_prefix) = {
        // `git_diff_ui_config()` has already run whether or not a repository was
        // found: without one the cascade is still system + `~/.gitconfig` +
        // `GIT_CONFIG_*`, so `git -c diff.mnemonicPrefix=true diff --no-index a b`
        // prints `1/`/`2/` from anywhere on the filesystem.
        let cfg: gix::config::File = match &repo {
            Some(repo) => repo.config_snapshot().plumbing().clone(),
            None => crate::config::global_config(),
        };
        let flag = |key: &str| cfg.boolean(key).ok().flatten() == Some(true);
        let text = |key: &str| cfg.string(key).map(Vec::from);
        let (no_prefix, mnemonic, cfg_src, cfg_dst) = (
            flag("diff.noPrefix"),
            flag("diff.mnemonicPrefix"),
            text("diff.srcPrefix"),
            text("diff.dstPrefix"),
        );
        let (mut a, mut b): (Option<Vec<u8>>, Option<Vec<u8>>) = if no_prefix {
            (Some(Vec::new()), Some(Vec::new()))
        } else if !mnemonic {
            (
                Some(cfg_src.unwrap_or_else(|| b"a/".to_vec())),
                Some(cfg_dst.unwrap_or_else(|| b"b/".to_vec())),
            )
        } else {
            (None, None)
        };
        if let Some(p) = flag_src_prefix {
            a = Some(p);
        }
        if let Some(p) = flag_dst_prefix {
            b = Some(p);
        }
        (
            a.unwrap_or_else(|| b"1/".to_vec()),
            b.unwrap_or_else(|| b"2/".to_vec()),
        )
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
    if fmt.exclusive_conflict() {
        eprintln!(
            "fatal: options '--name-only', '--name-status', '--check', and '-s' cannot be used together"
        );
        return Ok(ExitCode::from(128));
    }
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
        inter_hunk_ctx,
        irreversible_delete,
        binary,
        skip_or_rotate,
        compression_level: match &repo {
            Some(repo) => super::binary_patch::loose_compression_level(repo),
            None => 1,
        },
        stat_widths,
        z,
        line_prefix,
        dirstat,
        ignore_blank_lines,
        filter,
        paint: diff_color::PaintOptions { ws_error_highlight, ..Default::default() },
        extra: match &repo {
            Some(repo) => match move_word.resolve(repo) {
                Ok(extra) => extra,
                Err(msg) => {
                    eprintln!("{msg}");
                    return Ok(ExitCode::from(128));
                }
            },
            // `init_diff_words_data()` reads `diff.wordRegex` off the same cascade
            // the colours came from, which outside a repository is the global one.
            None => match move_word.resolve_config(&crate::config::global_config()) {
                Ok(extra) => extra,
                Err(msg) => {
                    eprintln!("{msg}");
                    return Ok(ExitCode::from(128));
                }
            },
        },
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
        break_opt,
        hash_kind: HASH_KIND,
    };

    // `whitespace_rule()` for a path with no `.gitattributes` behind it, which is
    // every no-index path: `core.whitespace`, else `WS_DEFAULT_RULE`.
    let ws_rule = match &repo {
        Some(repo) => diff_color::whitespace_rule_cfg(repo),
        None => match crate::config::global_config().string("core.whitespace") {
            Some(v) => diff_color::parse_whitespace_rule(&v.to_string()),
            None => diff_color::WS_DEFAULT_RULE,
        },
    };
    let (out, changed, paints, check_failed) =
        match compare(&pair[0], &pair[1], reverse, &opts, &rename_opts, ws_rule, stdin_data) {
            Ok(result) => result,
            Err(message) => {
                eprintln!("error: {message}");
                return Ok(ExitCode::from(1));
            }
        };

    if !opts.fmt.quiet {
        let painted = diff_color::colorize_patch_ex(
            &out,
            &opts.colors,
            &opts.paint,
            &paints,
            diff_color::FilePaint::new(ws_rule),
            &opts.extra,
        );
        // `--line-prefix`: `emit_line_0()` writes `diff_line_prefix(o)` in front of
        // every line it emits, which is every line of the finished stream.
        let painted = super::diff::apply_line_prefix(painted, &opts.line_prefix);
        use std::io::Write;
        // `--output=<file>` swapped the diff stream for a file back at parse time.
        match output_file {
            Some(mut f) => {
                f.write_all(&painted)?;
                f.flush()?;
            }
            None => {
                let mut stdout = std::io::stdout().lock();
                stdout.write_all(&painted)?;
                stdout.flush()?;
            }
        }
    }
    // `diff_result_code()`: `01` for a difference — git-diff(1): "this option
    // implies --exit-code" — and `02` for `--check`'s `check_failed`.
    let code = u8::from(changed) | (u8::from(check_failed) << 1);
    Ok(ExitCode::from(code))
}

/// The comparison itself: `queue_diff()`, then `diffcore_std()`'s passes, then
/// `diff_flush()`. Returns the uncoloured output and git's `has_changes`.
fn compare(
    lhs: &str,
    rhs: &str,
    reverse: bool,
    opts: &Opts,
    rename_opts: &super::diffcore_rename::Options,
    ws_rule: u32,
    stdin_data: Option<Vec<u8>>,
) -> std::result::Result<(Vec<u8>, bool, Vec<diff_color::FilePaint>, bool), String> {
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
    // The stream stands in for the file the `-` side would have been read from.
    if let Some(data) = stdin_data {
        content.cache.insert(BString::from(STDIN_NAME), Some(data));
    }
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
    // each surviving pair into its status letter. `-B`'s break and merge-broken
    // passes bracket rename detection inside [`super::diffcore_rename::run`].
    let mut q = super::diffcore_rename::Queue::default();
    for (a, b) in &pairs {
        let one = q.add_spec(spec_of(a));
        let two = q.add_spec(spec_of(b));
        q.add_pair(one, two);
    }
    super::diffcore_rename::run(&mut q, rename_opts, &mut content).emit("diff.renameLimit");
    super::diffcore_rename::resolve_rename_copy(&mut q);

    // `diffcore_rotate()` (diff.c:6763): re-anchor the queue on the pair whose
    // *post-image* name is the target — `p->two->path`, which for a no-index
    // comparison is the right-hand operand's own name (`R/ccc`, not `ccc`).
    // `builtin_diff_no_index()` never raises `rotate_to_strict`, so a target that
    // names no pair returns quietly instead of dying the way `git diff --skip-to`
    // does. Measured against 2.55.0: `git diff --no-index --skip-to=zzz L R` prints
    // the whole diff and exits 1.
    if let Some((is_skip, target)) = &opts.skip_or_rotate {
        if let Some(k) = q
            .pairs
            .iter()
            .position(|p| q.specs[p.two].path == *target)
        {
            if *is_skip {
                q.pairs.drain(..k);
            } else {
                q.pairs.rotate_left(k);
            }
        }
    }

    // `diffcore_apply_filter()` is the last pass in `diffcore_std()` — after
    // `diff_resolve_rename_copy()` has given every pair its status letter, which is
    // what the letter set is matched against.
    {
        let classes: Vec<(u8, Option<u32>)> = q
            .pairs
            .iter()
            .map(|p| (p.status, (p.status == b'M' && p.score != 0).then_some(p.score)))
            .collect();
        let keep = super::diff_filter::apply(opts.filter, &classes);
        let mut it = keep.into_iter();
        q.pairs.retain(|_| it.next().unwrap_or(true));
    }

    // `diff_flush()` (diff.c:6828) writes the non-patch formats first, then a blank
    // separator line, then the patch — so the two streams are built apart and joined
    // below. `--binary` is what makes the combination reachable without `-p`: it
    // turns the patch on by itself while a `--stat` on the same line stays selected.
    let mut out: Vec<u8> = Vec::new();
    let mut patch: Vec<u8> = Vec::new();
    // `--check`'s stream and `o->flags.check_failed`.
    let mut check_out: Vec<u8> = Vec::new();
    let mut check_failed = false;
    let mut changed = false;
    // `diff_setup_done()` (diff.c:4899): the whitespace family makes "is there a
    // change?" a question only the rendered content can answer, so it raises
    // `flags.diff_from_contents`, and `diff_flush()` then reports `found_changes`
    // instead of "the queue was not empty" and runs the raw and name formats
    // through `diff_flush_patch_quietly()` first. `--ignore-blank-lines` is
    // deliberately not on that list.
    let from_contents = opts.ws != super::diff::Whitespace::Keep;
    let mut stats: Vec<Row> = Vec::new();
    // One entry per `diff --git` section, in the order they are written, which is
    // what `colorize_patch_ex()` indexes to reproduce `emit_line_ws_markup()`'s
    // per-file whitespace state.
    let mut paints: Vec<diff_color::FilePaint> = Vec::new();
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
        // `builtin_diffstat()` asks `diff_filespec_is_binary()` directly, which never
        // sees `DIFF_OPT_TEXT` — so `--text` renders a patch for a pair the stat
        // formats still report as `Bin <old> -> <new> bytes`.
        let stat_binary =
            super::diff::looks_binary(&old_data) || super::diff::looks_binary(&new_data);
        let binary = !opts.text && stat_binary;
        let (added, deleted, body) =
        super::diff::no_index_body(
            &old_data,
            &new_data,
            &opts.ctx_geometry(),
            opts.ws,
            binary,
            opts.algorithm,
            opts.ignore_blank_lines,
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
            // `builtin_diffstat()` (diff.c:3900) never counts a binary pair's lines:
            //
            // ```c
            // if (diff_filespec_is_binary(o->repo, one) || diff_filespec_is_binary(o->repo, two)) {
            //         data->is_binary = 1;
            //         data->added = diff_filespec_size(o->repo, two);
            //         data->deleted = diff_filespec_size(o->repo, one);
            // }
            // ```
            //
            // so the two fields carry *sizes* instead, which is what `show_stats()`
            // prints as `Bin <old> -> <new> bytes`. Every consumer that counts lines
            // — `--numstat`'s `-\t-`, `--shortstat`'s totals — skips a binary row.
            added: if stat_binary { new_data.len() as u32 } else { added },
            deleted: if stat_binary { old_data.len() as u32 } else { deleted },
            binary: stat_binary,
            a_exists: a.file.is_some(),
            b_exists: b.file.is_some(),
            a_mode: a.mode,
            b_mode: b.mode,
            a_oid: spec_oid(pair.one),
            b_oid: spec_oid(pair.two),
            status: pair.status,
            score: pair.score,
            shown: false,
            damage: if opts.fmt.dirstat && !opts.dirstat.by_file && !opts.dirstat.by_line {
                match (a.file.is_some(), b.file.is_some()) {
                    (true, true) => {
                        let (copied, added) = super::diff_files::count_changes_sides(
                            &old_data, !binary, &new_data, !binary,
                        );
                        (old_data.len() as u64).saturating_sub(copied) + added
                    }
                    (true, false) => old_data.len() as u64,
                    (false, true) => new_data.len() as u64,
                    (false, false) => 0,
                }
            } else {
                0
            },
        });
        // `builtin_diff()` builds the header into a strbuf and hands it to
        // `fn_out_consume()`, which emits it only when the first hunk line goes out.
        // `must_show_header` forces it out early for a creation (diff.c:3613), a
        // deletion (diff.c:3620), a mode change (diff.c:3627), and — through
        // `fill_metainfo()` (diff.c:4491) — a rename or copy. A plain modification
        // whose comparison found nothing, the usual result of `-w` over a
        // whitespace-only edit, therefore prints no `diff --git` line at all.
        let must_show = a.file.is_none()
            || b.file.is_none()
            || a.mode != b.mode
            || matches!(pair.status, b'R' | b'C')
            || !body.is_empty()
            // The binary arm prints `Binary files … differ` and its header with it,
            // but only once the two sides are known to differ (diff.c:3672).
            || (binary && !same_content);
        stats.last_mut().expect("just pushed").shown = must_show;
        // `diff_flush_checkdiff()` (diff.c): an unmodified pair is skipped, and only
        // the *new* side is examined — `--check` reports what the change introduces.
        if opts.fmt.check && !same_content && !stat_binary {
            check_failed |= emit_check(
                &mut check_out,
                b.display_name(),
                &old_data,
                &new_data,
                ws_rule,
                &opts.colors,
            );
        }
        changed |= !from_contents || must_show;
        if opts.fmt.patch && must_show {
            paints.push(diff_color::FilePaint {
                ws_rule,
                blank_at_eof: diff_color::check_blank_at_eof(&old_data, &new_data),
                // This command does not resolve a path's userdiff driver, so no driver word
                // regex is available; `diff.wordRegex` still reaches the emitter through
                // [`diff_color::ExtraPaint`].
                word_regex: None,
            });
            emit_header(&mut patch, a, b, &old_data, &new_data, &opts, same_content, binary, &pair);
            // `builtin_diff()` (diff.c:3596): with `-D`, a pair whose post-image label
            // is `/dev/null` stops right after its header — no `---`/`+++` pair, no
            // hunks, and no `Binary files ... differ` either, since the jump lands
            // past that arm as well.
            if opts.irreversible_delete && b.file.is_none() {
                continue;
            }
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
                // `emit_binary_diff()` (diff.c:2909): with `--binary` the two images
                // go out as a `GIT binary patch` payload — literal or delta,
                // whichever deflates smaller — instead of the one-line notice, and
                // no `---`/`+++` pair is printed for either form.
                if opts.binary {
                    super::binary_patch::emit(
                        &mut patch,
                        &old_data,
                        &new_data,
                        opts.compression_level,
                    );
                } else {
                    patch.extend_from_slice(b"Binary files ");
                    push_name(&mut patch, &opts.src_prefix, a.header_name(b), a.file.is_some());
                    patch.extend_from_slice(b" and ");
                    push_name(&mut patch, &opts.dst_prefix, b.header_name(a), b.file.is_some());
                    patch.extend_from_slice(b" differ\n");
                }
            } else {
                patch.extend_from_slice(&body);
            }
        }
    }

    if opts.fmt.check {
        out.extend_from_slice(&check_out);
    }
    if !stats.is_empty() && non_patch_format(opts) {
        render_non_patch(&mut out, &stats, opts, from_contents);
    }
    // `diff_flush()`: dirstat sits between the stat formats and the summary. The
    // names it buckets by are the *post-image* ones, so a deletion is charged to
    // `/dev/` exactly as stock 2.55.0 reports it.
    if opts.fmt.dirstat && !stats.is_empty() {
        let files: Vec<(BString, u64)> = stats
            .iter()
            .map(|r| {
                let damage = if opts.dirstat.by_file {
                    1
                } else if opts.dirstat.by_line {
                    let lines = u64::from(r.added) + u64::from(r.deleted);
                    if r.binary { lines.div_ceil(64) } else { lines }
                } else {
                    // `show_dirstat()`'s content damage, and the single unit it
                    // charges a pair that changed at all.
                    let d = r.damage;
                    if d == 0 { 1 } else { d }
                };
                (r.b_name.clone(), damage)
            })
            .collect();
        super::diff_files::render_dirstat(&mut out, files, &opts.dirstat);
    }
    if !patch.is_empty() {
        // `DIFF_SYMBOL_SEPARATOR`: one empty line — a NUL under `-z` — and only
        // when a format already wrote something.
        if !out.is_empty() {
            out.push(if opts.z { 0 } else { b'\n' });
        }
        out.extend_from_slice(&patch);
    }
    Ok((out, changed, paints, check_failed))
}

/// `builtin_checkdiff()` (diff.c:4281) driving `checkdiff_consume()` (diff.c:3555)
/// for one no-index pair.
///
/// The diff it walks is its own: `xecfg.ctxlen = 1` with `xpp.flags = 0`, so no
/// `-w`/`-b`, no `--ignore-blank-lines`, no indent heuristic and no
/// `--diff-algorithm` reach it. Returns `o->flags.check_failed` for the pair.
///
/// A no-index path has no `.gitattributes` behind it, so the whitespace rule is the
/// one `core.whitespace` set for the whole run and the conflict-marker size is the
/// built-in seven.
fn emit_check(
    out: &mut Vec<u8>,
    name: &BString,
    old_data: &[u8],
    new_data: &[u8],
    ws_rule: u32,
    colors: &diff_color::DiffColors,
) -> bool {
    use gix::diff::blob::InternedInput;

    let mut failed = false;
    let set = colors.get(diff_color::DiffSlot::New);
    let ws_color = colors.get(diff_color::DiffSlot::Whitespace);
    let reset = colors.reset();

    let before = super::diff::byte_lines(old_data);
    let after = super::diff::byte_lines(new_data);
    let mut input: InternedInput<Vec<u8>> = InternedInput::default();
    input.update_before(before.iter().map(|l| l.to_vec()));
    input.update_after(after.iter().map(|l| l.to_vec()));
    let mut diff = gix::diff::blob::Diff::compute(gix::diff::blob::Algorithm::Myers, &input);
    diff.postprocess_no_heuristic(&input);

    // xdiff hands `checkdiff_consume()` whole lines, and the record for a final line
    // without a terminator arrives with the newline `xdl_emit_diff()` writes before
    // the `\ No newline at end of file` marker — so `ws_check()` never sees the
    // missing terminator, and `WS_INCOMPLETE_LINE` is reported by the marker's own
    // branch instead.
    let mut last_added_is_final = false;
    for h in diff.hunks() {
        let start = h.after.start as usize;
        for (k, line) in after[start..start + h.after.len()].iter().enumerate() {
            let lineno = start + k + 1;
            let mut body: Vec<u8> = (*line).to_vec();
            if body.last() != Some(&b'\n') {
                body.push(b'\n');
                last_added_is_final = true;
            }
            if super::diff_files::is_conflict_marker_sized(&body, 7) {
                failed = true;
                out.extend_from_slice(name.as_slice());
                out.extend_from_slice(format!(":{lineno}: leftover conflict marker\n").as_bytes());
            }
            let bad = super::diff_files::ws_check(&body, ws_rule);
            if bad == 0 {
                continue;
            }
            failed = true;
            out.extend_from_slice(name.as_slice());
            out.extend_from_slice(
                format!(":{lineno}: {}.\n", super::diff_files::whitespace_error_string(bad))
                    .as_bytes(),
            );
            out.extend_from_slice(set.as_bytes());
            out.push(b'+');
            out.extend_from_slice(reset.as_bytes());
            diff_color::ws_check_emit(out, &body, ws_rule, set, reset, ws_color);
        }
    }

    if ws_rule & diff_color::WS_INCOMPLETE_LINE != 0 && last_added_is_final {
        failed = true;
        out.extend_from_slice(name.as_slice());
        out.extend_from_slice(
            format!(
                ":{}: {}.\n",
                after.len(),
                super::diff_files::whitespace_error_string(diff_color::WS_INCOMPLETE_LINE)
            )
            .as_bytes(),
        );
    }

    // `check_blank_at_eof()` runs over the whole file rather than the hunk stream.
    if ws_rule & diff_color::WS_BLANK_AT_EOF != 0 {
        let (_, post) = diff_color::check_blank_at_eof(old_data, new_data);
        if post != 0 {
            failed = true;
            out.extend_from_slice(name.as_slice());
            out.extend_from_slice(
                format!(
                    ":{post}: {}.\n",
                    super::diff_files::whitespace_error_string(diff_color::WS_BLANK_AT_EOF)
                )
                .as_bytes(),
            );
        }
    }
    failed
}

/// Whether any format other than the patch was asked for, so [`render_non_patch`] has
/// something to write.
fn non_patch_format(opts: &Opts) -> bool {
    let f = &opts.fmt;
    f.stat || f.numstat || f.shortstat || f.name_only || f.name_status || f.raw || f.summary
}

/// `options->line_termination`: a newline, or a NUL under `-z`.
fn terminator(opts: &Opts) -> u8 {
    if opts.z { 0 } else { b'\n' }
}

impl Opts {
    /// `--no-index` reads no gitattributes at all (`diff_no_index()` never opens a
    /// repository), so no userdiff driver can apply and the built-in `def_ff` is the
    /// only heading heuristic there is.
    fn ctx_geometry(&self) -> super::diff_pairs::EmitGeometry<'static> {
        super::diff_pairs::EmitGeometry {
            ctx: self.ctx as usize,
            inter_hunk_ctx: self.inter_hunk_ctx,
            func_context: self.func_context,
            funcname: None,
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

    // `fill_metainfo()` (diff.c:4491) widens the `index` line to full object names
    // under `--full-index`, and also under `--binary` — but only for a pair that
    // really is binary, so text pairs in the same run keep the abbreviation.
    // `diff_abbrev_oid()` truncates whatever id it is given to that same width, so
    // the null id of an absent — or standard-input — side widens with it.
    let hexsz = HASH_KIND.len_in_hex();
    let width = if opts.full_index || (opts.binary && binary) { hexsz } else { opts.abbrev.min(hexsz) };
    let hash = |data: &[u8], exists: bool, is_stdin: bool| -> String {
        // `diff_fill_oid_info()` returns the null id for a filespec fed from
        // standard input rather than hashing the stream.
        if !exists || is_stdin {
            return "0".repeat(width);
        }
        blob_id(data).to_hex().to_string()[..width].to_string()
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
    out.extend_from_slice(hash(old_data, a.file.is_some(), a.name == STDIN_NAME).as_bytes());
    out.extend_from_slice(b"..");
    out.extend_from_slice(hash(new_data, b.file.is_some(), b.name == STDIN_NAME).as_bytes());
    // The mode is repeated on the index line only when both sides share it.
    if a.file.is_some() && b.file.is_some() && a.mode == b.mode {
        out.extend_from_slice(format!(" {:o}", a.mode).as_bytes());
    }
    out.push(b'\n');

    // `emit_diff_symbol(DIFF_SYMBOL_FILEPAIR_*)` is skipped for a binary pair:
    // there are no line markers to introduce. `-D` skips them for the same reason —
    // its `goto` (diff.c:3596) leaves `builtin_diff()` before the arm that prints
    // them, and the caller then skips the body as well.
    if binary || (opts.irreversible_delete && b.file.is_none()) {
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
fn render_non_patch(out: &mut Vec<u8>, rows: &[Row], opts: &Opts, from_contents: bool) {
    // `diff_flush()` (diff.c:7210): under `diff_from_contents` the raw and name
    // formats skip a pair whose rendered content turned out to be empty.
    let listed: Vec<&Row> = rows.iter().filter(|r| !from_contents || r.shown).collect();
    // `diff_flush_name()` prints the post-image name; `diff_flush_raw()` and
    // `--name-status` print the pre-image one, which for an addition is the only
    // name there is.
    let term = terminator(opts);
    if opts.fmt.name_only {
        for row in &listed {
            out.extend_from_slice(row.b_name.as_bytes());
            out.push(term);
        }
        return;
    }
    // `flush_one_pair()` (diff.c:6323) prefers `diff_flush_raw()` over the name
    // format, and `diff_flush_raw()` itself drops the `:<modes> <oids> ` prefix when
    // `DIFF_FORMAT_NAME_STATUS` is on. The raw/name block runs *before* the stat
    // family in `diff_flush()`, so `--raw --stat` prints both, raw first.
    if opts.fmt.name_status || opts.fmt.raw {
        for row in &listed {
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
            // similarity as three digits and then *both* names. The separator is
            // `o->line_termination ? '\t' : '\0'` (diff.c:6270-6296), so `-z`
            // replaces every tab as well as the record terminator.
            let sep = if opts.z { 0 } else { b'\t' };
            if row.renamed() {
                out.extend_from_slice(
                    format!("{:03}", super::diffcore_rename::similarity_index(row.score)).as_bytes(),
                );
                out.push(sep);
                out.extend_from_slice(row.a_name.as_bytes());
                out.push(sep);
                out.extend_from_slice(row.b_name.as_bytes());
            } else {
                out.push(sep);
                let name = if row.a_exists { &row.a_name } else { &row.b_name };
                out.extend_from_slice(name.as_bytes());
            }
            out.push(term);
        }
    }
    let stat_rows: Vec<(BString, BString, u32, u32, bool)> = rows
        .iter()
        .map(|r| (r.a_name.clone(), r.b_name.clone(), r.added, r.deleted, r.binary))
        .collect();
    if opts.fmt.numstat {
        super::diff::render_rows_numstat(out, &stat_rows, opts.z);
    }
    if opts.fmt.stat {
        super::diff::render_rows_stat_ex(
            out,
            &stat_rows,
            &opts.colors,
            &opts.stat_widths,
            opts.fmt.compact_summary,
        );
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
            damage: 0,
            shown: true,
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
        let (out, ..) = compare(&s.at("A"), &s.at("B"), false, &o, rename, 0, None)
            .expect("readable operands");
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
            // The rest of `diff_setup()`'s defaults, which these cases do not vary:
            // no `--inter-hunk-context`, no `-D`, no `--binary`, no
            // `--skip-to`/`--rotate-to`, and `zlib_compression_level`'s `Z_BEST_SPEED`
            // — which is also what a `--no-index` run outside a repository uses,
            // there being no `core.looseCompression` to read.
            inter_hunk_ctx: 0,
            irreversible_delete: false,
            binary: false,
            skip_or_rotate: None,
            compression_level: 1,
            stat_widths: super::super::diffstat::StatWidths::default(),
            z: false,
            line_prefix: Vec::new(),
            dirstat: super::super::diff_files::DirStat::default(),
            ignore_blank_lines: false,
            filter: super::super::diff_filter::Filter::default(),
            paint: diff_color::PaintOptions::default(),
            extra: diff_color::ExtraPaint::default(),
        }
    }

    fn rendered(rows: &[Row], o: &Opts) -> String {
        let mut out = Vec::new();
        render_non_patch(&mut out, rows, o, false);
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
            damage: 0,
            shown: true,
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
            damage: 0,
            shown: true,
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

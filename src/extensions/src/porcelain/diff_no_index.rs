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

use anyhow::Result;
use gix::bstr::{BString, ByteSlice};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use super::diff_color;

/// `/dev/null` as an operand: git's `DIFF_FILE_VALID` for the side is false, and
/// the pair takes the other side's name.
const DEV_NULL: &str = "/dev/null";

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

/// One emitted pair, as the non-patch formats need it: the two display names, the
/// line counts, whether it was binary, whether each side existed, and the modes.
type Row = (BString, BString, u32, u32, bool, bool, bool, u32, u32);

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
    abbrev: usize,
    full_index: bool,
    text: bool,
    colors: diff_color::DiffColors,
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
    let mut abbrev = 7usize;
    let mut full_index = false;
    let mut text = false;
    let mut color_when: Option<diff_color::ColorWhen> = None;
    let mut operands: Vec<String> = Vec::new();
    let mut after_dashdash = false;
    // `OPT_BOOL('R', NULL, &options->flags.reverse_diff, …)` (diff.c:6254). The
    // NULL long name is why there is no `--no-R` to turn it back off, and why a
    // repeated `-R` re-sets rather than toggles.
    let mut reverse = false;

    for a in args {
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
            s if s.starts_with("--abbrev=") => {
                abbrev = s["--abbrev=".len()..].parse().unwrap_or(7);
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
    let colors = match (want_color, gix::discover(".")) {
        (true, Ok(repo)) => diff_color::DiffColors::resolve(&repo, true),
        _ => diff_color::DiffColors::disabled(),
    };
    // `builtin_diff()`: `a_prefix = o->b_prefix; b_prefix = o->a_prefix` under
    // `-R` (diff.c:3862-3868). The exchange is of whatever the two prefixes ended
    // up as, so an explicit `--src-prefix`/`--dst-prefix` pair swaps with them and
    // `--no-prefix`'s two empty strings swap to no visible effect.
    if reverse {
        std::mem::swap(&mut src_prefix, &mut dst_prefix);
    }
    let opts = Opts {
        fmt: fmt.resolved(),
        ctx,
        ws,
        func_context,
        src_prefix,
        dst_prefix,
        abbrev,
        full_index,
        text,
        colors,
    };

    let mut pairs = match queue(&operands[0], &operands[1]) {
        Ok(pairs) => pairs,
        Err(message) => {
            eprintln!("error: {message}");
            return Ok(ExitCode::from(1));
        }
    };
    // `queue_diff()`'s `SWAP(mode1, mode2); SWAP(name1, name2); SWAP(special1,
    // special2)` (diff-no-index.c:279-283). It sits at the leaf of the walk, past
    // the directory recursion that decided which names pair with which, so the
    // queue keeps its order and each pair simply trades sides.
    if reverse {
        for (a, b) in &mut pairs {
            std::mem::swap(a, b);
        }
    }

    let mut out: Vec<u8> = Vec::new();
    let mut changed = false;
    let mut stats: Vec<Row> = Vec::new();
    for (a, b) in &pairs {
        let old_data = read_side(a)?;
        let new_data = read_side(b)?;
        let same_content = a.file.is_some() == b.file.is_some() && old_data == new_data;
        if same_content && a.mode == b.mode {
            continue;
        }
        changed = true;
        let binary = !opts.text
            && (super::diff::looks_binary(&old_data) || super::diff::looks_binary(&new_data));
        let (added, deleted, body) =
            super::diff::no_index_body(&old_data, &new_data, &opts.ctx_geometry(), opts.ws, binary);
        stats.push((
            a.display_name().clone(),
            b.display_name().clone(),
            added,
            deleted,
            binary,
            a.file.is_some(),
            b.file.is_some(),
            a.mode,
            b.mode,
        ));
        if opts.fmt.patch {
            emit_header(&mut out, a, b, &old_data, &new_data, &opts, same_content, binary);
            if binary {
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
        render_non_patch(&mut out, &stats, &opts);
    }

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

impl Opts {
    fn ctx_geometry(&self) -> super::diff_pairs::EmitGeometry {
        super::diff_pairs::EmitGeometry {
            ctx: self.ctx as usize,
            inter_hunk_ctx: 0,
            func_context: self.func_context,
        }
    }
}

/// `<prefix><name>`, with `/dev/null` written bare for a side that is not there.
fn push_name(out: &mut Vec<u8>, prefix: &[u8], name: &BString, exists: bool) {
    if !exists {
        out.extend_from_slice(b"/dev/null");
        return;
    }
    out.extend_from_slice(prefix);
    out.extend_from_slice(name.as_bytes());
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
) {
    out.extend_from_slice(b"diff --git ");
    out.extend_from_slice(&opts.src_prefix);
    out.extend_from_slice(a.header_name(b).as_bytes());
    out.push(b' ');
    out.extend_from_slice(&opts.dst_prefix);
    out.extend_from_slice(b.header_name(a).as_bytes());
    out.push(b'\n');

    let hash = |data: &[u8], exists: bool| -> String {
        if !exists {
            return "0".repeat(opts.abbrev);
        }
        let id = gix::objs::compute_hash(gix::hash::Kind::Sha1, gix::objs::Kind::Blob, data)
            .expect("sha1 of an in-memory blob");
        let hex = id.to_hex().to_string();
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
    // A pure mode change has no content to describe, so no `index` line and no
    // file markers follow it.
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
        for (_, b, _, _, _, _, _, _, _) in rows {
            out.extend_from_slice(b.as_bytes());
            out.push(b'\n');
        }
        return;
    }
    if opts.fmt.name_status || opts.fmt.raw {
        for (a, b, _, _, _, a_exists, b_exists, a_mode, b_mode) in rows {
            let status = match (a_exists, b_exists) {
                (false, true) => b'A',
                (true, false) => b'D',
                _ => b'M',
            };
            if opts.fmt.raw {
                out.extend_from_slice(
                    format!(":{:06o} {:06o} {} {} ", a_mode, b_mode, "0".repeat(7), "0".repeat(7))
                        .as_bytes(),
                );
            }
            out.push(status);
            out.push(b'\t');
            out.extend_from_slice(if *a_exists { a.as_bytes() } else { b.as_bytes() });
            out.push(b'\n');
        }
        return;
    }
    let stat_rows: Vec<(BString, BString, u32, u32, bool)> = rows
        .iter()
        .map(|(a, b, add, del, bin, _, _, _, _)| (a.clone(), b.clone(), *add, *del, *bin))
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
            .filter(|(_, _, _, _, bin, _, _, _, _)| !bin)
            .fold((0u32, 0u32), |(a, d), (_, _, add, del, _, _, _, _, _)| (a + add, d + del));
        super::diff::stat_summary(out, rows.len() as u32, adds, dels);
    }
    if opts.fmt.summary {
        for (_, b, _, _, _, a_exists, b_exists, _, b_mode) in rows {
            match (a_exists, b_exists) {
                (false, true) => {
                    out.extend_from_slice(format!(" create mode {:06o} {b}\n", b_mode).as_bytes())
                }
                (true, false) => {
                    out.extend_from_slice(format!(" delete mode {:06o} {b}\n", b_mode).as_bytes())
                }
                _ => {}
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn present(name: &str) -> Side {
        Side { name: BString::from(name), file: Some(PathBuf::from(name)), mode: 0o100644 }
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

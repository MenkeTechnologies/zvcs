//! `git fast-import` — read a command/data stream on stdin and write the objects
//! and refs it describes into the current repository.
//!
//! Covered: the whole stream language as documented in `git-fast-import(1)` —
//! `blob`, `commit` (with `mark`, `original-oid`, `author`, `committer`,
//! `encoding`, `data`, `from`, `merge`, and the `M`/`D`/`C`/`R`/`N`/`deleteall`
//! file-change commands, inline or by dataref), `tag`, `reset`, `alias`,
//! `checkpoint`, `progress`, `done`, `get-mark`, `cat-blob`, `ls`, `feature` and
//! `option`, plus `#` comment lines, both `data` forms (byte count and `<<`
//! delimiter), C-style quoted paths, and the optional trailing LF after every
//! command. Commit and tag objects are assembled byte-for-byte the way
//! `fast-import.c` assembles them — identity strings are copied verbatim from the
//! stream, the author line defaults to the committer line, mode words are
//! canonicalized (`644` → `100644`), and trees are sorted with git's
//! `base_name_compare` — so the object ids match stock git's exactly.
//!
//! Command line: `--force`, `--quiet`, `--stats`, `--done`, `--date-format=`,
//! `--export-marks=`, `--import-marks=`, `--import-marks-if-exists=`,
//! `--relative-marks`, `--no-relative-marks`, `--cat-blob-fd=`,
//! `--signed-commits=`, `--signed-tags=`, `--allow-unsafe-features`, and the
//! pack-tuning knobs `--depth=`, `--active-branches=`, `--big-file-threshold=`
//! and `--max-pack-size=`, which are validated the way git validates them and
//! then ignored because they only steer packing. Refs are updated at EOF (and at
//! `checkpoint`) with the reflog message `fast-import`; without `--force` a
//! branch that would lose commits is left alone with git's
//! `warning: not updating …` and the run exits 1.
//!
//! git has three distinct failure contracts here and this module reproduces all
//! three. An argument that is not an option, or an option value outside the set
//! git names, ends the run with git's one-line `usage:` text and exit 129 having
//! touched nothing. Anything else fatal prints `fatal: <reason>` and exits 128 —
//! and, as git's `die_nicely` does, still writes the `--export-marks` file on the
//! way out, unless an `--import-marks` file was named and never successfully
//! read, in which case `dump_marks` declines to overwrite an export from a
//! half-loaded table. Options take effect strictly left to right, so a failure
//! leaves every option to its left already applied — which is what decides
//! whether the marks file exists after a fatal.
//!
//! Signatures follow git's `--signed-commits=`/`--signed-tags=` modes:
//! `verbatim` and `warn-verbatim` keep them (a commit's `gpgsig`/`gpgsig-sha256`
//! header is folded exactly as git folds it, one space per continuation line),
//! `strip` and `warn-strip` drop them, and `abort` refuses. The three
//! `-if-invalid` modes are accepted on the command line, as git accepts them,
//! and refused at the first signature they would have to verify: verification
//! needs a gpg driver the vendored crates do not provide.
//!
//! Not covered, each rejected with a precise message rather than guessed at:
//!   * `--date-format=rfc2822` / `now` — accepted on the command line, as git
//!     accepts them, and refused at the first identity line that would have to
//!     be parsed in them. Only `raw` and `raw-permissive` are ported, so a
//!     stream in another date format fails instead of storing a timestamp that
//!     might not be git's.
//!   * `--rewrite-submodules-from/-to=<name>:<file>` — the marks file is read
//!     where git reads it, so a missing or corrupt one fails identically, but a
//!     stream that actually carries a gitlink to rewrite is refused.
//!   * `--export-pack-edges=<file>` — the file is created where git creates it
//!     and a stream that would write objects is refused, because this port has
//!     no packs and so no pack edges to record.
//!   * `N` once a notes ref would exceed 255 notes, where git re-fans-out the
//!     whole notes tree.
//!
//! Two deliberate substrate differences, neither of which changes the objects or
//! refs a caller can observe: objects are written as loose objects rather than
//! into a new packfile (git itself explodes small packs into loose objects below
//! `fastimport.unpackLimit`, which defaults to 100), and the `--stats` block —
//! stderr only, and full of allocator and pack-window counters that have no
//! equivalent here — is never printed. No crash report is dumped on a fatal
//! error either; the `fast_import_crash_<pid>` file could never match anyway,
//! and it lives inside `.git` where nothing observes it.

use anyhow::{anyhow, bail, Context, Result};
use std::collections::{BTreeMap, HashMap};
use std::io::{BufRead, Read, Write as _};
use std::process::ExitCode;

use gix::hash::ObjectId;
use gix::objs::{Kind, Write as _};
use gix::refs::transaction::{Change, LogChange, PreviousValue, RefEdit, RefLog};
use gix::refs::{FullName, Target};

/// git's one-line `fast-import` usage summary, byte-for-byte including its LF.
const USAGE: &str = concat!(
    "usage: git fast-import [--date-format=<f>] [--max-pack-size=<n>]",
    " [--big-file-threshold=<n>] [--depth=<n>] [--active-branches=<n>]",
    " [--export-marks=<marks.file>]\n",
);

/// A command line git rejects through `usage()` — stderr, exit 129, and none of
/// the cleanup `die_nicely` runs — rather than through `die()`.
///
/// Carries the complete stderr text, trailing LF included, because git uses two
/// shapes: the bare option summary for an argument it cannot place at all, and
/// `usage: <complaint>` for an option value outside the set it names.
#[derive(Debug)]
struct UsageError(String);

impl std::fmt::Display for UsageError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.0.trim_end())
    }
}

impl std::error::Error for UsageError {}

/// The bare option summary, for an argument that is not an option at all.
fn usage() -> anyhow::Error {
    UsageError(USAGE.to_string()).into()
}

/// Which `--date-format` the stream uses.
#[derive(Clone, Copy, PartialEq)]
enum DateFormat {
    /// `<seconds> SP <±hhmm>`, with git's sanity checks on the offset.
    Raw,
    /// The same syntax with the offset sanity check relaxed.
    RawPermissive,
    /// Accepted so the command line parses as git's does, then refused at the
    /// first identity line: parsing RFC 2822 dates means reimplementing git's
    /// date parser, and a near-miss silently stores the wrong timestamp.
    Rfc2822,
    /// Likewise accepted and then refused. `now` ignores the stream's timestamp
    /// and takes the wall clock, which no reproducible import can rely on.
    Now,
}

impl DateFormat {
    /// The spelling this format has on the command line, for error text.
    fn name(self) -> &'static str {
        match self {
            DateFormat::Raw => "raw",
            DateFormat::RawPermissive => "raw-permissive",
            DateFormat::Rfc2822 => "rfc2822",
            DateFormat::Now => "now",
        }
    }
}

/// `--signed-commits=`/`--signed-tags=`: what to do with a signature in the stream.
#[derive(Clone, Copy, PartialEq)]
enum SignedMode {
    /// Import the signature silently. git's default.
    Verbatim,
    /// Import it, after a warning.
    WarnVerbatim,
    /// Drop the signature silently.
    Strip,
    /// Drop it, after a warning.
    WarnStrip,
    /// Die on the first signature.
    Abort,
    /// `strip-if-invalid`, `abort-if-invalid` and `sign-if-invalid[=<keyid>]`.
    /// All three decide by *verifying* the signature, which needs a gpg driver
    /// the vendored crates do not provide, so they are accepted here — git
    /// accepts them — and refused at the first signature rather than guessed at.
    NeedsVerification,
}

/// Map a `--signed-commits=`/`--signed-tags=` value onto a mode, rejecting an
/// unknown one through `usage()` as git does.
fn signed_mode(flag: &str, value: &str) -> Result<SignedMode> {
    Ok(match value {
        "verbatim" => SignedMode::Verbatim,
        "warn-verbatim" => SignedMode::WarnVerbatim,
        "strip" => SignedMode::Strip,
        "warn-strip" => SignedMode::WarnStrip,
        "abort" => SignedMode::Abort,
        "strip-if-invalid" | "abort-if-invalid" | "sign-if-invalid" => {
            SignedMode::NeedsVerification
        }
        _ if starts(value, "sign-if-invalid=") => SignedMode::NeedsVerification,
        _ => {
            return Err(UsageError(format!("usage: unknown {flag} mode '{value}'\n")).into());
        }
    })
}

/// Everything the command line and the stream's `feature`/`option` commands can set.
struct Opts {
    force: bool,
    date_format: DateFormat,
    require_done: bool,
    export_marks: Option<String>,
    relative_marks: bool,
    allow_unsafe: bool,
    cat_blob_fd: Option<i32>,
    signed_commits: SignedMode,
    signed_tags: SignedMode,
    /// `--export-pack-edges=<file>`, kept only so a run that would actually have
    /// pack edges to report can refuse instead of leaving the file empty.
    export_pack_edges: Option<String>,
    /// True once an `--import-marks` file has been named and not yet read. git's
    /// `dump_marks` refuses to write while this is outstanding, so a fatal
    /// before the read leaves the export file untouched.
    import_marks_pending: bool,
}

impl Opts {
    /// git's defaults before argv is read.
    fn new() -> Self {
        Opts {
            force: false,
            date_format: DateFormat::Raw,
            require_done: false,
            export_marks: None,
            relative_marks: false,
            allow_unsafe: false,
            cat_blob_fd: None,
            signed_commits: SignedMode::Verbatim,
            signed_tags: SignedMode::Verbatim,
            export_pack_edges: None,
            import_marks_pending: false,
        }
    }
}

/// A ref being built in memory, exactly one per name the stream has named.
struct Branch {
    /// Full ref name, e.g. `refs/heads/main`.
    name: String,
    /// The tip this branch will be written at, or `None` while it has none.
    head: Option<ObjectId>,
    /// The tree the next commit on this branch starts from.
    tree: Dir,
    /// Set by `from <null oid>`: the ref is to be deleted rather than updated.
    delete: bool,
    /// Notes counted in `tree`, used to pick the notes fanout.
    notes: u64,
}

/// One entry in an in-memory tree: a blob/symlink/gitlink, or a sub-directory.
#[derive(Clone)]
enum Node {
    Leaf { mode: u32, oid: ObjectId },
    Dir(Dir),
}

/// An in-memory tree. Keyed by raw file name; git's ordering is applied on write.
#[derive(Clone, Default)]
struct Dir {
    entries: BTreeMap<Vec<u8>, Node>,
}

/// `git fast-import` — import a stream of objects and ref updates from stdin.
///
/// The three exit contracts are git's. A command line git cannot parse prints
/// `usage: …` and exits 129; any other failure prints `fatal: <reason>` and
/// exits 128; a run that imported cleanly but declined a non-fast-forward ref
/// update exits 1.
pub fn fast_import(args: &[String]) -> Result<ExitCode> {
    // Dispatch passes the subcommand itself at index 0.
    let args = match args.first() {
        Some(a) if a == "fast-import" => &args[1..],
        _ => args,
    };
    match run(args) {
        Ok(code) => Ok(code),
        Err(e) => match e.downcast_ref::<UsageError>() {
            Some(u) => {
                eprint!("{}", u.0);
                Ok(ExitCode::from(129))
            }
            None => {
                eprintln!("fatal: {e}");
                Ok(ExitCode::from(128))
            }
        },
    }
}

/// Open the repository, parse the command line, then drive the stream.
fn run(args: &[String]) -> Result<ExitCode> {
    // git runs `setup_git_directory()` before it looks at argv, so even a usage
    // error outside a repository comes out as "not a git repository".
    let repo = gix::discover(".")?;
    // Serialize object and ref writes through the repo coordinator, as the other
    // writing porcelain does, so concurrent zvcs writers queue instead of racing.
    let _lock = crate::lock::RepoLock::acquire(repo.git_dir());

    let mut imp = Importer {
        repo,
        opts: Opts::new(),
        marks: HashMap::new(),
        branches: Vec::new(),
        by_name: HashMap::new(),
        tags: Vec::new(),
        failed: false,
        seen_data_command: false,
        submodule_rewrites: Vec::new(),
    };

    match imp.import(args) {
        Ok(code) => Ok(code),
        // git's `usage()` exits without running `die_nicely`, so a usage error
        // leaves the marks file alone; every other failure still dumps it.
        Err(e) if e.downcast_ref::<UsageError>().is_some() => Err(e),
        Err(e) => {
            imp.dump_marks_on_fatal();
            Err(e)
        }
    }
}

/// `str::starts_with` under a shorter name, so the option table stays readable.
fn starts(s: &str, prefix: &str) -> bool {
    s.starts_with(prefix)
}

/// Map a `--date-format=`/`feature date-format=` value onto a format.
///
/// Every name git knows parses here; the two this port cannot evaluate are
/// refused at the identity line that would need them, not on the command line,
/// because that is where git's own failure would be observable.
fn date_format(name: &str) -> Result<DateFormat> {
    match name {
        "raw" => Ok(DateFormat::Raw),
        "raw-permissive" => Ok(DateFormat::RawPermissive),
        "rfc2822" => Ok(DateFormat::Rfc2822),
        "now" => Ok(DateFormat::Now),
        _ => bail!("unknown --date-format argument {name}"),
    }
}

/// git's `parse_non_negative_integer`, used for `--depth`, `--active-branches`
/// and `--cat-blob-fd`.
fn non_negative(flag: &str, value: &str) -> Result<u64> {
    value
        .parse::<u64>()
        .map_err(|_| anyhow!("{flag}: argument must be a non-negative integer"))
}

/// git's `parse_ulong_with_suffix`, used for the two byte-size options. A value
/// it cannot read is not a distinct error: git falls through and reports the
/// whole argument as an unknown option.
fn byte_size(value: &str) -> Option<u64> {
    // Every suffix git accepts is one ASCII byte, so trimming one byte off the
    // end cannot split a character.
    let (digits, scale) = match value.chars().last() {
        Some('k' | 'K') => (&value[..value.len() - 1], 1024_u64),
        Some('m' | 'M') => (&value[..value.len() - 1], 1024 * 1024),
        Some('g' | 'G') => (&value[..value.len() - 1], 1024 * 1024 * 1024),
        _ => (value, 1),
    };
    digits.parse::<u64>().ok()?.checked_mul(scale)
}

// ---------------------------------------------------------------------------
// Input
// ---------------------------------------------------------------------------

/// The command stream on stdin, with git's one-line pushback and its
/// "consume a trailing LF if there is one" primitive.
struct Input {
    stdin: std::io::StdinLock<'static>,
    /// A command line that was read and handed back with [`Input::unread`].
    pending: Option<Vec<u8>>,
}

impl Input {
    fn new() -> Self {
        Input {
            stdin: std::io::stdin().lock(),
            pending: None,
        }
    }

    /// One raw line without its LF, or `None` at EOF.
    fn line(&mut self) -> Result<Option<Vec<u8>>> {
        if let Some(line) = self.pending.take() {
            return Ok(Some(line));
        }
        let mut buf = Vec::new();
        if self.stdin.read_until(b'\n', &mut buf)? == 0 {
            return Ok(None);
        }
        if buf.last() == Some(&b'\n') {
            buf.pop();
        }
        Ok(Some(buf))
    }

    /// The next command line, skipping `#` comments as `read_next_command` does.
    fn command(&mut self) -> Result<Option<Vec<u8>>> {
        loop {
            match self.line()? {
                None => return Ok(None),
                Some(l) if l.first() == Some(&b'#') => continue,
                Some(l) => return Ok(Some(l)),
            }
        }
    }

    /// Hand a command line back, to be returned by the next [`Input::command`].
    fn unread(&mut self, line: Vec<u8>) {
        self.pending = Some(line);
    }

    /// Exactly `n` bytes of raw data.
    fn exact(&mut self, n: usize) -> Result<Vec<u8>> {
        let mut buf = vec![0u8; n];
        self.stdin
            .read_exact(&mut buf)
            .map_err(|_| anyhow!("EOF in data ({n} bytes remaining)"))?;
        Ok(buf)
    }

    /// git's `skip_optional_lf`: eat one LF if the next byte is one.
    fn skip_optional_lf(&mut self) -> Result<()> {
        debug_assert!(self.pending.is_none(), "a pushed-back line hides the stream");
        if self.stdin.fill_buf()?.first() == Some(&b'\n') {
            self.stdin.consume(1);
        }
        Ok(())
    }

    /// A `data` command's payload, in either the byte-count or delimiter form.
    fn data(&mut self, spec: &[u8]) -> Result<Vec<u8>> {
        let out = if let Some(delim) = spec.strip_prefix(b"<<") {
            let delim = delim.to_vec();
            let mut out = Vec::new();
            loop {
                let Some(line) = self.line()? else {
                    bail!(
                        "EOF in data (terminator '{}' not found)",
                        String::from_utf8_lossy(&delim)
                    );
                };
                if line == delim {
                    break;
                }
                out.extend_from_slice(&line);
                out.push(b'\n');
            }
            out
        } else {
            let text = std::str::from_utf8(spec).unwrap_or("");
            let n: usize = text
                .trim()
                .parse()
                .map_err(|_| anyhow!("invalid count in data command: {text}"))?;
            self.exact(n)?
        };
        self.skip_optional_lf()?;
        Ok(out)
    }
}

// ---------------------------------------------------------------------------
// Importer
// ---------------------------------------------------------------------------

/// The whole import: the repository, the mark table, and the refs in flight.
struct Importer {
    repo: gix::Repository,
    opts: Opts,
    marks: HashMap<u64, ObjectId>,
    /// Branches in the order the stream first named them.
    branches: Vec<Branch>,
    by_name: HashMap<String, usize>,
    /// `(<tag name>, <object id>)`, in stream order; written under `refs/tags/`.
    tags: Vec<(String, ObjectId)>,
    /// Set when a ref update was declined; makes the process exit 1.
    failed: bool,
    /// Whether a command other than `feature`/`option` has been seen, which is
    /// what makes a later `option` command an error.
    seen_data_command: bool,
    /// Submodule names named by `--rewrite-submodules-from/-to`. Only their
    /// presence is used: a stream that carries a gitlink is refused, because
    /// rewriting the object id it names is not ported.
    submodule_rewrites: Vec<String>,
}

impl Importer {
    /// Parse the command line, read the marks it names, then drive the stream.
    fn import(&mut self, args: &[String]) -> Result<ExitCode> {
        let import_marks = self.parse_args(args)?;
        for (path, if_exists) in &import_marks {
            self.import_marks(path, *if_exists)?;
        }
        self.opts.import_marks_pending = false;

        let mut input = Input::new();
        let saw_done = self.stream(&mut input)?;
        if self.opts.require_done && !saw_done {
            bail!("stream ends early");
        }

        self.checkpoint()?;
        Ok(if self.failed {
            ExitCode::FAILURE
        } else {
            ExitCode::SUCCESS
        })
    }

    /// Apply argv to `self.opts`, returning the `--import-marks` files to read.
    ///
    /// Mirrors git's `parse_argv`: options are applied strictly left to right,
    /// so a failure leaves everything to its left in effect, and the first
    /// argument that is not an option — or a bare `--` — ends option parsing and,
    /// because git has no positional arguments here, ends the run with `usage()`.
    fn parse_args(&mut self, args: &[String]) -> Result<Vec<(String, bool)>> {
        let mut import_marks: Vec<(String, bool)> = Vec::new();
        let mut i = 0;
        while i < args.len() {
            let a = args[i].as_str();
            if !a.starts_with('-') || a == "--" {
                break;
            }
            match a {
                "--force" => self.opts.force = true,
                // Both only steer the stderr statistics block, which is not printed.
                "--quiet" | "--stats" => {}
                "--done" => self.opts.require_done = true,
                "--allow-unsafe-features" => self.opts.allow_unsafe = true,
                "--relative-marks" => self.opts.relative_marks = true,
                "--no-relative-marks" => self.opts.relative_marks = false,
                // Pack tuning only: identical objects and refs either way, but
                // the values are still validated so a typo fails as it does in git.
                _ if starts(a, "--depth=") => {
                    non_negative("--depth", &a["--depth=".len()..])?;
                }
                _ if starts(a, "--active-branches=") => {
                    non_negative("--active-branches", &a["--active-branches=".len()..])?;
                }
                _ if starts(a, "--big-file-threshold=") => {
                    byte_size(&a["--big-file-threshold=".len()..])
                        .ok_or_else(|| anyhow!("unknown option {a}"))?;
                }
                _ if starts(a, "--max-pack-size=") => {
                    byte_size(&a["--max-pack-size=".len()..])
                        .ok_or_else(|| anyhow!("unknown option {a}"))?;
                }
                _ if starts(a, "--date-format=") => {
                    self.opts.date_format = date_format(&a["--date-format=".len()..])?;
                }
                _ if starts(a, "--export-marks=") => {
                    self.opts.export_marks = Some(a["--export-marks=".len()..].to_string());
                }
                _ if starts(a, "--import-marks=") => {
                    import_marks.push((a["--import-marks=".len()..].to_string(), false));
                    self.opts.import_marks_pending = true;
                }
                _ if starts(a, "--import-marks-if-exists=") => {
                    import_marks.push((a["--import-marks-if-exists=".len()..].to_string(), true));
                    self.opts.import_marks_pending = true;
                }
                _ if starts(a, "--cat-blob-fd=") => {
                    let v = &a["--cat-blob-fd=".len()..];
                    let fd = non_negative("--cat-blob-fd", v)?;
                    self.opts.cat_blob_fd = Some(
                        i32::try_from(fd)
                            .map_err(|_| anyhow!("--cat-blob-fd: argument must be a non-negative integer"))?,
                    );
                }
                _ if starts(a, "--signed-commits=") => {
                    self.opts.signed_commits =
                        signed_mode("--signed-commits", &a["--signed-commits=".len()..])?;
                }
                _ if starts(a, "--signed-tags=") => {
                    self.opts.signed_tags =
                        signed_mode("--signed-tags", &a["--signed-tags=".len()..])?;
                }
                _ if starts(a, "--rewrite-submodules-from=") => {
                    self.submodule_rewrite(&a["--rewrite-submodules-from=".len()..])?;
                }
                _ if starts(a, "--rewrite-submodules-to=") => {
                    self.submodule_rewrite(&a["--rewrite-submodules-to=".len()..])?;
                }
                _ if starts(a, "--export-pack-edges=") => {
                    let path = &a["--export-pack-edges=".len()..];
                    // git opens the file the moment it parses the option, which
                    // is why it exists even when the run dies further along.
                    std::fs::File::create(path)
                        .with_context(|| format!("Cannot open '{path}'"))?;
                    self.opts.export_pack_edges = Some(path.to_string());
                }
                _ => bail!("unknown option {a}"),
            }
            i += 1;
        }
        if i != args.len() {
            return Err(usage());
        }
        Ok(import_marks)
    }

    /// `--rewrite-submodules-from/-to=<name>:<marks file>`: validate the spec and
    /// read the marks file, so both failures land exactly where git's do.
    fn submodule_rewrite(&mut self, spec: &str) -> Result<()> {
        let (name, file) = spec
            .split_once(':')
            .ok_or_else(|| anyhow!("expected format name:filename for submodule rewrite option"))?;
        // Parsed for its errors: a missing or corrupt marks file is the only
        // observable effect of this option until a gitlink actually appears.
        read_mark_file(std::path::Path::new(file), file, false)?;
        self.submodule_rewrites.push(name.to_string());
        Ok(())
    }

    /// git's `die_nicely` path: a fatal error still writes the `--export-marks`
    /// file on the way out.
    fn dump_marks_on_fatal(&self) {
        let _ = self.export_marks();
    }

    /// Refuse a stream that writes objects while `--export-pack-edges` is set.
    ///
    /// git names one pack boundary per pack it produced; this port writes loose
    /// objects, so it would leave the file git created empty. An empty edges file
    /// reads as "the import produced no packs", which is a wrong answer that
    /// looks like a right one, so the run stops instead.
    fn check_pack_edges(&self) -> Result<()> {
        match &self.opts.export_pack_edges {
            None => Ok(()),
            Some(path) => bail!(
                "unsupported flag \"--export-pack-edges={path}\" for a stream that writes \
                 objects (this port writes loose objects, so there are no pack edges to report)"
            ),
        }
    }

    /// Read commands until `done` or EOF. Returns whether `done` ended the stream.
    fn stream(&mut self, input: &mut Input) -> Result<bool> {
        while let Some(line) = input.command()? {
            let cmd = line.as_slice();
            if cmd == b"blob" {
                self.seen_data_command = true;
                self.check_pack_edges()?;
                self.parse_blob(input)?;
            } else if let Some(v) = after(cmd, b"commit ") {
                self.seen_data_command = true;
                self.check_pack_edges()?;
                let name = utf8(v, "ref name")?;
                self.parse_commit(input, &name)?;
            } else if let Some(v) = after(cmd, b"tag ") {
                self.seen_data_command = true;
                self.check_pack_edges()?;
                let name = utf8(v, "tag name")?;
                self.parse_tag(input, &name)?;
            } else if let Some(v) = after(cmd, b"reset ") {
                self.seen_data_command = true;
                let name = utf8(v, "ref name")?;
                self.parse_reset(input, &name)?;
            } else if cmd == b"alias" {
                self.seen_data_command = true;
                self.parse_alias(input)?;
            } else if cmd == b"checkpoint" {
                self.seen_data_command = true;
                self.checkpoint()?;
                input.skip_optional_lf()?;
            } else if let Some(v) = after(cmd, b"progress ") {
                self.seen_data_command = true;
                let mut out = b"progress ".to_vec();
                out.extend_from_slice(v);
                out.push(b'\n');
                stdout_write(&out)?;
                input.skip_optional_lf()?;
            } else if let Some(v) = after(cmd, b"get-mark ") {
                self.seen_data_command = true;
                let id = self.mark_ref(v)?;
                self.respond(format!("{id}\n").as_bytes())?;
            } else if let Some(v) = after(cmd, b"cat-blob ") {
                self.seen_data_command = true;
                self.cat_blob(v)?;
            } else if let Some(v) = after(cmd, b"ls ") {
                self.seen_data_command = true;
                self.parse_ls(v, None)?;
            } else if let Some(v) = after(cmd, b"feature ") {
                self.parse_feature(utf8(v, "feature")?.as_str())?;
            } else if let Some(v) = after(cmd, b"option ") {
                self.parse_option(utf8(v, "option")?.as_str())?;
            } else if cmd == b"done" {
                return Ok(true);
            } else {
                bail!("unsupported command: {}", String::from_utf8_lossy(cmd));
            }
        }
        Ok(false)
    }

    // -- individual commands ------------------------------------------------

    /// `blob LF mark? original-oid? data`.
    fn parse_blob(&mut self, input: &mut Input) -> Result<()> {
        let mut mark = None;
        let mut line = input.command()?;
        if let Some(v) = field(&line, b"mark :") {
            mark = Some(parse_mark(&v)?);
            line = input.command()?;
        }
        if field(&line, b"original-oid ").is_some() {
            line = input.command()?;
        }
        let Some(spec) = field(&line, b"data ") else {
            bail!("expected 'data n' command");
        };
        let payload = input.data(&spec)?;
        let id = self.repo.write_blob(&payload)?.detach();
        if let Some(m) = mark {
            self.marks.insert(m, id);
        }
        Ok(())
    }

    /// `commit <ref>` with its header block, ancestry and file changes.
    fn parse_commit(&mut self, input: &mut Input, name: &str) -> Result<()> {
        let idx = self.branch(name)?;

        let mut mark = None;
        let mut author: Option<Vec<u8>> = None;
        let mut encoding: Option<Vec<u8>> = None;

        let mut line = input.command()?;
        if let Some(v) = field(&line, b"mark :") {
            mark = Some(parse_mark(&v)?);
            line = input.command()?;
        }
        if field(&line, b"original-oid ").is_some() {
            line = input.command()?;
        }
        if let Some(v) = field(&line, b"author ") {
            author = Some(self.ident(&v)?);
            line = input.command()?;
        }
        let Some(v) = field(&line, b"committer ") else {
            bail!("expected committer command");
        };
        let committer = self.ident(&v)?;
        line = input.command()?;

        // `gpgsig SP <git hash algo> SP <signature format> LF data`, at most one
        // per hash algorithm, sitting between `committer` and `encoding`.
        let mut signatures: Vec<(&'static str, Vec<u8>)> = Vec::new();
        while let Some(v) = field(&line, b"gpgsig ") {
            let spec = utf8(&v, "gpgsig")?;
            let algo = spec.split(' ').next().unwrap_or_default();
            let header = match algo {
                "sha1" => "gpgsig",
                "sha256" => "gpgsig-sha256",
                _ => bail!("unknown git hash algorithm in gpgsig: '{algo}'"),
            };
            line = input.command()?;
            let Some(spec) = field(&line, b"data ") else {
                bail!("expected 'data n' command");
            };
            signatures.push((header, input.data(&spec)?));
            line = input.command()?;
        }
        if !signatures.is_empty() {
            match self.opts.signed_commits {
                SignedMode::Verbatim => {}
                SignedMode::WarnVerbatim => {
                    eprintln!("warning: importing a commit signature verbatim");
                }
                SignedMode::Strip => signatures.clear(),
                SignedMode::WarnStrip => {
                    eprintln!("warning: stripping a commit signature");
                    signatures.clear();
                }
                SignedMode::Abort => {
                    bail!("encountered signed commit; use --signed-commits=<mode> to handle it")
                }
                SignedMode::NeedsVerification => bail!(
                    "unsupported flag \"--signed-commits=<mode>-if-invalid\" (the `-if-invalid` \
                     modes decide by verifying the signature, which needs a gpg driver the \
                     vendored crates do not provide)"
                ),
            }
        }

        if let Some(v) = field(&line, b"encoding ") {
            encoding = Some(v);
            line = input.command()?;
        }
        let Some(spec) = field(&line, b"data ") else {
            bail!("expected 'data n' command");
        };
        let message = input.data(&spec)?;

        // `from` resets the branch's ancestry and tree; `merge` only adds parents.
        let mut line = input.command()?;
        if let Some(from) = field(&line, b"from ") {
            self.parse_from(idx, &from)?;
            line = input.command()?;
        }
        let mut parents: Vec<ObjectId> = self.branches[idx].head.into_iter().collect();
        while let Some(spec) = field(&line, b"merge ") {
            parents.push(self.commitish(&spec)?);
            line = input.command()?;
        }

        // File changes run until a blank line, EOF, or a line we do not own.
        loop {
            let Some(cmd) = line.clone() else { break };
            if cmd.is_empty() {
                break;
            }
            if let Some(v) = after(&cmd, b"M ") {
                let v = v.to_vec();
                self.file_modify(input, idx, &v)?;
            } else if let Some(v) = after(&cmd, b"D ") {
                let path = unquote(v)?;
                dir_remove(&mut self.branches[idx].tree, &path);
            } else if let Some(v) = after(&cmd, b"C ") {
                let v = v.to_vec();
                self.file_copy(idx, &v, false)?;
            } else if let Some(v) = after(&cmd, b"R ") {
                let v = v.to_vec();
                self.file_copy(idx, &v, true)?;
            } else if let Some(v) = after(&cmd, b"N ") {
                let v = v.to_vec();
                self.note_change(input, idx, &v)?;
            } else if cmd == b"deleteall" {
                self.branches[idx].tree = Dir::default();
                self.branches[idx].notes = 0;
            } else if let Some(v) = after(&cmd, b"ls ") {
                let v = v.to_vec();
                self.parse_ls(&v, Some(idx))?;
            } else if let Some(v) = after(&cmd, b"cat-blob ") {
                let v = v.to_vec();
                self.cat_blob(&v)?;
            } else {
                input.unread(cmd);
                break;
            }
            line = input.command()?;
        }

        let tree = write_dir(&self.repo, &self.branches[idx].tree)?;
        let mut buf = Vec::new();
        buf.extend_from_slice(format!("tree {tree}\n").as_bytes());
        for p in &parents {
            buf.extend_from_slice(format!("parent {p}\n").as_bytes());
        }
        buf.extend_from_slice(b"author ");
        buf.extend_from_slice(author.as_ref().unwrap_or(&committer));
        buf.extend_from_slice(b"\ncommitter ");
        buf.extend_from_slice(&committer);
        buf.push(b'\n');
        for (header, payload) in &signatures {
            // git folds a multi-line header value by prefixing every line after
            // the first with one space, and the payload's trailing LF makes a
            // final empty — so folded to a bare space — line.
            buf.extend_from_slice(header.as_bytes());
            buf.push(b' ');
            let mut lines = payload.split(|&b| b == b'\n');
            if let Some(first) = lines.next() {
                buf.extend_from_slice(first);
            }
            buf.push(b'\n');
            for rest in lines {
                buf.push(b' ');
                buf.extend_from_slice(rest);
                buf.push(b'\n');
            }
        }
        if let Some(enc) = &encoding {
            buf.extend_from_slice(b"encoding ");
            buf.extend_from_slice(enc);
            buf.push(b'\n');
        }
        buf.push(b'\n');
        buf.extend_from_slice(&message);

        let id = self.repo.write_buf(Kind::Commit, &buf).map_err(to_anyhow)?;
        self.branches[idx].head = Some(id);
        self.branches[idx].delete = false;
        if let Some(m) = mark {
            self.marks.insert(m, id);
        }
        Ok(())
    }

    /// `tag <name> LF mark? from original-oid? tagger? data`.
    fn parse_tag(&mut self, input: &mut Input, name: &str) -> Result<()> {
        let mut mark = None;
        let mut line = input.command()?;
        if let Some(v) = field(&line, b"mark :") {
            mark = Some(parse_mark(&v)?);
            line = input.command()?;
        }
        let Some(spec) = field(&line, b"from ") else {
            bail!("expected from command");
        };
        let object = self.commitish(&spec)?;
        line = input.command()?;

        if field(&line, b"original-oid ").is_some() {
            line = input.command()?;
        }
        let mut tagger = None;
        if let Some(v) = field(&line, b"tagger ") {
            tagger = Some(self.ident(&v)?);
            line = input.command()?;
        }
        let Some(spec) = field(&line, b"data ") else {
            bail!("expected 'data n' command");
        };
        let mut message = input.data(&spec)?;

        // A tag's signature lives in its message, so the mode is applied by
        // truncating at the marker git's `parse_signature` would find.
        if let Some(at) = signature_offset(&message) {
            match self.opts.signed_tags {
                SignedMode::Verbatim => {}
                SignedMode::WarnVerbatim => {
                    eprintln!("warning: importing a tag signature verbatim for tag '{name}'");
                }
                SignedMode::Strip => message.truncate(at),
                SignedMode::WarnStrip => {
                    eprintln!("warning: stripping a tag signature for tag '{name}'");
                    message.truncate(at);
                }
                SignedMode::Abort => {
                    bail!("encountered signed tag; use --signed-tags=<mode> to handle it")
                }
                SignedMode::NeedsVerification => bail!(
                    "unsupported flag \"--signed-tags=<mode>-if-invalid\" (the `-if-invalid` \
                     modes decide by verifying the signature, which needs a gpg driver the \
                     vendored crates do not provide)"
                ),
            }
        }

        let kind = self.repo.find_header(object)?.kind();
        let mut buf = Vec::new();
        buf.extend_from_slice(format!("object {object}\ntype {kind}\ntag {name}\n").as_bytes());
        if let Some(t) = &tagger {
            buf.extend_from_slice(b"tagger ");
            buf.extend_from_slice(t);
            buf.push(b'\n');
        }
        buf.push(b'\n');
        buf.extend_from_slice(&message);

        let id = self.repo.write_buf(Kind::Tag, &buf).map_err(to_anyhow)?;
        self.tags.push((name.to_string(), id));
        if let Some(m) = mark {
            self.marks.insert(m, id);
        }
        Ok(())
    }

    /// `reset <ref> LF from? LF?`.
    fn parse_reset(&mut self, input: &mut Input, name: &str) -> Result<()> {
        let idx = self.branch(name)?;
        self.branches[idx].head = None;
        self.branches[idx].tree = Dir::default();
        self.branches[idx].notes = 0;

        if let Some(line) = input.command()? {
            // Take an owned copy so the pushback below is not holding a borrow.
            match after(&line, b"from ").map(<[u8]>::to_vec) {
                Some(from) => {
                    self.parse_from(idx, &from)?;
                    input.skip_optional_lf()?;
                }
                // The optional blank line that ends the command, or something
                // that belongs to whoever reads next.
                None if line.is_empty() => {}
                None => input.unread(line),
            }
        }
        Ok(())
    }

    /// `alias LF mark 'to' SP <commit-ish> LF LF?`.
    fn parse_alias(&mut self, input: &mut Input) -> Result<()> {
        let line = input.command()?;
        let Some(v) = field(&line, b"mark :") else {
            bail!("expected mark command");
        };
        let mark = parse_mark(&v)?;
        let line = input.command()?;
        let Some(spec) = field(&line, b"to ") else {
            bail!("expected to command");
        };
        let id = self.commitish(&spec)?;
        self.marks.insert(mark, id);
        input.skip_optional_lf()?;
        Ok(())
    }

    /// `feature <name>[=<argument>]`.
    fn parse_feature(&mut self, spec: &str) -> Result<()> {
        let (name, arg) = match spec.split_once('=') {
            Some((n, a)) => (n, Some(a)),
            None => (spec, None),
        };
        let unsafe_feature = matches!(name, "export-marks" | "import-marks" | "import-marks-if-exists");
        if unsafe_feature && !self.opts.allow_unsafe {
            bail!("feature '{spec}' forbidden in input without --allow-unsafe-features");
        }
        match name {
            "date-format" => {
                self.opts.date_format = date_format(arg.unwrap_or_default())?;
            }
            // Command-line marks options win, so a stream request is only honoured
            // when the command line was silent — which is git's ordering too.
            "export-marks" => {
                if self.opts.export_marks.is_none() {
                    self.opts.export_marks = Some(arg.unwrap_or_default().to_string());
                }
            }
            "import-marks" => self.import_marks(arg.unwrap_or_default(), false)?,
            "import-marks-if-exists" => self.import_marks(arg.unwrap_or_default(), true)?,
            "relative-marks" => self.opts.relative_marks = true,
            "no-relative-marks" => self.opts.relative_marks = false,
            "force" => self.opts.force = true,
            "done" => self.opts.require_done = true,
            // Capability probes for commands this port implements.
            "get-mark" | "cat-blob" | "ls" | "notes" => {}
            _ => bail!("this version of fast-import does not support feature {spec}."),
        }
        Ok(())
    }

    /// `option <option>`; only `git `-prefixed options are ours to interpret.
    fn parse_option(&mut self, spec: &str) -> Result<()> {
        let Some(opt) = spec.strip_prefix("git ") else {
            // Options addressed at another importer are silently ignored.
            return Ok(());
        };
        if self.seen_data_command {
            bail!("option command must be the first command in the stream");
        }
        match opt {
            "quiet" | "stats" => Ok(()),
            _ if starts(opt, "max-pack-size=")
                || starts(opt, "big-file-threshold=")
                || starts(opt, "depth=")
                || starts(opt, "active-branches=") =>
            {
                Ok(())
            }
            _ => bail!("this version of fast-import does not support option: {opt}"),
        }
    }

    // -- file changes -------------------------------------------------------

    /// `M SP <mode> SP <dataref> SP <path>`, with the dataref possibly `inline`.
    fn file_modify(&mut self, input: &mut Input, idx: usize, rest: &[u8]) -> Result<()> {
        let (mode_word, rest) = split_space(rest)
            .ok_or_else(|| anyhow!("Corrupt mode: M {}", String::from_utf8_lossy(rest)))?;
        let mode = canonical_mode(mode_word)
            .ok_or_else(|| anyhow!("Corrupt mode: M {}", String::from_utf8_lossy(mode_word)))?;

        let (oid, path) = if let Some(after_inline) = after(rest, b"inline ") {
            let path = unquote(after_inline)?;
            let line = input.command()?;
            let Some(spec) = field(&line, b"data ") else {
                bail!("expected 'data n' command");
            };
            let payload = input.data(&spec)?;
            if mode == 0o160000 {
                bail!("Git links cannot be specified 'inline'");
            }
            (self.repo.write_blob(&payload)?.detach(), path)
        } else {
            let (dataref, rest) = split_space(rest)
                .ok_or_else(|| anyhow!("Missing space after SHA1: M {}", String::from_utf8_lossy(rest)))?;
            (self.dataref(dataref)?, unquote(rest)?)
        };

        // A gitlink is the only thing `--rewrite-submodules-from/-to` would touch,
        // and mapping its object id through the two marks files is not ported, so
        // the run stops here rather than record the un-rewritten id.
        if mode == 0o160000 && !self.submodule_rewrites.is_empty() {
            bail!(
                "unsupported flag \"--rewrite-submodules-{{from,to}}={}\" for a stream that \
                 carries a gitlink (mapping its object id through the submodule marks files \
                 is not ported)",
                self.submodule_rewrites.join(",")
            );
        }

        // The object must actually be what the mode claims it is. A gitlink is
        // exempt from the presence check: the commit it names lives in the
        // submodule's repository, not this one, so git never looks for it here.
        let want = match mode {
            0o40000 => Kind::Tree,
            0o160000 => Kind::Commit,
            _ => Kind::Blob,
        };
        match self.repo.try_find_header(oid)? {
            None if mode == 0o160000 => {}
            None => bail!("{oid} not found"),
            Some(header) if header.kind() != want => bail!(
                "Not a {want} (actually a {}): {}",
                header.kind(),
                String::from_utf8_lossy(&path)
            ),
            Some(_) => {}
        }

        if mode == 0o40000 {
            // git does not track empty directories below the root.
            if oid == ObjectId::empty_tree(self.repo.object_hash()) && !path.is_empty() {
                dir_remove(&mut self.branches[idx].tree, &path);
                return Ok(());
            }
            let sub = load_dir(&self.repo, oid)?;
            if path.is_empty() {
                self.branches[idx].tree = sub;
            } else {
                dir_set(&mut self.branches[idx].tree, &path, Node::Dir(sub));
            }
        } else {
            dir_set(&mut self.branches[idx].tree, &path, Node::Leaf { mode, oid });
        }
        Ok(())
    }

    /// `C SP <src> SP <dst>` and `R SP <src> SP <dst>` (`remove_source` for `R`).
    fn file_copy(&mut self, idx: usize, rest: &[u8], remove_source: bool) -> Result<()> {
        let (src, dst) = split_two_paths(rest)?;
        let node = dir_get(&self.branches[idx].tree, &src)
            .cloned()
            .ok_or_else(|| anyhow!("Path {} not in branch", String::from_utf8_lossy(&src)))?;
        if remove_source {
            dir_remove(&mut self.branches[idx].tree, &src);
        }
        if dst.is_empty() {
            match node {
                Node::Dir(d) => self.branches[idx].tree = d,
                Node::Leaf { .. } => bail!("Path {} is not a tree", String::from_utf8_lossy(&src)),
            }
        } else {
            dir_set(&mut self.branches[idx].tree, &dst, node);
        }
        Ok(())
    }

    /// `N SP <dataref> SP <commit-ish>` or `N SP inline SP <commit-ish>` + data.
    fn note_change(&mut self, input: &mut Input, idx: usize, rest: &[u8]) -> Result<()> {
        let (oid, target) = if let Some(after_inline) = after(rest, b"inline ") {
            let target = after_inline.to_vec();
            let line = input.command()?;
            let Some(spec) = field(&line, b"data ") else {
                bail!("expected 'data n' command");
            };
            let payload = input.data(&spec)?;
            (self.repo.write_blob(&payload)?.detach(), target)
        } else {
            let (dataref, target) =
                split_space(rest).ok_or_else(|| anyhow!("Missing space after SHA1"))?;
            (self.dataref(dataref)?, target.to_vec())
        };
        let commit = self.commitish(&target)?;
        let hex = commit.to_string().into_bytes();

        // Drop any existing note for this commit, wherever its fanout put it.
        for fanout in 0..=19usize {
            let path = note_path(&hex, fanout);
            if dir_get(&self.branches[idx].tree, &path).is_some() {
                let branch = &mut self.branches[idx];
                dir_remove(&mut branch.tree, &path);
                branch.notes = branch.notes.saturating_sub(1);
                break;
            }
        }

        let notes = self.branches[idx].notes + 1;
        if notes > 255 {
            bail!(
                "notes ref would hold {notes} notes, which makes git re-fan-out the whole \
                 notes tree; that rebalancing is not ported"
            );
        }
        self.branches[idx].notes = notes;
        dir_set(
            &mut self.branches[idx].tree,
            &note_path(&hex, 0),
            Node::Leaf { mode: 0o100644, oid },
        );
        Ok(())
    }

    // -- queries ------------------------------------------------------------

    /// `cat-blob <dataref>` → `<oid> SP blob SP <size> LF <contents> LF`.
    fn cat_blob(&mut self, spec: &[u8]) -> Result<()> {
        let oid = self.dataref(spec)?;
        let object = self.repo.find_object(oid)?;
        if object.kind != Kind::Blob {
            bail!("Object {oid} is a {}, not a blob", object.kind);
        }
        let mut out = format!("{oid} blob {}\n", object.data.len()).into_bytes();
        out.extend_from_slice(&object.data);
        out.push(b'\n');
        // `object` borrows `self.repo`; release it before taking `&mut self`.
        drop(object);
        self.respond(&out)
    }

    /// `ls [<dataref> SP] <path>` → an `ls-tree` line, or `missing <path>`.
    fn parse_ls(&mut self, rest: &[u8], branch: Option<usize>) -> Result<()> {
        let (root, path) = if rest.first() == Some(&b'"') {
            let idx = branch.ok_or_else(|| anyhow!("Not in a commit: ls"))?;
            (self.branches[idx].tree.clone(), unquote(rest)?)
        } else {
            let (dataref, rest) =
                split_space(rest).ok_or_else(|| anyhow!("Missing space after tree-ish"))?;
            let oid = self.dataref(dataref)?;
            let tree = self.repo.find_object(oid)?.peel_to_tree()?.id;
            (load_dir(&self.repo, tree)?, unquote(rest)?)
        };

        let line = if path.is_empty() {
            let oid = write_dir(&self.repo, &root)?;
            let mut l = format!("040000 tree {oid}\t").into_bytes();
            l.push(b'\n');
            l
        } else {
            match dir_get(&root, &path) {
                None => {
                    let mut l = b"missing ".to_vec();
                    l.extend_from_slice(&path);
                    l.push(b'\n');
                    l
                }
                Some(Node::Leaf { mode, oid }) => {
                    let kind = if *mode == 0o160000 { "commit" } else { "blob" };
                    let mut l = format!("{mode:06o} {kind} {oid}\t").into_bytes();
                    l.extend_from_slice(&path);
                    l.push(b'\n');
                    l
                }
                Some(Node::Dir(d)) => {
                    let oid = write_dir(&self.repo, d)?;
                    let mut l = format!("040000 tree {oid}\t").into_bytes();
                    l.extend_from_slice(&path);
                    l.push(b'\n');
                    l
                }
            }
        };
        self.respond(&line)
    }

    /// Write a query response to `--cat-blob-fd`, or stdout when unset.
    fn respond(&mut self, buf: &[u8]) -> Result<()> {
        match self.opts.cat_blob_fd {
            None => stdout_write(buf),
            Some(fd) => {
                // SAFETY: the caller promised `fd` is open for writing; the
                // `ManuallyDrop` keeps the descriptor open for the next response
                // and for whoever owns it after this process is done with it.
                use std::os::fd::FromRawFd;
                let file = unsafe { std::fs::File::from_raw_fd(fd) };
                let mut file = std::mem::ManuallyDrop::new(file);
                file.write_all(buf)
                    .with_context(|| format!("unable to write to --cat-blob-fd={fd}"))?;
                file.flush()?;
                Ok(())
            }
        }
    }

    // -- resolution ---------------------------------------------------------

    /// The index of the branch called `name`, creating the in-memory entry on
    /// first mention. A newly named branch starts empty even when the ref exists
    /// — git only consults the repository when a `from` command asks it to.
    fn branch(&mut self, name: &str) -> Result<usize> {
        if let Some(idx) = self.by_name.get(name) {
            return Ok(*idx);
        }
        let _: FullName = name
            .try_into()
            .map_err(|_| anyhow!("invalid ref name: {name}"))?;
        let idx = self.branches.len();
        self.branches.push(Branch {
            name: name.to_string(),
            head: None,
            tree: Dir::default(),
            delete: false,
            notes: 0,
        });
        self.by_name.insert(name.to_string(), idx);
        Ok(idx)
    }

    /// Apply a `from <commit-ish>` to branch `idx`: adopt its tip and its tree.
    fn parse_from(&mut self, idx: usize, spec: &[u8]) -> Result<()> {
        let text = utf8(spec, "commit-ish")?;

        // A branch already in the table contributes its in-memory tree directly.
        if let Some(&other) = self.by_name.get(&text) {
            if other == idx {
                bail!("Can't create a branch from itself: {}", self.branches[idx].name);
            }
            let (head, tree, notes) = {
                let src = &self.branches[other];
                (src.head, src.tree.clone(), src.notes)
            };
            let dst = &mut self.branches[idx];
            dst.head = head;
            dst.tree = tree;
            dst.notes = notes;
            dst.delete = false;
            return Ok(());
        }

        let id = self.commitish_allow_null(spec)?;
        if id.is_null() {
            self.branches[idx].head = None;
            self.branches[idx].tree = Dir::default();
            self.branches[idx].notes = 0;
            self.branches[idx].delete = true;
            return Ok(());
        }
        let tree = self.repo.find_object(id)?.peel_to_commit()?.tree_id()?.detach();
        let tree = load_dir(&self.repo, tree)?;
        let notes = count_notes(&tree, &mut Vec::new());
        let dst = &mut self.branches[idx];
        dst.head = Some(id);
        dst.tree = tree;
        dst.notes = notes;
        dst.delete = false;
        Ok(())
    }

    /// Resolve a `<commit-ish>`: a mark, a branch, or any revision expression.
    fn commitish(&self, spec: &[u8]) -> Result<ObjectId> {
        let id = self.commitish_allow_null(spec)?;
        if id.is_null() {
            bail!("invalid null object id in commit-ish");
        }
        Ok(id)
    }

    /// As [`Importer::commitish`], but the all-zero id is a legal answer.
    fn commitish_allow_null(&self, spec: &[u8]) -> Result<ObjectId> {
        if let Some(v) = spec.strip_prefix(b":") {
            return self.mark_ref(v);
        }
        let text = utf8(spec, "commit-ish")?;
        if let Some(&idx) = self.by_name.get(&text) {
            return self.branches[idx]
                .head
                .ok_or_else(|| anyhow!("Branch {text} has no commits"));
        }
        if text.len() == self.repo.object_hash().len_in_hex() {
            if let Ok(id) = ObjectId::from_hex(text.as_bytes()) {
                return Ok(id);
            }
        }
        Ok(self
            .repo
            .rev_parse_single(text.as_str())
            .map_err(|_| anyhow!("Invalid ref name or SHA1 expression: {text}"))?
            .detach())
    }

    /// Resolve an `M`/`N`/`cat-blob`/`ls` dataref: a mark or a full hex id only.
    fn dataref(&self, spec: &[u8]) -> Result<ObjectId> {
        if let Some(v) = spec.strip_prefix(b":") {
            return self.mark_ref(v);
        }
        ObjectId::from_hex(spec)
            .map_err(|_| anyhow!("Invalid dataref: {}", String::from_utf8_lossy(spec)))
    }

    /// Look up `:<idnum>` (the leading colon already stripped).
    fn mark_ref(&self, spec: &[u8]) -> Result<ObjectId> {
        let spec = spec.strip_prefix(b":").unwrap_or(spec);
        let mark = parse_mark(spec)?;
        self.marks
            .get(&mark)
            .copied()
            .ok_or_else(|| anyhow!("mark :{mark} not declared"))
    }

    /// Validate and copy an identity line, keeping git's exact bytes.
    ///
    /// git stores the `<name> SP LT <email> GT SP <when>` text verbatim once the
    /// date validates, so the committer line in the object is the stream's own
    /// bytes; only the date is checked.
    fn ident(&self, raw: &[u8]) -> Result<Vec<u8>> {
        let gt = raw
            .iter()
            .rposition(|&b| b == b'>')
            .ok_or_else(|| anyhow!("Missing > in ident string: {}", String::from_utf8_lossy(raw)))?;
        let date = raw
            .get(gt + 1..)
            .and_then(|d| d.strip_prefix(b" "))
            .ok_or_else(|| anyhow!("Missing space after > in ident string: {}", String::from_utf8_lossy(raw)))?;
        let strict = match self.opts.date_format {
            DateFormat::Raw => true,
            DateFormat::RawPermissive => false,
            other => bail!(
                "unsupported flag \"--date-format={}\" (ported: raw, raw-permissive) — \
                 porting it would mean reimplementing git's date parser, and a near-miss \
                 would silently store the wrong timestamp",
                other.name()
            ),
        };
        if !valid_raw_date(date, strict) {
            bail!(
                "invalid raw date \"{}\" in ident: {}",
                String::from_utf8_lossy(date),
                String::from_utf8_lossy(raw)
            );
        }
        Ok(raw.to_vec())
    }

    // -- marks and refs -----------------------------------------------------

    /// Resolve a marks-file path, honouring `--relative-marks`.
    fn marks_path(&self, path: &str) -> std::path::PathBuf {
        if self.opts.relative_marks {
            self.repo.git_dir().join("info").join("fast-import").join(path)
        } else {
            std::path::PathBuf::from(path)
        }
    }

    /// Load `:<idnum> SP <oid>` lines into the mark table; later files win.
    fn import_marks(&mut self, path: &str, if_exists: bool) -> Result<()> {
        let full = self.marks_path(path);
        let display = full.display().to_string();
        for (mark, id) in read_mark_file(&full, &display, if_exists)? {
            self.marks.insert(mark, id);
        }
        Ok(())
    }

    /// Write out the mark table, ascending by mark number, as git does.
    ///
    /// git's `dump_marks` declines while an `--import-marks` file is named but
    /// unread, so a run that died before loading it does not overwrite the
    /// export with a half-populated table.
    fn export_marks(&self) -> Result<()> {
        if self.opts.import_marks_pending {
            return Ok(());
        }
        let Some(path) = &self.opts.export_marks else {
            return Ok(());
        };
        let full = self.marks_path(path);
        if let Some(parent) = full.parent() {
            std::fs::create_dir_all(parent).ok();
        }
        let mut marks: Vec<_> = self.marks.iter().collect();
        marks.sort_by_key(|(m, _)| **m);
        let mut out = String::new();
        for (mark, id) in marks {
            out.push_str(&format!(":{mark} {id}\n"));
        }
        std::fs::write(&full, out).with_context(|| format!("cannot write {}", full.display()))
    }

    /// Flush every pending ref update and the marks file — `checkpoint`, and the
    /// end of the stream.
    fn checkpoint(&mut self) -> Result<()> {
        for i in 0..self.branches.len() {
            self.update_branch(i)?;
        }
        for (name, id) in std::mem::take(&mut self.tags) {
            let full = format!("refs/tags/{name}");
            let old = self.current(&full)?;
            self.write_ref(&full, id, old)?;
        }
        self.export_marks()
    }

    /// Update one branch ref, applying git's fast-forward guard unless `--force`.
    fn update_branch(&mut self, idx: usize) -> Result<()> {
        let name = self.branches[idx].name.clone();
        let old = self.current(&name)?;

        let Some(new) = self.branches[idx].head else {
            if self.branches[idx].delete && old.is_some() {
                self.repo.edit_reference(RefEdit {
                    change: Change::Delete {
                        expected: PreviousValue::Any,
                        log: RefLog::AndReference,
                    },
                    name: name.as_str().try_into()?,
                    deref: false,
                })?;
                self.branches[idx].delete = false;
            }
            return Ok(());
        };

        if !self.opts.force {
            if let Some(old) = old {
                if old != new && !self.is_ancestor(old, new) {
                    eprintln!("warning: not updating {name} (new tip {new} does not contain {old})");
                    self.failed = true;
                    return Ok(());
                }
            }
        }
        self.write_ref(&name, new, old)
    }

    /// The id `name` currently points at, or `None` when the ref does not exist.
    fn current(&self, name: &str) -> Result<Option<ObjectId>> {
        Ok(match self.repo.try_find_reference(name)? {
            Some(r) => Some(r.into_fully_peeled_id()?.detach()),
            None => None,
        })
    }

    /// Whether `old` is reachable from `new`, i.e. the update loses no commits.
    fn is_ancestor(&self, old: ObjectId, new: ObjectId) -> bool {
        matches!(self.repo.merge_base(old, new), Ok(base) if base.detach() == old)
    }

    /// Point `name` at `new`, with git's `fast-import` reflog message.
    ///
    /// A ref that already holds `new` is left completely alone, so a `checkpoint`
    /// followed by the end-of-stream flush does not append a second reflog entry
    /// for an update that did not happen.
    fn write_ref(&self, name: &str, new: ObjectId, old: Option<ObjectId>) -> Result<()> {
        if old == Some(new) {
            return Ok(());
        }
        self.repo.edit_reference(RefEdit {
            change: Change::Update {
                log: LogChange {
                    mode: RefLog::AndReference,
                    force_create_reflog: false,
                    message: "fast-import".into(),
                },
                expected: match old {
                    Some(id) => PreviousValue::MustExistAndMatch(Target::Object(id)),
                    None => PreviousValue::MustNotExist,
                },
                new: Target::Object(new),
            },
            name: name.try_into()?,
            deref: false,
        })?;
        Ok(())
    }
}

/// Where a cryptographic signature starts inside a tag message, or `None` when
/// the message carries none.
///
/// git's `parse_signature` knows four armor headers, one per signature format it
/// supports, and each has to begin a line. When a message contains more than one
/// the last wins, so a body that merely quotes an armor header keeps everything
/// up to the real signature.
fn signature_offset(message: &[u8]) -> Option<usize> {
    const MARKERS: [&[u8]; 4] = [
        b"-----BEGIN PGP SIGNATURE-----",
        b"-----BEGIN PGP MESSAGE-----",
        b"-----BEGIN SIGNED MESSAGE-----",
        b"-----BEGIN SSH SIGNATURE-----",
    ];
    let starts_line = |at: usize| at == 0 || message[at - 1] == b'\n';
    (0..message.len())
        .rev()
        .find(|&at| {
            starts_line(at) && MARKERS.iter().any(|m| message[at..].starts_with(m))
        })
}

/// The bare `strerror` text of an I/O error. Rust appends ` (os error <n>)` to
/// its `Display`; git's `die_errno` does not, so it is trimmed off here.
fn strerror(e: &std::io::Error) -> String {
    let text = e.to_string();
    match text.find(" (os error ") {
        Some(i) => text[..i].to_string(),
        None => text,
    }
}

/// Read a `:<idnum> SP <oid>` marks file, as git's `read_mark_file` does.
///
/// `display` is the name to put in the error text — git reports the path it was
/// handed, which is the resolved one for `--import-marks` and the raw one for
/// `--rewrite-submodules-from/-to`. With `if_exists`, a missing file is not an
/// error and yields no marks; a corrupt line always is.
fn read_mark_file(
    path: &std::path::Path,
    display: &str,
    if_exists: bool,
) -> Result<Vec<(u64, ObjectId)>> {
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) if if_exists && e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => bail!("cannot read '{display}': {}", strerror(&e)),
    };
    let mut out = Vec::new();
    for line in text.lines() {
        if line.is_empty() {
            continue;
        }
        let corrupt = || anyhow!("corrupt mark line: {line}");
        let rest = line.strip_prefix(':').ok_or_else(|| corrupt())?;
        let (mark, hex) = rest.split_once(' ').ok_or_else(|| corrupt())?;
        let mark: u64 = mark.parse().map_err(|_| corrupt())?;
        let id = ObjectId::from_hex(hex.as_bytes()).map_err(|_| corrupt())?;
        out.push((mark, id));
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// Tree model
// ---------------------------------------------------------------------------

/// Read an existing tree object into the in-memory model, recursively.
fn load_dir(repo: &gix::Repository, oid: ObjectId) -> Result<Dir> {
    let tree = repo.find_object(oid)?.peel_to_tree()?;
    let entries: Vec<(u32, Vec<u8>, ObjectId)> = tree
        .decode()?
        .entries
        .iter()
        .map(|e| (mode_of(e.mode.kind()), e.filename.to_vec(), e.oid.to_owned()))
        .collect();

    let mut dir = Dir::default();
    for (mode, name, oid) in entries {
        let node = if mode == 0o40000 {
            Node::Dir(load_dir(repo, oid)?)
        } else {
            Node::Leaf { mode, oid }
        };
        dir.entries.insert(name, node);
    }
    Ok(dir)
}

/// Serialize the model back into tree objects, skipping directories that ended
/// up empty — git never records an empty tree as an entry, which is what makes a
/// delete cascade up through its now-empty parents.
fn write_dir(repo: &gix::Repository, dir: &Dir) -> Result<ObjectId> {
    let mut items: Vec<(&[u8], u32, ObjectId)> = Vec::new();
    for (name, node) in &dir.entries {
        match node {
            Node::Leaf { mode, oid } => items.push((name.as_slice(), *mode, *oid)),
            Node::Dir(sub) => {
                let oid = write_dir(repo, sub)?;
                if oid == ObjectId::empty_tree(repo.object_hash()) {
                    continue;
                }
                items.push((name.as_slice(), 0o40000, oid));
            }
        }
    }
    items.sort_by(|a, b| base_name_compare(a.0, a.1, b.0, b.1));

    let mut buf = Vec::new();
    for (name, mode, oid) in items {
        buf.extend_from_slice(format!("{mode:o} ").as_bytes());
        buf.extend_from_slice(name);
        buf.push(0);
        buf.extend_from_slice(oid.as_slice());
    }
    repo.write_buf(Kind::Tree, &buf).map_err(to_anyhow)
}

/// Find the node at `path`, or `None` when nothing lives there.
fn dir_get<'a>(dir: &'a Dir, path: &[u8]) -> Option<&'a Node> {
    let mut cur = dir;
    let mut parts = path.split(|&b| b == b'/').peekable();
    while let Some(part) = parts.next() {
        let node = cur.entries.get(part)?;
        if parts.peek().is_none() {
            return Some(node);
        }
        match node {
            Node::Dir(sub) => cur = sub,
            Node::Leaf { .. } => return None,
        }
    }
    None
}

/// Place `node` at `path`, creating intermediate directories. A file standing
/// where a directory is needed is replaced by that directory, as git's
/// `tree_content_set` replaces it.
fn dir_set(dir: &mut Dir, path: &[u8], node: Node) {
    match path.iter().position(|&b| b == b'/') {
        None => {
            dir.entries.insert(path.to_vec(), node);
        }
        Some(i) => {
            let slot = dir
                .entries
                .entry(path[..i].to_vec())
                .or_insert_with(|| Node::Dir(Dir::default()));
            if matches!(slot, Node::Leaf { .. }) {
                *slot = Node::Dir(Dir::default());
            }
            if let Node::Dir(sub) = slot {
                dir_set(sub, &path[i + 1..], node);
            }
        }
    }
}

/// Remove whatever is at `path`, then drop parents the removal left empty.
fn dir_remove(dir: &mut Dir, path: &[u8]) -> bool {
    let parts: Vec<&[u8]> = path.split(|&b| b == b'/').collect();
    remove_parts(dir, &parts)
}

/// The recursive half of [`dir_remove`], pruning empty directories on the way out.
fn remove_parts(dir: &mut Dir, parts: &[&[u8]]) -> bool {
    let (first, rest) = parts.split_first().expect("a path always has one component");
    if rest.is_empty() {
        return dir.entries.remove(*first).is_some();
    }
    let removed = match dir.entries.get_mut(*first) {
        Some(Node::Dir(sub)) => {
            let removed = remove_parts(sub, rest);
            (removed, sub.entries.is_empty())
        }
        _ => return false,
    };
    if removed.1 {
        dir.entries.remove(*first);
    }
    removed.0
}

/// Count the note blobs already in a notes tree, so the fanout stays consistent
/// with what an earlier import wrote. A note is a leaf whose path components
/// concatenate to one full object id in hex.
fn count_notes(dir: &Dir, prefix: &mut Vec<u8>) -> u64 {
    let mut total = 0;
    for (name, node) in &dir.entries {
        let before = prefix.len();
        prefix.extend_from_slice(name);
        match node {
            Node::Leaf { .. } => {
                // A note is named after the object it annotates, so its path
                // spells out one full object id (40 hex for SHA-1, 64 for SHA-256).
                if matches!(prefix.len(), 40 | 64) && prefix.iter().all(u8::is_ascii_hexdigit) {
                    total += 1;
                }
            }
            Node::Dir(sub) => total += count_notes(sub, prefix),
        }
        prefix.truncate(before);
    }
    total
}

/// The path a note takes at a given fanout: `fanout` two-character directories
/// followed by the rest of the id.
fn note_path(hex: &[u8], fanout: usize) -> Vec<u8> {
    let mut out = Vec::with_capacity(hex.len() + fanout);
    let mut i = 0;
    for _ in 0..fanout {
        if i + 2 >= hex.len() {
            break;
        }
        out.extend_from_slice(&hex[i..i + 2]);
        out.push(b'/');
        i += 2;
    }
    out.extend_from_slice(&hex[i..]);
    out
}

/// git's `base_name_compare`: unsigned byte order, except that a directory's
/// name compares as though it ended in `/`.
fn base_name_compare(n1: &[u8], m1: u32, n2: &[u8], m2: u32) -> std::cmp::Ordering {
    let common = n1.len().min(n2.len());
    match n1[..common].cmp(&n2[..common]) {
        std::cmp::Ordering::Equal => {}
        other => return other,
    }
    trailing_byte(n1, common, m1).cmp(&trailing_byte(n2, common, m2))
}

/// The byte just past the common prefix: the next real byte, or the implicit
/// terminator (`/` for a tree, NUL otherwise).
fn trailing_byte(name: &[u8], common: usize, mode: u32) -> u8 {
    match name.get(common) {
        Some(&byte) => byte,
        None if mode == 0o40000 => b'/',
        None => 0,
    }
}

/// The canonical mode git stores for each entry kind.
fn mode_of(kind: gix::object::tree::EntryKind) -> u32 {
    use gix::object::tree::EntryKind::*;
    match kind {
        Tree => 0o40000,
        Blob => 0o100644,
        BlobExecutable => 0o100755,
        Link => 0o120000,
        Commit => 0o160000,
    }
}

/// Accept the mode words `fast-import` accepts, canonicalized as it canonicalizes
/// them (`644` and `755` gain their file bits; everything else must already be a
/// mode git can store).
fn canonical_mode(word: &[u8]) -> Option<u32> {
    match word {
        b"644" | b"100644" => Some(0o100644),
        b"755" | b"100755" => Some(0o100755),
        b"120000" => Some(0o120000),
        b"040000" | b"40000" => Some(0o40000),
        b"160000" => Some(0o160000),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Small parsing helpers
// ---------------------------------------------------------------------------

/// The remainder of `line` after `prefix`, or `None` when it does not match.
fn after<'a>(line: &'a [u8], prefix: &[u8]) -> Option<&'a [u8]> {
    line.strip_prefix(prefix)
}

/// The *owned* remainder of a header line that starts with `prefix`.
///
/// Header parsing reads one command, decides whether it is the header it is
/// looking for, and then reads the next one — so the argument has to outlive the
/// line it came from. Returning it owned keeps that reassignment legal.
fn field(line: &Option<Vec<u8>>, prefix: &[u8]) -> Option<Vec<u8>> {
    line.as_deref()
        .and_then(|l| after(l, prefix))
        .map(<[u8]>::to_vec)
}

/// Split at the first space: `(before, after)`.
fn split_space(s: &[u8]) -> Option<(&[u8], &[u8])> {
    let i = s.iter().position(|&b| b == b' ')?;
    Some((&s[..i], &s[i + 1..]))
}

/// Split a `C`/`R` argument into its source and destination paths. The source
/// must be quoted when it contains a space, which is what makes this decidable.
fn split_two_paths(rest: &[u8]) -> Result<(Vec<u8>, Vec<u8>)> {
    if rest.first() == Some(&b'"') {
        let (src, consumed) = unquote_prefix(rest)?;
        let tail = rest
            .get(consumed..)
            .and_then(|t| t.strip_prefix(b" "))
            .ok_or_else(|| anyhow!("Missing space after source: {}", String::from_utf8_lossy(rest)))?;
        Ok((src, unquote(tail)?))
    } else {
        let (src, tail) = split_space(rest)
            .ok_or_else(|| anyhow!("Missing space after source: {}", String::from_utf8_lossy(rest)))?;
        Ok((src.to_vec(), unquote(tail)?))
    }
}

/// Decode a path: C-style unquoting when it is quoted, raw bytes otherwise.
fn unquote(path: &[u8]) -> Result<Vec<u8>> {
    if path.first() != Some(&b'"') {
        return Ok(path.to_vec());
    }
    let (out, consumed) = unquote_prefix(path)?;
    if consumed != path.len() {
        bail!("Garbage after path: {}", String::from_utf8_lossy(path));
    }
    Ok(out)
}

/// Unquote a leading `"…"`, returning the bytes and how much of the input the
/// quoted string occupied. Implements git's `unquote_c_style` escape set.
fn unquote_prefix(input: &[u8]) -> Result<(Vec<u8>, usize)> {
    let mut out = Vec::new();
    let mut i = 1; // past the opening quote
    while i < input.len() {
        match input[i] {
            b'"' => return Ok((out, i + 1)),
            b'\\' => {
                i += 1;
                let c = *input
                    .get(i)
                    .ok_or_else(|| anyhow!("Invalid quoting: {}", String::from_utf8_lossy(input)))?;
                i += 1;
                match c {
                    b'a' => out.push(0x07),
                    b'b' => out.push(0x08),
                    b'f' => out.push(0x0c),
                    b'n' => out.push(b'\n'),
                    b'r' => out.push(b'\r'),
                    b't' => out.push(b'\t'),
                    b'v' => out.push(0x0b),
                    b'"' | b'\\' => out.push(c),
                    b'0'..=b'7' => {
                        // Up to three octal digits, counting the one just read.
                        let mut value = u32::from(c - b'0');
                        for _ in 0..2 {
                            match input.get(i) {
                                Some(&d @ b'0'..=b'7') => {
                                    value = value * 8 + u32::from(d - b'0');
                                    i += 1;
                                }
                                _ => break,
                            }
                        }
                        out.push(value as u8);
                    }
                    _ => bail!("Invalid quoting: {}", String::from_utf8_lossy(input)),
                }
            }
            b => {
                out.push(b);
                i += 1;
            }
        }
    }
    bail!("Invalid quoting: {}", String::from_utf8_lossy(input))
}

/// `:<idnum>` where the number is a positive decimal integer.
fn parse_mark(spec: &[u8]) -> Result<u64> {
    let text = std::str::from_utf8(spec)
        .map_err(|_| anyhow!("invalid mark: {}", String::from_utf8_lossy(spec)))?;
    let text = text.trim_end();
    text.parse::<u64>()
        .ok()
        .filter(|m| *m > 0)
        .ok_or_else(|| anyhow!("invalid mark: {text}"))
}

/// git's `validate_raw_date`: `<seconds> SP [+-]<digits>`, with the offset
/// capped at 1400 and, under the strict (non-permissive) format, its minutes
/// part below 60.
fn valid_raw_date(date: &[u8], strict: bool) -> bool {
    let Some((secs, tz)) = split_space(date) else {
        return false;
    };
    if secs.is_empty() || !secs.iter().all(u8::is_ascii_digit) {
        return false;
    }
    let Some((&sign, digits)) = tz.split_first() else {
        return false;
    };
    if sign != b'+' && sign != b'-' {
        return false;
    }
    if digits.is_empty() || !digits.iter().all(u8::is_ascii_digit) {
        return false;
    }
    let Ok(offset) = std::str::from_utf8(digits).unwrap_or("x").parse::<u32>() else {
        return false;
    };
    if offset > 1400 {
        return false;
    }
    !(strict && offset % 100 > 59)
}

/// Decode a stream field that git requires to be text (a ref name, a spec).
fn utf8(bytes: &[u8], what: &str) -> Result<String> {
    String::from_utf8(bytes.to_vec()).map_err(|_| anyhow!("invalid UTF-8 in {what}"))
}

/// Write to stdout and flush, so query responses reach a frontend that is
/// waiting on them before we read more of the stream.
fn stdout_write(buf: &[u8]) -> Result<()> {
    let mut out = std::io::stdout().lock();
    out.write_all(buf)?;
    out.flush()?;
    Ok(())
}

/// Object-write errors are boxed and not `Sync`, so give them an owned message.
fn to_anyhow(e: gix::objs::write::Error) -> anyhow::Error {
    anyhow!("unable to write object: {e}")
}

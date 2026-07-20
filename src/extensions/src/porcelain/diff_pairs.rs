//! `git diff-pairs` — compare the content and mode of blob pairs read from stdin.
//!
//! The input is the NUL-terminated raw diff format produced by `git diff-tree -z -r --raw`:
//! `:<omode> <nmode> <ooid> <noid> <status>\0<path>\0` with a second path field for
//! rename/copy statuses. A lone NUL where a record header would start closes a *batch*:
//! the diffs accumulated so far are emitted and a NUL is written to delimit them.
//!
//! Backed entirely by the vendored gitoxide (`src/ported`). The blob diff runs through
//! `gix::diff::blob` exactly as [`super::diff`] does; the file headers are assembled here
//! to match `diff.c`'s `fill_metainfo` ordering (mode lines, similarity/rename lines,
//! `index` line, `---`/`+++`).
//!
//! ### Covered (byte-identical stdout, stderr and exit code against stock git)
//!
//! * patch output (the default), including
//!   - `new file mode` / `deleted file mode` / `old mode`+`new mode`
//!   - `similarity index <n>%` with `rename from`/`rename to` and `copy from`/`copy to`
//!   - the `index <old>..<new>[ <mode>]` line, omitted when both sides hash equal
//!   - `Binary files ... differ`, `\ No newline at end of file`
//!   - `@@ -a,b +c,d @@ <function>` hunk headers, using git's built-in `def_ff`
//!     function-name heuristic (a preceding line starting with a letter, `_` or `$`,
//!     truncated to 80 bytes and right-trimmed)
//!   - type changes (`T`), split into a deletion patch followed by a creation patch
//!   - gitlinks (`160000`), rendered as `Subproject commit <oid>` line diffs
//! * `--raw` (echoes the pair with full object ids), `--name-only`, `--name-status`,
//!   `--numstat`, `-s`/`--no-patch`
//! * `-p`/`-u`/`--patch`, `--patch-with-raw`, `-U<n>`/`--unified[=<n>]`
//! * `--full-index`, `--no-prefix`, `--default-prefix`, `--src-prefix=`, `--dst-prefix=`
//! * `--exit-code`, `--quiet` (implies `-s` and `--exit-code`)
//! * `--abbrev[=<n>]` — accepted and ignored, which is what stock git does here: verified
//!   against git 2.55, `--abbrev=4`, `--abbrev=20` and `--abbrev=40` all leave the `index`
//!   line at the `core.abbrev`-derived width. `core.abbrev` itself *is* honoured.
//! * `-h` (usage on stdout, exit 129); running without `-z` (usage line on stderr, exit 129)
//! * the fatal paths: `invalid raw diff input`, `unable to parse object id: ...`,
//!   `tree objects not supported`, `unable to read <oid>` — all exit 128
//!
//! ### Honest limitations (bailed on with a precise message, never silently ignored)
//!
//! * `--stat`/`--shortstat`/`--dirstat`/`--summary`/`--compact-summary` — the stat column
//!   layout depends on terminal width probing that is not reproduced here.
//! * `--color`, `--color-moved`, `--word-diff`, `--check`, `--binary`, `--line-prefix`,
//!   `--output`, `--output-indicator-*`, `--inter-hunk-context`, `-R`, `-a`/`--text`,
//!   `-W`/`--function-context`, whitespace-ignoring options (`-w`, `-b`, `-I`, ...),
//!   `--diff-algorithm`/`--patience`/`--histogram`/`--minimal`/`--anchored`, the pickaxe
//!   (`-S`/`-G`/`--find-object`), `--diff-filter`, `--rotate-to`/`--skip-to`/`-O`,
//!   `--textconv`/`--ext-diff`, `--submodule`, `--relative`.
//! * The rename/copy options (`-M`, `-C`, `-B`, `-l`, ...) are meaningless for this command
//!   — the pairs arrive pre-computed — and are rejected rather than quietly dropped.
//! * `gitattributes` diff drivers: neither external commands nor custom `funcname`
//!   patterns are applied, so hunk headers use git's built-in heuristic only.

use anyhow::{bail, Result};
use std::io::{Read, Write};
use std::process::ExitCode;

use gix::bstr::{BString, ByteSlice};
use gix::diff::blob::platform::prepare_diff::Operation;
use gix::diff::blob::unified_diff::{ConsumeHunk, ContextSize, DiffLineKind, HunkHeader};
use gix::diff::blob::{diff_with_slider_heuristics, InternedInput, ResourceKind, UnifiedDiff};
use gix::hash::ObjectId;
use gix::objs::tree::EntryKind;
use gix::prelude::ObjectIdExt;

/// Stock git's `diff-pairs` usage block, byte-for-byte (7320 bytes) including the
/// trailing blank line. Printed on `-h` (stdout, exit 129).
const USAGE: &str = r#"usage: git diff-pairs -z [<diff-options>]

Diff output format options
    -p, --patch           generate patch
    -s, --no-patch        suppress diff output
    -u                    generate patch
    -U, --unified[=<n>]   generate diffs with <n> lines context
    -W, --[no-]function-context
                          generate diffs with <n> lines context
    --raw                 generate the diff in raw format
    --patch-with-raw      synonym for '-p --raw'
    --patch-with-stat     synonym for '-p --stat'
    --numstat             machine friendly --stat
    --shortstat           output only the last line of --stat
    -X, --dirstat[=<param1>,<param2>...]
                          output the distribution of relative amount of changes for each sub-directory
    --cumulative          synonym for --dirstat=cumulative
    --dirstat-by-file[=<param1>,<param2>...]
                          synonym for --dirstat=files,<param1>,<param2>...
    --check               warn if changes introduce conflict markers or whitespace errors
    --summary             condensed summary such as creations, renames and mode changes
    --name-only           show only names of changed files
    --name-status         show only names and status of changed files
    --stat[=<width>[,<name-width>[,<count>]]]
                          generate diffstat
    --stat-width <width>  generate diffstat with a given width
    --stat-name-width <width>
                          generate diffstat with a given name width
    --stat-graph-width <width>
                          generate diffstat with a given graph width
    --stat-count <count>  generate diffstat with limited lines
    --[no-]compact-summary
                          generate compact summary in diffstat
    --binary              output a binary diff that can be applied
    --[no-]full-index     show full pre- and post-image object names on the "index" lines
    --[no-]color[=<when>] show colored diff
    --ws-error-highlight <kind>
                          highlight whitespace errors in the 'context', 'old' or 'new' lines in the diff
    -z                    do not munge pathnames and use NULs as output field terminators in --raw or --numstat
    --[no-]abbrev[=<n>]   use <n> digits to display object names
    --src-prefix <prefix> show the given source prefix instead of "a/"
    --dst-prefix <prefix> show the given destination prefix instead of "b/"
    --line-prefix <prefix>
                          prepend an additional prefix to every line of output
    --no-prefix           do not show any source or destination prefix
    --default-prefix      use default prefixes a/ and b/
    --inter-hunk-context <n>
                          show context between diff hunks up to the specified number of lines
    --output-indicator-new <char>
                          specify the character to indicate a new line instead of '+'
    --output-indicator-old <char>
                          specify the character to indicate an old line instead of '-'
    --output-indicator-context <char>
                          specify the character to indicate a context instead of ' '

Diff rename options
    -B, --break-rewrites[=<n>[/<m>]]
                          break complete rewrite changes into pairs of delete and create
    -M, --find-renames[=<n>]
                          detect renames
    -D, --irreversible-delete
                          omit the preimage for deletes
    -C, --find-copies[=<n>]
                          detect copies
    --[no-]find-copies-harder
                          use unmodified files as source to find copies
    --no-renames          disable rename detection
    --[no-]rename-empty   use empty blobs as rename source
    --[no-]follow         continue listing the history of a file beyond renames
    -l <n>                prevent rename/copy detection if the number of rename/copy targets exceeds given limit

Diff algorithm options
    --minimal             produce the smallest possible diff
    -w, --ignore-all-space
                          ignore whitespace when comparing lines
    -b, --ignore-space-change
                          ignore changes in amount of whitespace
    --ignore-space-at-eol ignore changes in whitespace at EOL
    --ignore-cr-at-eol    ignore carrier-return at the end of line
    --ignore-blank-lines  ignore changes whose lines are all blank
    -I, --[no-]ignore-matching-lines <regex>
                          ignore changes whose all lines match <regex>
    --[no-]indent-heuristic
                          heuristic to shift diff hunk boundaries for easy reading
    --patience            generate diff using the "patience diff" algorithm
    --histogram           generate diff using the "histogram diff" algorithm
    --diff-algorithm <algorithm>
                          choose a diff algorithm
    --anchored <text>     generate diff using the "anchored diff" algorithm
    --word-diff[=<mode>]  show word diff, using <mode> to delimit changed words
    --word-diff-regex <regex>
                          use <regex> to decide what a word is
    --color-words[=<regex>]
                          equivalent to --word-diff=color --word-diff-regex=<regex>
    --[no-]color-moved[=<mode>]
                          moved lines of code are colored differently
    --[no-]color-moved-ws <mode>
                          how white spaces are ignored in --color-moved

Other diff options
    --[no-]relative[=<prefix>]
                          when run from subdir, exclude changes outside and show relative paths
    -a, --[no-]text       treat all files as text
    -R                    swap two inputs, reverse the diff
    --[no-]exit-code      exit with 1 if there were differences, 0 otherwise
    --[no-]quiet          disable all output of the program
    --[no-]ext-diff       allow an external diff helper to be executed
    --[no-]textconv       run external text conversion filters when comparing binary files
    --ignore-submodules[=<when>]
                          ignore changes to submodules in the diff generation
    --submodule[=<format>]
                          specify how differences in submodules are shown
    --ita-invisible-in-index
                          hide 'git add -N' entries from the index
    --ita-visible-in-index
                          treat 'git add -N' entries as real in the index
    -S <string>           look for differences that change the number of occurrences of the specified string
    -G <regex>            look for differences that change the number of occurrences of the specified regex
    --pickaxe-all         show all changes in the changeset with -S or -G
    --pickaxe-regex       treat <string> in -S as extended POSIX regular expression
    -O <file>             control the order in which files appear in the output
    --rotate-to <path>    show the change in the specified path first
    --skip-to <path>      skip the output to the specified path
    --find-object <object-id>
                          look for differences that change the number of occurrences of the specified object
    --diff-filter [(A|C|D|M|R|T|U|X|B)...[*]]
                          select files by diff type
    --max-depth <depth>   maximum tree depth to recurse
    --output <file>       output to a specific file

"#;

/// git's `S_IFMT` mask: `100644` and `100755` share a type, `120000` and `160000` do not.
const IFMT: u32 = 0o170000;

/// `def_ff`'s scratch-buffer size in `xdiff/xutils.c`; the function name is truncated to it.
const FUNCNAME_MAX: usize = 80;

/// Which output formats are active. git ORs these together and only falls back to a
/// patch when none was requested, which is why they are independent flags here.
#[derive(Default)]
struct Formats {
    patch: bool,
    raw: bool,
    name_only: bool,
    name_status: bool,
    numstat: bool,
    no_output: bool,
}

impl Formats {
    /// Whether any format was requested explicitly (so the patch default does not apply).
    fn requested(&self) -> bool {
        self.patch || self.raw || self.name_only || self.name_status || self.numstat || self.no_output
    }

    /// Whether one of the per-pair "name" formats runs before the patch block.
    fn name_group(&self) -> bool {
        self.raw || self.name_only || self.name_status
    }
}

/// Parsed command-line options for a single `diff-pairs` invocation.
struct Opts {
    formats: Formats,
    ctx: u32,             // -U<n>
    full_index: bool,     // --full-index
    src_prefix: BString,  // --src-prefix / --no-prefix
    dst_prefix: BString,  // --dst-prefix / --no-prefix
    exit_code: bool,      // --exit-code / --quiet
}

/// One raw-format record: a pre-computed file pair.
#[derive(Clone)]
struct Pair {
    old_mode: u32,
    new_mode: u32,
    old_id: ObjectId,
    new_id: ObjectId,
    /// The status token verbatim, e.g. `M`, `A`, `R100`.
    status: BString,
    old_path: BString,
    /// Equal to `old_path` unless the status carries a second path (rename/copy).
    new_path: BString,
}

impl Pair {
    fn kind(&self) -> u8 {
        self.status[0]
    }

    /// The similarity/dissimilarity score encoded in the status token, if any.
    fn score(&self) -> u32 {
        std::str::from_utf8(&self.status[1..])
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(0)
    }

    fn old_valid(&self) -> bool {
        self.old_mode != 0
    }

    fn new_valid(&self) -> bool {
        self.new_mode != 0
    }

    /// git's `DIFF_PAIR_TYPE_CHANGED`: both sides exist but their `S_IFMT` differs.
    fn type_changed(&self) -> bool {
        self.old_valid() && self.new_valid() && (self.old_mode & IFMT) != (self.new_mode & IFMT)
    }
}

/// `git diff-pairs` — see the module documentation for the covered surface.
pub fn diff_pairs(args: &[String]) -> Result<ExitCode> {
    // Dispatch passes the subcommand itself at index 0.
    let args = match args.first().map(String::as_str) {
        Some("diff-pairs") => &args[1..],
        _ => args,
    };

    let mut opts = Opts {
        formats: Formats::default(),
        ctx: 3,
        full_index: false,
        src_prefix: BString::from("a/"),
        dst_prefix: BString::from("b/"),
        exit_code: false,
    };
    let mut nul = false;

    for a in args {
        let s = a.as_str();
        match s {
            "-h" => {
                print!("{USAGE}");
                return Ok(ExitCode::from(129));
            }
            "-z" => nul = true,
            "-p" | "-u" | "--patch" | "-U" => opts.formats.patch = true,
            "-s" | "--no-patch" => opts.formats.no_output = true,
            "--raw" => opts.formats.raw = true,
            "--name-only" => opts.formats.name_only = true,
            "--name-status" => opts.formats.name_status = true,
            "--numstat" => opts.formats.numstat = true,
            "--patch-with-raw" => {
                opts.formats.patch = true;
                opts.formats.raw = true;
            }
            "--full-index" => opts.full_index = true,
            "--no-full-index" => opts.full_index = false,
            "--no-prefix" => {
                opts.src_prefix.clear();
                opts.dst_prefix.clear();
            }
            "--default-prefix" => {
                opts.src_prefix = BString::from("a/");
                opts.dst_prefix = BString::from("b/");
            }
            "--exit-code" => opts.exit_code = true,
            "--no-exit-code" => opts.exit_code = false,
            "--quiet" => {
                opts.exit_code = true;
                opts.formats.no_output = true;
            }
            // Accepted and ignored, matching stock git: `--abbrev=<n>` has no effect on
            // this command's `index` lines (only `core.abbrev` does).
            "--abbrev" | "--no-abbrev" => {}
            _ if s.starts_with("--abbrev=") => {}
            _ if s.starts_with("-U") => {
                opts.ctx = parse_ctx(&s[2..])?;
                opts.formats.patch = true;
            }
            _ if s.starts_with("--unified=") => {
                opts.ctx = parse_ctx(&s["--unified=".len()..])?;
                opts.formats.patch = true;
            }
            "--unified" => opts.formats.patch = true,
            _ if s.starts_with("--src-prefix=") => {
                opts.src_prefix = BString::from(s["--src-prefix=".len()..].as_bytes());
            }
            _ if s.starts_with("--dst-prefix=") => {
                opts.dst_prefix = BString::from(s["--dst-prefix=".len()..].as_bytes());
            }
            _ => bail!(
                "unsupported flag {s:?} (ported: -z, -p/-u/--patch, -s/--no-patch, --raw, \
                 --name-only, --name-status, --numstat, --patch-with-raw, -U<n>/--unified=<n>, \
                 --full-index, --no-prefix, --default-prefix, --src-prefix=, --dst-prefix=, \
                 --exit-code, --quiet, --abbrev[=<n>], -h)"
            ),
        }
    }

    if !nul {
        eprintln!("usage: working without -z is not supported");
        return Ok(ExitCode::from(129));
    }
    if !opts.formats.requested() {
        opts.formats.patch = true;
    }

    let repo = match gix::discover(".") {
        Ok(r) => r,
        Err(_) => {
            eprintln!("fatal: not a git repository (or any of the parent directories): .git");
            return Ok(ExitCode::from(128));
        }
    };

    let mut input = Vec::new();
    std::io::stdin().read_to_end(&mut input)?;

    let hexsz = repo.object_hash().len_in_hex();
    let base_abbrev = base_abbrev(&repo);
    let mut cache = repo.diff_resource_cache_for_tree_diff()?;

    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    let mut any_pair = false;
    let mut batch: Vec<Pair> = Vec::new();
    let mut cursor = 0usize;

    // Records are NUL-terminated fields; a zero-length header field closes a batch.
    while cursor < input.len() {
        let Some(end) = input[cursor..].iter().position(|&b| b == 0) else {
            return Ok(fatal("invalid raw diff input"));
        };
        let header = &input[cursor..cursor + end];
        cursor += end + 1;

        if header.is_empty() {
            match flush(&mut out, &repo, &mut cache, &batch, &opts, base_abbrev)? {
                Ok(()) => {}
                Err(code) => return Ok(code),
            }
            out.write_all(b"\0")?;
            out.flush()?;
            batch.clear();
            continue;
        }

        let pair = match parse_header(header, hexsz) {
            Ok(p) => p,
            Err(msg) => return Ok(fatal(&msg)),
        };
        let (old_path, rest) = match take_field(&input, cursor) {
            Some(v) => v,
            None => return Ok(fatal("invalid raw diff input")),
        };
        cursor = rest;
        let new_path = if matches!(pair.0, b'R' | b'C') {
            let (p, rest) = match take_field(&input, cursor) {
                Some(v) => v,
                None => return Ok(fatal("invalid raw diff input")),
            };
            cursor = rest;
            p
        } else {
            old_path.clone()
        };

        let (_, mut pair) = pair;
        pair.old_path = old_path;
        pair.new_path = new_path;
        any_pair = true;
        batch.push(pair);
    }

    match flush(&mut out, &repo, &mut cache, &batch, &opts, base_abbrev)? {
        Ok(()) => {}
        Err(code) => return Ok(code),
    }
    out.flush()?;

    Ok(if opts.exit_code && any_pair {
        ExitCode::from(1)
    } else {
        ExitCode::SUCCESS
    })
}

/// Report a git-style fatal error and yield its exit code.
fn fatal(msg: &str) -> ExitCode {
    eprintln!("fatal: {msg}");
    ExitCode::from(128)
}

fn parse_ctx(s: &str) -> Result<u32> {
    s.parse::<u32>()
        .map_err(|_| anyhow::anyhow!("invalid context line count {s:?}"))
}

/// Read the NUL-terminated field starting at `at`, returning it and the next offset.
fn take_field(input: &[u8], at: usize) -> Option<(BString, usize)> {
    let end = input.get(at..)?.iter().position(|&b| b == 0)?;
    Some((BString::from(&input[at..at + end]), at + end + 1))
}

/// Parse `:<omode> <nmode> <ooid> <noid> <status>` into a pair with empty path fields.
///
/// The error strings are git's own: a malformed layout is `invalid raw diff input`, a
/// bad object id reports the header remainder starting at the offending id, and a tree
/// entry on either side is rejected outright.
fn parse_header(header: &[u8], hexsz: usize) -> Result<(u8, Pair), String> {
    let invalid = || "invalid raw diff input".to_string();
    if header.first() != Some(&b':') {
        return Err(invalid());
    }
    let body = &header[1..];
    // Fixed layout: 6 + 1 + 6 + 1 + hexsz + 1 + hexsz + 1 + status.
    let mode_end = 6;
    if body.len() < 6 + 1 + 6 + 1 {
        return Err(invalid());
    }
    let old_mode = parse_mode(&body[..mode_end]).ok_or_else(invalid)?;
    if body[6] != b' ' {
        return Err(invalid());
    }
    let new_mode = parse_mode(&body[7..13]).ok_or_else(invalid)?;
    if body[13] != b' ' {
        return Err(invalid());
    }

    let oid_at = 14;
    let old_id = parse_oid(body, oid_at, hexsz)?;
    let new_at = oid_at + hexsz + 1;
    let new_id = parse_oid(body, new_at, hexsz)?;

    let status_at = new_at + hexsz + 1;
    if status_at >= body.len() {
        return Err(invalid());
    }
    let status = BString::from(&body[status_at..]);
    if !status[0].is_ascii_uppercase() {
        return Err(invalid());
    }

    if old_mode & IFMT == 0o040000 || new_mode & IFMT == 0o040000 {
        return Err("tree objects not supported".to_string());
    }

    let kind = status[0];
    Ok((
        kind,
        Pair {
            old_mode,
            new_mode,
            old_id,
            new_id,
            status,
            old_path: BString::default(),
            new_path: BString::default(),
        },
    ))
}

fn parse_mode(field: &[u8]) -> Option<u32> {
    let s = std::str::from_utf8(field).ok()?;
    u32::from_str_radix(s, 8).ok()
}

/// Parse the full-length hex id at `at`, followed by a single space.
///
/// git rejects abbreviated ids here; its message quotes the header from the failing id
/// to the end, which is what is reproduced.
fn parse_oid(body: &[u8], at: usize, hexsz: usize) -> Result<ObjectId, String> {
    let fail = || {
        format!(
            "unable to parse object id: {}",
            body.get(at..).unwrap_or_default().as_bstr()
        )
    };
    let end = at + hexsz;
    if body.len() <= end || body[end] != b' ' {
        return Err(fail());
    }
    ObjectId::from_hex(&body[at..end]).map_err(|_| fail())
}

/// Render one batch of pairs in git's format order: the name group, then the stat group,
/// then the patch, with a NUL separator between groups that both produced output.
///
/// The inner `Result` carries a git fatal exit code (e.g. an unreadable blob) so the
/// caller can stop after the bytes already written.
#[allow(clippy::type_complexity)]
fn flush(
    out: &mut impl Write,
    repo: &gix::Repository,
    cache: &mut gix::diff::blob::Platform,
    batch: &[Pair],
    opts: &Opts,
    base_abbrev: usize,
) -> Result<std::result::Result<(), ExitCode>> {
    if batch.is_empty() || opts.formats.no_output {
        return Ok(Ok(()));
    }
    let mut buf: Vec<u8> = Vec::new();

    if opts.formats.name_group() {
        for p in batch {
            render_name(&mut buf, p, &opts.formats);
        }
    }
    if opts.formats.numstat {
        if !buf.is_empty() {
            buf.push(b'\0');
        }
        for p in batch {
            match numstat(repo, cache, p) {
                Ok((add, del)) => render_numstat(&mut buf, p, add, del),
                Err(code) => {
                    out.write_all(&buf)?;
                    return Ok(Err(code));
                }
            }
        }
    }
    if opts.formats.patch {
        if !buf.is_empty() {
            buf.push(b'\0');
        }
        for p in batch {
            // A type change becomes a deletion patch followed by a creation patch,
            // exactly as `run_diff()` splits it in diff.c.
            let steps: Vec<Pair> = if p.type_changed() {
                vec![as_deletion(p), as_creation(p)]
            } else {
                vec![p.clone()]
            };
            for step in &steps {
                if let Err(code) = render_patch(&mut buf, repo, cache, step, opts, base_abbrev) {
                    out.write_all(&buf)?;
                    return Ok(Err(code));
                }
            }
        }
    }

    out.write_all(&buf)?;
    Ok(Ok(()))
}

/// The "delete the old side" half of a type-change patch.
fn as_deletion(p: &Pair) -> Pair {
    Pair {
        old_mode: p.old_mode,
        new_mode: 0,
        old_id: p.old_id,
        new_id: ObjectId::null(p.old_id.kind()),
        status: BString::from("D"),
        old_path: p.old_path.clone(),
        new_path: p.old_path.clone(),
    }
}

/// The "create the new side" half of a type-change patch.
fn as_creation(p: &Pair) -> Pair {
    Pair {
        old_mode: 0,
        new_mode: p.new_mode,
        old_id: ObjectId::null(p.new_id.kind()),
        new_id: p.new_id,
        status: BString::from("A"),
        old_path: p.new_path.clone(),
        new_path: p.new_path.clone(),
    }
}

/// `--raw` / `--name-only` / `--name-status` for one pair (`--name-status` wins when
/// several are set, matching `flush_one_pair`'s precedence).
fn render_name(out: &mut Vec<u8>, p: &Pair, f: &Formats) {
    let two_paths = matches!(p.kind(), b'R' | b'C');
    if f.name_status {
        out.extend_from_slice(&p.status);
        out.push(b'\0');
    } else if f.name_only {
        out.extend_from_slice(&p.new_path);
        out.push(b'\0');
        return;
    } else {
        out.extend_from_slice(
            format!(
                ":{:06o} {:06o} {} {} ",
                p.old_mode,
                p.new_mode,
                p.old_id.to_hex(),
                p.new_id.to_hex()
            )
            .as_bytes(),
        );
        out.extend_from_slice(&p.status);
        out.push(b'\0');
    }
    out.extend_from_slice(&p.old_path);
    out.push(b'\0');
    if two_paths {
        out.extend_from_slice(&p.new_path);
        out.push(b'\0');
    }
}

/// `--numstat` for one pair; `None` counts render as `-` for binary content.
fn render_numstat(out: &mut Vec<u8>, p: &Pair, add: Option<u32>, del: Option<u32>) {
    let fmt = |n: Option<u32>| n.map_or_else(|| "-".to_string(), |v| v.to_string());
    out.extend_from_slice(format!("{}\t{}\t", fmt(add), fmt(del)).as_bytes());
    if matches!(p.kind(), b'R' | b'C') {
        out.push(b'\0');
        out.extend_from_slice(&p.old_path);
        out.push(b'\0');
        out.extend_from_slice(&p.new_path);
    } else {
        out.extend_from_slice(&p.new_path);
    }
    out.push(b'\0');
}

/// The added/removed line counts for a pair, or `(None, None)` for binary content.
#[allow(clippy::type_complexity)]
fn numstat(
    repo: &gix::Repository,
    cache: &mut gix::diff::blob::Platform,
    p: &Pair,
) -> std::result::Result<(Option<u32>, Option<u32>), ExitCode> {
    if is_gitlink(p) {
        let (add, del) = gitlink_counts(p);
        return Ok((Some(add), Some(del)));
    }
    check_readable(repo, p)?;
    let body = blob_body(repo, cache, p, None).map_err(|_| fatal("unable to diff blob pair"))?;
    Ok(match body {
        BlobBody::Binary => (None, None),
        BlobBody::Counts(add, del, _) => (Some(add), Some(del)),
    })
}

/// A pair whose surviving side is a submodule link; those never touch the object database.
fn is_gitlink(p: &Pair) -> bool {
    (p.old_valid() && p.old_mode & IFMT == 0o160000) || (p.new_valid() && p.new_mode & IFMT == 0o160000)
}

fn gitlink_counts(p: &Pair) -> (u32, u32) {
    (u32::from(p.new_valid()), u32::from(p.old_valid()))
}

/// Verify both non-null sides are present in the object database, as git does before
/// producing any patch body.
fn check_readable(repo: &gix::Repository, p: &Pair) -> std::result::Result<(), ExitCode> {
    for id in [p.old_id, p.new_id] {
        if id.is_null() {
            continue;
        }
        match repo.try_find_header(id) {
            Ok(Some(_)) => {}
            _ => return Err(fatal(&format!("unable to read {}", id.to_hex()))),
        }
    }
    Ok(())
}

/// The outcome of diffing the two blob contents of a pair.
enum BlobBody {
    Binary,
    /// Added lines, removed lines, and the rendered hunks (empty when identical).
    Counts(u32, u32, Vec<u8>),
}

/// Diff the pair's two blobs through the gitoxide blob platform.
///
/// `ctx` is `None` when only the line counts are wanted, in which case no hunk text is
/// produced.
fn blob_body(
    repo: &gix::Repository,
    cache: &mut gix::diff::blob::Platform,
    p: &Pair,
    ctx: Option<u32>,
) -> Result<BlobBody> {
    let old_kind = mode_kind(if p.old_valid() { p.old_mode } else { p.new_mode });
    let new_kind = mode_kind(if p.new_valid() { p.new_mode } else { p.old_mode });
    cache.set_resource(
        p.old_id,
        old_kind,
        p.old_path.as_bstr(),
        ResourceKind::OldOrSource,
        &repo.objects,
    )?;
    cache.set_resource(
        p.new_id,
        new_kind,
        p.new_path.as_bstr(),
        ResourceKind::NewOrDestination,
        &repo.objects,
    )?;

    let prep = cache.prepare_diff()?;
    match prep.operation {
        Operation::SourceOrDestinationIsBinary => Ok(BlobBody::Binary),
        Operation::ExternalCommand { .. } => {
            bail!("external diff drivers are not supported for {:?}", p.new_path)
        }
        Operation::InternalDiff { algorithm } => {
            let input = InternedInput::new(prep.old.intern_source(), prep.new.intern_source());
            let diff = diff_with_slider_heuristics(algorithm, &input);
            let (add, del) = (diff.count_additions(), diff.count_removals());
            let hunks = match ctx {
                None => Vec::new(),
                Some(ctx) => {
                    let old_lines: Vec<&[u8]> =
                        input.before.iter().map(|t| input.interner[*t]).collect();
                    let sink = PatchSink {
                        buf: Vec::new(),
                        old_lines,
                        func_prev: -1,
                    };
                    UnifiedDiff::new(&diff, &input, sink, ContextSize::symmetrical(ctx)).consume()?
                }
            };
            Ok(BlobBody::Counts(add, del, hunks))
        }
    }
}

/// Render one pair as a `diff --git` file section.
fn render_patch(
    out: &mut Vec<u8>,
    repo: &gix::Repository,
    cache: &mut gix::diff::blob::Platform,
    p: &Pair,
    opts: &Opts,
    base_abbrev: usize,
) -> std::result::Result<(), ExitCode> {
    let kind = p.kind();
    let renamed = matches!(kind, b'R' | b'C');

    // Compute the body first: git fills in and validates both blobs before emitting any
    // header line, so an unreadable object must leave this pair's output empty.
    let body = if is_gitlink(p) {
        gitlink_body(p)
    } else {
        check_readable(repo, p)?;
        match blob_body(repo, cache, p, Some(opts.ctx)) {
            Ok(b) => b,
            Err(_) => return Err(fatal("unable to diff blob pair")),
        }
    };

    // `diff --git <src><old> <dst><new>`; absent sides reuse the surviving path.
    let old_disp = if p.old_valid() || renamed {
        p.old_path.clone()
    } else {
        p.new_path.clone()
    };
    let new_disp = p.new_path.clone();

    out.extend_from_slice(b"diff --git ");
    out.extend_from_slice(&opts.src_prefix);
    out.extend_from_slice(&old_disp);
    out.push(b' ');
    out.extend_from_slice(&opts.dst_prefix);
    out.extend_from_slice(&new_disp);
    out.push(b'\n');

    // Mode lines come first, then the rename/copy block, then `index`.
    if !p.old_valid() {
        out.extend_from_slice(format!("new file mode {:06o}\n", p.new_mode).as_bytes());
    } else if !p.new_valid() {
        out.extend_from_slice(format!("deleted file mode {:06o}\n", p.old_mode).as_bytes());
    } else if p.old_mode != p.new_mode {
        out.extend_from_slice(
            format!("old mode {:06o}\nnew mode {:06o}\n", p.old_mode, p.new_mode).as_bytes(),
        );
    }

    if renamed {
        let verb = if kind == b'C' { "copy" } else { "rename" };
        out.extend_from_slice(format!("similarity index {}%\n", p.score()).as_bytes());
        out.extend_from_slice(format!("{verb} from ").as_bytes());
        out.extend_from_slice(&p.old_path);
        out.push(b'\n');
        out.extend_from_slice(format!("{verb} to ").as_bytes());
        out.extend_from_slice(&p.new_path);
        out.push(b'\n');
    }

    // The `index` line appears only when the two sides hash differently.
    if p.old_id != p.new_id {
        out.extend_from_slice(b"index ");
        out.extend_from_slice(oid_text(repo, &p.old_id, base_abbrev, opts.full_index).as_bytes());
        out.extend_from_slice(b"..");
        out.extend_from_slice(oid_text(repo, &p.new_id, base_abbrev, opts.full_index).as_bytes());
        if p.old_valid() && p.new_valid() && p.old_mode == p.new_mode {
            out.extend_from_slice(format!(" {:06o}", p.new_mode).as_bytes());
        }
        out.push(b'\n');
    }

    let old_label = if p.old_valid() {
        let mut s = opts.src_prefix.to_vec();
        s.extend_from_slice(&old_disp);
        s
    } else {
        b"/dev/null".to_vec()
    };
    let new_label = if p.new_valid() {
        let mut s = opts.dst_prefix.to_vec();
        s.extend_from_slice(&new_disp);
        s
    } else {
        b"/dev/null".to_vec()
    };

    match body {
        BlobBody::Binary => {
            out.extend_from_slice(b"Binary files ");
            out.extend_from_slice(&old_label);
            out.extend_from_slice(b" and ");
            out.extend_from_slice(&new_label);
            out.extend_from_slice(b" differ\n");
        }
        BlobBody::Counts(_, _, hunks) if !hunks.is_empty() => {
            out.extend_from_slice(b"--- ");
            out.extend_from_slice(&old_label);
            out.push(b'\n');
            out.extend_from_slice(b"+++ ");
            out.extend_from_slice(&new_label);
            out.push(b'\n');
            out.extend_from_slice(&hunks);
        }
        // Identical content (a pure mode change, or a 100% rename): headers only.
        BlobBody::Counts(..) => {}
    }
    Ok(())
}

/// The `Subproject commit <oid>` pseudo-diff git emits for `160000` entries.
fn gitlink_body(p: &Pair) -> BlobBody {
    let line = |id: &ObjectId| format!("Subproject commit {}\n", id.to_hex());
    let mut hunks = Vec::new();
    match (p.old_valid(), p.new_valid()) {
        (true, true) => {
            hunks.extend_from_slice(b"@@ -1 +1 @@\n");
            hunks.extend_from_slice(format!("-{}", line(&p.old_id)).as_bytes());
            hunks.extend_from_slice(format!("+{}", line(&p.new_id)).as_bytes());
        }
        (false, true) => {
            hunks.extend_from_slice(b"@@ -0,0 +1 @@\n");
            hunks.extend_from_slice(format!("+{}", line(&p.new_id)).as_bytes());
        }
        (true, false) => {
            hunks.extend_from_slice(b"@@ -1 +0,0 @@\n");
            hunks.extend_from_slice(format!("-{}", line(&p.old_id)).as_bytes());
        }
        (false, false) => {}
    }
    let (add, del) = gitlink_counts(p);
    BlobBody::Counts(add, del, hunks)
}

fn mode_kind(mode: u32) -> EntryKind {
    match mode & IFMT {
        0o120000 => EntryKind::Link,
        0o160000 => EntryKind::Commit,
        _ if mode & 0o111 != 0 => EntryKind::BlobExecutable,
        _ => EntryKind::Blob,
    }
}

/// The object id as it appears on an `index` line.
///
/// A null id (an addition or deletion) is rendered as zeros at the base width; a real id
/// is disambiguated upwards from that width the way `repo_find_unique_abbrev` does. An id
/// that is not in the object database at all — a submodule commit, for instance — is
/// simply truncated, which is what git falls back to as well.
fn oid_text(repo: &gix::Repository, id: &ObjectId, base: usize, full: bool) -> String {
    if full {
        id.to_hex().to_string()
    } else if id.is_null() {
        "0".repeat(base)
    } else {
        match id.attach(repo).shorten() {
            Ok(prefix) => prefix.to_string(),
            Err(_) => id.to_hex_with_len(base).to_string(),
        }
    }
}

/// git's default `index`-line width: `core.abbrev` when set, else derived from the
/// packed object count with a floor of 7.
fn base_abbrev(repo: &gix::Repository) -> usize {
    let hexsz = repo.object_hash().len_in_hex();
    let snapshot = repo.config_snapshot();
    match snapshot.string("core.abbrev") {
        Some(v) => {
            let v = v.to_str_lossy().into_owned();
            match v.as_str() {
                "auto" => auto_abbrev(repo),
                "no" | "false" | "off" => hexsz,
                _ => v
                    .parse::<usize>()
                    .map(|n| n.clamp(4, hexsz))
                    .unwrap_or_else(|_| auto_abbrev(repo)),
            }
        }
        None => auto_abbrev(repo),
    }
}

/// `calculate_auto_hex_len` from `gix::Id::shorten`, kept in sync so the zeros written
/// for a null id are as wide as the abbreviations gitoxide produces for real ids.
fn auto_abbrev(repo: &gix::Repository) -> usize {
    let count = repo.objects.packed_object_count().unwrap_or(0);
    (64 - count.leading_zeros()).div_ceil(2).max(7) as usize
}

/// One side of a hunk header (`@@ -<here> +<here> @@`): the length is omitted when it is
/// 1, and a zero length reports the preceding line number, exactly like `git diff`.
fn fmt_range(start: u32, len: u32) -> String {
    match len {
        1 => format!("{start}"),
        0 => format!("{},0", start.saturating_sub(1)),
        _ => format!("{start},{len}"),
    }
}

/// git's built-in `def_ff` heuristic: a record qualifies as a function line when it
/// starts with a letter, `_` or `$`; the text is capped at 80 bytes and right-trimmed.
fn def_ff(rec: &[u8]) -> Option<&[u8]> {
    let first = *rec.first()?;
    if !(first.is_ascii_alphabetic() || first == b'_' || first == b'$') {
        return None;
    }
    let mut len = rec.len().min(FUNCNAME_MAX);
    while len > 0 && rec[len - 1].is_ascii_whitespace() {
        len -= 1;
    }
    Some(&rec[..len])
}

/// A [`ConsumeHunk`] sink rendering unified-diff hunks with git's hunk-header function
/// context and `\ No newline at end of file` markers.
struct PatchSink<'a> {
    buf: Vec<u8>,
    /// The pre-image lines, used to search backwards for the enclosing function line.
    old_lines: Vec<&'a [u8]>,
    /// `funclineprev` from `xdl_emit_diff`: the previous hunk's search origin, which
    /// bounds how far back the next hunk may look.
    func_prev: i64,
}

impl<'a> PatchSink<'a> {
    /// `get_func_line`: scan the pre-image backwards from the line above the hunk down to
    /// (but not including) the previous hunk's origin.
    fn func_name(&mut self, before_hunk_start: u32) -> Option<&'a [u8]> {
        let origin = i64::from(before_hunk_start) - 2; // 0-based line above the hunk
        let limit = self.func_prev;
        self.func_prev = origin;

        let mut l = origin;
        let mut found = None;
        while l != limit && l >= 0 && (l as usize) < self.old_lines.len() {
            if def_ff(self.old_lines[l as usize]).is_some() {
                found = Some(l as usize);
                break;
            }
            l -= 1;
        }
        found.and_then(|i| def_ff(self.old_lines[i]))
    }
}

impl ConsumeHunk for PatchSink<'_> {
    type Out = Vec<u8>;

    fn consume_hunk(
        &mut self,
        header: HunkHeader,
        lines: &[(DiffLineKind, &[u8])],
    ) -> std::io::Result<()> {
        let mut head = Vec::new();
        head.extend_from_slice(b"@@ -");
        head.extend_from_slice(fmt_range(header.before_hunk_start, header.before_hunk_len).as_bytes());
        head.extend_from_slice(b" +");
        head.extend_from_slice(fmt_range(header.after_hunk_start, header.after_hunk_len).as_bytes());
        head.extend_from_slice(b" @@");
        if let Some(func) = self.func_name(header.before_hunk_start) {
            if !func.is_empty() {
                head.push(b' ');
                head.extend_from_slice(func);
            }
        }
        head.push(b'\n');
        self.buf.extend_from_slice(&head);

        for (kind, content) in lines {
            self.buf.push(match kind {
                DiffLineKind::Context => b' ',
                DiffLineKind::Add => b'+',
                DiffLineKind::Remove => b'-',
            });
            self.buf.extend_from_slice(content);
            // Tokens keep their line terminator; a token without one is the last line of
            // a file that lacks a trailing newline.
            if content.last() != Some(&b'\n') {
                self.buf.push(b'\n');
                self.buf.extend_from_slice(b"\\ No newline at end of file\n");
            }
        }
        Ok(())
    }

    fn finish(self) -> Vec<u8> {
        self.buf
    }
}

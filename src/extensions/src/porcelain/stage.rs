//! `git stage` — stage worktree paths into the index, served natively via the
//! vendored gitoxide crates so tools on PATH see the same staged index.
//!
//! Stock git implements `stage` as an alias entry in its command table pointing
//! at the very same `cmd_add` C function (`git-stage(1)`: "This is a synonym for
//! git-add(1)"), so the semantics below are `git add`'s semantics.
//!
//! Supported forms:
//! ```text
//!   * `git stage <pathspec>...`   — stage files/dirs (recurses, honors `.gitignore`)
//!   * `-A`/`--all`/`--no-ignore-removal`   — adds, modifications *and* deletions
//!   * `--no-all`/`--ignore-removal`        — adds and modifications, no deletions
//!   * `-u`/`--update` / `--no-update`      — restage tracked paths only
//!   * `-N`/`--intent-to-add`               — record untracked paths as empty blobs
//!   * `--refresh`                          — refresh stat info only, stage nothing
//!   * `--chmod=+x|-x`                      — force the index mode of matched paths
//!   * `--ignore-errors`                    — skip unreadable files, exit 1
//!   * `--ignore-missing` (with `--dry-run`) — non-matching pathspecs are not fatal
//!   * `--pathspec-from-file=<file>` / `--pathspec-file-nul` — read pathspecs from
//!     `<file>` (or stdin for `-`), NUL- or newline-separated, C-quoting honored
//!   * `-n/--dry-run`, `-v/--verbose`, `-f/--force`, `--sparse/--no-sparse`, `--`
//!   * `--warn-embedded-repo`/`--no-warn-embedded-repo` — accepted no-op: the
//!     embedded-repo warning is moot since gitlinks are never staged here
//! ```
//!
//! Deviations (bailed or noted, never faked):
//! ```text
//!   * `.gitattributes` content filters (`text`/`eol`/`core.autocrlf`,
//!     `working-tree-encoding`, `ident`, `clean` drivers) run on the way in, so a
//!     staged blob is byte-identical to the one `git add` writes — `cmd_stage()`
//!     *is* `cmd_add()`. `--renormalize` additionally drops the `text=auto` guard
//!     that normally leaves a file alone when the indexed blob already holds CRLF.
//!   * submodule gitlinks are skipped here (use `git zbump`).
//!   * `-e`/`--edit` is rejected with a precise message rather than silently
//!     ignored (it requires an interactive editor this port does not drive).
//!     `-p`/`--patch` and `-i`/`--interactive` are served: they hand the argv to
//!     [`add`](super::add), the same `cmd_add()` git dispatches `stage` to.
//!   * `-U/--unified`, `--inter-hunk-context`, `--[no-]auto-advance` only configure
//!     the interactive/patch diff: their values are magnitude-validated exactly as
//!     git's `OPT_MAGNITUDE` (bad value ⇒ exit 129), then — when neither interactive
//!     mode was asked for — git's
//!     `fatal: the option '<x>' requires '--interactive/--patch'` (exit 128) is
//!     reproduced. A bare `--auto-advance` is the default and stages normally; only
//!     `--no-auto-advance` triggers the fatal.
//! ```
//!
//! NOTE: the parts of `cmd_add()` whose behaviour is observable *per index entry*
//! are shared with [`add`](super::add) rather than reimplemented here — the
//! `--renormalize` cache scan ([`super::add::renormalize_tracked_files`]), the
//! sparse-checkout definition lookup ([`super::add::sparsity_to_consult`]) and its
//! guard ([`super::add::skipped_as_sparse`]), the `--chmod` cache pass
//! ([`super::add::apply_chmod_pathspec`]), the pathspec prefix pass
//! ([`super::add::resolve_pathspecs`]), the exit-code/report tail
//! ([`super::add::finish_code`]), the submodule gitlink pass
//! ([`super::add::moved_gitlinks`]), and the option table itself
//! ([`super::add::LONG_OPTS`]). Keeping two copies of the cache scan is what
//! previously let this verb miss the `ce_skip_worktree()` guard and the `-N`
//! empty-blob write that `add` already had.
//!
//! What is still this module's own is the worktree walk and the pathspec-accounting
//! built on it (`mark_seen_per_spec`, `unmatched_pathspec_exit`, the `--refresh`
//! porcelain report). The walk reproduces `run_diff_files(DIFF_RACY_IS_MODIFIED)`'s
//! selection rule for tracked paths — a stat mismatch under [`stat_match`], or the
//! racily-clean window [`racy_paths`] describes — because that rule decides three
//! observable things at once: which paths are hashed (and so which can raise the
//! `core.autocrlf` round-trip warning), which are reported, and which reach
//! `add_to_index()` under `-N` and pull the empty blob into the object database.
//! [`super::add`] applies the same rule off the same two helpers.

use anyhow::{anyhow, bail, Result};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::process::ExitCode;

use gix::bstr::{BStr, BString, ByteSlice};
use gix::index::entry::{Flags, Mode, Stage, Stat};

/// Exit code git uses for a fatal error.
const FATAL: u8 = 128;
/// Exit code git uses for a usage error (bad/missing option value).
const USAGE: u8 = 129;

// ---------------------------------------------------------------------------
// options
// ---------------------------------------------------------------------------

#[derive(Default)]
struct Opts {
    dry_run: bool,
    verbose: bool,
    force: bool,
    /// `-A`/`--all` is `Some(true)`, `--no-all`/`--ignore-removal` is `Some(false)`.
    /// `None` is git's default, which stages deletions for the paths it matched.
    addremove: Option<bool>,
    update: bool,
    intent_to_add: bool,
    refresh: bool,
    renormalize: bool,
    ignore_errors: bool,
    ignore_missing: bool,
    sparse: Option<bool>,
    /// The raw value of the last `--chmod=<v>` seen. git validates this only
    /// once, on the final occurrence (last-wins), so `--chmod=v1 --chmod=+x`
    /// succeeds — the good value overwrites the bad. Validated in `stage()`.
    chmod_arg: Option<String>,
    /// The validated form of `chmod_arg`: `Some(true)` for `+x`, `Some(false)`
    /// for `-x`. Filled in after option validation, never during parsing.
    chmod: Option<bool>,
    /// `-p`/`--patch` and `-i`/`--interactive` (git's `patch_interactive` and
    /// `add_interactive`). Both are `OPT_BOOL`, so the `--no-` forms clear them.
    /// Either one hands the whole argv to [`add`](super::add), which is the same
    /// `cmd_add()` git dispatches `stage` to and owns both interactive modes.
    patch_interactive: bool,
    add_interactive: bool,
    /// Hand the untouched argv to [`add`](super::add) instead of staging here:
    /// `-h` (print the option table, exit 129) and any option this parser does not
    /// know (git's `unknown option`/`unknown switch` usage error, exit 129). Both
    /// belong to the option table `stage` shares with `add`, so `add` renders them.
    delegate: bool,
    /// `-U`/`--unified` and `--inter-hunk-context` only feed the interactive/patch
    /// diff. Their values are magnitude-validated during parse and then discarded;
    /// only whether the option appeared is kept, to raise git's
    /// `requires '--interactive/--patch'` fatal in `stage()` when neither mode ran.
    unified_seen: bool,
    interhunk_seen: bool,
    /// Final value of `--[no-]auto-advance` (last occurrence wins). `Some(false)`
    /// (turned off) is the only setting that raises that same fatal; the default-on
    /// `--auto-advance` stages normally.
    auto_advance: Option<bool>,
    pathspec_file_nul: bool,
    pathspec_from_file: bool,
    /// The `<file>` argument of `--pathspec-from-file=<file>` (or its separate-arg
    /// form). `-` names stdin. Read in `stage()`, after option validation.
    pathspec_from_file_value: Option<String>,
    /// The pathspec elements as typed, which is what git quotes back in its
    /// diagnostics and what the matcher is handed (gitoxide resolves patterns
    /// against the repository prefix itself).
    pathspecs: Vec<String>,
    /// The same elements made repo-relative by `parse_pathspec()`'s prefix pass —
    /// the only form comparable with index and worktree paths. 1:1 with
    /// [`Opts::pathspecs`], filled in `stage()` once the repository is open.
    resolved: Vec<String>,
}

impl Opts {
    /// Deletions are staged unless `--no-all`/`--ignore-removal` turned them off.
    /// `-u` restages tracked paths, which includes removing the vanished ones.
    fn stages_deletions(&self) -> bool {
        self.addremove.unwrap_or(true)
    }
}

/// Parse the argument vector the way git's `parse_options` does for `cmd_add`:
/// every toggle honours its `--no-` twin and the last occurrence wins.
///
/// The outer `Result` carries hard errors (interactive modes this port cannot
/// serve); the inner `Result` carries a usage exit code (git's 129) for the
/// value-bearing options that were handed no value.
fn parse(args: &[String]) -> Result<std::result::Result<Opts, ExitCode>> {
    let mut o = Opts::default();
    let mut positional_only = false;

    let mut i = 0;
    // `parse_options()` stops at the first switch it cannot serve here, so the
    // delegating arms below leave the scan rather than keep reading argv: git
    // reports the *first* such switch, not the last.
    'argv: while i < args.len() {
        let a = &args[i];
        if positional_only {
            o.pathspecs.push(a.clone());
            i += 1;
            continue;
        }
        // Respell a unique abbreviation as the name it resolves to, ahead of the
        // value-taking pre-match arms below, so `--pathspec-from-f <file>` consumes
        // its separate value the way its full spelling does. The table is
        // [`super::add::LONG_OPTS`] itself rather than a copy: git.c:658 registers
        // `stage` as `cmd_add`, so the two verbs are one option table.
        let canonical;
        let a = match super::canonical_long(a, super::add::LONG_OPTS) {
            super::Long::Name(name) => {
                canonical = name;
                canonical.as_ref()
            }
            // An ambiguous abbreviation is parse-options' refusal, printed with
            // `git add`'s usage block — which is this verb's usage block. Hand the
            // untouched argv to `add` and let the one implementation report it.
            super::Long::Ambiguous(..) => {
                o.delegate = true;
                break 'argv;
            }
        };
        // `--chmod` and `--pathspec-from-file` take a value. git's parse_options
        // accepts both the sticky `--opt=val` and the separate `--opt val` forms,
        // consuming the following argv element for the latter (dying 129 if there
        // is none). Handle the separate form here; the sticky form falls through
        // to the `=`-prefixed arms below.
        if a == "--chmod" || a == "--pathspec-from-file" {
            let name = &a[2..];
            let Some(val) = args.get(i + 1) else {
                eprintln!("error: option `{name}' requires a value");
                return Ok(Err(ExitCode::from(USAGE)));
            };
            if a == "--chmod" {
                o.chmod_arg = Some(val.clone());
            } else {
                o.pathspec_from_file = true;
                o.pathspec_from_file_value = Some(val.clone());
            }
            i += 2;
            continue;
        }
        // `-U`/`--unified` and `--inter-hunk-context` also take a separate value,
        // which git magnitude-validates (bad value ⇒ usage exit 129) and then only
        // consumes under `--patch`/`--interactive`. git always eats the next argv
        // element as the value — so `-U -A` treats `-A` as the value — so consume it
        // unconditionally here, mirroring the `--chmod` handling above.
        if a == "-U" || a == "--unified" || a == "--inter-hunk-context" {
            let short = a == "-U";
            let name = if short {
                "U"
            } else if a == "--unified" {
                "unified"
            } else {
                "inter-hunk-context"
            };
            if let Err(code) = check_magnitude(args.get(i + 1).map(String::as_str), short, name) {
                return Ok(Err(code));
            }
            if a == "--inter-hunk-context" {
                o.interhunk_seen = true;
            } else {
                o.unified_seen = true;
            }
            i += 2;
            continue;
        }
        match a {
            "--" => positional_only = true,

            "-n" | "--dry-run" => o.dry_run = true,
            "--no-dry-run" => o.dry_run = false,
            "-v" | "--verbose" => o.verbose = true,
            "--no-verbose" => o.verbose = false,
            "-f" | "--force" => o.force = true,
            "--no-force" => o.force = false,

            // `--all` and `--no-ignore-removal` are the same switch, as are
            // `--no-all` and `--ignore-removal` (git-add(1), "--no-all").
            "-A" | "--all" | "--no-ignore-removal" => o.addremove = Some(true),
            "--no-all" | "--ignore-removal" => o.addremove = Some(false),

            "-u" | "--update" => o.update = true,
            "--no-update" => o.update = false,
            "-N" | "--intent-to-add" => o.intent_to_add = true,
            "--no-intent-to-add" => o.intent_to_add = false,
            "--refresh" => o.refresh = true,
            "--no-refresh" => o.refresh = false,
            "--renormalize" => o.renormalize = true,
            "--no-renormalize" => o.renormalize = false,
            "--ignore-errors" => o.ignore_errors = true,
            "--no-ignore-errors" => o.ignore_errors = false,
            "--ignore-missing" => o.ignore_missing = true,
            "--no-ignore-missing" => o.ignore_missing = false,
            "--sparse" => o.sparse = Some(true),
            "--no-sparse" => o.sparse = Some(false),
            // `--warn-embedded-repo`/`--no-warn-embedded-repo` (hidden in git,
            // builtin/add.c:393 `OPT_HIDDEN_BOOL`, default on) only toggles the
            // `adding embedded git repository:` warning `check_embedded_repo`
            // emits when a matched worktree directory is itself a git repo. This
            // port never stages gitlinks/embedded repos (the walk keeps only
            // File/Symlink entries), so the warning is never produced and both
            // toggles are accepted no-ops — matching git's exit-0 accept for the
            // non-embedded-repo case.
            "--warn-embedded-repo" | "--no-warn-embedded-repo" => {}
            "--pathspec-file-nul" => o.pathspec_file_nul = true,
            "--no-pathspec-file-nul" => o.pathspec_file_nul = false,

            other if other.starts_with("--chmod=") => {
                // git records the value and validates only the *last* one, after
                // option parsing (`chmod_callback` stores; `cmd_add` checks). So
                // `--chmod=v1 --chmod=+x` is accepted. Just keep the raw string.
                o.chmod_arg = Some(other["--chmod=".len()..].to_string());
            }

            other if other.starts_with("--pathspec-from-file=") => {
                o.pathspec_from_file = true;
                o.pathspec_from_file_value =
                    Some(other["--pathspec-from-file=".len()..].to_string());
            }

            // Interactive/patch-only diff toggles. A bare `--auto-advance` is git's
            // default (stages normally); `--no-auto-advance` raises the requires
            // fatal in `stage()`. A `=value` on either is a usage error.
            "--auto-advance" => o.auto_advance = Some(true),
            "--no-auto-advance" => o.auto_advance = Some(false),
            other if other.starts_with("--auto-advance=") => {
                eprintln!("error: option `auto-advance' takes no value");
                return Ok(Err(ExitCode::from(USAGE)));
            }
            other if other.starts_with("--no-auto-advance=") => {
                eprintln!("error: option `no-auto-advance' takes no value");
                return Ok(Err(ExitCode::from(USAGE)));
            }
            // Sticky value forms `-U<n>`, `--unified=<n>`, `--inter-hunk-context=<n>`
            // (the separate-value forms are consumed in the pre-match block above).
            other if other.starts_with("-U") && !other.starts_with("--") && other.len() > 2 => {
                if let Err(code) = check_magnitude(Some(&other[2..]), true, "U") {
                    return Ok(Err(code));
                }
                o.unified_seen = true;
            }
            other if other.starts_with("--unified=") => {
                if let Err(code) =
                    check_magnitude(Some(&other["--unified=".len()..]), false, "unified")
                {
                    return Ok(Err(code));
                }
                o.unified_seen = true;
            }
            other if other.starts_with("--inter-hunk-context=") => {
                if let Err(code) = check_magnitude(
                    Some(&other["--inter-hunk-context=".len()..]),
                    false,
                    "inter-hunk-context",
                ) {
                    return Ok(Err(code));
                }
                o.interhunk_seen = true;
            }

            // The two interactive modes; `stage()` hands them to `git add`.
            "-p" | "--patch" => o.patch_interactive = true,
            "--no-patch" => o.patch_interactive = false,
            "-i" | "--interactive" => o.add_interactive = true,
            "--no-interactive" => o.add_interactive = false,
            // Recognized git flags that this port does not implement: name them.
            "-e" | "--edit" => bail!("--edit is not supported"),

            // `-h` is handled by `parse_options()` before any other switch in the
            // same bundle, so `git stage -hv` still prints the table — `git add`'s
            // table, byte for byte, since both verbs share one option set.
            other if other.starts_with('-')
                && !other.starts_with("--")
                && other[1..].contains('h') =>
            {
                o.delegate = true;
                break 'argv;
            }
            // Bundled short flags like `-nv`; every char must be a known toggle.
            other if other.starts_with('-') && !other.starts_with("--") && other.len() > 1 => {
                for c in other[1..].chars() {
                    match c {
                        'n' => o.dry_run = true,
                        'v' => o.verbose = true,
                        'f' => o.force = true,
                        'A' => o.addremove = Some(true),
                        'u' => o.update = true,
                        'N' => o.intent_to_add = true,
                        'p' => o.patch_interactive = true,
                        'i' => o.add_interactive = true,
                        'e' => bail!("--edit is not supported"),
                        // parse-options reports the *first* unknown switch and
                        // stops; `add` re-reads the same argv and renders it.
                        _ => {
                            o.delegate = true;
                            break 'argv;
                        }
                    }
                }
            }
            other if other.starts_with("--") && other != "--" => {
                o.delegate = true;
                break 'argv;
            }
            // A non-option argument is handed back unchanged by the resolver.
            _ => o.pathspecs.push(a.to_string()),
        }
        i += 1;
    }
    Ok(Ok(o))
}

/// Split the bytes of a `--pathspec-from-file` file into pathspecs, matching
/// git's `parse_pathspec_file` (builtin/add.c → pathspec.c).
///
/// In NUL mode the elements are separated by NUL and taken verbatim. Otherwise
/// each line (LF-terminated, with one trailing CR stripped) is one pathspec, and
/// a line that begins with `"` is C-style unquoted. A trailing separator does not
/// yield a final empty element, but an empty line in the middle yields an empty
/// pathspec (which the caller rejects, exactly as git does).
///
/// Returns `Err(line)` — the raw offending line — when a quoted line is malformed,
/// so the caller can render git's `line is badly quoted: <line>` fatal.
fn parse_pathspec_file(raw: &[u8], nul: bool) -> std::result::Result<Vec<String>, String> {
    let mut out = Vec::new();
    if nul {
        let mut start = 0;
        for i in 0..raw.len() {
            if raw[i] == 0 {
                out.push(String::from_utf8_lossy(&raw[start..i]).into_owned());
                start = i + 1;
            }
        }
        if start < raw.len() {
            out.push(String::from_utf8_lossy(&raw[start..]).into_owned());
        }
        return Ok(out);
    }

    let mut start = 0;
    let mut i = 0;
    while i <= raw.len() {
        let at_end = i == raw.len();
        if !at_end && raw[i] != b'\n' {
            i += 1;
            continue;
        }
        // A trailing LF closes the file without a final empty line.
        if at_end && start == i {
            break;
        }
        let mut line = &raw[start..i];
        if line.last() == Some(&b'\r') {
            line = &line[..line.len() - 1];
        }
        if line.first() == Some(&b'"') {
            match unquote_c_style(line) {
                Some(s) => out.push(s),
                None => return Err(String::from_utf8_lossy(line).into_owned()),
            }
        } else {
            out.push(String::from_utf8_lossy(line).into_owned());
        }
        start = i + 1;
        i += 1;
    }
    Ok(out)
}

/// Decode a C-style quoted path exactly as git's `unquote_c_style` does: the input
/// begins with `"`, and decoding stops at the closing `"` (any bytes after it are
/// ignored, as git ignores its `endp`). Recognizes `\a \b \f \n \r \t \v \\ \"`
/// and one-to-three-digit octal escapes. Returns `None` on an unterminated string
/// or an unknown escape, which git treats as "badly quoted".
fn unquote_c_style(line: &[u8]) -> Option<String> {
    let mut out: Vec<u8> = Vec::new();
    let mut i = 1; // skip the opening quote
    while i < line.len() {
        match line[i] {
            b'"' => return Some(String::from_utf8_lossy(&out).into_owned()),
            b'\\' => {
                i += 1;
                let e = *line.get(i)?;
                match e {
                    b'a' => out.push(0x07),
                    b'b' => out.push(0x08),
                    b'f' => out.push(0x0c),
                    b'n' => out.push(b'\n'),
                    b'r' => out.push(b'\r'),
                    b't' => out.push(b'\t'),
                    b'v' => out.push(0x0b),
                    b'\\' => out.push(b'\\'),
                    b'"' => out.push(b'"'),
                    b'0'..=b'7' => {
                        let mut val = u32::from(e - b'0');
                        let mut digits = 1;
                        while digits < 3
                            && matches!(line.get(i + 1), Some(&d) if (b'0'..=b'7').contains(&d))
                        {
                            i += 1;
                            val = val * 8 + u32::from(line[i] - b'0');
                            digits += 1;
                        }
                        out.push(val as u8);
                    }
                    _ => return None,
                }
            }
            c => out.push(c),
        }
        i += 1;
    }
    None // reached end without a closing quote
}

/// Validate a `-U`/`--unified`/`--inter-hunk-context` value the way git's
/// `parse_options` does for an `OPT_MAGNITUDE`, emitting git's exact `error:` line
/// and usage exit 129 on failure. `short` selects the `switch `U'` vs
/// `option `<name>'` wording; `value` is `None` when the option carried no argument.
///
/// git's own error text differs by failure kind (verified against git 2.55.0):
/// ```text
///   * no argument             -> `<label> requires a value`
///   * present but empty        -> `<label> expects a numerical value`
///   * non-empty, not a number  -> `<label> expects an integer value with an optional k/m/g suffix`
/// ```
/// A value error is printed alone — it carries no usage block.
fn check_magnitude(value: Option<&str>, short: bool, name: &str) -> std::result::Result<(), ExitCode> {
    let label = if short {
        format!("switch `{name}'")
    } else {
        format!("option `{name}'")
    };
    match value {
        None => eprintln!("error: {label} requires a value"),
        Some("") => eprintln!("error: {label} expects a numerical value"),
        Some(v) if is_valid_magnitude(v) => return Ok(()),
        Some(_) => {
            eprintln!("error: {label} expects an integer value with an optional k/m/g suffix")
        }
    }
    Err(ExitCode::from(USAGE))
}

/// git's `OPT_MAGNITUDE` acceptance test, ported from `git_parse_unsigned` +
/// `get_unit_factor` (config.c): reject any `-`; `strtoumax(_, 0)` must consume at
/// least one digit (base auto-detected — `0x` hex, a leading `0` octal, else
/// decimal); the trailing unit must be empty or one of `k`/`m`/`g` (case-insensitive).
/// The parsed magnitude is discarded — only its validity is observable — so the
/// overflow-past-`ULONG_MAX` arm (which also fails, with the same exit code) is not
/// reproduced.
fn is_valid_magnitude(s: &str) -> bool {
    if s.contains('-') {
        return false;
    }
    let b = s.as_bytes();
    let mut i = 0;
    // strtoumax skips leading ASCII whitespace and an optional leading `+`.
    while i < b.len() && b[i].is_ascii_whitespace() {
        i += 1;
    }
    if i < b.len() && b[i] == b'+' {
        i += 1;
    }
    // Auto-detect the base the way strtoumax(_, 0) does.
    let (radix, digits_start) = if i + 1 < b.len() && b[i] == b'0' && (b[i + 1] | 0x20) == b'x' {
        (16u32, i + 2)
    } else if i < b.len() && b[i] == b'0' {
        // A lone leading `0` is itself an octal digit, so the run is never empty.
        (8u32, i)
    } else {
        (10u32, i)
    };
    let mut j = digits_start;
    while j < b.len() && (b[j] as char).is_digit(radix) {
        j += 1;
    }
    if j == digits_start {
        return false; // no digit consumed (e.g. "0x", "abc", "")
    }
    matches!(&s[j..], "" | "k" | "K" | "m" | "M" | "g" | "G")
}

// ---------------------------------------------------------------------------
// entry point
// ---------------------------------------------------------------------------

pub fn stage(args: &[String]) -> Result<ExitCode> {
    let mut o = match parse(args)? {
        Ok(o) => o,
        Err(code) => return Ok(code),
    };

    // `git stage` is an alias entry in git's command table pointing at the very same
    // `cmd_add()` (`git-stage(1)`: "This is a synonym for git-add(1)"; `git stage -h`
    // and `git add -h` print byte-identical usage), so `-p`/`-i` are `git add`'s
    // interactive modes rather than anything of `stage`'s own. Hand the untouched
    // argv to [`add`](super::add), which parses it the same way and owns the hunk
    // selector and the numbered menu — including `-p` implying `-i`, the
    // `requires '--interactive/--patch'` fatals and the two
    // "cannot be used together" fatals, all in git's order.
    if o.delegate || o.patch_interactive || o.add_interactive {
        return super::add::add(args);
    }

    // `-U`/`--unified`, `--inter-hunk-context`, and `--no-auto-advance` only feed
    // the interactive/patch diff machinery. git collects them into `add_p_opt` and,
    // when neither `--patch` nor `--interactive` is active, dies here — before every
    // other validation, including the `-A`/`-u` conflict (verified against git
    // 2.55.0). Both interactive modes left for `add` above, so reaching this point
    // means patch mode is off; reproduce the fatal. The cited option follows git's
    // fixed precedence: `--unified`, then `--inter-hunk-context`, then
    // `--no-auto-advance`.
    if o.unified_seen || o.interhunk_seen || o.auto_advance == Some(false) {
        let opt = if o.unified_seen {
            "--unified"
        } else if o.interhunk_seen {
            "--inter-hunk-context"
        } else {
            "--no-auto-advance"
        };
        eprintln!("fatal: the option '{opt}' requires '--interactive/--patch'");
        return Ok(ExitCode::from(FATAL));
    }

    // --- option validation, in git's own order ------------------------------
    // The order matters when an invocation violates several rules at once, and it
    // is not argv order. Verified against git 2.55.0, highest precedence first:
    // the `-A`/`-u` conflict, then `--ignore-missing` without `--dry-run`, then an
    // empty-string pathspec, then `--pathspec-file-nul` without its file.
    if o.addremove == Some(true) && o.update {
        eprintln!("fatal: options '-A' and '-u' cannot be used together");
        return Ok(ExitCode::from(FATAL));
    }
    if o.ignore_missing && !o.dry_run {
        eprintln!("fatal: the option '--ignore-missing' requires '--dry-run'");
        return Ok(ExitCode::from(FATAL));
    }
    // `--chmod` is validated here, once, on its last value — after the `-A`/`-u`
    // and `--ignore-missing` fatals outrank it, but before the empty-pathspec and
    // `--pathspec-file-nul` checks. Verified against git 2.55.0; the code is 128,
    // not the 129 an eager option-value rejection would give.
    if let Some(arg) = &o.chmod_arg {
        match arg.as_str() {
            "+x" => o.chmod = Some(true),
            "-x" => o.chmod = Some(false),
            _ => {
                eprintln!("fatal: --chmod param '{arg}' must be either -x or +x");
                return Ok(ExitCode::from(FATAL));
            }
        }
    }
    if o.pathspecs.iter().any(String::is_empty) {
        eprintln!("fatal: empty string is not a valid pathspec. please use . instead if you meant to match all paths");
        return Ok(ExitCode::from(FATAL));
    }
    if o.pathspec_file_nul && !o.pathspec_from_file {
        eprintln!("fatal: the option '--pathspec-file-nul' requires '--pathspec-from-file'");
        return Ok(ExitCode::from(FATAL));
    }
    // `--pathspec-from-file=<file>` replaces the (necessarily empty) command-line
    // pathspecs with the ones read from `<file>` (or stdin for `-`). git reads and
    // parses the file here — after the checks above, before the pathspecs are ever
    // matched — so a malformed file or a clash with argv pathspecs dies with the
    // repository and object database untouched. Verified against git 2.55.0.
    if o.pathspec_from_file {
        if !o.pathspecs.is_empty() {
            eprintln!("fatal: '--pathspec-from-file' and pathspec arguments cannot be used together");
            return Ok(ExitCode::from(FATAL));
        }
        let file = o.pathspec_from_file_value.clone().unwrap_or_default();
        let raw = if file == "-" {
            let mut buf = Vec::new();
            std::io::Read::read_to_end(&mut std::io::stdin(), &mut buf)?;
            buf
        } else {
            match std::fs::read(&file) {
                Ok(b) => b,
                Err(e) => {
                    // git prints `strerror(errno)` alone; std's Display appends a
                    // ` (os error N)` tail, so keep only the leading message.
                    let s = e.to_string();
                    let reason = s.split(" (os error").next().unwrap_or(&s);
                    eprintln!("fatal: could not open '{file}' for reading: {reason}");
                    return Ok(ExitCode::from(FATAL));
                }
            }
        };
        match parse_pathspec_file(&raw, o.pathspec_file_nul) {
            Ok(specs) => o.pathspecs = specs,
            Err(line) => {
                eprintln!("fatal: line is badly quoted: {line}");
                return Ok(ExitCode::from(FATAL));
            }
        }
        // The file's own pathspecs get the same empty-string validation git runs
        // over command-line pathspecs — an empty line is an empty pathspec.
        if o.pathspecs.iter().any(String::is_empty) {
            eprintln!("fatal: empty string is not a valid pathspec. please use . instead if you meant to match all paths");
            return Ok(ExitCode::from(FATAL));
        }
    }

    let repo = gix::discover(".")?;
    if repo.workdir().is_none() {
        eprintln!("fatal: this operation must be run in a work tree");
        return Ok(ExitCode::from(FATAL));
    }

    // `parse_pathspec()` (builtin/add.c:460) normalizes every element and resolves it
    // against the current prefix — so `git stage f.txt` in `sub/` means `sub/f.txt`,
    // and an element that climbs out of the worktree is a fatal here rather than a
    // pathspec nobody can match. Shared with `add`; see
    // [`super::add::resolve_pathspecs`] for why each element is carried twice. This
    // runs after the `--pathspec-from-file` read, because the elements that file
    // supplies go through the very same `parse_pathspec()`.
    o.resolved = super::add::resolve_pathspecs(&repo, &mut o.pathspecs)?
        .into_iter()
        .map(|(_typed, relative)| relative)
        .collect();

    // Only `-A` and `-u` imply a pathspec; every other flag alone is a no-op.
    if o.pathspecs.is_empty() && !(o.addremove == Some(true) || o.update) {
        eprintln!("Nothing specified, nothing added.");
        if crate::advice::enabled("addEmptyPathspec") {
            eprintln!("hint: Maybe you wanted to say 'git add .'?");
            eprintln!(
                "hint: Disable this message with \"git config set advice.addEmptyPathspec false\""
            );
        }
        return Ok(ExitCode::SUCCESS);
    }

    if o.refresh {
        return refresh(&repo, &o);
    }
    add(&repo, &o)
}

// ---------------------------------------------------------------------------
// shared helpers
// ---------------------------------------------------------------------------

fn open_index(repo: &gix::Repository) -> Result<gix::index::File> {
    Ok(if repo.index_path().exists() {
        repo.open_index()?
    } else {
        gix::index::File::from_state(gix::index::State::new(repo.object_hash()), repo.index_path())
    })
}

/// True for the pathspecs that mean "everything at the current prefix": `.`,
/// `./`, `./.`. Anything carrying magic (a leading `:`) is left alone.
fn denotes_prefix_dir(spec: &str) -> bool {
    let trimmed = spec.trim_end_matches('/');
    !trimmed.is_empty() && trimmed.split('/').all(|c| c == ".")
}

/// The pathspec strings to hand to gix, with `.` rewritten into a form gix can
/// actually match.
///
/// gix derives one search-wide common prefix from the glob text of the patterns
/// and then requires every candidate path to start with it
/// (gix-pathspec/src/search/init.rs:60, search/matching.rs:41). Normalizing `.`
/// leaves its path as the literal `"."` (gix-pathspec/src/pattern.rs:110), so the
/// common prefix becomes `"."` and no worktree path can ever clear that check —
/// `git stage .` then behaves as if every pathspec matched nothing. git resolves
/// `.` to "the directory I was run in", so state that directly instead: the
/// all-matching nil spec `:` at the top of the worktree, and an explicit
/// directory spec below it.
fn pathspec_patterns(repo: &gix::Repository, o: &Opts) -> Result<Vec<BString>> {
    let prefix = repo.prefix()?.unwrap_or_else(|| std::path::Path::new(""));
    let prefix = gix::path::to_unix_separators_on_windows(gix::path::into_bstr(prefix)).into_owned();

    Ok(o.pathspecs
        .iter()
        .map(|s| {
            if !denotes_prefix_dir(s) {
                return BString::from(s.as_bytes());
            }
            if prefix.is_empty() {
                return BString::from(":");
            }
            // `:(top)` keeps gix from prepending the prefix a second time.
            let mut out = BString::from(":(top)");
            out.extend_from_slice(&prefix);
            out.push(b'/');
            out
        })
        .collect())
}

/// A pathspec that carries `:(exclude)`/`:!` magic never has to match anything,
/// so it is exempt from the "did not match any files" check.
fn is_exclude_spec(spec: &str) -> bool {
    spec.starts_with(":!")
        || spec.starts_with(":^")
        || (spec.starts_with(":(") && spec[..spec.find(')').unwrap_or(0)].contains("exclude"))
}

/// True when the pathspec is a plain path with no magic and no wildcard, which is
/// the only form for which git reports the gitignore / not-known-to-git errors
/// instead of the generic "did not match any files".
fn is_literal_spec(spec: &str) -> bool {
    !spec.is_empty() && !spec.starts_with(':') && !spec.contains(['*', '?', '['])
}

/// Mark every positive pathspec that matches at least one of `paths` as seen.
///
/// git marks a pathspec seen the moment it matches any examined path on its own —
/// before exclude pathspecs are applied, and regardless of whether another
/// pathspec also matched that path. gix's combined matcher instead attributes each
/// path to a single pathspec and never yields a path an exclude pathspec shadowed,
/// so it under-reports overlapping specs (`src/ src/lib.rs` both matching
/// `src/lib.rs`) and exclude-shadowed specs (`*.md` whose only match is dropped by
/// `:(exclude)README.md`). Recover the rest by testing each still-unseen positive
/// pathspec against `paths` with its own single-pattern matcher, which carries no
/// exclude and so matches exactly what git counts. `paths` is the universe of
/// tracked and to-be-staged paths — never a gitignored-and-skipped one, so a
/// wildcard whose only match is gitignored still (correctly) stays unseen.
fn mark_seen_per_spec(
    repo: &gix::Repository,
    index: &gix::index::File,
    patterns: &[BString],
    o: &Opts,
    paths: &[BString],
    seen: &mut HashSet<usize>,
) -> Result<()> {
    // `patterns` is 1:1 with `o.pathspecs`, so the index doubles as the seen key.
    for (i, spec) in o.pathspecs.iter().enumerate() {
        if seen.contains(&i) || is_exclude_spec(spec) || spec.is_empty() {
            continue;
        }
        let mut ps = repo.pathspec(
            true,
            std::slice::from_ref(&patterns[i]),
            false,
            index,
            gix::worktree::stack::state::attributes::Source::IdMapping,
        )?;
        if paths.iter().any(|p| ps.is_included(p.as_bstr(), Some(false))) {
            seen.insert(i);
        }
    }
    Ok(())
}

/// Report the first pathspec that matched nothing, using the message git uses for
/// the mode we are in. Returns `None` when every pathspec was accounted for.
///
/// `seen` holds the sequence numbers handed out by the pathspec search, which are
/// indices into `o.pathspecs` in argv order.
/// `update_mode` is `-u` outside `--refresh`: that is the only mode in which git
/// swaps the fatal for its "known to git" wording. `refresh()` passes `false`
/// because its own check always uses the plain message.
fn unmatched_pathspec_exit(
    repo: &gix::Repository,
    o: &Opts,
    seen: &HashSet<usize>,
    update_mode: bool,
) -> Option<ExitCode> {
    // `--ignore-missing` (only legal with `--dry-run`) turns the check off.
    if o.ignore_missing {
        return None;
    }
    // Literal pathspecs that exist on disk yet matched nothing. In add mode the
    // only way that happens is a .gitignore exclusion, which git reports as a
    // block listing every such path at once — but only after the loop below has
    // had its chance to die, since the fatal outranks it in either argv order.
    let mut ignored: BTreeSet<&str> = BTreeSet::new();

    for (i, spec) in o.pathspecs.iter().enumerate() {
        if seen.contains(&i) || is_exclude_spec(spec) || spec.is_empty() {
            continue;
        }
        // `file_exists(pathspec.items[i].match)` (builtin/add.c:561): git asks about
        // the element *after* the prefix pass, which is the only form a worktree path
        // can be built from. The message still quotes `original`, i.e. `spec`.
        let relative = o.resolved.get(i).map(String::as_str).unwrap_or(spec.as_str());
        let on_disk = repo
            .workdir_path(BStr::new(relative.as_bytes()))
            .is_some_and(|abs| std::fs::symlink_metadata(abs).is_ok());

        if !on_disk || !is_literal_spec(spec) {
            eprintln!("fatal: pathspec '{spec}' did not match any files");
            return Some(ExitCode::from(FATAL));
        }
        if update_mode {
            // Present on disk but not tracked, and `-u` only touches tracked paths.
            eprintln!("error: pathspec '{spec}' did not match any file(s) known to git");
            return Some(ExitCode::from(FATAL));
        }
        // `dir_add_ignored()` records `pathspec.items[i].match`, so the block below
        // lists the repo-relative form, not the element as typed.
        ignored.insert(relative);
    }

    if !ignored.is_empty() {
        eprintln!("The following paths are ignored by one of your .gitignore files:");
        for p in &ignored {
            eprintln!("{p}");
        }
        if crate::advice::enabled("addIgnoredFile") {
            eprintln!("hint: Use -f if you really want to add them.");
            eprintln!(
                "hint: Disable this message with \"git config set advice.addIgnoredFile false\""
            );
        }
        return Some(ExitCode::FAILURE);
    }
    None
}

// ---------------------------------------------------------------------------
// --refresh
// ---------------------------------------------------------------------------

/// `--refresh` re-stats the matched *index* entries and stages nothing. With
/// `--verbose` git switches `refresh_index()` into porcelain mode, which prints
/// a header plus one `M`/`D` line per still-unstaged path on stdout.
fn refresh(repo: &gix::Repository, o: &Opts) -> Result<ExitCode> {
    let index = open_index(repo)?;
    let patterns = pathspec_patterns(repo, o)?;
    // `--refresh` compares the worktree against the index, and the index holds
    // *converted* content — so the comparison has to convert too, or every file
    // the filters touch looks modified. It writes no object, which is git's
    // `hash_flags` without `HASH_WRITE_OBJECT`.
    let mut filters = super::convert_to_git::WorktreeFilter::new(repo, false, false)?;

    let mut ps = repo.pathspec(
        true,
        &patterns,
        false,
        &index,
        gix::worktree::stack::state::attributes::Source::IdMapping,
    )?;

    let mut seen: HashSet<usize> = HashSet::new();
    // path -> refreshed stat, applied after the immutable scan.
    let mut restat: HashMap<BString, Stat> = HashMap::new();
    let mut unstaged: BTreeMap<BString, char> = BTreeMap::new();
    // Every refreshable index path, for the same per-spec seen accounting `add`
    // does — an entry a pathspec matches counts even if the combined matcher
    // attributed it to a different, overlapping pathspec.
    let mut universe: Vec<BString> = Vec::new();

    {
        let backing = index.path_backing();
        for e in index.entries() {
            if e.stage() != Stage::Unconflicted || e.mode == Mode::COMMIT {
                continue; // conflicted stages and gitlinks are not refreshable here
            }
            let path = e.path_in(backing);
            universe.push(path.to_owned());
            let Some(m) = ps.pattern_matching_relative_path(path, Some(false)) else {
                continue;
            };
            if m.is_excluded() {
                continue;
            }
            if m.sequence_number < patterns.len() {
                seen.insert(m.sequence_number);
            }

            let owned = path.to_owned();
            let Some(abs) = repo.workdir_path(path) else {
                continue;
            };
            let Ok(md) = gix::index::fs::Metadata::from_path_no_follow(&abs) else {
                unstaged.insert(owned, 'D');
                continue;
            };
            match read_worktree_blob(repo, &mut filters, path, &abs, &md) {
                Ok((id, mode)) if id == e.id && mode == e.mode => {
                    // Content is unchanged; adopt the fresh stat so later commands
                    // can take the lstat shortcut. Recording only genuine changes
                    // keeps a fully up-to-date index from being rewritten at all.
                    match Stat::from_fs(&md) {
                        Ok(stat) if stat != e.stat => {
                            restat.insert(owned, stat);
                        }
                        _ => {}
                    }
                }
                // Content differs, or could not be read: either way the path still
                // carries an unstaged modification.
                Ok(_) | Err(_) => {
                    unstaged.insert(owned, 'M');
                }
            }
        }
    }

    mark_seen_per_spec(repo, &index, &patterns, o, &universe, &mut seen)?;
    if let Some(code) = unmatched_pathspec_exit(repo, o, &seen, false) {
        return Ok(code);
    }

    if o.verbose && !unstaged.is_empty() {
        println!("Unstaged changes after refreshing the index:");
        for (path, kind) in &unstaged {
            println!("{kind}\t{path}");
        }
    }

    if !o.dry_run && !restat.is_empty() {
        let _lock = crate::lock::RepoLock::acquire(repo.git_dir());
        let mut index = open_index(repo)?;
        for (entry, path) in index.entries_mut_with_paths() {
            if let Some(stat) = restat.get(&path.to_owned()) {
                entry.stat = *stat;
            }
        }
        index.write(gix::index::write::Options::default())?;
    }
    Ok(ExitCode::SUCCESS)
}

// ---------------------------------------------------------------------------
// the staging path
// ---------------------------------------------------------------------------

/// Read a worktree path and return the blob id its content hashes to, plus the
/// index mode it should carry. Nothing is written to the object database — the
/// caller decides that, so `--dry-run` can stay side-effect free.
fn read_worktree_blob(
    repo: &gix::Repository,
    filters: &mut super::convert_to_git::WorktreeFilter,
    rela: &BStr,
    abs: &std::path::Path,
    md: &gix::index::fs::Metadata,
) -> Result<(gix::hash::ObjectId, Mode)> {
    let (bytes, mode) = read_converted_bytes(repo, filters, rela, abs, md)?;
    let id = gix::objs::compute_hash(repo.object_hash(), gix::objs::Kind::Blob, &bytes)?;
    Ok((id, mode))
}

/// The worktree bytes of `abs` as git would store them: a regular file goes
/// through `convert_to_git()` (`.gitattributes` filters, `core.autocrlf`, …), a
/// symlink's target is stored verbatim.
///
/// Shared with [`super::add::renormalize_tracked_files`], which writes the objects
/// of a `--renormalize` run for both verbs and needs exactly these bytes.
pub(super) fn read_converted_bytes(
    repo: &gix::Repository,
    filters: &mut super::convert_to_git::WorktreeFilter,
    rela: &BStr,
    abs: &std::path::Path,
    md: &gix::index::fs::Metadata,
) -> Result<(Vec<u8>, Mode)> {
    let (bytes, mode) = read_worktree_bytes(abs, md)?;
    if mode == Mode::SYMLINK {
        return Ok((bytes, mode));
    }
    let rela = gix::path::from_bstr(rela).into_owned();
    let converted = filters
        .convert(repo, &rela, &bytes)
        .map_err(|e| anyhow!("{e}"))?;
    Ok((converted, mode))
}

/// `create_ce_mode(st.st_mode)` (cache.h): the index mode `add_to_index()` gives an
/// entry from the worktree stat alone, before it has read a single byte. `-N` never
/// reads the file, so this is the only mode it has to compare against.
pub(super) fn mode_from_metadata(md: &gix::index::fs::Metadata) -> Mode {
    if md.is_symlink() {
        Mode::SYMLINK
    } else if md.is_executable() {
        Mode::FILE_EXECUTABLE
    } else {
        Mode::FILE
    }
}

fn read_worktree_bytes(
    abs: &std::path::Path,
    md: &gix::index::fs::Metadata,
) -> Result<(Vec<u8>, Mode)> {
    if md.is_symlink() {
        let target = std::fs::read_link(abs)?;
        #[cfg(unix)]
        let bytes = {
            use std::os::unix::ffi::OsStrExt;
            target.as_os_str().as_bytes().to_vec()
        };
        #[cfg(not(unix))]
        let bytes = target.to_string_lossy().into_owned().into_bytes();
        Ok((bytes, Mode::SYMLINK))
    } else {
        let bytes = std::fs::read(abs)?;
        let mode = if md.is_executable() {
            Mode::FILE_EXECUTABLE
        } else {
            Mode::FILE
        };
        Ok((bytes, mode))
    }
}

/// `ce_match_stat_basic()`'s field selection (read-cache.c), which is configurable
/// and therefore cannot be a constant.
///
/// * `core.trustctime` (default true) gates the `CTIME_CHANGED` comparison.
/// * `core.checkStat` — `default` (true) or `minimal` (false) — gates the ctime,
///   uid/gid and inode comparisons together. `minimal` leaves only mode, size and
///   mtime, which is what a repository whose worktree has been copied or restored
///   from a backup needs: an inode cannot survive a copy, so with the default
///   setting every entry in the copy reads as modified.
/// * `USE_NSEC` is a compile-time option git does not enable by default and stock
///   2.55.0 as shipped does not carry, so the nanosecond fields are never compared.
///   [`racy_paths`] leaves them out for the same reason.
/// * `st_dev` is left out for the same reason gitoxide leaves it out.
pub(super) fn stat_match(repo: &gix::Repository) -> gix::index::entry::stat::Options {
    let cfg = repo.config_snapshot();
    gix::index::entry::stat::Options {
        trust_ctime: cfg.boolean("core.trustctime").unwrap_or(true),
        check_stat: cfg
            .string("core.checkStat")
            .is_none_or(|v| v.as_bstr() != "minimal"),
        use_nsec: false,
        use_stdev: false,
    }
}

/// The stage-0 paths `is_racy_stat()` (read-cache.c) considers racily clean: the
/// index file's own timestamp is not newer than the entry's recorded mtime, so a
/// write in the same filesystem tick could have gone unnoticed.
///
/// `run_diff_files()` matches with `CE_MATCH_RACY_IS_DIRTY`, and so does
/// `add_to_index()` itself (read-cache.c:717), so those paths are reported as
/// modified regardless of content — which is why a freshly checked out or freshly
/// copied worktree hands every tracked path to `add_file_to_index()`, `-N`
/// included, and why every one of them is hashed with `INDEX_WRITE_OBJECT` and can
/// therefore raise the `core.safecrlf` round-trip warning.
///
/// The comparison is whole seconds. `is_racy_stat()` only consults the nanosecond
/// field under `USE_NSEC`, which is a compile-time option git does not enable by
/// default and which stock git 2.55.0 as shipped by Homebrew does not carry: a
/// repository whose index and worktree files were written in the same second
/// reports the warning indefinitely, not for a sub-second window. Verified against
/// that binary — a fixture whose index mtime is `…986.092380002` and whose
/// `crlf.txt` mtime is `…986.072577080` (index strictly *newer* in nanoseconds, so
/// not racy under `USE_NSEC`) still warns, three seconds later and every time.
/// [`stat_match`] leaves `use_nsec` off for the same reason.
pub(super) fn racy_paths(index: &gix::index::File, repo: &gix::Repository) -> HashSet<BString> {
    let Ok(meta) = std::fs::metadata(repo.index_path()) else {
        return HashSet::new();
    };
    let Ok(modified) = meta.modified() else {
        return HashSet::new();
    };
    let Ok(since) = modified.duration_since(std::time::UNIX_EPOCH) else {
        return HashSet::new();
    };
    let idx_sec = since.as_secs();
    if idx_sec == 0 {
        return HashSet::new();
    }
    let backing = index.path_backing();
    index
        .entries()
        .iter()
        .filter(|e| e.stage() == Stage::Unconflicted)
        .filter(|e| idx_sec <= u64::from(e.stat.mtime.secs))
        .map(|e| e.path_in(backing).to_owned())
        .collect()
}

/// A worktree path that will be written into the index.
struct Staged {
    path: BString,
    id: gix::hash::ObjectId,
    mode: Mode,
    stat: Stat,
    /// Intent-to-add entries carry the empty blob and the `INTENT_TO_ADD` flag.
    intent: bool,
}

fn add(repo: &gix::Repository, o: &Opts) -> Result<ExitCode> {
    let index = open_index(repo)?;
    // The content filters every staged blob passes through — the same pipeline
    // `git add` uses, since `cmd_stage()` *is* `cmd_add()`.
    //
    // git hashes and stores in one `index_path()` call; this verb splits that into
    // a scan that computes ids and a write pass that deposits the blobs the scan
    // selected. The `core.safecrlf` round-trip check belongs to the SCAN, because
    // the scan is what stands in for `index_path()`: git raises it for every path
    // `add_to_index()` indexes, including the racily-clean ones whose content turns
    // out to be unchanged and which therefore never reach this verb's write pass at
    // all. Giving the check to the write pass instead is what silently dropped the
    // warning for exactly those paths. The write pass runs with the check off so
    // the one conversion git performs is reported once.
    let mut filters = super::convert_to_git::WorktreeFilter::new(repo, false, o.renormalize)?;
    // `pretend ? 0 : INDEX_WRITE_OBJECT` (read-cache.c:723) plus the `intent_only`
    // arm that skips `index_path()` outright: `-n` and `-N` convert nothing git
    // would report on.
    let mut scan_filters = super::convert_to_git::WorktreeFilter::new(
        repo,
        !o.dry_run && !o.intent_to_add,
        o.renormalize,
    )?;

    // `fill_directory()` runs before any of the staging below and before the
    // pathspec check can die, so its diagnostic comes first. It is skipped for the
    // two modes that never look for untracked files (`add_new_files`,
    // builtin/add.c:451).
    if !o.update && !o.renormalize {
        super::add::warn_unopenable_walk_prefix(repo, &o.pathspecs, &o.resolved);
    }

    // git applies `--chmod` only when a pathspec is present: cmd_add runs it under
    // `if (chmod_arg && pathspec.nr)`, and `-A`/`-u` do not synthesize a pathspec.
    // So `stage -u --chmod=+x` (no pathspec) leaves every mode untouched — matching
    // git — rather than flipping the bit on every tracked entry.
    let chmod = if o.pathspecs.is_empty() { None } else { o.chmod };

    // --- sparse-checkout accounting, identical to `add`'s -------------------
    // `--sparse` is git's `include_sparse`, which switches every test below off.
    // Without it a path outside the definition is never staged, and how git says so
    // depends on which half of `cmd_add()` found it: `add_files()` collects the
    // untracked ones into `matched_sparse_paths` and names them
    // (builtin/add.c:356-362, exit 1), while `update_callback()` drops the tracked
    // ones without a word (read-cache.c:3979-3981). This verb used to reject the
    // flag outright instead of implementing any of it, which also meant a plain
    // `stage` in a sparse-checkout repository ran with no sparse test at all.
    let include_sparse = o.sparse == Some(true);
    let sparsity = super::add::sparsity_to_consult(repo, include_sparse)?;
    let outside_sparse = |path: &BString| -> bool {
        sparsity.as_ref().is_some_and(|s| !s.includes(&path.to_str_lossy()))
    };
    let mut sparse_skipped: BTreeSet<BString> = BTreeSet::new();
    // See `add`'s copy for why these two sets are not the same one:
    // `PS_IGNORE_SKIP_WORKTREE` tests the bit alone and always applies, while
    // `matches_skip_worktree()` tests the bit or the definition and only matters
    // without `--sparse`.
    let (skip_worktree_entries, sparse_hidden): (HashSet<BString>, Vec<BString>) = {
        let backing = index.path_backing();
        let mut bit = HashSet::new();
        let mut hidden = Vec::new();
        for e in index.entries().iter().filter(|e| e.stage() == Stage::Unconflicted) {
            let path = e.path_in(backing);
            if e.flags.contains(Flags::SKIP_WORKTREE) {
                bit.insert(path.to_owned());
            }
            if super::add::skipped_as_sparse(e.flags, path, include_sparse, sparsity.as_ref()) {
                hidden.push(path.to_owned());
            }
        }
        (bit, hidden)
    };

    // Repo-relative stage-0 entries: the tracked set, with what is staged today.
    let tracked: HashMap<BString, (gix::hash::ObjectId, Mode)> = {
        let backing = index.path_backing();
        index
            .entries()
            .iter()
            .filter(|e| e.stage() == Stage::Unconflicted)
            .map(|e| (e.path_in(backing).to_owned(), (e.id, e.mode)))
            .collect()
    };
    // The stage-0 paths `is_racy_stat()` calls racily clean: their recorded
    // mtime is not older than the index file's own, so the stat cache cannot
    // prove them unchanged. `ie_match_stat()` is asked with
    // `CE_MATCH_RACY_IS_DIRTY` here (that is what `run_diff_files()` passes), so
    // every one of them counts as modified whatever its content says.
    let racily_clean: HashSet<BString> = racy_paths(&index, &repo);
    // `ce_match_stat_basic()`'s configurable field selection; see [`stat_match`].
    let stat_match = stat_match(repo);
    // The stat cache itself, for the `match_stat_data()` comparison that decides
    // which tracked paths `run_diff_files()` hands to `add_file_to_index()`.
    let tracked_stat: HashMap<BString, Stat> = {
        let backing = index.path_backing();
        index
            .entries()
            .iter()
            .filter(|e| e.stage() == Stage::Unconflicted)
            .map(|e| (e.path_in(backing).to_owned(), e.stat))
            .collect()
    };

    let patterns = pathspec_patterns(repo, o)?;

    // --- directory walk over the worktree, filtered by the pathspecs --------
    // Ignored entries are emitted too so a path that is both tracked and
    // gitignored can still be restaged; they are filtered right below.
    let options = repo
        .dirwalk_options()?
        .emit_tracked(true)
        .emit_ignored(Some(gix::dir::walk::EmissionMode::Matching));
    let dirwalk_index = repo.index_or_load_from_head_or_empty()?;
    let mut iter = repo.dirwalk_iter(dirwalk_index, patterns.clone(), Default::default(), options)?;

    // (path, was-ignored) for every stageable worktree file the walk turned up.
    let mut candidates: Vec<(BString, bool)> = Vec::new();
    for item in iter.by_ref() {
        let entry = item?.entry;
        match entry.disk_kind {
            Some(gix::dir::entry::Kind::File) | Some(gix::dir::entry::Kind::Symlink) => {}
            _ => continue, // directories, submodule repositories, untrackable things
        }
        let is_ignored = matches!(entry.status, gix::dir::entry::Status::Ignored(_));
        candidates.push((entry.rela_path, is_ignored));
    }
    drop(iter);
    // Put the walk into the order `cmd_add()` visits paths in before anything is
    // read: every per-path line this scan emits — the `core.autocrlf` round-trip
    // warnings and the `error: unable to index file` pairs alike — comes out in it.
    // See [`super::add::staging_order`].
    candidates.sort_by_key(|(path, _)| super::add::staging_order(tracked.contains_key(path), path));

    // A second, independent matcher: unlike the walk's, this one reports *which*
    // pathspec matched, which is what drives the "did not match any files" check
    // and, in turn, makes directory specs like `src/` resolve correctly.
    let mut ps = repo.pathspec(
        true,
        &patterns,
        false,
        &index,
        gix::worktree::stack::state::attributes::Source::IdMapping,
    )?;
    // Sequence numbers are indices into `patterns` in argv order; the synthetic
    // match an exclude-only pathspec set produces carries `patterns.len()`
    // (gix-pathspec/src/search/matching.rs:114), so it is filtered out here.
    let mut seen: HashSet<usize> = HashSet::new();

    // --- decide what each candidate becomes ---------------------------------
    let mut staged: Vec<Staged> = Vec::new();
    // What each path became, keyed by path. The ORDER git prints them in is not
    // this map's — see [`report_lines`] — so nothing here may be a sorted map.
    let mut touched: HashMap<BString, &'static str> = HashMap::new();
    let mut had_error = ReadErrors::default();
    // Whether any path actually reached `add_to_index()` under `-N`, which is
    // what decides the empty-blob side effect further down.
    let mut intent_visited = false;
    // The paths git counts toward "did the pathspec match anything": everything
    // that cleared the ignore/update filters below, plus the tracked set added
    // after the loop. Fed to `mark_seen_per_spec` so overlapping and
    // exclude-shadowed pathspecs are attributed the way git attributes them.
    let mut universe: Vec<BString> = Vec::new();

    for (path, is_ignored) in candidates {
        let current = tracked.get(&path);
        let already_tracked = current.is_some();

        // An ignored path is only staged when forced or already tracked, and an
        // ignored-and-skipped path deliberately does NOT mark its pathspec seen —
        // that is what makes `git stage <ignored>` report the gitignore error.
        if is_ignored && !o.force && !already_tracked {
            continue;
        }
        // `-u/--update` restages tracked paths only; brand-new files are not its business.
        // `--renormalize` likewise: it clears `add_new_files`, and
        // `renormalize_tracked_files()` walks the index, so an untracked path is
        // never reported and never staged.
        if (o.update || o.renormalize) && !already_tracked {
            continue;
        }
        // `PS_IGNORE_SKIP_WORKTREE`: an entry carrying the bit never marks its
        // pathspec matched, so it must stay out of the seen accounting as well.
        if !skip_worktree_entries.contains(&path) {
            universe.push(path.clone());
            if let Some(m) = ps.pattern_matching_relative_path(path.as_bstr(), Some(false)) {
                if !m.is_excluded() && m.sequence_number < patterns.len() {
                    seen.insert(m.sequence_number);
                }
            }
        }
        // The sparse test sits *after* the seen accounting, because git marks a
        // pathspec matched in `prune_directory()` — before `add_files()` ever looks
        // at the definition — which is why `git add out/extra.txt` reports the
        // sparse block rather than "did not match any files".
        if outside_sparse(&path) {
            if !already_tracked {
                sparse_skipped.insert(path);
            }
            continue;
        }

        let Some(abs) = repo.workdir_path(&path) else {
            continue;
        };
        let md = match gix::index::fs::Metadata::from_path_no_follow(&abs) {
            Ok(md) => md,
            Err(e) => {
                if !o.renormalize {
                    // `--renormalize` runs `renormalize_tracked_files()` INSTEAD of
                    // `add_files_to_cache()` (builtin/add.c:587-592), so this walk is
                    // not a git code path under that flag and reports nothing: the
                    // index scan below owns every `unable to index file` line, and
                    // the -1 that follows them.
                    report_read_error(path.as_ref(), &e, already_tracked, &mut had_error);
                }
                continue;
            }
        };

        let stat_now = Stat::from_fs(&md).unwrap_or_default();

        // `run_diff_files(&rev, DIFF_RACY_IS_MODIFIED)` (builtin/add.c:590) is what
        // picks the tracked paths `add_file_to_index()` ever sees, and it picks them
        // on the **stat cache**, never on content: `ie_match_stat()` with
        // `CE_MATCH_RACY_IS_DIRTY` calls a path modified when the recorded stat
        // differs from the worktree's, and calls a racily-clean one modified
        // whatever its bytes say. A tracked path it skips is never hashed — so it is
        // not reported, and it cannot raise the round-trip warning either.
        //
        // `--renormalize` replaces that walk with `renormalize_tracked_files()`'s
        // index scan (builtin/add.c:587-592), which carries no such filter.
        if already_tracked
            && !o.renormalize
            && !racily_clean.contains(&path)
            && tracked_stat
                .get(&path)
                .is_some_and(|recorded| recorded.matches(&stat_now, stat_match))
        {
            continue;
        }

        // `-N` records untracked paths as the empty blob, leaving already-tracked
        // entries untouched: `add_to_index()` takes the `intent_only` arm, which
        // skips `index_path()` entirely (so nothing is hashed and nothing is
        // converted) and adds with `ADD_CACHE_NEW_ONLY`.
        if o.intent_to_add {
            // `set_object_name_for_intent_to_add_entry()` (read-cache.c:704-710)
            // deposits the empty blob for every path that reaches `add_to_index()`,
            // before `ADD_CACHE_PRETEND` is ever consulted.
            intent_visited = true;
            let empty = repo.object_hash().empty_blob();
            if already_tracked {
                // `was_same` (read-cache.c:794-797) compares the entry the index
                // already holds with the one just built — under `-N` that is the
                // empty blob at the mode the worktree stat gives. Only a difference
                // is reported, and the entry itself is left alone.
                if current != Some(&(empty, mode_from_metadata(&md))) {
                    touched.insert(path.clone(), "add");
                }
                continue;
            }
            touched.insert(path.clone(), "add");
            staged.push(Staged {
                path,
                id: empty,
                // `create_ce_mode(st_mode)` again: an intent-to-add entry keeps the
                // worktree's own mode, so `git stage -N` over an executable records
                // 100755 and over a symlink 120000. Recording 100644 for all three
                // made `ls-files -s` disagree with stock for both.
                mode: mode_from_metadata(&md),
                stat: stat_now,
                intent: true,
            });
            continue;
        }

        let (raw, mode) = match read_worktree_bytes(&abs, &md) {
            Ok(v) => v,
            Err(e) => {
                if !o.renormalize {
                    // `--renormalize` runs `renormalize_tracked_files()` INSTEAD of
                    // `add_files_to_cache()` (builtin/add.c:587-592), so this walk is
                    // not a git code path under that flag and reports nothing: the
                    // index scan below owns every `unable to index file` line, and
                    // the -1 that follows them.
                    report_read_error(path.as_ref(), &e, already_tracked, &mut had_error);
                }
                continue;
            }
        };
        // A symlink's target is stored verbatim; a regular file goes through
        // `convert_to_git()`. This is git's one `index_path()` conversion, so this
        // is where the `core.autocrlf` round-trip warning — and, under
        // `core.safecrlf=true`, the refusal that stages nothing and exits 128 —
        // comes from. The two are told apart from a read failure here rather than
        // in a shared helper, because a refused conversion is a fatal while an
        // unreadable file is an `unable to index file` the run can survive.
        let bytes = if mode == Mode::SYMLINK {
            raw
        } else {
            let rela = gix::path::from_bstr(path.as_bstr()).into_owned();
            match scan_filters.convert(repo, &rela, &raw) {
                Ok(converted) => converted,
                Err(err) => {
                    eprintln!("fatal: {err}");
                    return Ok(ExitCode::from(FATAL));
                }
            }
        };
        let id = gix::objs::compute_hash(repo.object_hash(), gix::objs::Kind::Blob, &bytes)?;

        // `was_same`: unchanged content and mode, so nothing to report and nothing
        // to write. This is what keeps `--verbose` quiet for paths git would leave
        // alone even though it hashed them.
        //
        // `--renormalize` is the exception: `add_to_index()` skips its `alias`
        // lookup under `ADD_CACHE_RENORMALIZE`, so `was_same` never becomes true
        // and every matched tracked blob is reported.
        if !o.renormalize && current == Some(&(id, mode)) {
            continue;
        }
        touched.insert(path.clone(), "add");
        staged.push(Staged { path, id, mode, stat: stat_now, intent: false });
    }

    // --- submodule gitlinks: stage a moved submodule's new HEAD (mode 160000) ---
    // The walk above kept only files and symlinks, so a submodule worktree
    // (`Kind::Repository`) never reached it. Stock git's `stage` *is* `cmd_add`,
    // so it records the same pointer moves `git add <submodule>` does — off the
    // shared index-driven pass, not a second copy of the rule.
    {
        let already: HashSet<BString> = staged.iter().map(|s| s.path.clone()).collect();
        for (path, id, stat) in super::add::moved_gitlinks(&repo, &index, &already, |p| {
            match ps.pattern_matching_relative_path(p, Some(false)) {
                Some(m) if !m.is_excluded() => {
                    if m.sequence_number < patterns.len() {
                        seen.insert(m.sequence_number);
                    }
                    true
                }
                _ => false,
            }
        }) {
            touched.insert(path.clone(), "add");
            staged.push(Staged { path, id, mode: Mode::COMMIT, stat, intent: false });
        }
    }

    // --- deletions: tracked stage-0 paths, matched, whose file is gone ------
    let staged_set: HashSet<BString> = staged.iter().map(|s| s.path.clone()).collect();
    let mut deletions: Vec<BString> = Vec::new();
    {
        let backing = index.path_backing();
        for e in index.entries() {
            if e.stage() != Stage::Unconflicted || e.mode == Mode::COMMIT {
                continue; // leave conflicted stages and submodule gitlinks alone
            }
            // `PS_IGNORE_SKIP_WORKTREE`: an entry the sparse-checkout definition keeps
            // out of the worktree is absent by design, never a deletion. Without this,
            // `stage -A` in a cone-sparse repo staged the removal of every path outside
            // the cone — `git stage --renormalize --all -v` reported
            // `remove 'out/f.txt'` and dropped it from the index, where git touches
            // neither. `add` has carried this guard all along.
            if e.flags.contains(Flags::SKIP_WORKTREE) {
                continue;
            }
            let path = e.path_in(backing);
            let owned = path.to_owned();
            if staged_set.contains(&owned) || touched.contains_key(&owned) {
                continue;
            }
            match ps.pattern_matching_relative_path(path, Some(false)) {
                Some(m) if !m.is_excluded() => {
                    let gone = match repo.workdir_path(path) {
                        Some(p) => std::fs::symlink_metadata(p).is_err(),
                        None => true,
                    };
                    // A tracked path that still exists was handled by the walk; a
                    // vanished one marks its pathspec seen either way, because git
                    // considers a removal a match.
                    if m.sequence_number < patterns.len() {
                        seen.insert(m.sequence_number);
                    }
                    // `update_callback()`'s sparse guard (read-cache.c:3979-3981)
                    // sits ahead of its `DIFF_STATUS_DELETED` arm, so a removal
                    // outside the definition is dropped silently — no report, and
                    // no place in `matched_sparse_paths`, which only `add_files()`
                    // fills.
                    if outside_sparse(&owned) {
                        continue;
                    }
                    if gone && (o.stages_deletions() || o.update) {
                        deletions.push(owned.clone());
                        touched.insert(owned, "remove");
                    }
                }
                _ => continue,
            }
        }
    }

    // --- validate that every pathspec matched something ---------------------
    // Runs before any object or index write, matching git: a bad pathspec leaves
    // the repository, and the object database, completely untouched. The tracked
    // set joins the walked candidates so a pathspec that matched only a tracked
    // (possibly exclude-shadowed) path is still recorded as seen.
    universe.extend(
        tracked.keys().filter(|p| !skip_worktree_entries.contains(*p)).cloned(),
    );
    mark_seen_per_spec(repo, &index, &patterns, o, &universe, &mut seen)?;
    // `if (!include_sparse && matches_skip_worktree(&pathspec, i, ...))`
    // (builtin/add.c:549-554): a pathspec that matched nothing so far but *does*
    // match an index entry the sparse-checkout definition hides is named through
    // `advise_on_updating_sparse_paths()` with exit 1, not killed with the
    // "did not match any files" fatal. `find_pathspecs_matching_skip_worktree()`
    // asks the question with an ordinary `ce_path_match()`, which is exactly what
    // [`mark_seen_per_spec`] runs — so it is reused over the hidden paths and the
    // pathspecs it newly marks are the ones to report.
    let mut hidden_seen = seen.clone();
    mark_seen_per_spec(repo, &index, &patterns, o, &sparse_hidden, &mut hidden_seen)?;
    for i in hidden_seen.difference(&seen) {
        sparse_skipped.insert(BString::from(o.pathspecs[*i].as_bytes()));
    }
    if let Some(code) = unmatched_pathspec_exit(repo, o, &hidden_seen, o.update) {
        return Ok(code);
    }

    // `--renormalize` is driven by the *index*, not by the worktree walk above:
    // `cmd_add()` calls `renormalize_tracked_files()` INSTEAD of
    // `add_files_to_cache()`, and does so only after the pathspec validation — so
    // `stage --renormalize a.txt nosuch` over a deleted `a.txt` reports the
    // unmatched `nosuch`, not the failed stat. One implementation serves both
    // verbs, because stock git dispatches `stage` to that same `cmd_add()`.
    let mut index_failed = false;
    if o.renormalize {
        let outcome = super::add::renormalize_tracked_files(
            repo,
            &index,
            &super::add::RenormalizeOpts {
                include_sparse,
                dry_run: o.dry_run,
                verbose: o.verbose,
                intent_to_add: o.intent_to_add,
            },
            &mut filters,
            |p| {
                ps.pattern_matching_relative_path(p, Some(false))
                    .is_some_and(|m| !m.is_excluded())
            },
        )?;
        if let Some(code) = outcome.aborted {
            return Ok(code);
        }
        // An entry the scan could not index is `exit_status |= -1` — 255 at the
        // end, with the index still written. See [`super::add::RenormalizeOutcome`].
        index_failed = outcome.failed;
    }

    // An unreadable file is only survivable under `--ignore-errors`; otherwise git
    // reports every failure it hit and then dies once, after the scan.
    if had_error.any && !o.ignore_errors {
        let verb = if had_error.tracked { "updating" } else { "adding" };
        eprintln!("fatal: {verb} files failed");
        return Ok(ExitCode::from(FATAL));
    }

    // `--intent-to-add` deposits the empty blob into the object store as a side
    // effect of `set_object_name_for_intent_to_add_entry()`, which
    // `add_to_index()` calls *before* the `pretend` check — so `--dry-run`
    // leaves it behind too. It is not unconditional though: only a path that
    // actually reaches `add_to_index()` triggers it, which is a modified tracked
    // path or an untracked one. `-N` over an all-clean pathspec (`-N -u` in a
    // clean worktree, `-N .` with nothing modified) writes no object at all,
    // because `run_diff_files()` hands it no path to index.
    // The write is atomic and idempotent, so it needs no index lock.
    if o.intent_to_add && intent_visited {
        repo.write_blob(b"")?;
    }

    let lines = report_lines(&index, &tracked, &touched);
    let finish = |chmod_errors: &'_ [String]| -> ExitCode {
        super::add::finish_code(super::add::Finish {
            had_errors: had_error.any,
            ignore_errors: o.ignore_errors,
            sparse_skipped: &sparse_skipped,
            chmod_errors,
            index_failed,
        })
    };

    // --- dry run: report only, never touch the index or the odb -------------
    // `chmod_pathspec(show_only = 1)` still runs its `S_ISREG` test over every
    // matched entry, so a matched symlink is `cannot chmod` and 255 here too.
    if o.dry_run {
        report(&lines);
        let chmod_errors = match chmod {
            None => Vec::new(),
            Some(flip) => {
                super::add::chmod_pathspec(&index, flip, include_sparse, sparsity.as_ref(), |p| {
                    ps.pattern_matching_relative_path(p, Some(false))
                        .is_some_and(|m| !m.is_excluded())
                })
                .1
            }
        };
        return Ok(finish(&chmod_errors));
    }

    // Nothing to write: report and leave the index file alone, so its extensions
    // (notably the tree cache dropped below) survive a run that changed nothing.
    if staged.is_empty() && deletions.is_empty() && chmod.is_none() {
        if o.verbose {
            report(&lines);
        }
        return Ok(finish(&[]));
    }

    // --- write path: serialize the read-modify-write through the coordinator.
    // Hold the lock across a FRESH re-read of the on-disk index and the write, so
    // a concurrent writer's changes to other paths are not clobbered — only the
    // paths this invocation touches are replaced.
    let _lock = crate::lock::RepoLock::acquire(repo.git_dir());
    let mut index = open_index(repo)?;

    // Write the blobs only now — after the pathspec check has passed — so a bad
    // pathspec leaves the object database byte-identical, the way git does.
    // The id recorded in the index is the one the write returned, so the two can
    // never disagree even if the file changed since the scan hashed it.
    for s in &mut staged {
        // A gitlink has no blob and no worktree bytes: its id is the submodule's
        // HEAD commit, which lives in the submodule's own object database. Reading
        // the path would just be a read of a directory.
        if s.mode == Mode::COMMIT {
            continue;
        }
        s.id = if s.intent {
            repo.write_blob(b"")?.detach()
        } else {
            let abs = repo.workdir_path(&s.path).expect("path came from this worktree");
            let md = gix::index::fs::Metadata::from_path_no_follow(&abs)?;
            let (bytes, mode) = read_converted_bytes(repo, &mut filters, s.path.as_ref(), &abs, &md)?;
            s.mode = mode;
            repo.write_blob(&bytes)?.detach()
        };
    }

    // Drop every prior version (any stage) of a staged path and every deletion,
    // then append the fresh stage-0 entries and restore sort order.
    let remove: HashSet<BString> = staged_set.iter().cloned().chain(deletions.iter().cloned()).collect();
    index.remove_entries(|_, path, _| remove.contains(&path.to_owned()));
    for s in &staged {
        // `EXTENDED` has to accompany `INTENT_TO_ADD` or the flag is dropped on
        // write: gix only emits the extended-flag word, and picks index v3, when
        // that bit is set (gix-index/src/entry/write.rs:27, write.rs:148).
        let flags = if s.intent {
            Flags::INTENT_TO_ADD | Flags::EXTENDED
        } else {
            Flags::empty()
        };
        index.dangerously_push_entry(s.stat, s.id, flags, s.mode, s.path.as_ref());
    }
    index.sort_entries();

    // `chmod_pathspec()` (builtin/add.c:601-602) runs last, over the cache staging
    // has already updated, and never contributes to the verbose report. One
    // implementation serves both verbs — see [`super::add::apply_chmod_pathspec`].
    // The matcher has to be rebuilt against THIS index (the locked re-read), which
    // is why it is not the `ps` the scan used.
    let chmod_errors = if chmod.is_none() {
        Vec::new()
    } else {
        // The matcher borrows the index, so its answers are collected and it is
        // dropped before the flip asks for the index exclusively.
        let selected: HashSet<BString> = {
            let mut matcher = repo.pathspec(
                true,
                &patterns,
                false,
                &index,
                gix::worktree::stack::state::attributes::Source::IdMapping,
            )?;
            let backing = index.path_backing();
            index
                .entries()
                .iter()
                .map(|e| e.path_in(backing).to_owned())
                .filter(|p| matcher.is_included(p.as_bstr(), Some(false)))
                .collect()
        };
        super::add::apply_chmod_pathspec(&mut index, chmod, include_sparse, sparsity.as_ref(), |p| {
            selected.contains(&p.to_owned())
        })
    };

    // The tree-cache extension is written verbatim by `File::write`; drop it after
    // mutating entries so a later commit can't capture a stale subtree.
    index.remove_tree();
    index.write(gix::index::write::Options::default())?;
    super::add::record_stage_event(repo, staged.len() + deletions.len());

    if o.verbose {
        report(&lines);
    }
    Ok(finish(&chmod_errors))
}

/// git prints both of these lines for each unreadable file and keeps going; only
/// after the whole run does it decide between dying (128) and, under
/// `--ignore-errors`, reporting the skips with exit 1 — which is
/// [`super::add::finish_code`], shared with `add`.
///
/// `index_path()` renders the reason with `strerror(errno)` alone; Rust appends a
/// ` (os error N)` tail, which is stripped here for the same reason `add` strips it.
/// `tracked` records which of git's two callers this failure belongs to, because
/// they die with different words — see [`ReadErrors`].
fn report_read_error(
    path: &BStr,
    err: &dyn std::fmt::Display,
    tracked: bool,
    errors: &mut ReadErrors,
) {
    eprintln!("error: open(\"{path}\"): {}", super::add::strip_os_error(&err.to_string()));
    eprintln!("error: unable to index file '{path}'");
    errors.any = true;
    errors.tracked |= tracked;
}

/// Which of git's two indexing callers reported a failure.
///
/// `update_callback()` handles the tracked paths and dies `updating files failed`
/// (read-cache.c:3993-3995); `add_files()` handles the untracked ones and dies
/// `adding files failed` (builtin/add.c:363-365). `add_files_to_cache()` runs first
/// (builtin/add.c:590), so a tracked failure always reaches its die ahead of an
/// untracked one.
#[derive(Default)]
struct ReadErrors {
    any: bool,
    tracked: bool,
}


/// The `-n`/`-v` report, in the order `cmd_add()` produces it: two passes, not one
/// sorted run.
///
/// `add_files_to_cache()` goes first and walks the *index* — `run_diff_files()`
/// iterates the cache, and `update_callback()` prints `add`/`remove` as it goes
/// (read-cache.c:3993-4006) — so the matched tracked paths come out in cache order
/// with adds and removes interleaved. `add_files()` runs second over the sorted
/// `dir->entries` (builtin/add.c:356-370) and prints the brand-new ones after all
/// of them.
///
/// Verified against git 2.55.0: over a modified `m.txt` and `z.txt`, a deleted
/// `d.txt` and new `a_new.txt`/`n_new.txt`, `git add -n -A` prints
/// `remove 'd.txt'`, `add 'm.txt'`, `add 'z.txt'`, `add 'a_new.txt'`,
/// `add 'n_new.txt'` — the new files last. Merging the two groups into one sorted
/// map, which this verb used to do, floats `a_new.txt` to the top instead.
fn report_lines(
    index: &gix::index::File,
    tracked: &HashMap<BString, (gix::hash::ObjectId, Mode)>,
    touched: &HashMap<BString, &'static str>,
) -> Vec<String> {
    let mut lines = Vec::with_capacity(touched.len());
    let backing = index.path_backing();
    for e in index.entries() {
        if e.stage() != Stage::Unconflicted {
            continue;
        }
        let path = e.path_in(backing).to_owned();
        if let Some(kind) = touched.get(&path) {
            lines.push(format!("{kind} '{path}'"));
        }
    }
    let mut fresh: Vec<&BString> = touched.keys().filter(|p| !tracked.contains_key(*p)).collect();
    fresh.sort();
    for path in fresh {
        lines.push(format!("add '{path}'"));
    }
    lines
}

fn report(lines: &[String]) {
    for line in lines {
        println!("{line}");
    }
}

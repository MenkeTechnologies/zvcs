//! git's parse-time validation of a diff option's *value*.
//!
//! Every command that takes `<common-diff-options>` — `diff`, `diff-index`,
//! `diff-files`, `diff-tree`, `log`, `show`, `whatchanged` — runs its argument
//! list through the one option table `diff.c:diff_opt_parse()` builds. Several of
//! those options validate their value in a callback and `error()` out, which
//! parse-options turns into exit 129, *before* the command resolves a single
//! revision. A port that merely records the option and moves on answers `0` where
//! git answers `129`, and the difference shows up on any argument list that mixes
//! a malformed value with an otherwise-valid invocation.
//!
//! The checks live here rather than in each subcommand because there is exactly
//! one option table in git and six callers of it here; a per-command copy would
//! drift the first time one of them was corrected.
//!
//! What is *not* here: options whose value git accepts unconditionally, and
//! options a specific command rejects for its own reasons. This module answers
//! only "would `diff.c`'s callback have called `error()` on this argument's
//! value", so a caller can print the same line and exit 129 at the same point.
//!
//! Two callbacks need more than a yes/no answer — the caller has to act on the
//! value it accepted — so their `diff.c` function is ported here in full and
//! [`reject`] delegates to it: [`parse_algorithm_value`] and, in `objname`,
//! `diff_opt_find_object()`.

use gix::diff::blob::Algorithm;

/// The exact bytes `diff_opt_diff_algorithm()` writes for an unrecognized value,
/// without its trailing newline. `error()` from an option callback is
/// parse-options' `PARSE_OPT_ERROR`: exit 129 with no usage block after it.
pub(super) const DIFF_ALGORITHM_ERR: &str =
    "error: option diff-algorithm accepts \"myers\", \"minimal\", \"patience\" and \"histogram\"";

/// `parse_algorithm_value()` (`diff.c`), the one table `--diff-algorithm=<v>`, the
/// separated `--diff-algorithm <v>` form, the `--minimal`/`--patience`/`--histogram`
/// aliases and `diff.algorithm` all read:
///
/// ```c
/// static int parse_algorithm_value(const char *value)
/// {
///         if (!value)
///                 return -1;
///         else if (!strcasecmp(value, "myers") || !strcasecmp(value, "default"))
///                 return 0;
///         else if (!strcasecmp(value, "minimal"))
///                 return XDF_NEED_MINIMAL;
///         else if (!strcasecmp(value, "patience"))
///                 return XDF_PATIENCE_DIFF;
///         else if (!strcasecmp(value, "histogram"))
///                 return XDF_HISTOGRAM_DIFF;
///         return -1;
/// }
/// ```
///
/// `strcasecmp`, so the value is matched case-insensitively, and `default` is git's
/// spelling for Myers (`xdl_opts` stay at 0). All four names have a port behind
/// them: `src/ported/gix-imara-diff/src/patience.rs` is `xdiff/xpatience.c` and
/// `lib.rs` wires it as [`Algorithm::Patience`], alongside `MyersMinimal` and
/// `Histogram`.
///
/// `None` is git's `-1`, which every caller answers with [`DIFF_ALGORITHM_ERR`].
/// [`reject`] is the parse-time half of this same call, so it delegates here rather
/// than repeating the name list.
pub(super) fn parse_algorithm_value(value: &str) -> Option<Algorithm> {
    match value.to_ascii_lowercase().as_str() {
        "myers" | "default" => Some(Algorithm::Myers),
        "minimal" => Some(Algorithm::MyersMinimal),
        "patience" => Some(Algorithm::Patience),
        "histogram" => Some(Algorithm::Histogram),
        _ => None,
    }
}

/// The callbacks in this module that roll their own `strtol(arg, &end, 10)`
/// instead of using `OPT_INTEGER`, as [`crate::optint::strtol_all`] describes
/// them: `Some(v)` when the whole string was consumed, `None` when a byte was
/// left over — git's `if (*end)`.
///
/// The empty string is the one case `strtol_all` and git disagree on: C leaves
/// `end` at the terminating NUL, so `*end` is false and git *accepts* it, which
/// is why `--stat-width=` and `--unified=` are not errors while
/// `--stat-width=abc` is. Overflow is not an error either — `strtol` saturates
/// and none of these callbacks looks at `errno`.
fn strtol10(s: &str) -> Result<i64, ()> {
    if s.is_empty() {
        return Ok(0);
    }
    crate::optint::strtol_all(s).ok_or(())
}

/// Would `diff.c` have rejected this argument's value?
///
/// Returns the exact `error:` line git writes — newline excluded — for a caller
/// to print before exiting 129. `None` means git accepts the argument, whether or
/// not the caller can then honour it.
pub(super) fn reject(arg: &str) -> Option<String> {
    // `--stat[=<width>[,<name-width>[,<count>]]]`: three `strtoul`s separated by
    // commas, and any byte left over after them fails as a whole.
    if let Some(v) = arg.strip_prefix("--stat=") {
        let mut rest = v;
        for _ in 0..3 {
            if strtol10(rest).is_ok() {
                return None;
            }
            // git re-enters `strtoul` only past a comma; anything else is garbage.
            match consumed_then_comma(rest) {
                Some(next) => rest = next,
                None => return Some(format!("error: invalid --stat value: {v}")),
            }
        }
        return Some(format!("error: invalid --stat value: {v}"));
    }

    // The `--stat-*` family: one `strtoul` each, and the option's own long name in
    // the message (`diff_opt_stat`, which uses `opt->long_name`, not `optname()`).
    for name in ["stat-width", "stat-name-width", "stat-graph-width", "stat-count"] {
        if let Some(v) = arg.strip_prefix(&format!("--{name}=")) {
            return strtol10(v)
                .is_err()
                .then(|| format!("error: {name} expects a numerical value"));
        }
    }

    // `-U<n>` / `--unified=<n>` (`diff_opt_unified`). Both are `PARSE_OPT_OPTARG`,
    // so only the attached form carries a value at all — a separated `-U 5` leaves
    // the `5` to be read as a revision, and is not this module's business.
    let unified = arg
        .strip_prefix("--unified=")
        .or_else(|| arg.strip_prefix("-U").filter(|v| !v.is_empty()));
    if let Some(v) = unified {
        return match strtol10(v) {
            Err(()) => Some("error: --unified expects a numerical value".to_string()),
            Ok(n) if n < 0 => {
                Some("error: --unified expects a non-negative integer".to_string())
            }
            Ok(_) => None,
        };
    }

    // `--word-diff=<mode>` (`diff_opt_word_diff`), matched exactly — `PLAIN` is as
    // invalid as `v1`.
    if let Some(v) = arg.strip_prefix("--word-diff=") {
        return (!matches!(v, "plain" | "color" | "porcelain" | "none"))
            .then(|| format!("error: bad --word-diff argument: {v}"));
    }

    // `--diff-algorithm=<value>`, decided by the one [`parse_algorithm_value`] port.
    if let Some(v) = arg.strip_prefix("--diff-algorithm=") {
        return parse_algorithm_value(v).is_none().then(|| DIFF_ALGORITHM_ERR.to_string());
    }

    // `--submodule=<format>` (`parse_submodule_params`), matched exactly. Bare
    // `--submodule` defaults to `log` and never fails.
    if let Some(v) = arg.strip_prefix("--submodule=") {
        return (!matches!(v, "log" | "short" | "diff"))
            .then(|| format!("error: failed to parse --submodule option parameter: '{v}'"));
    }

    // `--color=<when>` (`OPT_COLOR_FLAG`), matched case insensitively against
    // exactly three words — `true`, `1` and the empty string are all rejected.
    if let Some(v) = arg.strip_prefix("--color=") {
        return (!matches!(v.to_ascii_lowercase().as_str(), "always" | "auto" | "never"))
            .then(|| "error: option `color' expects \"always\", \"auto\", or \"never\"".to_string());
    }

    // `--inter-hunk-context=<n>`: parse-options' unsigned arm over a `u32` target,
    // which `crate::optint` already speaks — base 0, a k/m/g multiplier, and one
    // message per failure mode.
    if let Some(v) = arg.strip_prefix("--inter-hunk-context=") {
        let name = crate::optint::long_opt("inter-hunk-context");
        return crate::optint::unsigned_prec(&name, v, 4)
            .err()
            .map(|e| format!("error: {}", e.message()));
    }

    // `-M`/`--find-renames` and `-C`/`--find-copies` (`diff_opt_find_renames`,
    // `diff_opt_find_copies`): a similarity score, and every byte of the value has
    // to be part of it. Bare `-M` passes `""`, which scores fine.
    for (long, short) in [("find-renames", "-M"), ("find-copies", "-C")] {
        if let Some(v) = value_of(arg, long, short) {
            let (_, rest) = super::diffcore_rename::parse_rename_score(v);
            return (!rest.is_empty()).then(|| format!("error: invalid argument to {long}"));
        }
    }

    // `-B`/`--break-rewrites` (`diff_opt_break_rewrites`): the same score, twice,
    // separated by a slash. Its own message names the form rather than the option.
    if let Some(v) = value_of(arg, "break-rewrites", "-B") {
        return super::diffcore_rename::parse_break_opt(v)
            .is_err()
            .then(|| "error: break-rewrites expects <n>/<m> form".to_string());
    }

    // `--output-indicator-{new,old,context}` (`diff_opt_char`): git stores one
    // `char`, and rejects anything whose second byte is not the terminator — so an
    // empty value is accepted and a multi-byte character is not.
    for name in ["output-indicator-new", "output-indicator-old", "output-indicator-context"] {
        if let Some(v) = arg.strip_prefix(&format!("--{name}=")) {
            return (v.len() > 1)
                .then(|| format!("error: {name} expects a character, got '{v}'"));
        }
    }

    None
}

/// The value of an option spelled either `--<long>=<v>` or `-<X><v>`, when it
/// carries one. Bare `--<long>` and bare `-<X>` are `PARSE_OPT_OPTARG` forms that
/// git treats as an empty value, which every caller here accepts, so they are
/// reported as `Some("")` rather than skipped.
fn value_of<'a>(arg: &'a str, long: &str, short: &str) -> Option<&'a str> {
    if arg == short {
        return Some("");
    }
    if let Some(v) = arg.strip_prefix(short) {
        return Some(v);
    }
    if arg == format!("--{long}") {
        return Some("");
    }
    arg.strip_prefix(&format!("--{long}="))
}

/// The `--dirstat` family's own value check, which is a `die()` rather than the
/// `error()` every callback in [`reject`] raises — so it exits 128 and its text
/// spans two lines.
///
/// `diff_opt_dirstat()` (diff.c) hands the value to `parse_dirstat_params()` and
/// dies on whatever that could not read, listing each offending parameter. It runs
/// while the option list is being read, so it precedes a command's own "no
/// tree-ish given" usage error: `git diff-tree --dirstat=bogus` reports the bad
/// parameter, not the synopsis.
///
/// Returns the whole `fatal:` text — trailing newline included, matching git's
/// blank line after the parameter list — for a caller to print before exiting 128.
pub(super) fn dirstat_reject(arg: &str) -> Option<String> {
    let value = ["--dirstat=", "--dirstat-by-file=", "-X"]
        .iter()
        .find_map(|p| arg.strip_prefix(p))?;
    // A bare `-X` carries no value, and `--dirstat-by-file` is `files` already.
    if value.is_empty() {
        return None;
    }
    let mut ignored = super::diff_files::DirStat::default();
    let errors = super::diff_files::parse_dirstat_params(value, &mut ignored);
    (!errors.is_empty())
        .then(|| format!("fatal: Failed to parse --dirstat/-X option parameter:\n{errors}\n"))
}

/// `diff_opt_pickaxe_string()` and `diff_opt_pickaxe_regex()`'s one refusal
/// (`diff.c:5888` and `diff.c:5901`):
///
/// ```c
/// options->pickaxe = arg;
/// options->pickaxe_opts |= DIFF_PICKAXE_KIND_S;
/// if (arg && !*arg)
///         return error(_("-S requires a non-empty argument"));
/// ```
///
/// `error()` from a callback is parse-options' `PARSE_OPT_ERROR`: exit 129 with no
/// usage block, raised at the flag's own argv position — so it beats
/// `diff_setup_done()`'s conflict `die()`s, which only run once the scan is over,
/// and loses to a positional standing in front of it. Note the kind bit is set
/// *before* the refusal, which is why `git diff -S '' -Gx` still reports the empty
/// argument rather than the two-kind conflict.
pub(super) fn pickaxe_empty(kind: u8) -> String {
    format!("error: -{} requires a non-empty argument", kind as char)
}

/// git's `diff_setup_done()` check that at most one of `-G`, `-S` and
/// `--find-object` was given (`diff.c`, `HAS_MULTI_BITS(pickaxe_opts &
/// DIFF_PICKAXE_KINDS_MASK)`), which is a `die()` — exit 128 — rather than the
/// usage error every value check above produces.
///
/// Whole-argv rather than per-argument, because it is the *combination* that
/// fails; git runs it once, after the option scan and before any diff is walked.
pub(super) const PICKAXE_CONFLICT: &str =
    "fatal: options '-G', '-S', and '--find-object' cannot be used together";

/// The next `diff_setup_done()` `die()` in the same family
/// (`HAS_MULTI_BITS(pickaxe_opts & DIFF_PICKAXE_KINDS_ALL_OBJFIND_MASK)`, diff.c:5271):
///
/// ```c
/// die(_("options '%s' and '%s' cannot be used together, use '%s' with '%s' and '%s'"),
///     "--pickaxe-all", "--find-object", "--pickaxe-all", "-G", "-S");
/// ```
///
/// `--pickaxe-all` keeps every pair once *one* matched, which `pickaxe_match()`'s
/// objfind branch has no notion of — so git refuses the combination rather than
/// picking a meaning for it. Exit 128, after the whole option scan.
pub(super) const PICKAXE_ALL_OBJFIND_CONFLICT: &str =
    "fatal: options '--pickaxe-all' and '--find-object' cannot be used together, \
     use '--pickaxe-all' with '-G' and '-S'";

/// Whether `args` names more than one pickaxe kind, i.e. whether git would die
/// with [`PICKAXE_CONFLICT`]. Stops at `--`, which ends option parsing.
pub(super) fn pickaxe_conflict(args: &[String]) -> bool {
    let mut kinds = 0u32;
    for a in args {
        if a == "--" {
            break;
        }
        let kind = if a == "-S" || a.starts_with("-S") {
            1
        } else if a == "-G" || a.starts_with("-G") {
            2
        } else if a == "--find-object" || a.starts_with("--find-object=") {
            4
        } else {
            continue;
        };
        kinds |= kind;
    }
    kinds.count_ones() > 1
}

/// For `--stat=`: the tail after the number `strtol` just read, when the byte it
/// stopped on is a comma. `None` when it stopped on anything else, which is the
/// failure git reports.
fn consumed_then_comma(s: &str) -> Option<&str> {
    let b = s.as_bytes();
    let mut i = 0;
    while i < b.len() && matches!(b[i], b' ' | b'\t' | b'\n' | 0x0b | 0x0c | b'\r') {
        i += 1;
    }
    if matches!(b.get(i), Some(b'-') | Some(b'+')) {
        i += 1;
    }
    let start = i;
    while i < b.len() && b[i].is_ascii_digit() {
        i += 1;
    }
    // No digits means `strtoul` never advanced; git's `end` is still at the start.
    if i == start {
        i = 0;
    }
    (b.get(i) == Some(&b',')).then(|| &s[i + 1..])
}

#[cfg(test)]
mod tests {
    use super::reject;

    /// The values stock git 2.55.0 accepts, verified by running each one against
    /// `git diff-tree <value> HEAD` and observing exit 0.
    #[test]
    fn accepts_what_git_accepts() {
        for arg in [
            "--stat=",
            "--stat=5",
            "--stat= 5",
            "--stat=+5",
            "--stat=-0",
            "--stat=1,2",
            "--stat=1,2,3",
            "--stat-width=",
            "--stat-width=0",
            "--stat-width=-1",
            "--stat-width=99999999999999999999999999",
            "--stat-count=-5",
            "--unified=",
            "--unified=0",
            "--unified=10",
            "--unified=99999999999999999999999999",
            "-U0",
            "-U",
            "--word-diff=plain",
            "--word-diff=color",
            "--word-diff=porcelain",
            "--word-diff=none",
            "--diff-algorithm=myers",
            "--diff-algorithm=MYERS",
            "--diff-algorithm=default",
            "--diff-algorithm=histogram",
            "--submodule=short",
            "--submodule=log",
            "--submodule=diff",
            "--find-renames=",
            "--find-renames=50",
            "--find-renames=50%",
            "--find-renames=0.5",
            "-M",
            "-M50",
            "-M50%",
            "-C",
            "-C50%",
            "--break-rewrites=",
            "--break-rewrites=50",
            "--break-rewrites=50/60",
            "-B",
            "-B50/60",
            "--output-indicator-new=",
            "--output-indicator-new=x",
            "--output-indicator-new=>",
            "--color=always",
            "--color=NEVER",
            "--color=auto",
            "--inter-hunk-context=0",
            "--inter-hunk-context=1",
            "--inter-hunk-context=0x10",
            "--inter-hunk-context=1k",
            // Not a value-carrying option this module knows: never its call.
            "--raw",
            "-p",
        ] {
            assert_eq!(reject(arg), None, "{arg} should be accepted");
        }
    }

    /// The pickaxe kinds are mutually exclusive, and the check ends at `--`.
    #[test]
    fn pickaxe_kinds_conflict_only_when_they_differ() {
        use super::pickaxe_conflict;
        let v = |xs: &[&str]| xs.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert!(pickaxe_conflict(&v(&["-Gfn", "-Sfn"])));
        assert!(pickaxe_conflict(&v(&["-Sfn", "--find-object=HEAD"])));
        assert!(pickaxe_conflict(&v(&["-G", "x", "--find-object", "y"])));
        // One kind, however often it is repeated, is not a conflict.
        assert!(!pickaxe_conflict(&v(&["-Gfn", "-Gother"])));
        assert!(!pickaxe_conflict(&v(&["-Sfn"])));
        assert!(!pickaxe_conflict(&v(&[])));
        // Past `--` these are paths, not options.
        assert!(!pickaxe_conflict(&v(&["-Gfn", "--", "-Sfn"])));
    }

    /// The rejections, with the exact line stock git 2.55.0 printed for each.
    #[test]
    fn rejects_with_gits_own_message() {
        for (arg, want) in [
            ("--stat=5 ", "error: invalid --stat value: 5 "),
            ("--stat=5x", "error: invalid --stat value: 5x"),
            ("--stat=0x10", "error: invalid --stat value: 0x10"),
            ("--stat=1,2,3x", "error: invalid --stat value: 1,2,3x"),
            ("--stat-width=abc", "error: stat-width expects a numerical value"),
            ("--stat-width=0x10", "error: stat-width expects a numerical value"),
            ("--stat-width=1k", "error: stat-width expects a numerical value"),
            ("--stat-count=abc", "error: stat-count expects a numerical value"),
            (
                "--stat-graph-width=abc",
                "error: stat-graph-width expects a numerical value",
            ),
            (
                "--stat-name-width=abc",
                "error: stat-name-width expects a numerical value",
            ),
            ("--unified==", "error: --unified expects a numerical value"),
            ("--unified=abc", "error: --unified expects a numerical value"),
            ("--unified=0x10", "error: --unified expects a numerical value"),
            ("--unified=-1", "error: --unified expects a non-negative integer"),
            ("-Uabc", "error: --unified expects a numerical value"),
            ("-U-1", "error: --unified expects a non-negative integer"),
            ("--word-diff=v1", "error: bad --word-diff argument: v1"),
            ("--word-diff=PLAIN", "error: bad --word-diff argument: PLAIN"),
            ("--word-diff=", "error: bad --word-diff argument: "),
            (
                "--diff-algorithm=bogus",
                "error: option diff-algorithm accepts \"myers\", \"minimal\", \"patience\" and \"histogram\"",
            ),
            (
                "--diff-algorithm=",
                "error: option diff-algorithm accepts \"myers\", \"minimal\", \"patience\" and \"histogram\"",
            ),
            (
                "--submodule==",
                "error: failed to parse --submodule option parameter: '='",
            ),
            (
                "--submodule=",
                "error: failed to parse --submodule option parameter: ''",
            ),
            ("--find-renames=abc", "error: invalid argument to find-renames"),
            ("--find-renames=50x", "error: invalid argument to find-renames"),
            ("--find-renames=0x10", "error: invalid argument to find-renames"),
            ("-Mabc", "error: invalid argument to find-renames"),
            ("--find-copies=abc", "error: invalid argument to find-copies"),
            ("-Cabc", "error: invalid argument to find-copies"),
            ("--break-rewrites=abc", "error: break-rewrites expects <n>/<m> form"),
            ("--break-rewrites=50/abc", "error: break-rewrites expects <n>/<m> form"),
            ("--break-rewrites=0x10", "error: break-rewrites expects <n>/<m> form"),
            ("-Babc", "error: break-rewrites expects <n>/<m> form"),
            ("-B50/abc", "error: break-rewrites expects <n>/<m> form"),
            (
                "--output-indicator-new=abc",
                "error: output-indicator-new expects a character, got 'abc'",
            ),
            (
                "--output-indicator-old=abc",
                "error: output-indicator-old expects a character, got 'abc'",
            ),
            (
                "--output-indicator-context=abc",
                "error: output-indicator-context expects a character, got 'abc'",
            ),
            ("--color=abc", "error: option `color' expects \"always\", \"auto\", or \"never\""),
            ("--color=", "error: option `color' expects \"always\", \"auto\", or \"never\""),
            ("--color=true", "error: option `color' expects \"always\", \"auto\", or \"never\""),
            (
                "--inter-hunk-context=",
                "error: option `inter-hunk-context' expects a numerical value",
            ),
            (
                "--inter-hunk-context=abc",
                "error: option `inter-hunk-context' expects a non-negative integer value with an optional k/m/g suffix",
            ),
            (
                "--inter-hunk-context=-1",
                "error: option `inter-hunk-context' expects a non-negative integer value with an optional k/m/g suffix",
            ),
            (
                "--inter-hunk-context=99999999999999999999999999",
                "error: value 99999999999999999999999999 for option `inter-hunk-context' not in range [0,4294967295]",
            ),
        ] {
            assert_eq!(reject(arg).as_deref(), Some(want), "for {arg}");
        }
    }
}

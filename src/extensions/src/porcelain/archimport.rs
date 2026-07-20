//! `git archimport` — import a GNU Arch repository into git.
//! **The import itself is not ported: every path that would read an Arch
//! archive bails.**
//!
//! Stock `git-archimport` is a ~1100-line Perl script (`$(git --exec-path)/
//! git-archimport`, `#!/usr/bin/perl` on line 1). It does no git object work of
//! its own worth speaking of; it drives the external GNU Arch client — `my $TLA
//! = $ENV{'ARCH_CLIENT'} || 'tla';` at line 126 — through `tla abrowse`,
//! `tla cat-archive-log`, `tla get`, `tla replay` and friends, and pipes the
//! resulting trees into `git fast-import`-era plumbing. There is no Arch
//! archive reader anywhere in the vendored crates under `src/ported/`: nothing
//! parses `.arch-ids`, patch-log files, changesets, or the
//! `archive,category--branch--version` namespace. Reimplementing `tla` is the
//! prerequisite, and it is out of scope for a gitoxide-backed port.
//!
//! What *is* ported is the surface that is byte-verifiable without an archive:
//! the whole command line, which the script hands to `Getopt::Std::getopts`
//! with the spec `"fThvat:D:"` (line 87), and the three checks that follow it
//! (lines 87-90). All of it was checked against git 2.55.0 on Darwin.
//!
//! `getopts` is not `parse_options`, so the diagnostics differ from most git
//! commands — no `error: unknown option`, no abbreviation matching, no
//! `fatal:` prefix. Reproduced exactly, against Getopt::Std 1.13
//! (`/System/Library/Perl/5.34/Getopt/Std.pm`, `sub getopts` at line 234):
//!
//!   * `-h`, no arguments at all, and a trailing `-t`/`-D` with no value (which
//!     sets `$errs` at line 253) → the 192-byte usage block on **stderr**,
//!     exit 1. All three funnel through the script's `usage()` (line 78), which
//!     ends in `exit(1)`.
//!   * Every unrecognised switch letter → `Unknown option: <c>` on stderr
//!     (line 295), one line per letter, *then* the usage block, exit 1. The
//!     scan continues after each unknown letter, so `-Zq` prints two lines.
//!   * Option clustering and the two value forms: `-vfT`, `-t<dir>` and
//!     `-t <dir>`, `-D<n>` and `-D <n>`, with `$ARGV[0] = "-$rest"` re-splicing
//!     the tail of a cluster (line 278).
//!   * `--` ends option scanning and is removed (line 244); a bare `-` fails
//!     the `/^-(.)(.*)/s` match and ends scanning as an ordinary argument;
//!     scanning also stops at the first non-`-` argument, so `foo/bar -v`
//!     leaves `-v` as an Arch branch spec rather than a flag.
//!   * `-o`, which the man page still lists in the SYNOPSIS but which is absent
//!     from the `"fThvat:D:"` spec, is therefore an unknown option and prints
//!     `Unknown option: o`.
//!
//! NOT reproduced — these `bail!` rather than pretending to have imported:
//!
//!   1. **The import.** Requires a GNU Arch client; see above.
//!   2. **The `Initial import needs an empty current working directory.` check**
//!      (line 109) and the `Problems with tla abrowse` failure (line 134). Both
//!      are bare Perl `die`s, so their exit status is `$!` — the lingering
//!      errno, observed as 2 (`ENOENT`) here only because the failed
//!      `exec("tla")` set it. That is not a value this module can reproduce
//!      portably, and the second one is unreachable without `tla` anyway.
//!   3. **`--help` / `--version`.** `-` is not in the option spec, so
//!      `getopts` routes both to its own `help_mess`/`version_mess` handlers
//!      (lines 283-294) — but the `git` wrapper intercepts `--help` and opens
//!      the man page before the script ever runs, so the script's own handling
//!      is dead code under a `git archimport` invocation.

use anyhow::{bail, Result};
use std::collections::VecDeque;
use std::process::ExitCode;

/// The usage block `archimport` writes to stderr: 192 bytes, 3 lines.
const USAGE: &str = concat!(
    "usage: git archimport     # fetch/update GIT from Arch\n",
    "       [ -h ] [ -v ] [ -o ] [ -a ] [ -f ] [ -T ] [ -D depth ] [ -t tempdir ]\n",
    "       repository/arch-branch [ repository/arch-branch] ...\n",
);

/// `git archimport` — import a GNU Arch repository into git.
///
/// Parses the command line exactly as the Perl script's
/// `getopts("fThvat:D:")` does and reproduces every path that terminates
/// before an Arch archive is touched. Anything that would actually import
/// bails, naming the missing substrate.
pub fn archimport(args: &[String]) -> Result<ExitCode> {
    // The dispatcher passes the argument tail, but tolerate the subcommand
    // being present at index 0 so both calling conventions behave the same.
    let args = match args.first() {
        Some(a) if a == "archimport" => &args[1..],
        _ => args,
    };

    // `@ARGV`, owned: `getopts` both shifts from the front and rewrites the
    // head in place when a cluster still has letters left.
    let mut argv: VecDeque<String> = args.iter().cloned().collect();

    let mut errs = 0usize;
    let mut opt_h = false;

    while let Some(head) = argv.front().cloned() {
        // Perl's `/^-(.)(.*)/s`: a leading `-` plus at least one more
        // character. A bare `-`, or any non-`-` word, ends the scan.
        let mut chars = head.chars();
        if chars.next() != Some('-') {
            break;
        }
        let Some(first) = chars.next() else {
            break;
        };
        let rest: String = chars.collect();

        // `if (/^--$/) { shift @ARGV; last }` — checked before the spec lookup.
        if head == "--" {
            argv.pop_front();
            break;
        }

        if takes_value(first) {
            argv.pop_front();
            let value = if rest.is_empty() {
                // `++$errs unless @ARGV; $rest = shift(@ARGV);` — a missing
                // value is recorded as an error but prints no diagnostic of
                // its own; only the usage block is emitted, at the end.
                if argv.is_empty() {
                    errs += 1;
                }
                argv.pop_front()
            } else {
                Some(rest)
            };
            // `-t <tempdir>` would set `$ENV{TMPDIR}` and `-D <depth>` would
            // bound the merge-ancestry walk; both only matter to the import,
            // which bails below, so the value is parsed and discarded.
            let _ = value;
        } else if is_flag(first) {
            if first == 'h' {
                opt_h = true;
            }
            // `-v`, `-f`, `-T`, `-a` only steer the import; recorded as seen.
            splice_tail(&mut argv, &rest);
        } else {
            eprintln!("Unknown option: {first}");
            errs += 1;
            splice_tail(&mut argv, &rest);
        }
    }

    // The script's three post-parse checks, in order (lines 87-90):
    // `getopts(...) or usage();`, `usage if $opt_h;`, `@ARGV >= 1 or usage();`.
    if errs > 0 || opt_h || argv.is_empty() {
        return Ok(usage());
    }

    bail!(
        "unsupported command \"archimport\": importing from GNU Arch requires a tla/baz archive \
         reader (patch logs, changesets, the archive,category--branch--version namespace), which \
         no vendored crate under src/ported provides; stock git-archimport shells out to $ARCH_CLIENT"
    )
}

/// Whether `c` is a switch that consumes a value — the `t:`/`D:` of the
/// `"fThvat:D:"` spec.
fn takes_value(c: char) -> bool {
    matches!(c, 't' | 'D')
}

/// Whether `c` is a valueless switch in the `"fThvat:D:"` spec. Note the
/// absence of `o`, which the man page's SYNOPSIS still advertises.
fn is_flag(c: char) -> bool {
    matches!(c, 'f' | 'T' | 'h' | 'v' | 'a')
}

/// Consume the switch just read from the head of `argv`: drop the argument when
/// the cluster is exhausted, else put its remaining letters back as `-<rest>`.
/// This is `getopts`' shared tail at lines 274-279 and 297-302.
fn splice_tail(argv: &mut VecDeque<String>, rest: &str) {
    if rest.is_empty() {
        argv.pop_front();
    } else {
        argv[0] = format!("-{rest}");
    }
}

/// The script's `usage()` (line 78): the block on stderr, `exit(1)`.
fn usage() -> ExitCode {
    eprint!("{USAGE}");
    ExitCode::from(1)
}

//! `git cvsexportcommit` — export a single commit to a CVS checkout.
//!
//! Stock git ships this as a Perl script
//! (`git-core/git-cvsexportcommit`). It is a bridge to a *foreign* SCM: the
//! real work is driving the `cvs` binary (`cvs -q update`, `cvs status`,
//! `cvs add`, `cvs remove`, `cvs commit`), parsing `CVS/Entries` and
//! `CVS/Repository` in the checkout, and shelling out to `patch(1)` (with
//! `--fuzz=0` under `-p`) to apply the generated diff. None of that has any
//! substrate in gitoxide — there is no CVS client, no `CVS/Entries` reader,
//! and no `patch` hunk applier in the vendored `gix*` crates, and the
//! command's observable effect is on a CVS working copy rather than on the
//! git repository at all.
//!
//! Rather than fabricate an implementation that would diverge the moment it
//! touched a real checkout, this module is an honest skeleton:
//!
//! Covered:
//!   * `-h` — the exact usage line stock prints on stderr, exit status 1.
//!   * The no-argument path — stock's `Need at least one commit identifier!`
//!     die, exit status 255.
//!   * `getopts('uhPpvcfkam:d:w:W')` in full, including `Getopt::Std`'s
//!     behaviour on an unknown flag: it is *not* fatal. Each unrecognised
//!     character warns `Unknown option: <c>` on stderr and parsing continues
//!     with the next character of the same cluster, which is why
//!     `git cvsexportcommit --no-such-flag` prints eight of those lines — one
//!     per character outside the option set — and then, because the `h` in
//!     `--no-such-flag` *is* in the set, prints the usage line and exits 1.
//!
//! Not covered — every invocation that would actually export a commit
//! `bail!`s, naming the missing substrate. Nothing is attempted against the
//! CVS checkout, and no repository state is modified.
//!
//! Known divergence: stock's die message carries Perl's
//! ` at <script> line 21.` suffix, which names the Perl script's path; that
//! suffix is not reproduced here.

use anyhow::{bail, Result};
use std::io::Write;
use std::process::ExitCode;

/// The stock usage text, verbatim from `sub usage` in the Perl script.
const USAGE: &str = "usage: GIT_DIR=/path/to/.git git cvsexportcommit [-h] [-p] [-v] [-c] [-f] [-u] [-k] [-w cvsworkdir] [-m msgprefix] [ parent ] commit\n";

/// Options taking a value, per `getopts('uhPpvcfkam:d:w:W')`.
const VALUE_FLAGS: [char; 3] = ['m', 'd', 'w'];
/// Boolean options, per the same `getopts` spec.
const BOOL_FLAGS: [char; 10] = ['u', 'h', 'P', 'p', 'v', 'c', 'f', 'k', 'a', 'W'];

/// `git cvsexportcommit` — see the module docs for what is and is not covered.
pub fn cvsexportcommit(args: &[String]) -> Result<ExitCode> {
    let mut help = false;

    // `Getopt::Std::getopts('uhPpvcfkam:d:w:W')`, ported from its loop rather
    // than approximated, because the corpus reaches three of its quirks at once:
    // its loop guard is `$ARGV[0] =~ /^-(.)(.*)/s`, so scanning stops at the
    // first argument that is not `-<something>` — a bare `-`, or any operand —
    // and everything after it is an operand even if it looks like a flag; an
    // unknown character is a `warn`, not a death; and a value option whose value
    // is missing entirely records an error count nobody reads and prints
    // nothing.
    let mut rest: Vec<&String> = args.iter().collect();
    // The unconsumed tail of the current cluster, i.e. Perl's `$rest` written
    // back into `$ARGV[0]` as `-$rest`.
    let mut cluster = String::new();
    while let Some(front) = rest.first() {
        let body = if cluster.is_empty() {
            match front.strip_prefix('-') {
                Some(body) if !body.is_empty() => body.to_string(),
                // `-` alone, or an operand: the loop guard fails and option
                // scanning is over.
                _ => break,
            }
        } else {
            std::mem::take(&mut cluster)
        };
        // `if (/^--$/) { shift @ARGV; last; }`.
        if body == "-" {
            rest.remove(0);
            break;
        }

        let mut chars = body.chars();
        let first = chars.next().expect("the guard rejected an empty body");
        let tail: String = chars.collect();

        if VALUE_FLAGS.contains(&first) {
            // The option always consumes its argv entry; an attached value is
            // used as-is, otherwise the next entry is taken — and if there is
            // none, `getopts` only bumps its error count.
            rest.remove(0);
            if tail.is_empty() && !rest.is_empty() {
                rest.remove(0);
            }
            continue;
        }

        if BOOL_FLAGS.contains(&first) {
            if first == 'h' {
                help = true;
            }
        } else {
            // `warn "Unknown option: $first\n"` — a warning, not a death, so the
            // rest of the cluster is still parsed and a later valid flag (the
            // `h` in `--no-such-flag`) still takes effect.
            eprintln!("Unknown option: {first}");
        }

        // A flag consumes only its character: the remainder of the cluster is
        // pushed back for the next iteration, and an exhausted cluster consumes
        // the argv entry.
        if tail.is_empty() {
            rest.remove(0);
        } else {
            cluster = tail;
        }
    }
    let positionals = rest;

    if help {
        let mut err = std::io::stderr().lock();
        err.write_all(USAGE.as_bytes())?;
        err.flush()?;
        return Ok(ExitCode::from(1));
    }

    if positionals.is_empty() {
        eprintln!("Need at least one commit identifier!");
        return Ok(ExitCode::from(255));
    }

    bail!(
        "cvsexportcommit is not ported: requires a CVS client (cvs update/status/add/remove/commit), \
         a CVS/Entries reader, and a patch(1) hunk applier — none of which exist in gitoxide"
    )
}

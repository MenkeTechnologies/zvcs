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
//!   * Flag recognition for the full `getopts('uhPpvcfkam:d:w:W')` set, so an
//!     unknown flag is reported instead of silently ignored.
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
    let mut positionals: Vec<&str> = Vec::new();
    let mut iter = args.iter().peekable();
    let mut no_more_opts = false;

    while let Some(a) = iter.next() {
        if no_more_opts || a == "-" || !a.starts_with('-') {
            positionals.push(a);
            continue;
        }
        if a == "--" {
            no_more_opts = true;
            continue;
        }
        // Getopt::Std clusters short flags and accepts attached values.
        let mut chars = a[1..].chars();
        while let Some(c) = chars.next() {
            if VALUE_FLAGS.contains(&c) {
                let rest: String = chars.by_ref().collect();
                if rest.is_empty() && iter.next().is_none() {
                    bail!("option requires an argument -- {c}");
                }
                break;
            } else if BOOL_FLAGS.contains(&c) {
                if c == 'h' {
                    help = true;
                }
            } else {
                bail!("unsupported flag {:?} (ported: -h only)", format!("-{c}"));
            }
        }
    }

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

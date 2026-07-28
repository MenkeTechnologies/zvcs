//! `git annotate` — `git blame` in the CVS-compatible output format.

use anyhow::Result;
use std::process::ExitCode;

/// `git annotate` — line-by-line last-modifying commit in the CVS-compatible
/// output format (`builtin/blame.c`'s `OUTPUT_ANNOTATE_COMPAT` path).
///
/// `builtin/annotate.c` is the whole command:
///
/// ```c
/// strvec_pushl(&args, "annotate", "-c", NULL);
/// for (int i = 1; i < argc; i++)
///     strvec_push(&args, argv[i]);
/// ret = cmd_blame(args.nr, args_copy, prefix, repo);
/// ```
///
/// So `git annotate <args>` *is* `git blame -c <args>`, with `argv[0]` left as
/// `annotate` so `parse_options` renders the usage line under that name. This
/// port says the same thing: it splices `-c` in and calls
/// [`super::blame::blame_with`].
///
/// Delegating rather than reimplementing is the point. A separate annotate body
/// had diverged from blame's on the input it reads: blame lays the *working
/// tree* over the suspect commit (git's `fake_working_tree_commit`), while the
/// separate body blamed `HEAD:<path>` directly, so on a conflicted or dirty
/// worktree annotate silently dropped every uncommitted line — `git annotate
/// conflict.txt` mid-merge printed 1 line where stock printed 5. Every such
/// behaviour is now shared by construction instead of by two ports agreeing.
pub fn annotate(args: &[String]) -> Result<ExitCode> {
    // `args[0]` is the subcommand itself when dispatched; tolerate its absence.
    let rest = match args.first() {
        Some(a) if a == "annotate" => &args[1..],
        _ => args,
    };
    let mut spliced = Vec::with_capacity(rest.len() + 1);
    spliced.push("-c".to_string());
    spliced.extend_from_slice(rest);
    super::blame::blame_with(&spliced, "annotate")
}

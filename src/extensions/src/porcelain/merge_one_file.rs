//! `git merge-one-file` — the per-file merge helper `git merge-index` drives.
//!
//! Stock git ships this as a POSIX shell script (`git-merge-one-file.sh`,
//! Torvalds 2005) whose entire body is a `case` over the three input blob ids
//! followed by calls to git's own plumbing. This port keeps that shape: the
//! decision table, the messages and the file-system steps are native, and each
//! `git <plumbing>` step re-invokes this same binary (`current_exe()`) so the
//! already-ported `update-index`, `checkout-index`, `unpack-file` and
//! `merge-file` implementations do the work — exactly the commands, argument
//! vectors and exit-code propagation the script uses. Re-deriving index and
//! checkout logic here would be a second, divergent implementation of code that
//! already exists in this crate.
//!
//! Covered — the whole command, verified branch by branch against stock git:
//!   * the argument contract `<orig blob> <our blob> <their blob> <path>
//!     <orig mode> <our mode> <their mode>`, with empty strings for absent sides
//!   * `-h` as the first argument (usage on stdout, exit 0) and the wrong
//!     argument count (the same usage on stdout, exit 1) — including the fact
//!     that `git-sh-setup` prepends its own copy of the usage line, so the block
//!     git prints really does contain it twice
//!   * `cd_to_toplevel` / `require_work_tree`: a bare repository prints
//!     `fatal: this operation must be run in a work tree` followed by
//!     `Cannot chdir to $cdup, the toplevel of the working tree` and exits 1,
//!     before the argument count is ever checked
//!   * deleted in both / deleted in one and unchanged in the other, including
//!     the permission-change refusal (which, faithfully, also fires for the
//!     deleted-in-both case whenever the original mode is non-empty), the
//!     `Removing <path>` line, the `rm` + `rmdir -p` cleanup and the trailing
//!     `update-index --remove`
//!   * added on one side only (silent for ours, `Adding <path>` plus the
//!     `untracked <path> is overwritten by the merge` refusal for theirs)
//!   * added identically in both, with the `permissions conflict` refusal
//!   * modified in both (and added in both differently): the symlink and
//!     submodule refusals, `Auto-merging <path>` / `Added <path> in both, but
//!     differently.`, the three-way merge, `checkout-index --stage=2` followed
//!     by overwriting the worktree file with the merge result, temp-file
//!     cleanup, the combined `content conflict` / `permissions conflict` message
//!     and the final `update-index`
//!   * the unhandled-combination fallback
//!     `ERROR: <path>: Not handling case <o> -> <a> -> <b>`, exit 1
//!
//! Bug-for-bug faithful: when the base side is absent the script asks
//! `unpack-file` for the empty blob, which fails in a repository that has never
//! stored it. Stock git prints `fatal: unable to read blob object <oid>`, hands
//! `merge-file` an empty path, and lets that fail too — so the merge is reported
//! as a content conflict without a merge ever running. That path is reproduced
//! rather than papered over.
//!
//! Not covered: conflict-marker labels are the `.merge_file_XXXXXX` temp names,
//! whose six random characters differ run-to-run under stock git as well, so a
//! conflicted file's contents are only identical up to those two labels. The
//! merge itself is whatever the ported `merge-file` produces, carrying that
//! module's documented deviations from `xdl_merge`.

use anyhow::{anyhow, Result};
use std::path::Path;
use std::process::{Command, ExitCode, Stdio};

/// The usage block git prints, doubled exactly as `git-sh-setup` doubles it:
/// the script sets `LONG_USAGE`, and sourcing `git-sh-setup` prefixes it with a
/// freshly built `usage: <dashless> <USAGE>` line plus a blank line.
const LONG_USAGE: &str = "\
usage: git merge-one-file <orig blob> <our blob> <their blob> <path> <orig mode> <our mode> <their mode>

usage: git merge-one-file <orig blob> <our blob> <their blob> <path> <orig mode> <our mode> <their mode>

Blob ids and modes should be empty for missing files.";

/// The seven positional arguments, named as the script names them.
struct Args<'a> {
    /// `$1` — the merge base's blob id, empty when the path is absent there.
    orig: &'a str,
    /// `$2` — our blob id (index stage 2), empty when absent.
    ours: &'a str,
    /// `$3` — their blob id (index stage 3), empty when absent.
    theirs: &'a str,
    /// `$4` — the path, relative to the worktree root.
    path: &'a str,
    /// `$5` — the base's 6-digit octal mode, empty when absent.
    orig_mode: &'a str,
    /// `$6` — our mode, empty when absent.
    our_mode: &'a str,
    /// `$7` — their mode, empty when absent.
    their_mode: &'a str,
}

/// `git merge-one-file` — resolve one path after `git read-tree -m` left it
/// unmerged.
///
/// Exit codes match stock git: 0 when the path was resolved, 1 for every refusal
/// and for a usage error, and whatever the final plumbing step returns for the
/// branches that end in `exec`.
pub fn merge_one_file(args: &[String]) -> Result<ExitCode> {
    // Tolerate the subcommand at index 0 the way the sibling ports do; a blob id
    // can never collide with this name, so the guard is unambiguous.
    let args = match args.first() {
        Some(a) if a == "merge-one-file" => &args[1..],
        _ => args,
    };

    // `git-sh-setup` is sourced before anything touches the repository, and it
    // answers `-h` in the first position regardless of what follows.
    if args.first().map(String::as_str) == Some("-h") {
        println!("{LONG_USAGE}");
        return Ok(ExitCode::SUCCESS);
    }

    // cd_to_toplevel + require_work_tree, both of which run before the argument
    // count is checked.
    let repo = gix::discover(".")?;
    let Some(workdir) = repo.workdir().map(Path::to_path_buf) else {
        // `git rev-parse --show-toplevel` fails in a bare repository, so
        // cd_to_toplevel's `||` arm reports the failure — with `$cdup` literal.
        eprintln!("fatal: this operation must be run in a work tree");
        eprintln!("Cannot chdir to $cdup, the toplevel of the working tree");
        return Ok(ExitCode::from(1));
    };
    std::env::set_current_dir(&workdir)?;

    if args.len() != 7 {
        // `echo`, not `echo >&2`: git really does put this on stdout.
        println!("{LONG_USAGE}");
        return Ok(ExitCode::from(1));
    }
    let a = Args {
        orig: &args[0],
        ours: &args[1],
        theirs: &args[2],
        path: &args[3],
        orig_mode: &args[4],
        our_mode: &args[5],
        their_mode: &args[6],
    };

    // The script's `case "${1:-.}${2:-.}${3:-.}"` reduced to its meaning. Blob
    // ids are fixed-width hex, so every pattern is a plain equality test, and the
    // arms are tried in the script's order.
    let deleted = !a.orig.is_empty()
        && ((a.ours.is_empty() && a.theirs.is_empty())
            || (a.ours.is_empty() && a.theirs == a.orig)
            || (a.ours == a.orig && a.theirs.is_empty()));
    let added_ours = a.orig.is_empty() && !a.ours.is_empty() && a.theirs.is_empty();
    let added_theirs = a.orig.is_empty() && a.ours.is_empty() && !a.theirs.is_empty();
    let added_same =
        a.orig.is_empty() && !a.ours.is_empty() && !a.theirs.is_empty() && a.ours == a.theirs;
    let modified_both = (!a.orig.is_empty() && !a.ours.is_empty() && !a.theirs.is_empty())
        || (a.orig.is_empty() && !a.ours.is_empty() && !a.theirs.is_empty());

    if deleted {
        delete(&a)
    } else if added_ours {
        // The other side did not add, so there is nothing to do beyond marking
        // the path merged; no message is printed.
        exec(&["update-index", "--add", "--cacheinfo", a.our_mode, a.ours, a.path])
    } else if added_theirs {
        add_theirs(&a)
    } else if added_same {
        add_same(&a)
    } else if modified_both {
        merge(&repo, &a)
    } else {
        eprintln!(
            "ERROR: {}: Not handling case {} -> {} -> {}",
            a.path, a.orig, a.ours, a.theirs
        );
        Ok(ExitCode::from(1))
    }
}

/// Deleted in both, or deleted on one side and untouched on the other.
fn delete(a: &Args<'_>) -> Result<ExitCode> {
    // A side that dropped the path contributes no mode, so a mode that differs
    // from the base on the *surviving* side means the two branches disagree
    // about more than existence. With both sides gone this fires whenever the
    // base has a mode at all, which is how stock git behaves.
    if (a.our_mode.is_empty() && a.orig_mode != a.their_mode)
        || (a.their_mode.is_empty() && a.orig_mode != a.our_mode)
    {
        eprintln!("ERROR: File {} deleted on one branch but had its", a.path);
        eprintln!("ERROR: permissions changed on the other.");
        return Ok(ExitCode::from(1));
    }

    if a.ours.is_empty() {
        // read-tree already checked that the index matches HEAD, so the path is
        // not tracked here. Any worktree file of that name is unrelated and is
        // left alone; only the index entry has to go.
        return exec(&["update-index", "--remove", "--", a.path]);
    }
    println!("Removing {}", a.path);

    // `test -f` follows symlinks and is false for directories, so only a regular
    // file is unlinked. Pruning the now-possibly-empty parents is best-effort.
    if std::fs::metadata(a.path).is_ok_and(|m| m.is_file()) {
        let _ = std::fs::remove_file(a.path);
        remove_empty_parents(a.path);
    }
    exec(&["update-index", "--remove", "--", a.path])
}

/// Added on their side only: register the blob and materialise it, refusing to
/// clobber an untracked file of the same name.
fn add_theirs(a: &Args<'_>) -> Result<ExitCode> {
    // Printed before the check, so a refusal is preceded by this line.
    println!("Adding {}", a.path);
    if std::fs::metadata(a.path).is_ok_and(|m| m.is_file()) {
        eprintln!("ERROR: untracked {} is overwritten by the merge.", a.path);
        return Ok(ExitCode::from(1));
    }
    let code = git(&[
        "update-index",
        "--add",
        "--cacheinfo",
        a.their_mode,
        a.theirs,
        a.path,
    ])?;
    // The script's `&&` short-circuits here and drops out of the `case`, landing
    // on the trailing `exit 1` — the child's own code is not propagated.
    if code != 0 {
        return Ok(ExitCode::from(1));
    }
    exec(&["checkout-index", "-u", "-f", "--", a.path])
}

/// Added on both sides with identical content; only the modes can still clash.
fn add_same(a: &Args<'_>) -> Result<ExitCode> {
    if a.our_mode != a.their_mode {
        eprintln!("ERROR: File {} added identically in both branches,", a.path);
        eprintln!(
            "ERROR: but permissions conflict {}->{}.",
            a.our_mode, a.their_mode
        );
        return Ok(ExitCode::from(1));
    }
    println!("Adding {}", a.path);
    let code = git(&[
        "update-index",
        "--add",
        "--cacheinfo",
        a.our_mode,
        a.ours,
        a.path,
    ])?;
    // As above: a failure here exits 1, not with `update-index`'s status.
    if code != 0 {
        return Ok(ExitCode::from(1));
    }
    exec(&["checkout-index", "-u", "-f", "--", a.path])
}

/// Modified on both sides, or added on both sides with different content.
fn merge(repo: &gix::Repository, a: &Args<'_>) -> Result<ExitCode> {
    // Neither side of a symlink or submodule change can be merged line-wise, and
    // the check looks at both modes.
    for mode in [a.our_mode, a.their_mode] {
        if mode == "120000" {
            eprintln!("ERROR: {}: Not merging symbolic link changes.", a.path);
            return Ok(ExitCode::from(1));
        }
        if mode == "160000" {
            eprintln!(
                "ERROR: {}: Not merging conflicting submodule changes.",
                a.path
            );
            return Ok(ExitCode::from(1));
        }
    }

    // The temp names become the conflict-marker labels, so they are produced by
    // the ported `unpack-file` rather than invented here. A failed unpack yields
    // an empty name, exactly as command substitution does in the script.
    let src1 = unpack_file(a.ours)?;
    let src2 = unpack_file(a.theirs)?;
    let orig = if a.orig.is_empty() {
        println!("Added {} in both, but differently.", a.path);
        // `git hash-object /dev/null` — the empty blob's id, not written out, so
        // this unpack fails in a repository that has never stored it.
        unpack_file(&gix::hash::ObjectId::empty_blob(repo.object_hash()).to_hex().to_string())?
    } else {
        println!("Auto-merging {}", a.path);
        unpack_file(a.orig)?
    };

    // `merge-file` rewrites `src1` in place with the merged result.
    let merged = git(&["merge-file", &src1, &orig, &src2])?;
    let mut msg = String::new();
    // A missing base is always a conflict, even when the two sides merge cleanly.
    let mut conflicted = merged != 0 || a.orig.is_empty();
    if conflicted {
        msg.push_str("content conflict");
    }

    // Create the worktree file from our staged version — that establishes the
    // parent directories and the file mode — then replace its contents with the
    // merge result.
    let code = git(&["checkout-index", "-f", "--stage=2", "--", a.path])?;
    if code != 0 {
        // The temp files are deliberately not cleaned up here: the script's `rm`
        // is on the far side of this `|| exit 1`.
        return Ok(ExitCode::from(1));
    }
    if copy_over(&src1, a.path).is_err() {
        return Ok(ExitCode::from(1));
    }
    for f in [&orig, &src1, &src2] {
        let _ = std::fs::remove_file(f);
    }

    if a.our_mode != a.their_mode {
        if !msg.is_empty() {
            msg.push_str(", ");
        }
        msg.push_str(&format!(
            "permissions conflict: {}->{},{}",
            a.orig_mode, a.our_mode, a.their_mode
        ));
        conflicted = true;
    }

    if conflicted {
        eprintln!("ERROR: {msg} in {}", a.path);
        return Ok(ExitCode::from(1));
    }
    exec(&["update-index", "--", a.path])
}

/// `git unpack-file <blob>`: the temp file's name on success, the empty string
/// when the command failed (mirroring `$(...)` on a failing command).
///
/// The child's stderr is inherited, so its diagnostics reach the user in place.
fn unpack_file(blob: &str) -> Result<String> {
    let out = command(&["unpack-file", blob])?
        .stdout(Stdio::piped())
        .spawn()?
        .wait_with_output()?;
    if !out.status.success() {
        return Ok(String::new());
    }
    let name = String::from_utf8_lossy(&out.stdout);
    // Command substitution strips trailing newlines.
    Ok(name.trim_end_matches(['\n', '\r']).to_string())
}

/// Run one plumbing step in this same binary and return its exit code.
///
/// Death by signal is reported as 128, the value git's `run_command` falls back
/// to when it cannot map a wait status.
fn git(argv: &[&str]) -> Result<u8> {
    let status = command(argv)?.status()?;
    Ok(match status.code() {
        Some(c) => (c & 0xff) as u8,
        None => 128,
    })
}

/// The same step, with its exit code becoming this command's — the script's
/// `exec git ...` endings.
fn exec(argv: &[&str]) -> Result<ExitCode> {
    Ok(ExitCode::from(git(argv)?))
}

/// A `Command` re-invoking this binary with `argv`, rooted at the worktree top
/// (the process has already chdir'd there).
fn command(argv: &[&str]) -> Result<Command> {
    let exe = std::env::current_exe()
        .map_err(|e| anyhow!("cannot locate the running executable: {e}"))?;
    let mut cmd = Command::new(exe);
    cmd.args(argv);
    Ok(cmd)
}

/// `cat "$src" >"$dst"`: truncate `dst` and write `src`'s bytes into it, leaving
/// the file's existing mode and inode-visible identity to `checkout-index`.
fn copy_over(src: &str, dst: &str) -> std::io::Result<()> {
    let data = std::fs::read(src)?;
    std::fs::write(dst, data)
}

/// `rmdir -p "$(expr "z$path" : 'z\(.*\)/')"`: drop the deepest directory of
/// `path` and then each ancestor, stopping at the first one that is not empty.
///
/// A path with no `/` has no directory part, so nothing is removed. Every
/// failure is ignored, as the script's `2>/dev/null || :` ignores them.
fn remove_empty_parents(path: &str) {
    let Some(cut) = path.rfind('/') else {
        return;
    };
    let mut dir = Path::new(&path[..cut]);
    while std::fs::remove_dir(dir).is_ok() {
        match dir.parent() {
            Some(p) if !p.as_os_str().is_empty() => dir = p,
            _ => break,
        }
    }
}

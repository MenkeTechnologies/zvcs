//! `git commit-tree` — create a commit object from an existing tree.
//!
//! Covered: `<tree>`, `-p <parent>` (repeatable, with git's duplicate-parent
//! dedup), `-m <message>` and `-F <file>` (both repeatable and freely
//! interleaved, joined in git's own order), reading the message from stdin when
//! neither is given, `--no-gpg-sign`, and `--`. Attached short-option values
//! (`-mfoo`, `-Fmsg.txt`, `-p<oid>`) are accepted exactly as git's
//! `parse_options` accepts them. Stdout is the new commit id followed by a
//! newline, and the object bytes are byte-identical to git's, so the id matches.
//!
//! Author and committer come from `author.*`/`committer.*`, the `GIT_AUTHOR_*`
//! and `GIT_COMMITTER_*` environment variables, then `user.*` — gitoxide's
//! resolution order, which is git's. The `encoding` header is emitted when
//! `i18n.commitEncoding` names something other than UTF-8, as git does.
//!
//! Not covered: `-S`/`--gpg-sign` (commit signing needs a gpg driver that the
//! vendored crates do not provide), and git's gecos-derived identity fallback
//! when nothing is configured — both fail with a precise message rather than
//! writing a commit that would differ from git's.
//!
//! One resolution caveat: revision parsing goes through gitoxide's
//! `rev_parse_single`, which may peel an annotated tag where git's
//! `get_oid_tree`/`get_oid_commit` hand the tag object itself to
//! `assert_oid_type`. `commit-tree tag` / `-p tag` can therefore succeed here
//! where stock git fails; every other spec form resolves identically.
//!
//! Exit codes follow git rather than the caller's generic failure path: usage
//! errors exit 129, fatal errors 128, and a rejected message 1.

use anyhow::{bail, Result};
use std::io::Read;
use std::process::ExitCode;

use gix::bstr::{BString, ByteSlice};
use gix::hash::ObjectId;
use gix::objs::Kind;

/// git's own usage block, printed on stderr next to `error: unknown …`.
const USAGE: &str = "\
usage: git commit-tree <tree> [(-p <parent>)...]
   or: git commit-tree [(-p <parent>)...] [-S[<keyid>]] [(-m <message>)...]
                       [(-F <file>)...] <tree>

    -p <parent>           id of a parent commit object
    -m <message>          commit message
    -F <file>             read commit log message from file
    -S, --[no-]gpg-sign[=<key-id>]
                          GPG sign commit
";

/// `git commit-tree` — write a commit object naming `<tree>` and print its id.
///
/// The commit is written to the object database and nothing else changes: no ref
/// is updated, no reflog entry is made, the index and worktree are untouched.
///
/// Argument handling mirrors `builtin/commit-tree.c`: options and the tree may
/// appear in any order, `-p`/`-F` are resolved eagerly so failures surface in
/// command-line order, and the tree is *not* peeled — a commit or tag given
/// where a tree is expected is an error, just as `assert_oid_type` makes it one.
pub fn commit_tree(args: &[String]) -> Result<ExitCode> {
    // Dispatch passes the subcommand itself at index 0.
    let args = match args.first() {
        Some(a) if a == "commit-tree" => &args[1..],
        _ => args,
    };

    let repo = gix::discover(".")?;

    let mut message: Vec<u8> = Vec::new();
    let mut have_message = false;
    let mut parents: Vec<ObjectId> = Vec::new();
    let mut trees: Vec<ObjectId> = Vec::new();
    let mut sign = false;
    let mut no_more_opts = false;

    let mut i = 0;
    while i < args.len() {
        let a = args[i].as_str();

        // A bare `-` is a positional to `parse_options`, not an option.
        if no_more_opts || a == "-" || !a.starts_with('-') {
            match resolve(&repo, a, Kind::Tree, "tree") {
                Ok(id) => trees.push(id),
                Err(m) => return fatal(&m),
            }
            i += 1;
            continue;
        }

        // Long options. `commit-tree` has no long form for -p/-m/-F.
        if let Some(long) = a.strip_prefix("--") {
            match long {
                "" => no_more_opts = true,
                "gpg-sign" => sign = true,
                "no-gpg-sign" => sign = false,
                _ if long.starts_with("gpg-sign=") => sign = true,
                _ => {
                    eprintln!("error: unknown option `{long}'");
                    eprintln!("{USAGE}");
                    return Ok(ExitCode::from(129));
                }
            }
            i += 1;
            continue;
        }

        // Short options, with git's "value may be attached or separate" rule.
        let flag = a[1..].chars().next().expect("`-` alone was handled above");
        let attached = &a[1 + flag.len_utf8()..];
        match flag {
            'S' => {
                sign = true;
                i += 1;
                continue;
            }
            'p' | 'm' | 'F' => {}
            _ => {
                eprintln!("error: unknown switch `{flag}'");
                eprintln!("{USAGE}");
                return Ok(ExitCode::from(129));
            }
        }

        let value = if attached.is_empty() {
            i += 1;
            match args.get(i) {
                Some(v) => v.as_str(),
                None => {
                    eprintln!("error: switch `{flag}' requires a value");
                    return Ok(ExitCode::from(129));
                }
            }
        } else {
            attached
        };

        match flag {
            'p' => match resolve(&repo, value, Kind::Commit, "commit") {
                // git keeps the first mention and reports the rest, but still succeeds.
                Ok(id) if parents.contains(&id) => eprintln!("error: duplicate parent {id} ignored"),
                Ok(id) => parents.push(id),
                Err(m) => return fatal(&m),
            },
            'm' => {
                // Each -m is its own paragraph, and always ends a line.
                separate(&mut message);
                message.extend_from_slice(value.as_bytes());
                if !message.ends_with(b"\n") {
                    message.push(b'\n');
                }
                have_message = true;
            }
            'F' => {
                separate(&mut message);
                // Unlike -m, file content is appended verbatim.
                if value == "-" {
                    let mut buf = Vec::new();
                    std::io::stdin().lock().read_to_end(&mut buf)?;
                    message.extend_from_slice(&buf);
                } else {
                    match std::fs::read(value) {
                        Ok(buf) => message.extend_from_slice(&buf),
                        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                            return fatal(&format!(
                                "could not open '{value}' for reading: No such file or directory"
                            ))
                        }
                        Err(e) => {
                            return fatal(&format!("could not open '{value}' for reading: {e}"))
                        }
                    }
                }
                have_message = true;
            }
            _ => unreachable!("flag was validated above"),
        }
        i += 1;
    }

    if sign {
        bail!("`-S`/`--gpg-sign` is not supported (no signing driver in the vendored crates)");
    }
    if trees.len() != 1 {
        return fatal("must give exactly one tree");
    }
    let tree = trees[0];

    // With no -m and no -F the whole message is stdin, verbatim.
    if !have_message {
        std::io::stdin().lock().read_to_end(&mut message)?;
    }
    if message.contains(&0) {
        // git reports this and returns 1, having written nothing.
        eprintln!("error: a NUL byte in commit log message not allowed.");
        return Ok(ExitCode::FAILURE);
    }

    let author = identity(repo.author(), "author")?;
    let committer = identity(repo.committer(), "committer")?;

    // git writes the `encoding` header only when the configured commit encoding
    // is something other than UTF-8 (`is_encoding_utf8`).
    let snapshot = repo.config_snapshot();
    let encoding = snapshot.string("i18n.commitEncoding").and_then(|v| {
        let is_utf8 = {
            let name = v.to_str_lossy();
            name.eq_ignore_ascii_case("utf-8") || name.eq_ignore_ascii_case("utf8")
        };
        (!is_utf8).then_some(v)
    });
    drop(snapshot);

    // Serialize the object write through the repo coordinator, as the other
    // writing porcelain does, so concurrent zvcs writers queue instead of racing.
    let _lock = crate::lock::RepoLock::acquire(repo.git_dir());

    let commit = gix::objs::Commit {
        tree,
        parents: parents.into_iter().collect(),
        author,
        committer,
        encoding,
        message: BString::from(message),
        extra_headers: Vec::new(),
    };
    let id = repo.write_object(&commit)?;
    println!("{}", id.detach());
    Ok(ExitCode::SUCCESS)
}

/// Insert git's paragraph separator before appending the next message chunk.
fn separate(message: &mut Vec<u8>) {
    if !message.is_empty() {
        message.push(b'\n');
    }
}

/// Report a git `fatal:` failure on stderr and yield git's exit code for it.
fn fatal(msg: &str) -> Result<ExitCode> {
    eprintln!("fatal: {msg}");
    Ok(ExitCode::from(128))
}

/// Resolve `spec` and require that it names an object of `kind`.
///
/// This is `repo_get_oid_tree`/`repo_get_oid_commit` followed by
/// `assert_oid_type`: the revision syntax does the peeling (`<rev>^{tree}`,
/// `<rev>:`), and an object of the wrong type is rejected rather than peeled.
/// `Err` carries git's exact message text, without the `fatal: ` prefix.
fn resolve(repo: &gix::Repository, spec: &str, kind: Kind, label: &str) -> Result<ObjectId, String> {
    let id = repo
        .rev_parse_single(spec)
        .map_err(|_| format!("not a valid object name {spec}"))?
        .detach();
    let found = repo
        .find_header(id)
        .map_err(|_| format!("not a valid object name {spec}"))?
        .kind();
    if found != kind {
        return Err(format!("{id} is not a valid '{label}' object"));
    }
    Ok(id)
}

/// Turn a configured identity into an owned signature, or explain what is missing.
///
/// git falls back to a name derived from the passwd/gecos entry and the host
/// name; that fallback is not ported, so an unconfigured identity is an error
/// instead of a commit whose author line would not match git's.
fn identity(
    configured: Option<Result<gix::actor::SignatureRef<'_>, gix::config::time::Error>>,
    role: &str,
) -> Result<gix::actor::Signature> {
    let Some(signature) = configured else {
        let upper = role.to_uppercase();
        bail!(
            "no {role} identity configured (set user.name/user.email or \
             GIT_{upper}_NAME/GIT_{upper}_EMAIL); git's gecos fallback is not ported"
        );
    };
    Ok(signature?.to_owned()?)
}

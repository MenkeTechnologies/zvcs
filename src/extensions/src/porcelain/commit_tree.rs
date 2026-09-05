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
//! When no `encoding` header is written — i.e. the commit encoding *is* UTF-8 —
//! the finished object is passed through `verify_utf8()`, which transcribes every
//! byte that starts an ill-formed UTF-8 sequence as if it were Latin-1 and prints
//! `Warning: commit message did not conform to UTF-8.` once. The check covers the
//! whole buffer, headers included, so an author name carrying a raw `0xe9` is
//! rewritten by it too — which is what `git am` on an `ISO-8859-1` mail depends
//! on, since `mailinfo` leaves that header's bytes alone.
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
//! One ordering deviation, deliberate and not observable in stdout/stderr/exit:
//! git checks the tree's *type* inside `commit_tree()`, i.e. after it has
//! drained stdin for the message, whereas [`resolve`] checks it where the tree
//! is resolved. Only the stdin drain differs; the same `fatal:` line and exit
//! 128 follow either way. `-p`'s type check sits in `parse_options`' callback in
//! git too, so that one is already in git's place.
//!
//! Exit codes follow git rather than the caller's generic failure path: usage
//! errors exit 129, fatal errors 128, and a rejected message 1.

use anyhow::{bail, Result};
use std::io::Read;
use std::process::ExitCode;

use gix::bstr::{BString, ByteSlice};
use gix::hash::ObjectId;
use gix::objs::{Kind, Write as _};

/// git's own usage block, printed on stderr next to `error: unknown …`.
/// `cmd_commit_tree()`'s `struct option options[]` (builtin/commit-tree.c), in
/// table order, as [`super::resolve_long`] reads it. `-p`, `-m` and `-F` are
/// short-only and so have no entry.
const LONG_OPTS: &[super::LongOpt] = &[
    super::LongOpt { name: "gpg-sign",                    neg: true,  arg: super::Arg::Optional },
];

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
/// appear in any order, `-p`/`-F` are resolved eagerly inside `parse_options`'
/// callbacks so their failures surface in command-line order, and the tree is
/// *not* peeled — a commit or tag given where a tree is expected is an error,
/// just as `assert_oid_type` makes it one.
///
/// The tree itself is a *non-option* argument, which `parse_options` only
/// collects: `argc != 1` and `repo_get_oid_tree()` run after the whole command
/// line has been parsed. A bad option therefore outranks a bad tree however
/// they are ordered — `commit-tree nosuchtree --bogus` is `error: unknown
/// option` at 129, not `fatal: not a valid object name`, and two unresolvable
/// trees are `must give exactly one tree` rather than a resolution failure.
pub fn commit_tree(args: &[String]) -> Result<ExitCode> {
    // Dispatch passes the subcommand itself at index 0.
    let args = match args.first() {
        Some(a) if a == "commit-tree" => &args[1..],
        _ => args,
    };

    // `show_usage_with_options_if_asked()` (builtin/commit-tree.c:131) runs
    // ahead of `parse_options` and of anything that could fail: a lone `-h`
    // prints the block on stdout at 129, before the identity check below could
    // complain about a repository that has no author configured.
    if let Some(code) = super::show_usage_if_asked(args, USAGE) {
        return Ok(code);
    }

    // The object this writes carries an identity, and git fills the halves
    // the user did not give rather than refusing — except under
    // `user.useConfigOnly`, which is the one case it says so.
    let mut repo = crate::setup::discover()?;
    if let Some(code) = crate::ensure_object_identity(&mut repo, "Author") {
        return Ok(code);
    }

    let mut message: Vec<u8> = Vec::new();
    let mut have_message = false;
    let mut parents: Vec<ObjectId> = Vec::new();
    // Non-options, kept unresolved: `parse_options` only gathers them.
    let mut trees: Vec<&str> = Vec::new();
    let mut sign = false;
    let mut no_more_opts = false;

    let mut i = 0;
    while i < args.len() {
        let a = args[i].as_str();

        // A bare `-` is a positional to `parse_options`, not an option.
        if no_more_opts || a == "-" || !a.starts_with('-') {
            trees.push(a);
            i += 1;
            continue;
        }

        // `if (internal_help && !strcmp(arg + 2, "help-all"))`
        // (parse-options.c:1122): an exact match tested ahead of
        // parse_long_opt(), so it neither abbreviates nor takes an `=<value>`.
        // This table has no `PARSE_OPT_HIDDEN` entry, so `USAGE_FULL` renders
        // the same block `-h` prints.
        if a == "--help-all" {
            return Ok(super::show_usage(USAGE));
        }

        // Respell a unique abbreviation as the name it resolves to, so an
        // abbreviation lands on the arm its full spelling lands on.
        let canonical;
        let a = match super::canonical_long(a, LONG_OPTS) {
            super::Long::Name(name) => {
                canonical = name;
                canonical.as_ref()
            }
            super::Long::Ambiguous(first, second) => {
                return Ok(super::ambiguous_option(a, &first, &second, USAGE))
            }
        };
        // Long options. `commit-tree` has no long form for -p/-m/-F.
        if let Some(long) = a.strip_prefix("--") {
            match long {
                "" => no_more_opts = true,
                "gpg-sign" => sign = true,
                "no-gpg-sign" => sign = false,
                _ if long.starts_with("gpg-sign=") => sign = true,
                _ => {
                    eprintln!("error: unknown option `{long}'");
                    eprint!("{USAGE}");
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
            // parse_options' `internal_help` inside the short-option loop,
            // which the entry-point check only covers for a lone `-h`.
            'h' => return Ok(super::show_usage(USAGE)),
            _ => {
                eprintln!("error: unknown switch `{flag}'");
                eprint!("{USAGE}");
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
    // `repo_get_oid_tree(the_repository, argv[0], &tree_oid)` — after the whole
    // command line parsed, so an option failure always reports before this one.
    let tree = match resolve(&repo, trees[0], Kind::Tree, "tree") {
        Ok(id) => id,
        Err(m) => return fatal(&m),
    };

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

    // Serialize the object write through the repo coordinator, as the other
    // writing porcelain does, so concurrent zvcs writers queue instead of racing.
    let _lock = crate::lock::RepoLock::acquire(repo.git_dir());

    let commit = gix::objs::Commit {
        tree,
        parents: parents.into_iter().collect(),
        author,
        committer,
        encoding: encoding.clone(),
        message: BString::from(message),
        extra_headers: Vec::new(),
    };

    // ```c
    // /* And check the encoding */
    // if (encoding_is_utf8 && !verify_utf8(&buffer))
    //         fprintf(stderr, _(commit_utf8_warn));
    // ```
    //
    // (`commit.c:1571-1573`.) The check runs over the finished buffer, headers
    // and all, so an author name carrying a raw Latin-1 byte is rewritten by it
    // just as the message is — which is how `git am` on an `ISO-8859-1` mail
    // ends up with a UTF-8 `author` line even though `mailinfo` left that
    // header's bytes alone. `encoding_is_utf8` is exactly "no `encoding` header
    // was written", so `i18n.commitEncoding=ISO-8859-1` keeps the bytes.
    let mut buffer = Vec::new();
    gix::objs::WriteTo::write_to(&commit, &mut buffer)?;
    if encoding.is_none() && !verify_utf8(&mut buffer) {
        eprintln!(
            "Warning: commit message did not conform to UTF-8.\n\
             You may want to amend it after fixing the message, or set the config\n\
             variable i18n.commitEncoding to the encoding your project uses."
        );
    }
    let id = repo
        .objects
        .write_buf(Kind::Commit, &buffer)
        .map_err(|e| anyhow::anyhow!("{e}"))?;
    println!("{id}");
    Ok(ExitCode::SUCCESS)
}

/// `find_invalid_utf8()` (`commit.c:1417`): the offset of the first byte that
/// starts an ill-formed UTF-8 sequence, or `None` when the whole slice is valid.
///
/// It is stricter than a bare decoder: an overlong encoding, a surrogate, a
/// `U+xxFFFE`/`U+xxFFFF` non-character and anything in `U+FDD0..U+FDEF` are all
/// rejected, each of them at the offset the sequence *started*.
fn find_invalid_utf8(buf: &[u8]) -> Option<usize> {
    const MAX_CODEPOINT: [u32; 4] = [0x7f, 0x7ff, 0xffff, 0x10_ffff];
    let mut offset = 0usize;
    while offset < buf.len() {
        let c = buf[offset];
        let bad_offset = offset;
        offset += 1;
        // "Simple US-ASCII? No worries."
        if c < 0x80 {
            continue;
        }
        // "Count how many more high bits set: that's how many more bytes this
        // sequence should have."
        let mut shifted = c;
        let mut bytes = 0usize;
        while shifted & 0x40 != 0 {
            shifted <<= 1;
            bytes += 1;
        }
        // "Must be between 1 and 3 more bytes." Longer sequences would land
        // beyond U+10FFFF.
        if !(1..=3).contains(&bytes) || buf.len() - offset < bytes {
            return Some(bad_offset);
        }
        let mut codepoint = u32::from(shifted & 0x7f) >> bytes;
        let (min_val, max_val) = (MAX_CODEPOINT[bytes - 1] + 1, MAX_CODEPOINT[bytes]);
        // "And verify that they are good continuation bytes".
        for _ in 0..bytes {
            let b = buf[offset];
            offset += 1;
            codepoint = (codepoint << 6) | u32::from(b & 0x3f);
            if b & 0xc0 != 0x80 {
                return Some(bad_offset);
            }
        }
        if codepoint < min_val
            || codepoint > max_val
            || codepoint & 0x1f_f800 == 0xd800
            || codepoint & 0xfffe == 0xfffe
            || (0xfdd0..=0xfdef).contains(&codepoint)
        {
            return Some(bad_offset);
        }
    }
    None
}

/// `verify_utf8()` (`commit.c:1504`).
///
/// > This verifies that the buffer is in proper utf8 format.
/// >
/// > If it isn't, it assumes any non-utf8 characters are Latin1, and does the
/// > conversion.
///
/// Returns whether the buffer was already valid; a `false` answer means bytes
/// were rewritten and the caller prints the warning.
fn verify_utf8(buf: &mut Vec<u8>) -> bool {
    let mut ok = true;
    let mut pos = 0usize;
    loop {
        let Some(bad) = find_invalid_utf8(&buf[pos..]) else {
            return ok;
        };
        pos += bad;
        ok = false;
        // "We know 'c' must be in the range 128-255": the one offending byte is
        // replaced by its two-byte Latin-1 transcription, and the scan resumes
        // after it.
        let c = buf[pos];
        buf[pos] = 0xc0 + (c >> 6);
        buf.insert(pos + 1, 0x80 + (c & 0x3f));
        pos += 2;
    }
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
/// `odb_assert_oid_type`: the revision syntax does the peeling (`<rev>^{tree}`,
/// `<rev>:`), and an object of the wrong type is rejected rather than peeled.
/// `Err` carries git's exact message text, without the `fatal: ` prefix.
///
/// The two steps report *different* failures, and conflating them was the bug
/// here. `repo_get_oid_*` only fails when the name resolves to nothing, and a
/// full-length hex id always resolves — `get_oid_basic()` decodes it and returns
/// without asking the object database, which is what [`crate::objname::resolve`]
/// reproduces. Whether that object exists is then `odb_assert_oid_type`'s
/// question (odb.c:977):
///
/// ```c
/// enum object_type type = odb_read_object_info(odb, oid, NULL);
/// if (type < 0)
///         die(_("%s is not a valid object"), oid_to_hex(oid));
/// if (type != expect)
///         die(_("%s is not a valid '%s' object"), oid_to_hex(oid), type_name(expect));
/// ```
///
/// So a well-formed but absent id is `<oid> is not a valid object`, naming the
/// *canonical hex* — not `not a valid object name <spec>`, which names the spec
/// as typed and belongs to names that do not resolve at all.
fn resolve(repo: &gix::Repository, spec: &str, kind: Kind, label: &str) -> Result<ObjectId, String> {
    let id = crate::objname::resolve(repo, spec)
        .ok_or_else(|| format!("not a valid object name {spec}"))?;
    match repo.find_header(id) {
        Err(_) => Err(format!("{id} is not a valid object")),
        Ok(header) if header.kind() != kind => Err(format!("{id} is not a valid '{label}' object")),
        Ok(_) => Ok(id),
    }
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

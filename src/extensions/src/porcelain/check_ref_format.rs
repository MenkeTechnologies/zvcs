//! `git check-ref-format` — validate a reference name.
//!
//! This is a faithful port of `builtin/check-ref-format.c` plus the two
//! functions it leans on, `refs.c::check_refname_format` /
//! `check_refname_component` (including the 256-entry `refname_disposition`
//! table) and `strbuf_check_branch_ref`. The rules are transcribed from the C
//! source rather than delegated to `gix_validate::reference::name`, because the
//! vendored validator answers a different question: it rejects any lower-case
//! one-level name outright (`gix-validate/src/reference.rs:136`, the
//! `SomeLowercase` arm), which is precisely the case `--allow-onelevel` exists
//! to accept, and it has no notion of `--refspec-pattern`'s single-`*` budget.
//!
//! The command touches a repository only for `--branch`, which runs the name
//! through `repo_interpret_branch_name()`: the `@{-N}` "previous checkout"
//! syntax off the HEAD reflog, and the `@{upstream}`/`@{u}`/`@{push}` marks off
//! the branch configuration. Everything else works outside a repository, as it
//! does with stock git.
//!
//! ### Covered (byte-identical stdout/stderr and exit code against stock git)
//!
//! * `git check-ref-format <refname>` — exit 0 when well formed, 1 otherwise,
//!   with no output either way
//! * `--normalize` (and its deprecated spelling `--print`) — leading slashes
//!   dropped, runs of slashes collapsed, the result echoed on stdout when valid
//! * `--allow-onelevel` / `--no-allow-onelevel`, honoured in argument order
//! * `--refspec-pattern` — a single `*` anywhere in the whole refname
//! * `--branch <shorthand>` — prints the branch name, exit 0; on rejection
//!   `fatal: '<arg>' is not a valid branch name` on stderr, exit 128
//! * `@{-N}` expansion for `--branch` inside a repository
//! * the `@{u}` / `@{upstream}` / `@{push}` marks for `--branch`: the `die()`
//!   inside `interpret_branch_mark()` when the mark names no upstream
//!   (`fatal: no such branch: 'x'` for `--branch x@{u}`, which replaces the
//!   `not a valid branch name` refusal), and the expansion itself for the one
//!   value `branch_interpret_allowed()` lets through under
//!   `INTERPRET_BRANCH_LOCAL` — an upstream that is itself a local branch
//! * `-h` as the only argument — usage on stdout, exit 129; a missing argument,
//!   an unknown option, or more than one refname — the same usage on stderr,
//!   exit 129
//!
//! ### Honest limitations
//!
//! * Git re-interprets the tail after an `@{-N}` prefix recursively
//!   (`refs.c::reinterpret`), so a pathological `@{-1}@{-1}` expands twice. This
//!   expands a single leading `@{-N}` and appends the remainder verbatim, which
//!   covers `@{-1}`, `@{-2}`, and `@{-1}~2`-style input but not the nested form.
//! * `interpret_branch_name()` keeps scanning the remaining `@` positions when a
//!   mark resolved to a ref `branch_interpret_allowed()` rejects, so stock
//!   diagnoses the *second* mark of `main@{u}@{u}` (`no such branch:
//!   'main@{u}'`). Only the first mark position is interpreted here, so that
//!   input is rejected as an invalid branch name instead. Sharing the scan would
//!   need `crate::objname`'s `branch_get_upstream` ladder to be callable for a
//!   branch name rather than for a whole operand — the same thing that would fix
//!   `a^{}@{u}`, where the ladder is reached through `ambiguity_base()` and that
//!   strips the `^{}` peel before the `@` scan, so the mark is never seen. (Stock
//!   dies with `no such branch: 'a^{}'`; `git rev-parse` here has the same gap.)
//! * The `N` in `@{-N}` is parsed with Rust's integer parser rather than
//!   `strtol`, which additionally skips leading whitespace. Whitespace is an
//!   invalid refname byte regardless, so the only effect is that such input
//!   stays unexpanded and is then rejected — the same exit code, via a
//!   different path.

use anyhow::Result;
use std::io::Write;
use std::process::ExitCode;

use gix::bstr::ByteSlice;

/// Stock git's usage block for this command, byte-for-byte. Stdout on a bare
/// `-h`, stderr on any argument error; both exit 129.
const USAGE: &str = "usage: git check-ref-format [--normalize] [<options>] <refname>\n   \
                     or: git check-ref-format --branch <branchname-shorthand>\n";

/// `REFNAME_ALLOW_ONELEVEL` — waive the "at least two components" rule.
const ALLOW_ONELEVEL: u32 = 1;
/// `REFNAME_REFSPEC_PATTERN` — permit exactly one `*` in the whole refname.
const REFSPEC_PATTERN: u32 = 2;

/// `refs.c::refname_disposition`, transcribed verbatim.
///
/// * 0 — an acceptable character
/// * 1 — end of component (NUL or `/`)
/// * 2 — `.`, look for a preceding `.` to reject `..`
/// * 3 — `{`, look for a preceding `@` to reject `@{`
/// * 4 — a bad character: ASCII control codes, DEL, and `:?[\^~`, SP, TAB
/// * 5 — `*`, rejected unless `REFSPEC_PATTERN` is still set
///
/// Bytes at or above 0x80 are acceptable, matching the C array's zero tail.
#[rustfmt::skip]
const DISPOSITION: [u8; 256] = [
    1, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4,
    4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4,
    4, 0, 0, 0, 0, 0, 0, 0, 0, 0, 5, 0, 0, 0, 2, 1,
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 4, 0, 0, 0, 0, 4,
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 4, 4, 0, 4, 0,
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 3, 0, 0, 4, 4,
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
];

/// `git check-ref-format` — ensure a reference name is well formed.
///
/// See the module docs for the covered surface and the two `@{-N}` deviations.
pub fn check_ref_format(args: &[String]) -> Result<ExitCode> {
    // Dispatch passes the argument list without the subcommand; tolerate a
    // leading `check-ref-format` so either calling convention is correct.
    let argv: &[String] = match args.first() {
        Some(first) if first == "check-ref-format" => &args[1..],
        _ => args,
    };

    // `-h` and `--help-all` are honoured only as the sole argument, exactly as
    // the C `argc == 2` guard does; `-h <anything>` falls through to the option
    // loop and errors. The option table has no `PARSE_OPT_HIDDEN` entry, so
    // `USAGE_FULL` renders the same block as `USAGE_NORMAL`.
    if let Some(code) = super::show_usage_if_asked(argv, USAGE) {
        return Ok(code);
    }

    if argv.len() == 2 && argv[0] == "--branch" {
        return check_ref_format_branch(&argv[1]);
    }

    let mut normalize = false;
    let mut flags: u32 = 0;
    let mut i = 0;
    while i < argv.len() && argv[i].starts_with('-') {
        match argv[i].as_str() {
            "--normalize" | "--print" => normalize = true,
            "--allow-onelevel" => flags |= ALLOW_ONELEVEL,
            "--no-allow-onelevel" => flags &= !ALLOW_ONELEVEL,
            "--refspec-pattern" => flags |= REFSPEC_PATTERN,
            _ => return Ok(usage_error()),
        }
        i += 1;
    }

    // Exactly one non-option argument must remain, and it must be the last one.
    if i + 1 != argv.len() {
        return Ok(usage_error());
    }

    let raw = argv[i].as_bytes();
    let normalized;
    let refname: &[u8] = if normalize {
        normalized = collapse_slashes(raw);
        &normalized
    } else {
        raw
    };

    if !check_refname_format(refname, flags) {
        return Ok(ExitCode::from(1));
    }
    if normalize {
        let stdout = std::io::stdout();
        let mut out = stdout.lock();
        out.write_all(refname)?;
        out.write_all(b"\n")?;
    }
    Ok(ExitCode::SUCCESS)
}

/// Git's argument-error path: the usage block on stderr, exit 129.
fn usage_error() -> ExitCode {
    eprint!("{USAGE}");
    ExitCode::from(129)
}

/// `builtin/check-ref-format.c::check_ref_format_branch`, via
/// `refs.c::check_branch_ref`.
///
/// ```c
/// int check_branch_ref(struct strbuf *sb, const char *name)
/// {
///         if (startup_info->have_repository)
///                 copy_branchname(sb, name, INTERPRET_BRANCH_LOCAL);
///         else
///                 strbuf_addstr(sb, name);
///         strbuf_splice(sb, 0, 0, "refs/heads/", 11);
///         if (*name == '-' || !strcmp(sb->buf, "refs/heads/HEAD"))
///                 return -1;
///         return check_refname_format(sb->buf, 0);
/// }
/// ```
///
/// The leading-dash and `refs/heads/HEAD` checks run *after* the expansion, which
/// is why `--branch -x@{u}` reports the upstream failure rather than the dash:
/// `copy_branchname` dies inside `interpret_branch_mark` before either check is
/// reached. Rejection is git's `die()`: the message on stderr and exit 128.
/// Acceptance prints the expanded shorthand.
fn check_ref_format_branch(arg: &str) -> Result<ExitCode> {
    let expanded = match crate::setup::discover() {
        Ok(repo) => match copy_branchname(&repo, arg) {
            Ok(name) => name,
            // `interpret_branch_mark`'s `die("%s", err.buf)`. It fires while the
            // name is being expanded, so it *replaces* the caller's own refusal
            // rather than being reported alongside it.
            Err(message) => {
                eprintln!("fatal: {message}");
                return Ok(ExitCode::from(128));
            }
        },
        // `startup_info->have_repository` is false: the name is taken verbatim
        // and no `@{…}` shorthand means anything.
        Err(_) => arg.as_bytes().to_vec(),
    };

    let mut full = b"refs/heads/".to_vec();
    full.extend_from_slice(&expanded);

    let rejected = arg.as_bytes().first() == Some(&b'-')
        || full == b"refs/heads/HEAD"
        || !check_refname_format(&full, 0);
    if rejected {
        eprintln!("fatal: '{arg}' is not a valid branch name");
        return Ok(ExitCode::from(128));
    }

    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    out.write_all(&expanded)?;
    out.write_all(b"\n")?;
    Ok(ExitCode::SUCCESS)
}

/// `refs.c::copy_branchname` with `INTERPRET_BRANCH_LOCAL`, over
/// `object-name.c::repo_interpret_branch_name`.
///
/// ```c
/// void copy_branchname(struct strbuf *sb, const char *name,
///                      enum interpret_branch_kind allowed)
/// {
///         int len = strlen(name);
///         struct interpret_branch_name_options options = { .allowed = allowed };
///         int used = repo_interpret_branch_name(the_repository, name, len, sb, &options);
///
///         if (used < 0)
///                 used = 0;
///         strbuf_add(sb, name + used, len - used);
/// }
/// ```
///
/// `repo_interpret_branch_name` runs `interpret_nth_prior_checkout` (gated on
/// `INTERPRET_BRANCH_LOCAL`, which is set) and then walks the `@` positions left
/// to right trying `@{upstream}`/`@{u}` and then `@{push}` at each. Two of the
/// three arms are *not* reachable from here:
///
///   * `interpret_empty_at` — a bare `@` meaning `HEAD` — is gated on
///     `INTERPRET_BRANCH_HEAD`, which `check_branch_ref` does not pass, so
///     `git check-ref-format --branch @` prints `@`.
///   * a mark that resolves is only *applied* when `branch_interpret_allowed`
///     accepts what it resolved to, and with `INTERPRET_BRANCH_LOCAL` alone that
///     is only a `refs/heads/` value. An ordinary upstream is a
///     `refs/remotes/` ref, so `main@{u}` is left unexpanded and then rejected
///     for the `@{` in it — while a branch tracking another *local* branch
///     (`branch.<n>.remote = .`) does expand.
///
/// `Err` is the `die()` inside `interpret_branch_mark`, which fires before either
/// of those gates and so is reported even for a mark whose value would have been
/// thrown away.
fn copy_branchname(repo: &gix::Repository, name: &str) -> Result<Vec<u8>, String> {
    let bytes = name.as_bytes();

    // `interpret_nth_prior_checkout`. A recognised `@{-N}` ends the walk either
    // way: expanded when the reflog holds that many switches, and otherwise
    // through the `return len` for "syntax Ok, not enough switches", which never
    // reaches the `@` scan below.
    if let Some((nth, used)) = parse_nth_prior(bytes) {
        return Ok(match nth_branch_switch(repo, nth) {
            Some(mut branch) => {
                branch.extend_from_slice(&bytes[used..]);
                branch
            }
            None => bytes.to_vec(),
        });
    }

    // `interpret_branch_mark`'s `die()`, for either mark. `upstream_mark_fatal`
    // is the shared port of that ladder — `branch_get_upstream`'s four arms and,
    // for a `@{push}`, the whole of `branch_get_push_1` — and it applies the same
    // left-to-right `@` scan and the same `memchr(name, ':', at)` guard.
    if let Some(message) = crate::objname::upstream_mark_fatal(repo, name) {
        return Err(message);
    }

    let Some((at, mark_len, mark)) = first_mark(name) else {
        return Ok(bytes.to_vec());
    };
    // `if (memchr(name, ':', at)) return -1;` — and a `:` before the first mark
    // precedes every later one too, so the scan has nothing left to find.
    if bytes[..at].contains(&b':') {
        return Ok(bytes.to_vec());
    }

    // `branch_get(NULL)` and `branch_get("HEAD")` are the same lookup — the
    // branch HEAD points at. A detached HEAD has none, which the `die()` above
    // has already reported.
    let named = &name[..at];
    let branch = if named.is_empty() || named == "HEAD" {
        match repo.head_name() {
            Ok(Some(full)) => full.shorten().to_string(),
            _ => return Ok(bytes.to_vec()),
        }
    } else {
        named.to_string()
    };

    let refname = format!("refs/heads/{branch}");
    let value = match mark {
        Mark::Upstream => crate::porcelain::branch::upstream_ref(repo, refname.as_str().into()),
        Mark::Push => crate::porcelain::branch::push_ref(repo, refname.as_str().into()),
    };
    // The `die()` above already covered every value the C reports as missing, so
    // anything unresolved here is a mark this port simply cannot apply; leaving
    // the name alone is `interpret_branch_mark`'s own `return -1`.
    let Some(value) = value else {
        return Ok(bytes.to_vec());
    };

    // `branch_interpret_allowed(value, INTERPRET_BRANCH_LOCAL)`.
    if !value.as_bstr().starts_with(b"refs/heads/") {
        return Ok(bytes.to_vec());
    }
    // `set_shortened_ref`, then `copy_branchname`'s `strbuf_add(sb, name + used,
    // len - used)` for whatever followed the mark.
    let mut out = crate::refname::shorten_unambiguous(repo, value.as_bstr(), false);
    out.extend_from_slice(&bytes[at + mark_len..]);
    Ok(out)
}

/// Which of the two marks `interpret_branch_name`'s scan reaches first.
#[derive(Clone, Copy)]
enum Mark {
    /// `@{upstream}` / `@{u}`, read with `branch_get_upstream`.
    Upstream,
    /// `@{push}`, read with `branch_get_push`.
    Push,
}

/// The first `@` position holding a mark, the mark's length, and which mark it
/// is — the state `interpret_branch_name`'s loop is in when it first gets a
/// non-negative answer out of `interpret_branch_mark`.
///
/// The loop tries `upstream_mark` before `push_mark` at each `@`, so the earlier
/// position wins and no position can hold both.
fn first_mark(name: &str) -> Option<(usize, usize, Mark)> {
    let upstream = crate::objname::upstream_mark_at(name);
    let push = crate::objname::push_mark_at(name);
    let (at, mark) = match (upstream, push) {
        (Some(u), Some(p)) if p < u => (p, Mark::Push),
        (Some(u), _) => (u, Mark::Upstream),
        (None, Some(p)) => (p, Mark::Push),
        (None, None) => return None,
    };
    // `at_mark` compares `@{upstream}` before `@{u}`, and only one of the two can
    // prefix a given position.
    const UPSTREAM: &[u8] = b"@{upstream}";
    let rest = &name.as_bytes()[at..];
    let len = match mark {
        Mark::Upstream if rest.len() >= UPSTREAM.len()
            && rest[..UPSTREAM.len()].eq_ignore_ascii_case(UPSTREAM) => UPSTREAM.len(),
        Mark::Upstream => "@{u}".len(),
        Mark::Push => "@{push}".len(),
    };
    Some((at, len, mark))
}

/// The syntax half of `refs.c::interpret_nth_prior_checkout`.
///
/// Recognises a leading `@{-N}` with `N > 0` and returns `(N, bytes consumed)`.
/// The closing brace is the first `}` in the input and the number must run
/// exactly up to it, as git's `strtol`/`num_end` comparison requires.
pub(crate) fn parse_nth_prior(name: &[u8]) -> Option<(usize, usize)> {
    if name.len() < 4 || !name.starts_with(b"@{-") {
        return None;
    }
    let brace = name.iter().position(|&c| c == b'}')?;
    let nth: i64 = std::str::from_utf8(&name[3..brace]).ok()?.parse().ok()?;
    if nth <= 0 {
        return None;
    }
    Some((nth as usize, brace + 1))
}

/// The reflog half: `refs.c::grab_nth_branch_switch` over HEAD's log, newest
/// entry first, returning the source branch of the `nth` checkout found.
pub(crate) fn nth_branch_switch(repo: &gix::Repository, nth: usize) -> Option<Vec<u8>> {
    let head = repo.head().ok()?;
    let mut platform = head.log_iter();
    let log = platform.rev().ok()??;

    let mut remaining = nth;
    for line in log.filter_map(Result::ok) {
        let Some(from_to) = line.message.strip_prefix(b"checkout: moving from ") else {
            continue;
        };
        let Some(pos) = from_to.find(" to ") else {
            continue;
        };
        remaining -= 1;
        if remaining == 0 {
            return Some(from_to[..pos].to_vec());
        }
    }
    None
}

/// `builtin/check-ref-format.c::collapse_slashes` — drop leading slashes and
/// squeeze every run of slashes down to one.
fn collapse_slashes(refname: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(refname.len());
    let mut prev = b'/';
    for &ch in refname {
        if prev == b'/' && ch == b'/' {
            continue;
        }
        out.push(ch);
        prev = ch;
    }
    out
}

/// `refs.c::check_refname_format`, returning `true` when the name is well
/// formed. `flags` is taken by value because the `*` budget it carries is
/// consumed as the components are walked.
///
/// Shared with the reference-database check in [`super::fsck`], which is git's
/// `git refs verify` and calls the same function for `badRefName` and
/// `badReferentName`.
/// `check_refname_format(refname, REFNAME_ALLOW_ONELEVEL)` — the form
/// `ref_transaction_update()` applies to every ref it is about to write, which
/// is why `git update-ref main <oid>` is legal and lands in `$GIT_DIR/main`.
pub(crate) fn check_refname_format_onelevel(refname: &[u8]) -> bool {
    check_refname_format(refname, ALLOW_ONELEVEL)
}

pub(crate) fn check_refname_format(refname: &[u8], mut flags: u32) -> bool {
    if refname == b"@" {
        return false;
    }

    let mut rest = refname;
    let mut component_len;
    let mut component_count = 0usize;
    loop {
        let len = check_refname_component(rest, &mut flags);
        if len <= 0 {
            return false;
        }
        component_len = len as usize;
        component_count += 1;

        // The byte terminating the component is either the end of the string
        // (C's NUL) or the `/` introducing the next one.
        if component_len == rest.len() || rest[component_len] == 0 {
            break;
        }
        rest = &rest[component_len + 1..];
    }

    if rest[component_len - 1] == b'.' {
        return false; // the final component ends with '.'
    }
    if flags & ALLOW_ONELEVEL == 0 && component_count < 2 {
        return false;
    }
    true
}

/// `refs.c::check_refname_component` — the length of the component starting at
/// `refname`, `0` when it is empty, or `-1` when it is invalid.
fn check_refname_component(refname: &[u8], flags: &mut u32) -> isize {
    let mut last = 0u8;
    let mut i = 0usize;

    let end = loop {
        // C walks a NUL-terminated string; past the end we synthesise the NUL,
        // whose disposition (1) ends the component.
        let ch = refname.get(i).copied().unwrap_or(0);
        match DISPOSITION[ch as usize] {
            1 => break i,
            2 if last == b'.' => return -1, // ".."
            3 if last == b'@' => return -1, // "@{"
            4 => return -1,
            5 => {
                if *flags & REFSPEC_PATTERN == 0 {
                    return -1;
                }
                // One asterisk per refspec: spend the budget on first use.
                *flags &= !REFSPEC_PATTERN;
            }
            _ => {}
        }
        last = ch;
        i += 1;
    };

    if end == 0 {
        return 0; // zero-length component
    }
    if refname[0] == b'.' {
        return -1; // component starts with '.'
    }
    if end >= 5 && &refname[end - 5..end] == b".lock" {
        return -1;
    }
    end as isize
}

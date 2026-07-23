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
//! The command touches a repository only for `--branch`, where the `@{-N}`
//! "previous checkout" syntax is expanded from the HEAD reflog via
//! `gix::Head::log_iter`. Everything else works outside a repository, as it
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

    // `-h` is honoured only as the sole argument, exactly as the C `argc == 2`
    // guard does; `-h <anything>` falls through to the option loop and errors.
    if argv.len() == 1 && argv[0] == "-h" {
        print!("{USAGE}");
        return Ok(ExitCode::from(129));
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
/// `strbuf_check_branch_ref`.
///
/// The shorthand is expanded (`@{-N}`) when a repository is present, prefixed
/// with `refs/heads/`, and validated. Rejection is git's `die()`: the message on
/// stderr and exit 128. Acceptance prints the expanded shorthand.
fn check_ref_format_branch(arg: &str) -> Result<ExitCode> {
    let expanded = match gix::discover(".") {
        Ok(repo) => branchname(&repo, arg),
        Err(_) => arg.as_bytes().to_vec(),
    };

    let mut full = b"refs/heads/".to_vec();
    full.extend_from_slice(&expanded);

    // `strbuf_check_branch_ref` rejects a leading dash on the *original* name
    // and the reserved `refs/heads/HEAD` before running the format check.
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

/// `strbuf_branchname` with `INTERPRET_BRANCH_LOCAL`: expand a leading `@{-N}`
/// into the branch it names, keeping any trailing text. Anything else, and any
/// `@{-N}` that names more checkouts than the reflog holds, is returned as is.
fn branchname(repo: &gix::Repository, name: &str) -> Vec<u8> {
    let bytes = name.as_bytes();
    let Some((nth, used)) = parse_nth_prior(bytes) else {
        return bytes.to_vec();
    };
    let Some(mut branch) = nth_branch_switch(repo, nth) else {
        return bytes.to_vec();
    };
    branch.extend_from_slice(&bytes[used..]);
    branch
}

/// The syntax half of `refs.c::interpret_nth_prior_checkout`.
///
/// Recognises a leading `@{-N}` with `N > 0` and returns `(N, bytes consumed)`.
/// The closing brace is the first `}` in the input and the number must run
/// exactly up to it, as git's `strtol`/`num_end` comparison requires.
fn parse_nth_prior(name: &[u8]) -> Option<(usize, usize)> {
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
fn nth_branch_switch(repo: &gix::Repository, nth: usize) -> Option<Vec<u8>> {
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
fn check_refname_format(refname: &[u8], mut flags: u32) -> bool {
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

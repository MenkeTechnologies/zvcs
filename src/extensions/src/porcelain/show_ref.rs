//! `git show-ref` — list references in a local repository.
//!
//! Covered: the pattern-listing form (with `--head`, `--branches`/`--heads`,
//! `--tags`, `-d`/`--dereference`, `-s`/`--hash[=<n>]`, `--abbrev[=<n>]`,
//! `-q`/`--quiet` and trailing `<pattern>...`), the `--verify` form, the
//! `--exists` form, and the `--exclude-existing[=<pattern>]` stdin-filter form,
//! including their exit codes (1 for "nothing matched", 2 for "--exists:
//! missing", 128 for the `fatal:` paths).

use anyhow::Result;
use std::collections::HashSet;
use std::io::{BufRead, Write};
use std::process::ExitCode;

use gix::bstr::BStr;
use gix::hash::ObjectId;
use gix::prelude::ObjectIdExt;

/// How object ids are rendered.
#[derive(Clone, Copy)]
enum Abbrev {
    /// Full hex id (the default, and what `--abbrev=0` / bare `--hash` select).
    Full,
    /// `core.abbrev`-configured length, disambiguated — bare `--abbrev`.
    Auto,
    /// An explicit `--abbrev=<n>` / `--hash=<n>` length (already clamped to >= 4).
    Len(usize),
}

/// Parsed command line for a single `show-ref` invocation.
struct Opts {
    head: bool,      // --head: always show HEAD, even when filtered out
    deref: bool,     // -d/--dereference: add a `<ref>^{}` line for tag objects
    hash_only: bool, // -s/--hash: print the id alone (the `^{}` line keeps its name)
    quiet: bool,     // -q/--quiet: no stdout, exit code only
    branches: bool,  // --branches/--heads: limit to refs/heads/
    tags: bool,      // --tags: limit to refs/tags/
    abbrev: Abbrev,
}

/// `git show-ref` — list references, verify one, or test for existence.
///
/// Exit codes match stock git: 0 when at least one ref was shown (or verified),
/// 1 when nothing matched (and for `--verify --quiet` on a missing ref), 2 for
/// `--exists` on a missing ref, 128 for the `fatal:` paths.
pub fn show_ref(args: &[String]) -> Result<ExitCode> {
    // Dispatch passes the flags only, but tolerate a leading subcommand name.
    let args = match args.first() {
        Some(a) if a == "show-ref" => &args[1..],
        _ => args,
    };

    let mut opts = Opts {
        head: false,
        deref: false,
        hash_only: false,
        quiet: false,
        branches: false,
        tags: false,
        abbrev: Abbrev::Full,
    };
    let mut verify = false;
    let mut exists = false;
    let mut exclude_existing = false;
    let mut exclude_pattern: Option<String> = None;
    let mut patterns: Vec<String> = Vec::new();
    let mut no_more_opts = false;

    for a in args {
        let a = a.as_str();
        if no_more_opts || !a.starts_with('-') || a == "-" {
            patterns.push(a.to_string());
            continue;
        }
        if a == "--" {
            no_more_opts = true;
            continue;
        }
        if let Some(long) = a.strip_prefix("--") {
            match long {
                "head" => opts.head = true,
                "dereference" => opts.deref = true,
                "hash" => opts.hash_only = true,
                "quiet" => opts.quiet = true,
                "tags" => opts.tags = true,
                "branches" | "heads" => opts.branches = true,
                "verify" => verify = true,
                "exists" => exists = true,
                "abbrev" => opts.abbrev = Abbrev::Auto,
                _ if long.starts_with("hash=") => {
                    opts.hash_only = true;
                    match parse_abbrev(&long["hash=".len()..]) {
                        Some(a) => opts.abbrev = a,
                        None => return numeric_error("hash"),
                    }
                }
                _ if long.starts_with("abbrev=") => match parse_abbrev(&long["abbrev=".len()..]) {
                    Some(a) => opts.abbrev = a,
                    None => return numeric_error("abbrev"),
                },
                "exclude-existing" => exclude_existing = true,
                _ if long.starts_with("exclude-existing=") => {
                    exclude_existing = true;
                    exclude_pattern = Some(long["exclude-existing=".len()..].to_string());
                }
                _ => return unknown_option("option", long),
            }
            continue;
        }

        // Grouped short flags, e.g. `-dq`; `-s` may carry its digits inline (`-s7`).
        let short: Vec<char> = a[1..].chars().collect();
        let mut i = 0;
        while i < short.len() {
            match short[i] {
                'd' => opts.deref = true,
                'q' => opts.quiet = true,
                's' => {
                    opts.hash_only = true;
                    let rest: String = short[i + 1..].iter().collect();
                    if !rest.is_empty() && rest.chars().all(|c| c.is_ascii_digit()) {
                        match parse_abbrev(&rest) {
                            Some(a) => opts.abbrev = a,
                            None => return numeric_error("hash"),
                        }
                        i = short.len();
                        continue;
                    }
                }
                c => return unknown_option("switch", &c.to_string()),
            }
            i += 1;
        }
    }

    // git validates the parsed options (unknown flag / numeric value above, exit
    // 129) *before* this post-parse compatibility check, which is a plain `die()`
    // — a single `fatal:` line and exit 128, with no usage block. git's
    // `die_for_incompatible_opt3` names the first two enabled modes in the fixed
    // order (--exclude-existing, --verify, --exists), regardless of CLI order.
    let mut enabled = Vec::new();
    if exclude_existing {
        enabled.push("--exclude-existing");
    }
    if verify {
        enabled.push("--verify");
    }
    if exists {
        enabled.push("--exists");
    }
    if enabled.len() > 1 {
        return die_incompatible(enabled[0], enabled[1]);
    }

    let repo = gix::discover(".")?;

    if exclude_existing {
        return run_exclude_existing(&repo, exclude_pattern.as_deref());
    }
    if exists {
        return run_exists(&repo, &patterns);
    }
    if verify {
        return run_verify(&repo, &opts, &patterns);
    }
    run_patterns(&repo, &opts, &patterns)
}


/// git's `parse_opt_abbrev_cb` numeric-value rejection: prints
/// `error: option `<name>' expects a numerical value` and exits 129 (the
/// parse-options usage-error code), where `<name>` is `abbrev` or `hash`.
fn numeric_error(name: &str) -> Result<ExitCode> {
    eprintln!("error: option `{name}' expects a numerical value");
    Ok(ExitCode::from(129))
}

/// git's parse-options usage block for `show-ref`, printed on stderr after the
/// `error:` line on any usage error (exit 129). Byte-for-byte from stock git
/// 2.55.0's `usage_with_options()`; ends with the trailing blank line git emits.
const USAGE: &str = r#"usage: git show-ref [--head] [-d | --dereference]
                    [-s | --hash[=<n>]] [--abbrev[=<n>]] [--branches] [--tags]
                    [--] [<pattern>...]
   or: git show-ref --verify [-q | --quiet] [-d | --dereference]
                    [-s | --hash[=<n>]] [--abbrev[=<n>]]
                    [--] [<ref>...]
   or: git show-ref --exclude-existing[=<pattern>]
   or: git show-ref --exists <ref>

    --[no-]tags           only show tags (can be combined with --branches)
    --[no-]branches       only show branches (can be combined with --tags)
    --[no-]exists         check for reference existence without resolving
    --[no-]verify         stricter reference checking, requires exact ref path
    --[no-]head           show the HEAD reference, even if it would be filtered out
    -d, --[no-]dereference
                          dereference tags into object IDs
    -s, --[no-]hash[=<n>] only show SHA1 hash using <n> digits
    --[no-]abbrev[=<n>]   use <n> digits to display object names
    -q, --[no-]quiet      do not print results to stdout (useful with --verify)
    --exclude-existing[=<pattern>]
                          show refs from stdin that aren't in local repository

"#;

/// git's parse-options rejection of an unrecognized flag, printed as
/// `error: unknown <kind> <name>` in git's quoting. `kind` is "option" for a
/// `--long` flag and "switch" for a short one; short groups report the single
/// offending character. Prints the usage block and exits 129 — the usage-error
/// code, not `bail!`'s collapsed 1.
fn unknown_option(kind: &str, name: &str) -> Result<ExitCode> {
    eprintln!("error: unknown {kind} `{name}'");
    eprint!("{USAGE}");
    Ok(ExitCode::from(129))
}

/// git's `die_for_incompatible_opt`: two flags that cannot be combined. Unlike
/// the parse-options 129 path this is a plain `die()` — one `fatal:` line, no
/// usage block, exit 128. The message names the options in git's fixed order
/// regardless of the order they appeared on the command line.
fn die_incompatible(a: &str, b: &str) -> Result<ExitCode> {
    eprintln!("fatal: options '{a}' and '{b}' cannot be used together");
    Ok(ExitCode::from(128))
}

/// Parse an `--abbrev=<n>` / `--hash=<n>` value exactly as git's
/// `parse_opt_abbrev_cb` does. The value is read with C `strtol` base-10
/// semantics (leading whitespace and an optional sign, then digits); any
/// remaining character — including trailing whitespace — is rejected (`None`,
/// git's exit-129 path). The parsed `long` is truncated to a C `int`, then `0`
/// keeps the full id, a nonzero value below git's 4-digit minimum is raised to
/// 4, and larger values are clamped to the hash width at render time.
fn parse_abbrev(s: &str) -> Option<Abbrev> {
    let v = strtol_i32(s)?;
    Some(if v == 0 {
        Abbrev::Full
    } else if v < 4 {
        Abbrev::Len(4)
    } else {
        Abbrev::Len(v as usize)
    })
}

/// C `strtol(s, &end, 10)` followed by assignment to a 32-bit `int`, as git
/// stores its abbrev value: skip leading whitespace, take an optional `+`/`-`
/// and base-10 digits, and require the whole string to be consumed. On integer
/// overflow the accumulator saturates to `i64::MIN`/`MAX` — whose low 32 bits
/// (`0` / `-1`) match a C `long`-to-`int` truncation of `LONG_MIN`/`LONG_MAX`.
fn strtol_i32(s: &str) -> Option<i32> {
    let b = s.as_bytes();
    let mut i = 0;
    // strtol's isspace: space, \t, \n, \v, \f, \r.
    while i < b.len() && (b[i] == 0x0B || b[i].is_ascii_whitespace()) {
        i += 1;
    }
    let neg = matches!(b.get(i), Some(b'-'));
    if matches!(b.get(i), Some(b'+' | b'-')) {
        i += 1;
    }
    let start = i;
    let mut acc: i64 = 0;
    while i < b.len() && b[i].is_ascii_digit() {
        let d = i64::from(b[i] - b'0');
        acc = if neg {
            acc.saturating_mul(10).saturating_sub(d)
        } else {
            acc.saturating_mul(10).saturating_add(d)
        };
        i += 1;
    }
    // No digits, or trailing junk after the number -> not a numerical value.
    if i == start || i != b.len() {
        return None;
    }
    Some(acc as i32)
}

/// Default form: walk every ref under `refs/` (optionally restricted to
/// `refs/heads/` and/or `refs/tags/`) and print the ones matching `patterns`.
fn run_patterns(repo: &gix::Repository, opts: &Opts, patterns: &[String]) -> Result<ExitCode> {
    let mut out = String::new();
    let mut found = false;

    // `--head` shows HEAD first and bypasses both the prefix and pattern filters.
    if opts.head {
        if let Ok(Some(mut head)) = repo.try_find_reference("HEAD") {
            if let Ok(id) = head.follow_to_object() {
                let id = id.detach();
                if missing_object(repo, id) {
                    return bad_ref(&out, "HEAD", id);
                }
                found = true;
                let peeled = opts.deref.then(|| peeled(&mut head)).flatten();
                emit(repo, &mut out, opts, "HEAD", id, peeled);
            }
        }
    }

    for reference in repo.references()?.all()? {
        // Broken refs are skipped, as git's ref iteration does.
        let Ok(mut reference) = reference else { continue };
        let name = reference.name().as_bstr().to_string();

        if (opts.branches || opts.tags) && !prefix_selected(&name, opts) {
            continue;
        }
        if !patterns.is_empty() && !patterns.iter().any(|p| matches_pattern(&name, p)) {
            continue;
        }
        let Ok(id) = reference.follow_to_object() else {
            continue;
        };
        let id = id.detach();
        if missing_object(repo, id) {
            return bad_ref(&out, &name, id);
        }
        found = true;
        let peeled = opts.deref.then(|| peeled(&mut reference)).flatten();
        emit(repo, &mut out, opts, &name, id, peeled);
    }

    print!("{out}");
    Ok(if found {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(1)
    })
}

/// `--verify`: every argument must name an existing ref by its exact full path.
fn run_verify(repo: &gix::Repository, opts: &Opts, refs: &[String]) -> Result<ExitCode> {
    if refs.is_empty() {
        eprintln!("fatal: --verify requires a reference");
        return Ok(ExitCode::from(128));
    }

    let mut out = String::new();
    for name in refs {
        let Some((id, mut reference)) = resolve_exact(repo, name) else {
            print!("{out}");
            if opts.quiet {
                return Ok(ExitCode::from(1));
            }
            eprintln!("fatal: '{name}' - not a valid ref");
            return Ok(ExitCode::from(128));
        };
        if missing_object(repo, id) {
            return bad_ref(&out, name, id);
        }
        let peeled = opts.deref.then(|| peeled(&mut reference)).flatten();
        emit(repo, &mut out, opts, name, id, peeled);
    }

    print!("{out}");
    Ok(ExitCode::SUCCESS)
}

/// `--exists`: test one exact ref name without resolving it to an object.
fn run_exists(repo: &gix::Repository, refs: &[String]) -> Result<ExitCode> {
    match refs.len() {
        0 => {
            eprintln!("fatal: --exists requires a reference");
            return Ok(ExitCode::from(128));
        }
        1 => {}
        _ => {
            eprintln!("fatal: --exists requires exactly one reference");
            return Ok(ExitCode::from(128));
        }
    }

    let name = refs[0].as_str();
    let present = matches!(
        repo.try_find_reference(name),
        Ok(Some(r)) if r.name().as_bstr().to_string() == name
    );
    if present {
        Ok(ExitCode::SUCCESS)
    } else {
        eprintln!("error: reference does not exist");
        Ok(ExitCode::from(2))
    }
}

/// `--exclude-existing[=<pattern>]`: read ref lines from stdin and echo back
/// only those whose ref is **not** already present in this repository.
///
/// Faithful port of git's `cmd_show_ref__exclude_existing`. git first collects
/// every ref under `refs/` into a set, then, for each stdin line, (1) strips the
/// trailing newline, (2) strips a trailing `^{}`, (3) takes the ref name as the
/// text after the last whitespace (so `<oid> SP <ref>` lines yield `<ref>`),
/// (4) with `--exclude-existing=<pattern>` keeps only refs whose name *begins*
/// with `<pattern>` (a head match, not a glob), (5) warns and skips malformed
/// ref names, and (6) prints the original line verbatim when the ref is absent.
/// Exit status is always 0.
fn run_exclude_existing(repo: &gix::Repository, pattern: Option<&str>) -> Result<ExitCode> {
    // git's `existing_refs`: every ref under `refs/`, compared by exact full
    // name. Broken refs are skipped, as git's ref iteration does.
    let mut existing: HashSet<Vec<u8>> = HashSet::new();
    for reference in repo.references()?.all()? {
        let Ok(reference) = reference else { continue };
        existing.insert(reference.name().as_bstr().to_vec());
    }

    let pat = pattern.map(str::as_bytes);
    let stdin = std::io::stdin();
    let mut handle = stdin.lock();
    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    let mut line: Vec<u8> = Vec::new();

    loop {
        line.clear();
        if handle.read_until(b'\n', &mut line)? == 0 {
            break;
        }
        // (1) drop the trailing newline, mirroring git's `buf[--len] = '\0'`.
        if line.last() == Some(&b'\n') {
            line.pop();
        }
        // (2) trim a trailing "^{}" (peeled-tag marker) off the whole line.
        let mut len = line.len();
        if len >= 3 && &line[len - 3..] == b"^{}" {
            len -= 3;
            line.truncate(len);
        }
        // (3) the ref is the text after the last whitespace on the line; with no
        // whitespace the entire line is the ref (git's backward `isspace` scan).
        let mut start = 0;
        let mut j = len;
        while j > 0 {
            if is_c_space(line[j - 1]) {
                start = j;
                break;
            }
            j -= 1;
        }
        let refname = &line[start..len];

        // (4) `--exclude-existing=<pattern>` is a prefix (head) match on the ref.
        if let Some(pat) = pat {
            if refname.len() < pat.len() || &refname[..pat.len()] != pat {
                continue;
            }
        }

        // (5) git warns on and skips names that fail `check_refname_format(ref,
        // 0)`. The vendored validator matches git's per-component rules but, with
        // no `REFNAME_ALLOW_ONELEVEL` notion, still accepts an all-uppercase
        // one-level name like `HEAD` that git's flags==0 rejects
        // (gix-validate/src/reference.rs:136). git's "at least two components"
        // rule is the extra `refname.contains('/')`: among validator-accepted
        // names (no empty/leading/trailing/repeated-slash components) a slash is
        // present iff there are >= 2 components.
        let well_formed =
            gix::validate::reference::name(BStr::new(refname)).is_ok() && refname.contains(&b'/');
        if !well_formed {
            eprintln!("warning: ref '{}' ignored", String::from_utf8_lossy(refname));
            continue;
        }

        // (6) echo the line (newline- and `^{}`-stripped) for absent refs.
        if !existing.contains(refname) {
            out.write_all(&line)?;
            out.write_all(b"\n")?;
        }
    }

    Ok(ExitCode::SUCCESS)
}

/// C `isspace` in the default "C" locale: space, tab, and the four vertical
/// whitespace controls. git splits the object id from the ref on these bytes.
fn is_c_space(c: u8) -> bool {
    matches!(c, b' ' | b'\t' | b'\n' | 0x0B | 0x0C | b'\r')
}

/// Look a ref up by its exact full name, following symbolic targets to an id.
///
/// `try_find_reference` applies git's partial-name lookup rules, so the name of
/// the ref it returns is compared against the request: `--verify main` must fail
/// even though `refs/heads/main` exists.
fn resolve_exact<'repo>(
    repo: &'repo gix::Repository,
    name: &str,
) -> Option<(ObjectId, gix::Reference<'repo>)> {
    let mut reference = repo.try_find_reference(name).ok().flatten()?;
    if reference.name().as_bstr().to_string() != name {
        return None;
    }
    let id = reference.follow_to_object().ok()?.detach();
    Some((id, reference))
}

/// The id this ref peels to once annotated tags are unwrapped, or `None` when
/// there is nothing to unwrap (git only prints a `^{}` line for tag objects).
fn peeled(reference: &mut gix::Reference<'_>) -> Option<ObjectId> {
    reference.peel_to_id().ok().map(|id| id.detach())
}

/// Whether a ref is inside the `--branches` / `--tags` selection.
fn prefix_selected(name: &str, opts: &Opts) -> bool {
    (opts.branches && name.starts_with("refs/heads/")) || (opts.tags && name.starts_with("refs/tags/"))
}

/// git's `match_ref_pattern`: the pattern must match the tail of the full ref
/// name on a `/` boundary, so `main` matches `refs/heads/main` and
/// `refs/remotes/origin/main`, but not `refs/heads/mymain`.
fn matches_pattern(name: &str, pattern: &str) -> bool {
    let (n, p) = (name.as_bytes(), pattern.as_bytes());
    n.len() >= p.len()
        && &n[n.len() - p.len()..] == p
        && (n.len() == p.len() || n[n.len() - p.len() - 1] == b'/')
}

/// Append the `<oid> SP <ref> LF` line (or `<oid> LF` under `--hash`), plus the
/// `<peeled-oid> SP <ref>^{} LF` line when `-d` was given and the ref is a tag.
fn emit(
    repo: &gix::Repository,
    out: &mut String,
    opts: &Opts,
    name: &str,
    id: ObjectId,
    peeled: Option<ObjectId>,
) {
    if opts.quiet {
        return;
    }
    let rendered = hex(repo, id, opts.abbrev);
    if opts.hash_only {
        out.push_str(&format!("{rendered}\n"));
    } else {
        out.push_str(&format!("{rendered} {name}\n"));
    }

    if opts.deref {
        if let Some(peeled) = peeled.filter(|p| *p != id) {
            // The dereferenced line keeps the ref name even under `--hash`.
            out.push_str(&format!("{} {name}^{{}}\n", hex(repo, peeled, opts.abbrev)));
        }
    }
}

/// Render an id, abbreviating like git's `find_unique_abbrev`: an explicit
/// length is extended as far as needed to stay unambiguous.
fn hex(repo: &gix::Repository, id: ObjectId, abbrev: Abbrev) -> String {
    match abbrev {
        Abbrev::Full => id.to_hex().to_string(),
        Abbrev::Auto => id.attach(repo).shorten_or_id().to_string(),
        Abbrev::Len(n) if n >= id.kind().len_in_hex() => id.to_hex().to_string(),
        Abbrev::Len(n) => gix::odb::store::prefix::disambiguate::Candidate::new(id, n)
            .ok()
            .and_then(|c| repo.objects.disambiguate_prefix(c).ok().flatten())
            .map_or_else(|| id.to_hex_with_len(n).to_string(), |p| p.to_string()),
    }
}

/// Whether the object a ref points at is absent from the object database.
fn missing_object(repo: &gix::Repository, id: ObjectId) -> bool {
    repo.find_header(id).is_err()
}

/// git dies with this exact message when a ref points at a missing object.
fn bad_ref(pending: &str, name: &str, id: ObjectId) -> Result<ExitCode> {
    print!("{pending}");
    eprintln!("fatal: git show-ref: bad ref {name} ({id})");
    Ok(ExitCode::from(128))
}

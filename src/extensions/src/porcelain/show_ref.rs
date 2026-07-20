//! `git show-ref` — list references in a local repository.
//!
//! Covered: the pattern-listing form (with `--head`, `--branches`/`--heads`,
//! `--tags`, `-d`/`--dereference`, `-s`/`--hash[=<n>]`, `--abbrev[=<n>]`,
//! `-q`/`--quiet` and trailing `<pattern>...`), the `--verify` form, and the
//! `--exists` form, including their exit codes (1 for "nothing matched", 2 for
//! "--exists: missing", 128 for the `fatal:` paths).
//!
//! Not covered: `--exclude-existing[=<pattern>]`, the stdin-filter form — it
//! bails rather than producing output that would diverge from git.

use anyhow::{bail, Result};
use std::process::ExitCode;

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
                    opts.abbrev = parse_abbrev(&long["hash=".len()..])?;
                }
                _ if long.starts_with("abbrev=") => {
                    opts.abbrev = parse_abbrev(&long["abbrev=".len()..])?;
                }
                "exclude-existing" => bail!("unsupported flag \"--exclude-existing\" ({PORTED})"),
                _ if long.starts_with("exclude-existing=") => {
                    bail!("unsupported flag \"--exclude-existing=\" ({PORTED})")
                }
                _ => bail!("unsupported flag {a:?} ({PORTED})"),
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
                        opts.abbrev = parse_abbrev(&rest)?;
                        i = short.len();
                        continue;
                    }
                }
                c => bail!("unsupported flag \"-{c}\" ({PORTED})"),
            }
            i += 1;
        }
    }

    if exists && verify {
        bail!("--exists and --verify are mutually exclusive");
    }

    let repo = gix::discover(".")?;

    if exists {
        return run_exists(&repo, &patterns);
    }
    if verify {
        return run_verify(&repo, &opts, &patterns);
    }
    run_patterns(&repo, &opts, &patterns)
}

/// The flags this port implements, quoted in every rejection message.
const PORTED: &str = "ported: --head, -d/--dereference, -s/--hash[=<n>], \
                      --abbrev[=<n>], --branches/--heads, --tags, --verify, \
                      --exists, -q/--quiet";

/// Parse an `--abbrev=<n>` / `--hash=<n>` value the way git does: `0` disables
/// abbreviation entirely, anything else is raised to git's 4-digit minimum.
fn parse_abbrev(s: &str) -> Result<Abbrev> {
    let n: usize = s
        .parse()
        .map_err(|_| anyhow::anyhow!("invalid abbreviation length {s:?}"))?;
    Ok(if n == 0 { Abbrev::Full } else { Abbrev::Len(n.max(4)) })
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

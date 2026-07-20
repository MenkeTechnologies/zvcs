//! `git ls-remote` — list references in a remote repository.
//!
//! Covered: the full listing form over gitoxide's blocking transport
//! (`git ls-remote [<repository> [<patterns>...]]`) with `-b`/`--branches`
//! (and the `-h`/`--heads` synonyms), `-t`/`--tags`, `--refs`, `--symref`,
//! `--exit-code`, `--get-url` and `-q`/`--quiet`, including git's `check_ref`
//! filter semantics, its `*/<pattern>` tail glob, its refname sort order, the
//! `From <url>` stderr header (printed only when `<repository>` is omitted),
//! and the exit codes (0 normally, 2 for `--exit-code` with no matching refs,
//! 128 when the remote cannot be reached, 129 for bare `-h`).
//!
//! Not covered: `--sort=<key>`, `--upload-pack=<exec>` and
//! `-o`/`--server-option=<option>` — they bail rather than silently producing
//! output that would diverge from git. Running outside a repository also bails:
//! gitoxide resolves transport, credential and `insteadOf` configuration
//! through a `Repository`, and there is no repository-less remote in the
//! vendored crates.

use anyhow::{bail, Result};
use std::process::ExitCode;

use gix::bstr::{BStr, ByteSlice};
use gix::protocol::handshake::Ref;

/// The flags this port implements, quoted in every rejection message.
const PORTED: &str = "ported: -b/--branches/-h/--heads, -t/--tags, --refs, \
                      --symref, --exit-code, --get-url, -q/--quiet";

/// `git ls-remote -h` used with nothing else prints this and exits 129.
const USAGE: &str = "\
usage: git ls-remote [--branches] [--tags] [--refs] [--upload-pack=<exec>]
                     [-q | --quiet] [--exit-code] [--get-url] [--sort=<key>]
                     [--symref] [<repository> [<patterns>...]]

    -q, --[no-]quiet      do not print remote URL
    --[no-]upload-pack <exec>
                          path of git-upload-pack on the remote host
    -t, --[no-]tags       limit to tags
    -b, --[no-]branches   limit to branches
    --[no-]refs           do not show peeled tags
    --[no-]get-url        take url.<base>.insteadOf into account
    --[no-]sort <key>     field name to sort on
    --[no-]exit-code      exit with exit code 2 if no matching refs are found
    --[no-]symref         show underlying ref in addition to the object pointed by it
    -o, --[no-]server-option <server-specific>
                          option to transmit

";

/// Parsed command line for a single `ls-remote` invocation.
struct Opts {
    branches: bool,  // -b/--branches (-h/--heads): git's REF_HEADS
    tags: bool,      // -t/--tags: git's REF_TAGS
    normal: bool,    // --refs: git's REF_NORMAL — drop HEAD and the `^{}` rows
    symref: bool,    // --symref: emit a `ref: <target> TAB <name>` line first
    quiet: bool,     // -q/--quiet: suppress the `From <url>` stderr header
    exit_code: bool, // --exit-code: exit 2 when nothing matched
    get_url: bool,   // --get-url: print the expanded URL and never connect
}

/// One output record: a ref advertised by the remote, or the synthetic `^{}`
/// row git emits for an annotated tag's peeled object.
struct Row {
    /// The full ref name as git prints it, e.g. `refs/tags/v1.0` or `…^{}`.
    name: String,
    /// The hex object id printed in the first column.
    oid: String,
    /// The symbolic target, for the `--symref` line (only on the base row).
    symref: Option<String>,
    /// Whether this is the synthetic `^{}` row (git's "magic fake tag ref").
    peel: bool,
}

/// `git ls-remote` — list references available in a remote repository.
///
/// Output is `<oid> TAB <ref> LF` per ref, sorted by refname, matching stock
/// git byte-for-byte for the supported flags. Annotated tags contribute a
/// second `<ref>^{}` row unless `--refs` is given.
pub fn ls_remote(args: &[String]) -> Result<ExitCode> {
    // Dispatch passes the flags only, but tolerate a leading subcommand name.
    let args = match args.first() {
        Some(a) if a == "ls-remote" => &args[1..],
        _ => args,
    };

    // Bare `-h` is help, consistent with other git subcommands; anywhere else
    // `-h` is the deprecated synonym for `--branches`.
    if args.len() == 1 && args[0] == "-h" {
        print!("{USAGE}");
        return Ok(ExitCode::from(129));
    }

    let mut opts = Opts {
        branches: false,
        tags: false,
        normal: false,
        symref: false,
        quiet: false,
        exit_code: false,
        get_url: false,
    };
    let mut positionals: Vec<&str> = Vec::new();
    let mut no_more_opts = false;

    for a in args {
        let a = a.as_str();
        if no_more_opts || !a.starts_with('-') || a == "-" {
            positionals.push(a);
            continue;
        }
        if a == "--" {
            no_more_opts = true;
            continue;
        }
        // `--no-<flag>` clears the corresponding boolean, as parse_options does.
        let (a, on) = match a.strip_prefix("--no-") {
            Some(rest) => (format!("--{rest}"), false),
            None => (a.to_string(), true),
        };
        match a.as_str() {
            "-b" | "--branches" | "-h" | "--heads" => opts.branches = on,
            "-t" | "--tags" => opts.tags = on,
            "--refs" => opts.normal = on,
            "--symref" => opts.symref = on,
            "-q" | "--quiet" => opts.quiet = on,
            "--exit-code" => opts.exit_code = on,
            "--get-url" => opts.get_url = on,
            "--sort" | "-o" | "--server-option" | "--upload-pack" => {
                bail!("unsupported flag {a:?} ({PORTED})")
            }
            s if s.starts_with("--sort=")
                || s.starts_with("--server-option=")
                || s.starts_with("--upload-pack=") =>
            {
                let flag = &s[..s.find('=').unwrap_or(s.len())];
                bail!("unsupported flag {flag:?} ({PORTED})")
            }
            s => bail!("unsupported flag {s:?} ({PORTED})"),
        }
    }

    let (repository, patterns): (Option<&str>, &[&str]) = match positionals.split_first() {
        Some((first, rest)) => (Some(*first), rest),
        None => (None, &[]),
    };

    // gitoxide resolves URL rewriting, transport and credential configuration
    // through a Repository; there is no repository-less remote to fall back on.
    let repo = match gix::discover(".") {
        Ok(repo) => repo,
        Err(_) => bail!("ls-remote outside a repository is not supported (no repository found)"),
    };

    let name_or_url = repository.map(BStr::new);
    let remote = match repo.find_fetch_remote(name_or_url) {
        Ok(remote) => remote,
        Err(e) => {
            eprintln!("fatal: {e}");
            return Ok(ExitCode::from(128));
        }
    };
    let url = remote
        .url(gix::remote::Direction::Fetch)
        .map(ToString::to_string)
        .unwrap_or_default();

    // `--get-url` expands `url.<base>.insteadOf` (applied by `find_fetch_remote`)
    // and exits without talking to the remote.
    if opts.get_url {
        println!("{url}");
        return Ok(ExitCode::SUCCESS);
    }

    // git prints the header only when `<repository>` was left off the command line.
    if repository.is_none() && !opts.quiet {
        eprintln!("From {url}");
    }

    // `prefix_from_spec_as_filter_on_remote` must be off: ls-remote lists every
    // advertised ref, not just the ones the remote's refspecs would fetch.
    let connection = match remote.connect(gix::remote::Direction::Fetch) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("fatal: {e}");
            return Ok(ExitCode::from(128));
        }
    };
    let ref_map = match connection.ref_map(
        gix::progress::Discard,
        gix::remote::ref_map::Options {
            prefix_from_spec_as_filter_on_remote: false,
            ..Default::default()
        },
    ) {
        Ok((map, _handshake)) => map,
        Err(e) => {
            eprintln!("fatal: {e}");
            return Ok(ExitCode::from(128));
        }
    };

    let mut rows: Vec<Row> = Vec::new();
    for r in &ref_map.remote_refs {
        push_rows(r, &mut rows);
    }
    rows.retain(|row| check_ref(row, &opts) && tail_match(patterns, &row.name));
    rows.sort_by(|a, b| a.name.as_bytes().cmp(b.name.as_bytes()));

    let mut out = String::new();
    for row in &rows {
        if opts.symref {
            if let Some(target) = &row.symref {
                out.push_str(&format!("ref: {target}\t{}\n", row.name));
            }
        }
        out.push_str(&format!("{}\t{}\n", row.oid, row.name));
    }
    print!("{out}");

    Ok(if rows.is_empty() && opts.exit_code {
        ExitCode::from(2)
    } else {
        ExitCode::SUCCESS
    })
}

/// Turn one advertised ref into its output rows.
///
/// An annotated tag yields two: the tag object under its own name, and the
/// object it points at under `<name>^{}` — exactly the pair `git upload-pack`
/// puts on the wire. Unborn refs are dropped: `ls-remote` never asks for them,
/// so stock git prints nothing for a remote with an unborn HEAD.
fn push_rows(r: &Ref, rows: &mut Vec<Row>) {
    let (name, oid, peeled, symref) = match r {
        Ref::Peeled {
            full_ref_name,
            tag,
            object,
        } => (full_ref_name, *tag, Some(*object), None),
        Ref::Direct {
            full_ref_name,
            object,
        } => (full_ref_name, *object, None, None),
        Ref::Symbolic {
            full_ref_name,
            target,
            tag,
            object,
        } => (
            full_ref_name,
            (*tag).unwrap_or(*object),
            tag.is_some().then_some(*object),
            Some(target.to_string()),
        ),
        Ref::Unborn { .. } => return,
    };

    let name = name.to_string();
    rows.push(Row {
        oid: oid.to_hex().to_string(),
        symref,
        peel: false,
        name: name.clone(),
    });
    if let Some(peeled) = peeled {
        rows.push(Row {
            name: format!("{name}^{{}}"),
            oid: peeled.to_hex().to_string(),
            symref: None,
            peel: true,
        });
    }
}

/// git's `check_ref` from `remote.c`, specialised to the rows we build.
///
/// With no type bits set everything passes. Otherwise the name must live under
/// `refs/` (so `HEAD` is dropped); `--refs` additionally drops the `^{}` rows
/// (the "magic fake tag refs" that fail `check_refname_format`); `-b`/`-t`
/// admit their own prefix; and anything else passes only when neither prefix
/// bit is set.
fn check_ref(row: &Row, opts: &Opts) -> bool {
    if !opts.branches && !opts.tags && !opts.normal {
        return true;
    }
    let Some(rest) = row.name.strip_prefix("refs/") else {
        return false;
    };
    if opts.normal && row.peel {
        return false;
    }
    if opts.branches && rest.starts_with("heads/") {
        return true;
    }
    if opts.tags && rest.starts_with("tags/") {
        return true;
    }
    !(opts.branches || opts.tags)
}

/// git's `tail_match`: each user pattern is glob-matched as `*/<pattern>`
/// against `/<refname>`, so `main` matches `refs/heads/main` but not
/// `refs/heads/mymain`, while a full name like `refs/heads/main` matches too.
///
/// `Mode::empty()` mirrors git's `wildmatch(..., 0)`, where `*` spans `/`.
fn tail_match(patterns: &[&str], name: &str) -> bool {
    if patterns.is_empty() {
        return true;
    }
    let path = format!("/{name}");
    patterns.iter().any(|p| {
        let pattern = format!("*/{p}");
        gix::glob::wildmatch(
            pattern.as_bytes().as_bstr(),
            path.as_bytes().as_bstr(),
            gix::glob::wildmatch::Mode::empty(),
        )
    })
}

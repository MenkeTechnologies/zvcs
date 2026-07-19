use anyhow::{bail, Result};
use std::collections::BTreeSet;
use std::process::ExitCode;

use gix::bstr::{BString, ByteSlice};

/// `git remote` — inspect configured remotes.
///
/// Implemented (read-only, offline):
///   * `git remote`                       → one remote name per line
///   * `git remote -v` / `--verbose`      → `<name>\t<url> (fetch)` and `(push)` lines
///   * `git remote show [-n] [<name>...]` → per-remote detail, rendered as the
///     "not queried" (offline) form that stock `git remote show -n` prints
///
/// `show` never contacts the network, so the HEAD branch and per-branch status
/// are always reported as `(not queried)` / `(status not queried)`, matching
/// `git remote show -n` exactly. The `-n`/`--no-query` flag is accepted and is a
/// no-op. Mutating subcommands (`add`, `remove`, `rename`, `set-url`, …) and the
/// networked (non-`-n`) `show` status query are not ported and are rejected.
pub fn remote(args: &[String]) -> Result<ExitCode> {
    let repo = gix::discover(".")?;

    let positionals: Vec<&str> = args
        .iter()
        .filter(|a| !a.starts_with('-'))
        .map(String::as_str)
        .collect();

    match positionals.first().copied() {
        None => list(&repo, args.iter().any(|a| a == "-v" || a == "--verbose")),
        Some("show") => show(&repo, &positionals[1..]),
        Some(other) => bail!(
            "subcommand '{other}' is not ported (only `remote [-v]` and `remote show [-n] [<name>...]` are supported)"
        ),
    }
}

/// Print configured remote names, optionally with their fetch/push URLs.
fn list(repo: &gix::Repository, verbose: bool) -> Result<ExitCode> {
    for name in repo.remote_names() {
        let name = name.to_str_lossy();
        if verbose {
            let fetch = first_effective_url(repo, &name, "url");
            let push = first_effective_url(repo, &name, "pushurl").or_else(|| fetch.clone());
            if let Some(url) = &fetch {
                println!("{name}\t{url} (fetch)");
            }
            if let Some(url) = &push {
                println!("{name}\t{url} (push)");
            }
        } else {
            println!("{name}");
        }
    }
    Ok(ExitCode::SUCCESS)
}

/// Render `git remote show -n` for each requested remote (all remotes if none
/// are named). An unknown remote name is an error, matching stock git.
fn show(repo: &gix::Repository, names: &[&str]) -> Result<ExitCode> {
    let all = repo.remote_names();

    let targets: Vec<String> = if names.is_empty() {
        all.iter().map(|n| n.to_str_lossy().into_owned()).collect()
    } else {
        let mut v = Vec::with_capacity(names.len());
        for n in names {
            if !all.iter().any(|rn| rn.to_str_lossy() == *n) {
                bail!("No such remote '{n}'");
            }
            v.push((*n).to_string());
        }
        v
    };

    for name in &targets {
        show_one(repo, name)?;
    }
    Ok(ExitCode::SUCCESS)
}

/// Render the offline (`-n`) detail block for a single remote.
fn show_one(repo: &gix::Repository, name: &str) -> Result<()> {
    let fetch = first_effective_url(repo, name, "url");
    let push = first_effective_url(repo, name, "pushurl").or_else(|| fetch.clone());

    println!("* remote {name}");
    println!("  Fetch URL: {}", fetch.as_deref().unwrap_or(name));
    println!("  Push  URL: {}", push.as_deref().unwrap_or(name));
    println!("  HEAD branch: (not queried)");

    // Remote-tracking branches (refs/remotes/<name>/*, excluding the HEAD symref).
    let branches = remote_branches(repo, name)?;
    if !branches.is_empty() {
        if branches.len() == 1 {
            println!("  Remote branch: (status not queried)");
        } else {
            println!("  Remote branches: (status not queried)");
        }
        for b in &branches {
            println!("    {b}");
        }
    }

    // Local branches configured for `git pull` (branch.<b>.remote == <name>).
    let pulls = pull_config(repo, name)?;
    if !pulls.is_empty() {
        if pulls.len() == 1 {
            println!("  Local branch configured for 'git pull':");
        } else {
            println!("  Local branches configured for 'git pull':");
        }
        let width = pulls
            .iter()
            .map(|(b, _, _)| b.chars().count())
            .max()
            .unwrap_or(0);
        let any_rebase = pulls.iter().any(|(_, rebase, _)| *rebase);

        for (bname, rebase, merges) in &pulls {
            let first = merges.first().map(String::as_str).unwrap_or("");
            if *rebase {
                // rebase requires a single merge ref; git errors on more.
                println!("    {bname:<width$} rebases onto remote {first}");
            } else if any_rebase {
                // When any listed branch rebases, git shifts the merge verb by one column.
                println!("    {bname:<width$}  merges with remote {first}");
                let also = "    and with remote";
                for m in merges.iter().skip(1) {
                    println!("    {pad:<width$} {also} {m}", pad = "");
                }
            } else {
                println!("    {bname:<width$} merges with remote {first}");
                let also = "   and with remote";
                for m in merges.iter().skip(1) {
                    println!("    {pad:<width$} {also} {m}", pad = "");
                }
            }
        }
    }

    // Push refspecs: only the default "matching" behaviour is ported.
    println!("  Local ref configured for 'git push' (status not queried):");
    println!("    (matching) pushes to (matching)");
    Ok(())
}

/// First effective URL value for `remote.<name>.<key>`, honouring git's rule
/// that an empty value clears all previously configured URLs. Returns `None`
/// when no URL remains.
fn first_effective_url(repo: &gix::Repository, name: &str, key: &str) -> Option<String> {
    let cfg = repo.config_snapshot();
    let values = cfg.plumbing().strings_by("remote", name, key)?;

    let mut effective: Vec<BString> = Vec::new();
    for value in values {
        if value.is_empty() {
            effective.clear();
        } else {
            effective.push(value);
        }
    }
    effective
        .into_iter()
        .next()
        .map(|u| u.to_str_lossy().into_owned())
}

/// Sorted short names of the remote-tracking branches for `<name>`, i.e. the
/// entries under `refs/remotes/<name>/` with the `<name>/` prefix stripped and
/// the `HEAD` symref excluded.
fn remote_branches(repo: &gix::Repository, name: &str) -> Result<Vec<String>> {
    let prefix = format!("refs/remotes/{name}/");
    let platform = repo.references()?;

    let mut out = Vec::new();
    for reference in platform.prefixed(prefix.as_bytes())? {
        let reference = reference.map_err(|e| anyhow::anyhow!("{e}"))?;
        let short = reference.name().shorten().to_str_lossy().into_owned();
        let branch = short
            .strip_prefix(&format!("{name}/"))
            .unwrap_or(&short)
            .to_string();
        if branch == "HEAD" {
            continue;
        }
        out.push(branch);
    }
    out.sort();
    Ok(out)
}

/// Local branches (sorted) whose `branch.<b>.remote` is `<name>` and that have
/// at least one `branch.<b>.merge`, returned as `(branch, rebase, merges)` with
/// merge refs shortened (`refs/heads/` stripped).
fn pull_config(repo: &gix::Repository, name: &str) -> Result<Vec<(String, bool, Vec<String>)>> {
    let cfg = repo.config_snapshot();
    let file = cfg.plumbing();

    // Collect every configured branch name (sorted, de-duplicated).
    let mut branch_names: BTreeSet<BString> = BTreeSet::new();
    if let Some(sections) = file.sections_by_name("branch") {
        for section in sections {
            if let Some(sub) = section.header().subsection_name() {
                branch_names.insert(sub.to_owned());
            }
        }
    }

    let mut out = Vec::new();
    for bn in branch_names {
        let remote = match file.string_by("branch", &bn, "remote") {
            Some(r) => r,
            None => continue,
        };
        if remote.to_str_lossy() != name {
            continue;
        }
        let merges = match file.strings_by("branch", &bn, "merge") {
            Some(m) if !m.is_empty() => m,
            _ => continue,
        };
        let rebase = file
            .boolean_by("branch", &bn, "rebase")
            .ok()
            .flatten()
            .unwrap_or(false);

        let merges: Vec<String> = merges
            .iter()
            .map(|m| {
                let s = m.to_str_lossy();
                s.strip_prefix("refs/heads/").unwrap_or(&s).to_string()
            })
            .collect();

        out.push((bn.to_str_lossy().into_owned(), rebase, merges));
    }
    out.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(out)
}

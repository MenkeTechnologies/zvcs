//! `git zscan` — a fleet-wide secret scanner.
//!
//! Scans the tracked file content of every selected indexed repo, in parallel,
//! for common credential patterns (AWS keys, private keys, provider tokens,
//! high-entropy `key = "..."` assignments), printing `path:line:pattern:snippet`
//! for each hit. Reuses the same native, fork-free scan `zgrep` does, over the
//! shared worker pool, so it sweeps a whole tree of repos in one command. Exits
//! non-zero when anything is found, so it doubles as a CI / pre-push gate.

use std::path::Path;
use std::process::ExitCode;

use anyhow::Result;
use gix::bstr::ByteSlice;

use crate::superset::query::{parallel_map, select_repos};
use crate::superset::select::Selector;

/// A named credential pattern.
struct Pat {
    name: &'static str,
    re: regex::bytes::Regex,
}

/// The built-in secret patterns. Malformed regexes are dropped rather than
/// panicking, so a bad entry can never take the scan down.
fn patterns() -> Vec<Pat> {
    let raw: &[(&str, &str)] = &[
        ("aws-access-key", r"\bAKIA[0-9A-Z]{16}\b"),
        ("private-key", r"-----BEGIN [A-Z ]*PRIVATE KEY-----"),
        ("github-token", r"\bgh[pousr]_[0-9A-Za-z]{36,}\b"),
        ("slack-token", r"\bxox[baprs]-[0-9A-Za-z-]{10,}\b"),
        ("google-api-key", r"\bAIza[0-9A-Za-z_\-]{35}\b"),
        ("jwt", r"\beyJ[A-Za-z0-9_-]{10,}\.[A-Za-z0-9_-]{10,}\.[A-Za-z0-9_-]{10,}\b"),
        (
            "generic-secret",
            r#"(?i)(api[_-]?key|secret|token|passwd|password)['"]?\s*[:=]\s*['"][0-9a-zA-Z/+_=.-]{16,}['"]"#,
        ),
    ];
    raw.iter()
        .filter_map(|(name, re)| regex::bytes::Regex::new(re).ok().map(|re| Pat { name, re }))
        .collect()
}

pub fn zscan(args: &[String]) -> Result<ExitCode> {
    let (sel, _rest) = Selector::parse(args);
    let Some(repos) = select_repos(&sel)? else { return Ok(ExitCode::SUCCESS) };
    let pats = patterns();
    let pats = &pats;
    let per = parallel_map(&repos, |gd, wd| scan_repo(gd, wd, pats));
    let mut total = 0usize;
    for hits in &per {
        for h in hits {
            println!("{h}");
            total += 1;
        }
    }
    eprintln!("zscan: {total} potential secret(s) across {} repos", repos.len());
    // Non-zero exit when secrets are found, so `git zscan` gates a push/CI run.
    Ok(if total > 0 { ExitCode::FAILURE } else { ExitCode::SUCCESS })
}

/// Scan one repo's tracked (non-conflicted) files for every pattern, capped so a
/// single flooded repo cannot dominate the output.
fn scan_repo(git_dir: &Path, workdir: &Path, pats: &[Pat]) -> Vec<String> {
    let Ok(repo) = gix::open(git_dir) else { return Vec::new() };
    let Ok(index) = repo.open_index() else { return Vec::new() };
    let mut out = Vec::new();
    for entry in index.entries() {
        if entry.stage() != gix::index::entry::Stage::Unconflicted {
            continue;
        }
        let rel = entry.path(&index);
        let full = workdir.join(rel.to_path_lossy());
        let Ok(bytes) = std::fs::read(&full) else { continue };
        if bytes.contains(&0) {
            continue; // binary — skip, like git grep
        }
        for (n, line) in bytes.split(|&b| b == b'\n').enumerate() {
            for pat in pats {
                if pat.re.is_match(line) {
                    let snippet = String::from_utf8_lossy(line);
                    let snippet = snippet.trim();
                    let snippet: String = snippet.chars().take(80).collect();
                    out.push(format!("{}:{}:{}: {}", full.display(), n + 1, pat.name, snippet));
                }
            }
        }
        if out.len() >= 500 {
            out.push(format!("{}: … (capped at 500 hits)", workdir.display()));
            break;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn patterns_compile_and_match_known_secrets() {
        let pats = patterns();
        assert!(!pats.is_empty(), "at least one pattern compiles");
        let hit = |sample: &str, name: &str| {
            pats.iter().any(|p| p.name == name && p.re.is_match(sample.as_bytes()))
        };
        assert!(hit("AKIAIOSFODNN7EXAMPLE", "aws-access-key"));
        assert!(hit("-----BEGIN RSA PRIVATE KEY-----", "private-key"));
        assert!(hit("ghp_0123456789abcdefghijklmnopqrstuvwxyz", "github-token"));
        assert!(hit(r#"api_key = "abcdef0123456789ABCDEF""#, "generic-secret"));
        // A benign line matches nothing.
        assert!(!pats.iter().any(|p| p.re.is_match(b"let x = 1; // hello world")));
    }
}

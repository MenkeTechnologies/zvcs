//! `help.autocorrect` — a faithful port of git's `help_unknown_cmd` (help.c) and
//! weighted Damerau-Levenshtein distance (levenshtein.c).
//!
//! When the leading token is neither a known verb nor an alias, git ranks every
//! command by edit distance to the typo and, depending on `help.autocorrect`,
//! either auto-runs the single nearest command or prints `git: '<cmd>' is not a
//! git command` with the closest suggestions. This reproduces that exactly,
//! including the config-value state machine (`never`/`show`/`prompt`/`immediate`
//! / a positive decisecond delay) and the prefix bonus for common commands.

use crate::dispatch;
use std::io::{IsTerminal, Write};
use std::time::Duration;

/// Distances of `SIMILARITY_FLOOR` or more are "not similar enough" to suggest
/// or correct (git's `SIMILAR_ENOUGH`).
const SIMILARITY_FLOOR: i32 = 7;

// git's help.c `AUTOCORRECT_*` sentinels for the parsed `help.autocorrect` value.
// A positive value is a delay in deciseconds; `0` means unset/undecided.
const SHOW: i32 = -4;
const PROMPT: i32 = -3;
const NEVER: i32 = -2;
const IMMEDIATELY: i32 = -1;

/// git's "common" commands — the `mainporcelain` entries that carry a help group
/// (`git help`'s grouped list, command-list.txt). A candidate in this set that
/// has the typo as a prefix earns a perfect score, git's prefix bonus.
const COMMON_CMDS: &[&str] = &[
    "add", "backfill", "bisect", "branch", "clone", "commit", "diff", "fetch", "grep", "history",
    "init", "log", "merge", "mv", "pull", "push", "rebase", "reset", "restore", "rm", "show",
    "status", "switch", "tag",
];

/// The outcome of running autocorrect over an unknown verb.
pub enum Correction {
    /// The typo resolved to this verb; dispatch it with the original arguments.
    Use(String),
    /// No correction — a diagnostic (git's "not a git command", plus any nearest
    /// suggestions, or a declined prompt) has already been printed to stderr; the
    /// caller should exit non-zero.
    None,
}

/// Port of `help_unknown_cmd`: rank commands by distance to `cmd` and act per
/// `help.autocorrect`.
pub fn correct(cmd: &str) -> Correction {
    let mut autocorrect = read_autocorrect();

    // A prompt makes no sense without a terminal to answer it — git downgrades
    // the prompt to "never" when stdin or stderr is not a tty.
    if autocorrect == PROMPT && (!std::io::stdin().is_terminal() || !std::io::stderr().is_terminal())
    {
        autocorrect = NEVER;
    }
    if autocorrect == NEVER {
        eprintln!("git: '{cmd}' is not a git command. See 'git --help'.");
        return Correction::None;
    }

    // Candidate set: every dispatchable verb plus configured aliases (git adds
    // aliases to the lookup too), scored by distance. The prefix bonus gives a
    // common command that starts with the typo a perfect score.
    let mut scored: Vec<(String, i32)> = candidate_names()
        .into_iter()
        .map(|name| {
            let score = if COMMON_CMDS.contains(&name.as_str()) && name.starts_with(cmd) {
                0
            } else {
                levenshtein(cmd, &name, 0, 2, 1, 3) + 1
            };
            (name, score)
        })
        .collect();

    if scored.is_empty() {
        eprintln!("git: '{cmd}' is not a git command. See 'git --help'.");
        return Correction::None;
    }

    // git sorts by distance, ties broken by name (its QSORT is stable on name).
    scored.sort_by(|a, b| a.1.cmp(&b.1).then_with(|| a.0.cmp(&b.0)));

    // Count leading prefix matches (score 0), then widen to all sharing the best
    // non-prefix distance — `n` ends as the size of the best-scoring group and
    // `best_similarity` as that distance, exactly as help.c computes them.
    let mut n = 0;
    while n < scored.len() && scored[n].1 == 0 {
        n += 1;
    }
    let best_similarity = if scored.len() <= n {
        // Everything is a prefix match: too ambiguous to act on.
        SIMILARITY_FLOOR + 1
    } else {
        let best = scored[n].1;
        n += 1;
        while n < scored.len() && scored[n].1 == best {
            n += 1;
        }
        best
    };
    let similar_enough = best_similarity < SIMILARITY_FLOOR;

    // Auto-correct only when enabled, not "show", and there is a single unmatched
    // best candidate close enough to trust.
    if autocorrect != 0 && autocorrect != SHOW && n == 1 && similar_enough {
        let assumed = scored[0].0.clone();
        eprintln!("WARNING: You called a Git command named '{cmd}', which does not exist.");
        match autocorrect {
            IMMEDIATELY => {
                eprintln!("Continuing under the assumption that you meant '{assumed}'.");
            }
            PROMPT => {
                eprint!("Run '{assumed}' instead [y/N]? ");
                let _ = std::io::stderr().flush();
                let mut answer = String::new();
                let _ = std::io::stdin().read_line(&mut answer);
                let answer = answer.trim_start();
                if !(answer.starts_with('y') || answer.starts_with('Y')) {
                    return Correction::None;
                }
            }
            delay => {
                // Positive value: seconds = delay/10, then run.
                eprintln!(
                    "Continuing in {:.1} seconds, assuming that you meant '{assumed}'.",
                    delay as f64 / 10.0
                );
                std::thread::sleep(Duration::from_millis(delay as u64 * 100));
            }
        }
        return Correction::Use(assumed);
    }

    eprintln!("git: '{cmd}' is not a git command. See 'git --help'.");
    if similar_enough {
        if n == 1 {
            eprintln!("\nThe most similar command is");
        } else {
            eprintln!("\nThe most similar commands are");
        }
        for (name, _) in scored.iter().take(n) {
            eprintln!("\t{name}");
        }
    }
    Correction::None
}

/// Read and parse `help.autocorrect` (`0` when unset or outside a repository).
fn read_autocorrect() -> i32 {
    let Ok(repo) = gix::discover(".") else {
        return 0;
    };
    match repo.config_snapshot().string("help.autocorrect") {
        Some(raw) => parse_autocorrect(&raw.to_string()),
        None => 0,
    }
}

/// Port of git's `parse_autocorrect` + the integer fallback in
/// `git_unknown_cmd_config`: a boolean, one of the keywords, or a decisecond
/// count (with `<0` and `1` collapsing to "immediate"). Unrecognized text is `0`
/// (undecided → no autocorrection).
fn parse_autocorrect(value: &str) -> i32 {
    match maybe_bool(value) {
        Some(true) => return IMMEDIATELY,
        Some(false) => return SHOW,
        None => {}
    }
    match value {
        "prompt" => return PROMPT,
        "never" => return NEVER,
        "immediate" => return IMMEDIATELY,
        "show" => return SHOW,
        _ => {}
    }
    match value.trim().parse::<i32>() {
        Ok(v) if v < 0 || v == 1 => IMMEDIATELY,
        Ok(v) => v,
        Err(_) => 0,
    }
}

/// git's `git_parse_maybe_bool_text` for the values relevant here: the canonical
/// boolean spellings (case-insensitive), with a valueless key counting as true.
fn maybe_bool(value: &str) -> Option<bool> {
    match value.to_ascii_lowercase().as_str() {
        "" | "true" | "yes" | "on" | "1" => Some(true),
        "false" | "no" | "off" | "0" => Some(false),
        _ => None,
    }
}

/// Every dispatchable verb plus configured alias names, de-duplicated — git's
/// candidate universe for the nearest-command search.
fn candidate_names() -> Vec<String> {
    let mut names = Vec::new();
    let mut seen = std::collections::HashSet::new();
    let builtins = dispatch::SUPERSET_VERBS
        .iter()
        .chain(dispatch::PORCELAIN_VERBS.iter());
    for &verb in builtins {
        if seen.insert(verb.to_string()) {
            names.push(verb.to_string());
        }
    }
    for alias in alias_names() {
        if seen.insert(alias.clone()) {
            names.push(alias);
        }
    }
    names
}

/// The names of every configured `alias.<name>` across all config scopes, in
/// both the `alias.name = …` and `[alias "name"] command = …` forms.
fn alias_names() -> Vec<String> {
    let Ok(repo) = gix::discover(".") else {
        return Vec::new();
    };
    let file = repo.config_snapshot().plumbing().clone();
    let mut names = Vec::new();
    if let Some(sections) = file.sections_by_name("alias") {
        for section in sections {
            match section.header().subsection_name() {
                Some(sub) => names.push(sub.to_string()),
                None => names.extend(section.value_names()),
            }
        }
    }
    names
}

/// Port of git's weighted Damerau-Levenshtein (levenshtein.c): edit distance
/// from `s1` to `s2` with per-operation weights `w` (swap), `s` (substitution),
/// `a` (insertion), `d` (deletion). Only the last three matrix rows are kept.
fn levenshtein(s1: &str, s2: &str, w: i32, s: i32, a: i32, d: i32) -> i32 {
    let b1 = s1.as_bytes();
    let b2 = s2.as_bytes();
    let (len1, len2) = (b1.len(), b2.len());

    let mut row0 = vec![0i32; len2 + 1];
    let mut row1 = vec![0i32; len2 + 1];
    let mut row2 = vec![0i32; len2 + 1];

    for j in 0..=len2 {
        row1[j] = j as i32 * a;
    }
    for i in 0..len1 {
        row2[0] = (i as i32 + 1) * d;
        for j in 0..len2 {
            // substitution
            row2[j + 1] = row1[j] + s * i32::from(b1[i] != b2[j]);
            // swap
            if i > 0
                && j > 0
                && b1[i - 1] == b2[j]
                && b1[i] == b2[j - 1]
                && row2[j + 1] > row0[j - 1] + w
            {
                row2[j + 1] = row0[j - 1] + w;
            }
            // deletion
            if row2[j + 1] > row1[j + 1] + d {
                row2[j + 1] = row1[j + 1] + d;
            }
            // insertion
            if row2[j + 1] > row2[j] + a {
                row2[j + 1] = row2[j] + a;
            }
        }
        // Rotate rows: row0 <- row1 <- row2 <- (old row0), as git does.
        std::mem::swap(&mut row0, &mut row1);
        std::mem::swap(&mut row1, &mut row2);
    }
    row1[len2]
}

#[cfg(test)]
mod tests {
    use super::{levenshtein, parse_autocorrect, IMMEDIATELY, NEVER, PROMPT, SHOW};

    #[test]
    fn distance_matches_git_weights() {
        // Weights (w=0, s=2, a=1, d=3), as help.c calls it.
        assert_eq!(levenshtein("gg", "gc", 0, 2, 1, 3), 2); // one substitution
        assert_eq!(levenshtein("comit", "commit", 0, 2, 1, 3), 1); // one insertion
        assert_eq!(levenshtein("chekout", "checkout", 0, 2, 1, 3), 1);
        assert_eq!(levenshtein("abc", "abc", 0, 2, 1, 3), 0);
    }

    #[test]
    fn parses_autocorrect_values() {
        assert_eq!(parse_autocorrect("1"), IMMEDIATELY);
        assert_eq!(parse_autocorrect("true"), IMMEDIATELY);
        assert_eq!(parse_autocorrect("immediate"), IMMEDIATELY);
        assert_eq!(parse_autocorrect("0"), SHOW);
        assert_eq!(parse_autocorrect("false"), SHOW);
        assert_eq!(parse_autocorrect("show"), SHOW);
        assert_eq!(parse_autocorrect("never"), NEVER);
        assert_eq!(parse_autocorrect("prompt"), PROMPT);
        assert_eq!(parse_autocorrect("2"), 2); // deciseconds
        assert_eq!(parse_autocorrect("30"), 30);
        assert_eq!(parse_autocorrect("-1"), IMMEDIATELY);
        assert_eq!(parse_autocorrect("garbage"), 0);
    }
}

//! Scoring and reporting.
//!
//! Two independent numbers, deliberately not blended:
//!
//!   * **Coverage** — of the subcommands stock git ships, how many does zvcs
//!     dispatch at all. Probed empirically, never read from a hand-maintained
//!     list, so it cannot drift from reality or be edited upward.
//!   * **Parity** — of the cases actually run, how many matched stock git.
//!
//! A high parity over a tiny corpus is not progress, so both are always printed
//! together and `Unsupported` counts as a failure rather than a skip.

use crate::env;
use crate::runner::{Outcome, Verdict};
use anyhow::{Context, Result};
use std::collections::BTreeMap;
use std::path::Path;
use std::process::Command;

/// The subcommands stock git ships, straight from the installed git.
///
/// Derived at runtime rather than hardcoded: a literal list would go stale the
/// moment git is upgraded, and would let the denominator be edited.
pub fn stock_subcommands() -> Result<Vec<String>> {
    let out = Command::new("git")
        .arg("--list-cmds=main")
        .output()
        .context("running `git --list-cmds=main`")?;
    anyhow::ensure!(out.status.success(), "`git --list-cmds=main` failed");
    let mut cmds: Vec<String> = String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(str::to_string)
        .collect();
    cmds.sort();
    cmds.dedup();
    Ok(cmds)
}

/// Probe which subcommands zvcs dispatches, by invoking each and reading the
/// refusal. Empirical by design — the alternative is trusting a list that
/// nothing verifies.
pub fn dispatched(zvcs_bin: &Path, home: &Path, cmds: &[String], probe_dir: &Path) -> Vec<String> {
    let mut out = Vec::new();
    for cmd in cmds {
        let mut c = Command::new(zvcs_bin);
        env::harden(&mut c, home);
        c.current_dir(probe_dir).arg(cmd);
        let Ok(res) = c.output() else { continue };
        let stderr = String::from_utf8_lossy(&res.stderr);
        // Only the dispatch-table miss means "absent". Any other outcome —
        // success, a usage error, an unsupported *flag* — means the arm exists.
        if !stderr.contains("not yet ported") {
            out.push(cmd.clone());
        }
    }
    out
}

/// Per-command tally.
#[derive(Default, Clone)]
pub struct Tally {
    pub matched: usize,
    pub unsupported: usize,
    pub stdout_diff: usize,
    pub exit_diff: usize,
    pub state_diff: usize,
    pub crash: usize,
}

impl Tally {
    pub fn total(&self) -> usize {
        self.matched + self.unsupported + self.stdout_diff + self.exit_diff + self.state_diff + self.crash
    }

    fn record(&mut self, v: Verdict) {
        match v {
            Verdict::Match => self.matched += 1,
            Verdict::Unsupported => self.unsupported += 1,
            Verdict::StdoutDiff => self.stdout_diff += 1,
            Verdict::ExitDiff => self.exit_diff += 1,
            Verdict::StateDiff => self.state_diff += 1,
            Verdict::Crash => self.crash += 1,
        }
    }

    pub fn pct(&self) -> f64 {
        if self.total() == 0 {
            0.0
        } else {
            100.0 * self.matched as f64 / self.total() as f64
        }
    }
}

pub struct Report {
    pub by_cmd: BTreeMap<String, Tally>,
    pub overall: Tally,
    pub failures: Vec<Outcome>,
}

pub fn tally(outcomes: Vec<Outcome>) -> Report {
    let mut by_cmd: BTreeMap<String, Tally> = BTreeMap::new();
    let mut overall = Tally::default();
    let mut failures = Vec::new();
    for o in outcomes {
        by_cmd.entry(o.case.cmd.to_string()).or_default().record(o.verdict);
        overall.record(o.verdict);
        if !o.verdict.is_match() {
            failures.push(o);
        }
    }
    Report { by_cmd, overall, failures }
}

/// Truncate long diffs so one pathological case cannot bury the rest.
fn clip(s: &str, lines: usize) -> String {
    let mut out: Vec<&str> = s.lines().take(lines).collect();
    if s.lines().count() > lines {
        out.push("… (truncated)");
    }
    out.join("\n")
}

impl Report {
    pub fn print(&self, coverage: (usize, usize), missing: &[String], verbose: bool) {
        let (have, total) = coverage;
        println!("\n=== zvcs parity report ===\n");
        println!(
            "coverage : {have}/{total} stock subcommands dispatched ({:.1}%)",
            if total == 0 { 0.0 } else { 100.0 * have as f64 / total as f64 }
        );
        println!(
            "parity   : {}/{} cases matched ({:.1}%)",
            self.overall.matched,
            self.overall.total(),
            self.overall.pct()
        );
        println!(
            "           unsupported={} stdout-diff={} exit-diff={} state-diff={} crash={}",
            self.overall.unsupported,
            self.overall.stdout_diff,
            self.overall.exit_diff,
            self.overall.state_diff,
            self.overall.crash
        );

        println!("\n--- per subcommand ---");
        println!("{:<14} {:>6} {:>6} {:>7} {:>6} {:>6} {:>6} {:>7}", "cmd", "total", "match", "unsupp", "out", "exit", "state", "parity");
        for (cmd, t) in &self.by_cmd {
            println!(
                "{:<14} {:>6} {:>6} {:>7} {:>6} {:>6} {:>6} {:>6.1}%",
                cmd, t.total(), t.matched, t.unsupported, t.stdout_diff, t.exit_diff, t.state_diff, t.pct()
            );
        }

        if !missing.is_empty() {
            println!("\n--- not dispatched ({}) ---", missing.len());
            for chunk in missing.chunks(8) {
                println!("  {}", chunk.join(" "));
            }
        }

        if verbose && !self.failures.is_empty() {
            println!("\n--- failures ({}) ---", self.failures.len());
            for f in &self.failures {
                println!("\n[{}] {}", f.verdict.label(), f.case.id());
                println!("  exit: stock={:?} zvcs={:?}", f.stock_code, f.zvcs_code);
                if f.stock_stdout != f.zvcs_stdout {
                    println!("  stock stdout:\n{}", clip(&f.stock_stdout, 12));
                    println!("  zvcs  stdout:\n{}", clip(&f.zvcs_stdout, 12));
                }
                if f.stock_state != f.zvcs_state {
                    println!("  !! post-state diverged");
                }
                // Both stderrs are shown, not compared: reading them side by
                // side is how you tell a real disagreement from terser prose.
                if !f.stock_stderr.is_empty() {
                    println!("  stock stderr: {}", clip(&f.stock_stderr, 4));
                }
                if !f.zvcs_stderr.is_empty() {
                    println!("  zvcs  stderr: {}", clip(&f.zvcs_stderr, 4));
                }
            }
        }
    }
}

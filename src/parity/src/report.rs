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
use crate::runner::{config_premise, AltFinding, OracleSurface, Outcome, Surface, Verdict};
use anyhow::{Context, Result};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::io::Read as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// The subcommands stock git ships, straight from the installed git.
///
/// Derived at runtime rather than hardcoded: a literal list would go stale the
/// moment git is upgraded, and would let the denominator be edited.
pub fn stock_subcommands() -> Result<Vec<String>> {
    let out = crate::stock::command()?
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
    /// Cases that agreed on stdout, exit code and post-state, and where stock git
    /// still read the two finished repositories differently.
    ///
    /// Its own field rather than a share of `state_diff` because the two say
    /// different things: a state difference means the repository's *contents*
    /// diverged, this one means they did not and the port nevertheless wrote a
    /// structure git would not have. Folding it in would send a reader looking
    /// for a content difference that is not there — which is what the whole
    /// cache-tree afternoon was.
    pub interop_diff: usize,
    /// Cases that agreed on stdout, exit code and state but not on the message —
    /// only reachable for the cases that opted into stderr comparison.
    pub stderr_diff: usize,
    pub crash: usize,
    pub nondeterministic: usize,
    /// Cases where stock never answered inside the timeout, so nothing could be
    /// compared. Excluded from the denominator like `nondeterministic`, and
    /// counted apart from it because the cause is the machine, not stock.
    pub stock_timeout: usize,
    pub hang: usize,
    /// Cases where **zvcs** did not reproduce its own stdout or post-state while
    /// stock reproduced its own.
    ///
    /// Inside the denominator, counted as a failure — see
    /// [`Verdict::ZvcsNondeterministic`] for the argument. Its own field rather
    /// than a share of `stdout_diff`/`state_diff` because "zvcs is wrong here"
    /// and "this case is not measurable" are different findings and a reader has
    /// to be able to tell them apart at a glance; folding a flake into a content
    /// bucket is how two non-bugs came to be filed as defects.
    pub zvcs_nondeterministic: usize,
    /// Cases where the two stock gits disagree with each other and zvcs
    /// reproduces the **second** one — the git the report is not measured
    /// against. Only ever non-zero on a run that had a second oracle.
    ///
    /// Inside the denominator, counted as a failure — see
    /// [`Verdict::VersionSkew`] for the argument, which is
    /// `zvcs_nondeterministic`'s argument: the condition is one the binary under
    /// test can trigger, and an exclusion a port can trigger pays a port for
    /// triggering it. Its own field because "the port is wrong" and "the port is
    /// right about a different release" are different findings, and a reader who
    /// cannot tell them apart will spend an afternoon making code match a
    /// behaviour upstream changed on purpose.
    pub version_skew: usize,
}

impl Tally {
    /// Every case run, including ones nothing could score.
    pub fn total(&self) -> usize {
        self.scored() + self.nondeterministic + self.stock_timeout
    }

    /// Cases a byte comparison can actually judge — the parity denominator.
    ///
    /// Non-deterministic cases are excluded rather than counted as failures:
    /// stock git does not reproduce them itself, so no implementation could
    /// match, and scoring them against zvcs would understate parity as surely
    /// as passing them would overstate it. The count is always printed beside
    /// the percentage so the exclusion is visible, never inferred.
    ///
    /// A zvcs-side flake is **in** this number, as a failure. The exclusion above
    /// is safe only because nothing zvcs does can trigger it; an exclusion the
    /// binary under test can trigger itself would pay a randomly-wrong port in
    /// removed cases.
    pub fn scored(&self) -> usize {
        self.matched
            + self.unsupported
            + self.stdout_diff
            + self.exit_diff
            + self.state_diff
            + self.interop_diff
            + self.stderr_diff
            + self.crash
            + self.hang
            + self.zvcs_nondeterministic
            + self.version_skew
    }

    fn record(&mut self, v: Verdict) {
        match v {
            Verdict::Match => self.matched += 1,
            Verdict::Unsupported => self.unsupported += 1,
            Verdict::StdoutDiff => self.stdout_diff += 1,
            Verdict::ExitDiff => self.exit_diff += 1,
            Verdict::StateDiff => self.state_diff += 1,
            Verdict::InteropDiff => self.interop_diff += 1,
            Verdict::StderrDiff => self.stderr_diff += 1,
            Verdict::Crash => self.crash += 1,
            Verdict::Hang => self.hang += 1,
            Verdict::ZvcsNondeterministic => self.zvcs_nondeterministic += 1,
            Verdict::VersionSkew => self.version_skew += 1,
            Verdict::Nondeterministic => self.nondeterministic += 1,
            Verdict::StockTimeout => self.stock_timeout += 1,
        }
    }

    pub fn pct(&self) -> f64 {
        if self.scored() == 0 {
            0.0
        } else {
            100.0 * self.matched as f64 / self.scored() as f64
        }
    }
}

pub struct Report {
    pub by_cmd: BTreeMap<String, Tally>,
    pub overall: Tally,
    pub failures: Vec<Outcome>,
    /// How many cases actually paid for the interop probe, and how many were run.
    ///
    /// This is the price of the interop dimension, measured rather than
    /// estimated. The whole cost argument for it is that mutating cases are a
    /// minority and the probe only fires where the git directory changed; an
    /// argument about a minority that nothing counts is one nobody can check, and
    /// a percentage written into a comment goes stale the first time a corpus
    /// entry is added. So every run prints its own.
    pub interop_probed: usize,
    pub interop_total: usize,
    /// What the second oracle cost and found, or `None` on a machine with one
    /// git — in which case nothing about this report differs from what it was
    /// before the dimension existed. See [`AltSummary`].
    pub alt: Option<AltSummary>,
}

/// One case on which the two stock gits gave different answers.
///
/// Kept for *every* such case, including the ones the port passed, because this
/// list is the dimension's most useful output on its own: it is the set of
/// behaviours where "parity" has no single answer, and therefore the set of
/// curated expectations that may be pinned to the wrong side of a git release. A
/// count alone would not let anybody check that — an unnamed number reads as
/// housekeeping, which is precisely what a version difference must never look
/// like.
pub struct OracleDisagreement {
    pub id: String,
    pub verdict: Verdict,
    pub finding: AltFinding,
    /// The first surface the two gits differed on, in `classify`'s own precedence
    /// order.
    pub surface: OracleSurface,
    /// What each of the three binaries produced on that surface. Rendered here,
    /// where the outcome is still in hand, rather than re-derived at print time
    /// from a verdict that might have been about a different surface.
    pub primary: String,
    pub alt: String,
    pub port: String,
}

/// What the second oracle cost this run, and what it concluded.
///
/// Every field is measured rather than estimated, for the reason the interop
/// counters are: the cost argument for the dimension is that it fires on the
/// small minority of cases that fail, and an argument about a minority that
/// nothing counts is one nobody can check. A percentage written into a comment
/// goes stale the first time a corpus entry is added, so every run prints its own.
pub struct AltSummary {
    pub path: PathBuf,
    pub version: (u32, u32, u32),
    /// Cases that actually paid for a third invocation.
    pub asked: usize,
    /// Cases run, so `asked/total` is the price of the dimension.
    pub total: usize,
    /// `--alt-git-every-case`: the failure gate was lifted.
    pub every_case: bool,
    /// All three binaries gave the same answer. Only reachable under
    /// `--alt-git-every-case`, where matching cases are adjudicated too.
    pub all_three_agree: usize,
    /// The two gits agreed and the port did not — a port defect corroborated by
    /// two independent releases, the strongest statement this harness makes.
    ///
    /// Counted apart from `all_three_agree` though both are "the gits agree",
    /// because the two say opposite things about the port and one number covering
    /// both would read as whichever the reader expected. An ungated run over a
    /// passing corpus produces thousands of the first and none of the second;
    /// printing their sum beside the words "the port gave another answer" would
    /// have been a false claim on every one of them.
    pub corroborated: usize,
    /// The two gits disagreed and the port reproduced the second one:
    /// [`Verdict::VersionSkew`].
    pub port_tracks_alt: usize,
    /// The two gits disagreed and the port reproduced neither.
    pub gits_disagree: usize,
    pub inconclusive: usize,
    pub disagreements: Vec<OracleDisagreement>,
}

/// What one binary produced on the surface two oracles differed on.
///
/// The exit code is rendered the way the failure block already renders it
/// (`Some(1)`), so a reader moving between the two sections is not asked to
/// learn a second notation for the same fact.
fn surface_value(
    surface: OracleSurface,
    code: Option<i32>,
    stdout: &str,
    state: &str,
    stderr: &str,
) -> String {
    match surface {
        OracleSurface::Exit => format!("{code:?}"),
        OracleSurface::Stdout => stdout.to_string(),
        OracleSurface::State => state.to_string(),
        OracleSurface::Stderr => stderr.to_string(),
    }
}

pub fn tally(
    outcomes: Vec<Outcome>,
    alt_oracle: Option<(PathBuf, (u32, u32, u32))>,
    alt_every_case: bool,
) -> Report {
    let mut by_cmd: BTreeMap<String, Tally> = BTreeMap::new();
    let mut overall = Tally::default();
    let mut failures = Vec::new();
    let mut interop_probed = 0;
    let mut interop_total = 0;
    let mut alt = alt_oracle.map(|(path, version)| AltSummary {
        path,
        version,
        asked: 0,
        total: 0,
        every_case: alt_every_case,
        all_three_agree: 0,
        corroborated: 0,
        port_tracks_alt: 0,
        gits_disagree: 0,
        inconclusive: 0,
        disagreements: Vec::new(),
    });
    for o in outcomes {
        by_cmd.entry(o.case.cmd.to_string()).or_default().record(o.verdict);
        overall.record(o.verdict);
        interop_total += 1;
        interop_probed += usize::from(o.interop_probed);
        if let Some(a) = alt.as_mut() {
            a.total += 1;
            if let Some(run) = &o.alt {
                a.asked += 1;
                match run.finding {
                    // Split on what the *port* did, which is the only thing that
                    // distinguishes the two: "both gits and the port agree" is a
                    // pass, "both gits agree and the port does not" is the
                    // strongest defect signal here, and they are the same finding
                    // from the oracles' point of view.
                    AltFinding::GitsAgree if o.verdict.is_match() => a.all_three_agree += 1,
                    AltFinding::GitsAgree => a.corroborated += 1,
                    AltFinding::PortTracksAlt => a.port_tracks_alt += 1,
                    AltFinding::GitsDisagree => a.gits_disagree += 1,
                    AltFinding::Inconclusive => a.inconclusive += 1,
                }
                if let (true, Some(surface)) = (run.finding.gits_disagreed(), run.surface) {
                    a.disagreements.push(OracleDisagreement {
                        id: o.id(),
                        verdict: o.verdict,
                        finding: run.finding,
                        surface,
                        primary: surface_value(
                            surface,
                            o.stock_code,
                            &o.stock_stdout,
                            &o.stock_state,
                            &o.stock_stderr,
                        ),
                        alt: surface_value(
                            surface,
                            run.code,
                            &run.stdout,
                            &run.state,
                            &run.stderr,
                        ),
                        port: surface_value(
                            surface,
                            o.zvcs_code,
                            &o.zvcs_stdout,
                            &o.zvcs_state,
                            &o.zvcs_stderr,
                        ),
                    });
                }
            }
        }
        if !o.verdict.is_match() {
            failures.push(o);
        }
    }
    Report { by_cmd, overall, failures, interop_probed, interop_total, alt }
}

/// Render a percentage that never rounds up to a milestone it has not reached.
///
/// 4119/4121 is 99.951%, which `{:.1}%` prints as "100.0%" — a number a reader
/// will take to mean "no failures". Anything short of every case matching is
/// capped just below, so only a genuine sweep can display 100%.
fn pct_str(matched: usize, scored: usize) -> String {
    if scored == 0 {
        return "n/a".to_string();
    }
    if matched == scored {
        return "100%".to_string();
    }
    let pct = 100.0 * matched as f64 / scored as f64;
    if pct > 99.9 {
        format!("{:.3}%", pct)
    } else {
        format!("{:.1}%", pct)
    }
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
            "parity   : {}/{} cases matched ({})",
            self.overall.matched,
            self.overall.scored(),
            pct_str(self.overall.matched, self.overall.scored())
        );
        println!(
            "           unsupported={} stdout-diff={} exit-diff={} state-diff={} \
             interop-diff={} crash={} hang={} zvcs-flaky={}",
            self.overall.unsupported,
            self.overall.stdout_diff,
            self.overall.exit_diff,
            self.overall.state_diff,
            self.overall.interop_diff,
            self.overall.crash,
            self.overall.hang,
            self.overall.zvcs_nondeterministic
        );
        // What the interop dimension cost this run, and what it reached. Printed
        // unconditionally, including the zero: a dimension that fired on nothing
        // is a dimension a reader must be able to see fired on nothing, or an
        // empty column reads as a clean bill of health rather than as an unasked
        // question.
        println!(
            "interop  : {}/{} cases probed — on every case that wrote under the git directory, \
             stock git re-read both finished repositories (`fsck --strict`, `write-tree`) \
             and the binary under test answered the same `write-tree` about each of them. \
             Three invocations per side where the gate opens, nothing at all where it does \
             not",
            self.interop_probed, self.interop_total
        );
        if self.overall.interop_diff > 0 {
            println!(
                "           interop-diff is inside the parity denominator above: stdout, exit \
                 code and post-state all agreed and stock git still read the two finished \
                 repositories differently — the port wrote a structure git would not have; \
                 `--verbose` shows what the probe saw on each side"
            );
        }
        // Three separate lines, never one "unmeasured" total: they mean different
        // things and only one of them is counted. Reading a single number here
        // would leave a reader unable to tell an oracle that would not answer from
        // a port that will not answer the same way twice.
        if self.overall.zvcs_nondeterministic > 0 {
            println!(
                "           zvcs-flaky is inside the parity denominator above: zvcs did not \
                 reproduce its own stdout or post-state while stock reproduced its own — \
                 counted as failures, never as matches; `--verbose` names them"
            );
        }
        if self.overall.nondeterministic > 0 {
            println!(
                "           excluded={} (stock git does not reproduce these itself)",
                self.overall.nondeterministic
            );
        }
        if self.overall.stock_timeout > 0 {
            println!(
                "           excluded={} (stock git did not answer within the case timeout \
                 — the machine is loaded, not the port)",
                self.overall.stock_timeout
            );
        }
        self.print_alt_summary();

        println!("\n--- per subcommand ---");
        // The version-skew column exists only on a run that had a second oracle.
        // A column that is structurally always zero reads as a clean bill of
        // health on a question nobody asked, and a one-git machine's report has
        // to stay byte-identical to what it was before this dimension existed —
        // that is the promise `stock::alt_git` returning `None` makes.
        let vskew = self.alt.is_some();
        println!(
            "{:<14} {:>6} {:>6} {:>7} {:>6} {:>6} {:>6} {:>7} {:>6}{} {:>7}",
            "cmd", "total", "match", "unsupp", "out", "exit", "state", "interop", "flaky",
            if vskew { format!("{:>7}", "vskew") } else { String::new() },
            "parity"
        );
        for (cmd, t) in &self.by_cmd {
            println!(
                "{:<14} {:>6} {:>6} {:>7} {:>6} {:>6} {:>6} {:>7} {:>6}{} {:>6.1}%",
                cmd, t.scored(), t.matched, t.unsupported, t.stdout_diff, t.exit_diff, t.state_diff, t.interop_diff, t.zvcs_nondeterministic,
                if vskew { format!("{:>7}", t.version_skew) } else { String::new() },
                t.pct()
            );
        }

        self.print_oracle_disagreements(verbose);

        if !missing.is_empty() {
            println!("\n--- not dispatched ({}) ---", missing.len());
            for chunk in missing.chunks(8) {
                println!("  {}", chunk.join(" "));
            }
        }

        // Every case nothing could score, and every case zvcs would not answer
        // twice the same way, by name and with the reason attached.
        //
        // The summary above prints counts, and a count with no names is
        // indistinguishable from coverage: "excluded=7" reads as housekeeping,
        // while seven ids under a heading that says why reads as seven cases
        // nobody measured. Printed ahead of the failures block because a reader
        // scanning a failing run needs to know what is *not* in it first.
        if verbose {
            let unscored: Vec<&Outcome> = self
                .failures
                .iter()
                .filter(|f| f.verdict.exclusion_reason().is_some())
                .collect();
            if !unscored.is_empty() {
                println!("\n--- unmeasurable + flaky ({}) ---", unscored.len());
                for f in unscored {
                    println!("  [{}] {}", f.verdict.label(), f.id());
                    if let Some(reason) = f.verdict.exclusion_reason() {
                        println!("      {reason}");
                    }
                }
            }
        }

        if verbose && !self.failures.is_empty() {
            println!("\n--- failures ({}) ---", self.failures.len());
            for f in &self.failures {
                println!("\n[{}] {}", f.verdict.label(), f.id());
                // The script, with the reported step marked. A sequence failure
                // is a statement about one invocation *and* about the premise the
                // steps before it built, and a reader who cannot see the premise
                // cannot tell a broken `--continue` from a broken `add` two lines
                // above it. The steps below the mark did not run: after a
                // divergence the two repositories are no longer the same premise,
                // so they are unmeasured — which the heading says, because
                // "unmeasured" and "passed" are different claims.
                if let Some(s) = &f.step {
                    println!(
                        "  sequence: step {} of {} ({} later step(s) not run)",
                        s.index,
                        s.total,
                        s.total - s.index
                    );
                    for line in &s.script {
                        println!("    {line}");
                    }
                }
                // The configuration that was installed into both fixtures
                // before the invocation ran.
                //
                // The id already carries every entry, and that is what makes the
                // failure reproducible. This is what makes it *readable*: a
                // reader looking at `repo:core.abbrev=4 repo:core.abbrev=12` has
                // to assemble two stanzas in their head to see which one wins,
                // and a reader looking at a raw line has to un-escape it. The
                // rendered file says both. Printed only when there is something
                // to print, so a case whose whole configuration is `-c` — which
                // the argv line above already shows — costs no extra lines.
                for (place, text) in config_premise(&f.case.config) {
                    println!("  config premise ({place}):");
                    for line in text.lines() {
                        println!("    {line}");
                    }
                }
                println!("  exit: stock={:?} zvcs={:?}", f.stock_code, f.zvcs_code);
                if f.stock_stdout != f.zvcs_stdout {
                    println!("  stock stdout:\n{}", clip(&f.stock_stdout, 12));
                    println!("  zvcs  stdout:\n{}", clip(&f.zvcs_stdout, 12));
                }
                if f.stock_state != f.zvcs_state {
                    println!("  !! post-state diverged");
                    // The probe is one `key: value` line per fact, so the lines
                    // that differ *are* the diagnosis. Printing the whole state of
                    // both sides buries it; printing only the differing keys is
                    // what a reader needs to know which fact moved.
                    for (a, b) in state_diff_lines(&f.stock_state, &f.zvcs_state).iter().take(6) {
                        println!("     stock| {a}");
                        println!("     zvcs | {b}");
                    }
                }
                // What stock git made of each side's finished repository.
                //
                // Printed whenever the two digests differ, not only when the
                // verdict is `INTEROP-DIFF`: a case classified on stdout can
                // still have left two repositories stock reads differently, and
                // that is the more actionable half of the finding. The differing
                // lines are the diagnosis — `stock index-repaired: no` beside
                // `yes` is the whole cache-tree bug in one line — so only those
                // are shown, followed by the full digest of each side, which is
                // what "print what the probe actually saw" has to mean if a
                // reader is to trust the classification without re-running.
                if f.stock_interop != f.zvcs_interop {
                    println!("  !! stock git reads the two finished repositories differently");
                    for (a, b) in state_diff_lines(&f.stock_interop, &f.zvcs_interop).iter().take(8) {
                        println!("     stock| {a}");
                        println!("     zvcs | {b}");
                    }
                    println!("  stock interop probe:\n{}", clip(&f.stock_interop, 24));
                    println!("  zvcs  interop probe:\n{}", clip(&f.zvcs_interop, 24));
                }
                // What the second zvcs run did. On a flake this is the whole
                // finding — the report has to show what differed between the two
                // zvcs runs, or "not reproducible" is an assertion rather than
                // evidence. On every other failure it is the confirmation that
                // the failure is worth an afternoon.
                if let Some(r) = &f.zvcs_repeat {
                    match r.disagreement {
                        Some(Surface::Stdout) => {
                            println!("  !! zvcs did not reproduce its own {}", Surface::Stdout.name());
                            println!("  zvcs  stdout (run 1):\n{}", clip(&f.zvcs_stdout, 12));
                            println!("  zvcs  stdout (run 2):\n{}", clip(&r.stdout, 12));
                        }
                        Some(Surface::State) => {
                            println!("  !! zvcs did not reproduce its own {}", Surface::State.name());
                            for (a, b) in state_diff_lines(&f.zvcs_state, &r.state).iter().take(6) {
                                println!("     run 1| {a}");
                                println!("     run 2| {b}");
                            }
                        }
                        // An interop difference the port does not reproduce.
                        // Only reachable on a case that was classified
                        // `INTEROP-DIFF`, which is the only verdict whose repeat
                        // is asked for this digest at all — see
                        // `runner::interop_disagreement` for why widening that
                        // would move numbers nobody chose to move.
                        Some(Surface::Interop) => {
                            println!("  !! zvcs did not reproduce its own {}", Surface::Interop.name());
                            for (a, b) in state_diff_lines(&f.zvcs_interop, &r.interop).iter().take(6) {
                                println!("     run 1| {a}");
                                println!("     run 2| {b}");
                            }
                        }
                        None if r.timed_out => println!(
                            "  !! zvcs repeat hit the case timeout — no conclusion drawn from it"
                        ),
                        None => println!("  (zvcs reproduced this exactly on a second run)"),
                    }
                    // Shown, never judged: the repeat compares stdout and
                    // post-state only (`repeat_disagreement`), so an exit code
                    // that moved between the two runs is information the reader
                    // needs and not a reason the case is in the bucket it is in.
                    if r.code != f.zvcs_code {
                        println!(
                            "  note: zvcs exit code differed between its own two runs \
                             (run 1={:?} run 2={:?}); not one of the surfaces the repeat judges",
                            f.zvcs_code, r.code
                        );
                    }
                }
                // Both stderrs are shown, not compared: reading them side by
                // side is how you tell a real disagreement from terser prose.
                if !f.stock_stderr.is_empty() {
                    println!("  stock stderr: {}", clip(&f.stock_stderr, 4));
                }
                if !f.zvcs_stderr.is_empty() {
                    println!("  zvcs  stderr: {}", clip(&f.zvcs_stderr, 4));
                }
                // Which git said what, on a failure a second oracle was asked
                // about. Printed inside the failure block rather than only in the
                // listing below, because this is the line that decides what a
                // reader does next: `both gits agree` means open an editor,
                // `the two gits disagree` means find out which release changed
                // and decide which one this port is tracking.
                if let Some(a) = &f.alt {
                    match a.finding {
                        AltFinding::GitsAgree => println!(
                            "  second oracle: git {} agrees with the primary oracle byte for \
                             byte — two independent releases say this is the port's difference",
                            crate::stock::version_label(a.version)
                        ),
                        AltFinding::PortTracksAlt => println!(
                            "  !! the two stock gits disagree on {} and zvcs reproduces git {} \
                             — a version difference, not a port defect; counted as a failure \
                             against the targeted git all the same",
                            a.surface.map(|s| s.name()).unwrap_or("this case"),
                            crate::stock::version_label(a.version)
                        ),
                        AltFinding::GitsDisagree => println!(
                            "  !! the two stock gits disagree on {} and zvcs matches neither — \
                             the expected value here is version-dependent, so no single answer \
                             makes this case a pass",
                            a.surface.map(|s| s.name()).unwrap_or("this case")
                        ),
                        AltFinding::Inconclusive => println!(
                            "  second oracle: git {} did not reproduce its own answer, or hit \
                             the case timeout — nothing concluded from it in either direction",
                            crate::stock::version_label(a.version)
                        ),
                    }
                    if a.finding.gits_disagreed() {
                        println!("  git {} said:\n{}", crate::stock::version_label(a.version), clip(&a.stdout, 12));
                    }
                }
            }
        }
    }

    /// What the second oracle cost this run and what it concluded — the header
    /// block's last stanza, and nothing at all when the machine has one git.
    ///
    /// Prints even when every count is zero, for the reason the interop line
    /// does: a dimension that fired on nothing is a dimension a reader has to be
    /// able to see fired on nothing, or the silence reads as a clean bill of
    /// health rather than as an unasked question. The price is stated as
    /// `asked/total` because that ratio *is* the cost argument for the gate, and
    /// a ratio nobody counts is one nobody can check.
    fn print_alt_summary(&self) {
        let Some(a) = &self.alt else {
            return;
        };
        println!(
            "oracle 2 : {} (git {}) — {}/{} cases re-run against it ({}), one extra invocation \
             and one extra state probe each",
            a.path.display(),
            crate::stock::version_label(a.version),
            a.asked,
            a.total,
            if a.every_case {
                "--alt-git-every-case: every case, including the ones that matched"
            } else {
                "failures only, and only the verdicts a second git can speak to"
            }
        );
        println!(
            "           all-three-agree={} corroborated-defect={} version-skew={} \
             gits-disagree={} inconclusive={}",
            a.all_three_agree,
            a.corroborated,
            a.port_tracks_alt,
            a.gits_disagree,
            a.inconclusive
        );
        if a.corroborated > 0 {
            println!(
                "           corroborated-defect is the strongest signal this harness produces: \
                 two independent git releases gave the same answer and the port gave another one"
            );
        }
        if a.gits_disagree > 0 {
            println!(
                "           gits-disagree={} is not a port excuse: the two gits differ and zvcs \
                 matches neither, so no choice of target version makes those cases pass. They \
                 keep the verdict they earned and are listed below because the expected value \
                 on them is version-dependent",
                a.gits_disagree
            );
        }
        if self.overall.version_skew > 0 {
            // The one number in this report whose treatment somebody will want
            // to argue about, so the argument is printed beside it rather than
            // left in a doc comment nobody reading a terminal will open.
            println!(
                "           vskew={} is inside the parity denominator above, counted as \
                 failures: the two gits disagree and zvcs reproduces git {}, which is not the \
                 git this port targets. Excluding them would be an exclusion the port can \
                 trigger — reproduce the older git on the hard cases and the denominator \
                 shrinks. Forgiving them instead would read {}/{} ({}); the headline number \
                 keeps its one meaning",
                self.overall.version_skew,
                crate::stock::version_label(a.version),
                self.overall.matched + self.overall.version_skew,
                self.overall.scored(),
                pct_str(
                    self.overall.matched + self.overall.version_skew,
                    self.overall.scored()
                )
            );
        }
    }

    /// Every case on which the two stock gits gave different answers.
    ///
    /// The dimension's most valuable output, and the reason it is printed
    /// unconditionally rather than under `--verbose`: this is the set of
    /// behaviours where "parity" has no single answer, so it is also the set of
    /// curated expectations that may be pinned to the wrong side of a git
    /// release. A reader who never passes `--verbose` still needs to know that
    /// the corpus contains such cases and which ones they are.
    ///
    /// `--verbose` adds what each of the three binaries actually produced, which
    /// is what a reader needs to decide which side an expectation should be
    /// pinned to — but the ids alone are already actionable, and printing three
    /// clipped payloads per case by default would bury them.
    fn print_oracle_disagreements(&self, verbose: bool) {
        let Some(a) = &self.alt else {
            return;
        };
        if a.disagreements.is_empty() {
            return;
        }
        // Asked of `stock` rather than carried on the summary: the primary
        // oracle's version is a property of the run, not of a case, and a second
        // copy of it is a second thing that can be wrong. `unwrap_or` cannot
        // fire here — a resolved second oracle implies a resolved first one — but
        // it is spelled rather than unwrapped so a report never panics.
        let primary_version = crate::stock::git_version().unwrap_or((0, 0, 0));
        println!(
            "\n--- stock git disagrees with stock git ({}) ---",
            a.disagreements.len()
        );
        println!(
            "  the primary oracle and git {} answered differently here, so there is no single \
             \"what git does\" for these cases",
            crate::stock::version_label(a.version)
        );
        for d in &a.disagreements {
            println!(
                "  [{}] {} — differ on {} ({})",
                d.finding.label(),
                d.id,
                d.surface.name(),
                d.verdict.label()
            );
            // The lines the two gits differ on, not the first eight lines of
            // each answer. That distinction is the whole usefulness of this
            // block: `filter-branch` prints an eight-line deprecation warning
            // before it says anything, so three clipped payloads printed in full
            // are three identical warnings and the finding is below the fold.
            // The report already has this exactly right for state and interop
            // digests, and this is the same tool pointed at a third pair.
            if verbose {
                let (pv, av) = (
                    crate::stock::version_label(primary_version),
                    crate::stock::version_label(a.version),
                );
                for (x, y) in state_diff_lines(&d.primary, &d.alt).iter().take(6) {
                    println!("     git {pv}| {x}");
                    println!("     git {av}| {y}");
                }
                // Where the port sits is already in the finding, so it is spelled
                // out only when the finding is "neither" — the one case where a
                // reader cannot derive zvcs's answer from the two above it.
                if d.finding == AltFinding::GitsDisagree && d.port != d.primary {
                    for (x, _) in state_diff_lines(&d.port, &d.primary).iter().take(4) {
                        println!("     zvcs     | {x}");
                    }
                }
            }
        }
    }
}

/// The `key: value` lines on which two state probes disagree, paired up by
/// position — including a line one side has and the other does not.
fn state_diff_lines(stock: &str, zvcs: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut a = stock.lines();
    let mut b = zvcs.lines();
    loop {
        match (a.next(), b.next()) {
            (None, None) => break,
            (x, y) if x == y => continue,
            (x, y) => out.push((
                x.unwrap_or("<absent>").to_string(),
                y.unwrap_or("<absent>").to_string(),
            )),
        }
    }
    out
}

/// Minimal HTML escape for the few dynamic strings the report interpolates
/// (command names, git version). Command names are `[a-z0-9-]`, so this only
/// ever matters for defense in depth.
fn esc(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}

/// Best-effort short git version (`2.55.0`) of the reference binary the corpus
/// was compared against.
fn git_version() -> String {
    crate::stock::command()
        .ok()
        .and_then(|mut c| c.arg("--version").output().ok())
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().trim_start_matches("git version ").to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "unknown".into())
}

/// Generation date: `PORT_REPORT_DATE` override (for reproducible builds), else
/// today via `date`. Never fails the report — falls back to "unknown".
fn report_date() -> String {
    if let Ok(d) = std::env::var("PORT_REPORT_DATE") {
        if !d.is_empty() {
            return d;
        }
    }
    Command::new("date")
        .arg("+%Y-%m-%d")
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "unknown".into())
}

/// Write the HTML port report to `path` from THIS run's real numbers.
///
/// Every figure on the page is derived from the arguments — empirical dispatch
/// coverage (`have`/`stock`, probed by invoking the binary) and per-command
/// differential parity (`rep`, byte-compared against stock git). Nothing is
/// hand-classified, so the page cannot drift from what the harness measured;
/// regenerate with `zvcs-parity --bin <git> --html docs/port_report.html`.
pub fn emit_html(
    path: &Path,
    rep: &Report,
    stock: &[String],
    have: &[String],
    missing: &[String],
    opts: &BTreeMap<String, CmdOpts>,
    cfg: &BTreeMap<String, bool>,
) -> Result<()> {
    let git_v = esc(&git_version());
    let date = esc(&report_date());

    let stock_n = stock.len();
    let dispatched = have.len();
    let cov_pct = if stock_n == 0 {
        0.0
    } else {
        100.0 * dispatched as f64 / stock_n as f64
    };
    let matched = rep.overall.matched;
    let scored = rep.overall.scored();
    let mismatches = scored - matched;
    let corpus_cmds = rep.by_cmd.len();
    let parity = pct_str(matched, scored);
    let cfg_total = cfg.len();
    let cfg_ok = cfg.values().filter(|v| **v).count();

    // Commands dispatched but not yet exercised by any corpus case — the honest
    // limit of the behavioral number: parity is 100% *of what was tested*, and
    // this is what was not.
    let tested: BTreeSet<&str> = rep.by_cmd.keys().map(String::as_str).collect();
    let mut untested: Vec<&str> = have
        .iter()
        .map(String::as_str)
        .filter(|c| !tested.contains(c))
        .collect();
    untested.sort_unstable();

    // Per-command parity rows, worst parity first so any regression sits at the top.
    let mut rows: Vec<(&String, &Tally)> = rep.by_cmd.iter().collect();
    rows.sort_by(|a, b| {
        a.1.pct()
            .partial_cmp(&b.1.pct())
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(a.0.cmp(b.0))
    });

    // The version-skew column exists on this page for exactly the run that could
    // have produced one. It is inside `cases` and outside `match` like every other
    // failure bucket, so without a column a version difference would show up as a
    // gap between the two that nothing on the page explains — which is the
    // "sends a reader hunting for a bug that is not there" failure the two
    // comments below are about. A run with one git emits the page it always did.
    let vskew_col = rep.alt.is_some();
    let mut cmd_rows = String::new();
    for (cmd, t) in &rows {
        let cls = if t.matched == t.scored() { "ok" } else { "bad" };
        let _ = write!(
            cmd_rows,
            "<tr data-h=\"{h}\"><td class=\"cmd\">{c}</td><td>{tot}</td><td>{m}</td>\
             <td>{o}</td><td>{e}</td><td>{s}</td><td>{i}</td><td>{f}</td>{v}<td class=\"{cls}\">{p}</td></tr>",
            h = esc(cmd),
            c = esc(cmd),
            tot = t.scored(),
            m = t.matched,
            o = t.stdout_diff,
            e = t.exit_diff,
            s = t.state_diff,

            // Its own column for the same reason it is its own verdict: a row

            // whose gap is explained by neither stdout, exit code nor state

            // sends a reader hunting for a content difference that is not there.

            i = t.interop_diff,
            // Its own column rather than folded into the diff columns: a row whose
            // gap between `cases` and `match` is explained by nothing else visible
            // sends a reader looking for a bug that is not there.
            f = t.zvcs_nondeterministic,
            // Its own column for the third time and the same reason: "the port is
            // wrong" and "the port is right about an older release" are different
            // findings, and one of them is not a bug to fix.
            v = if vskew_col { format!("<td>{}</td>", t.version_skew) } else { String::new() },
            cls = cls,
            p = pct_str(t.matched, t.scored()),
        );
    }

    let list_cells = |cmds: &[&str]| -> String {
        if cmds.is_empty() {
            return "<span class=\"none\">none</span>".to_string();
        }
        cmds.iter()
            .map(|c| format!("<span class=\"chip\">{}</span>", esc(c)))
            .collect::<Vec<_>>()
            .join(" ")
    };
    let missing_refs: Vec<&str> = missing.iter().map(String::as_str).collect();

    let mut html = String::with_capacity(24 * 1024);
    // Head: the shared HUD chrome (hud-static.css + tutorial.css + the Orbitron /
    // Share Tech Mono fonts), then this page's supplemental styles. The theme,
    // CRT/neon toggles and the eight-scheme picker all come from hud-theme.js,
    // exactly like index.html / report.html across the fleet.
    html.push_str(
        "<!DOCTYPE html>\n<html lang=\"en\">\n<head>\n\
         <meta charset=\"utf-8\">\n\
         <meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\n\
         <meta name=\"color-scheme\" content=\"dark light\">\n\
         <meta name=\"description\" content=\"zvcs — Git parity / port report. Machine-generated by the zvcs-parity differential harness: empirical dispatch coverage of stock git's main subcommands and byte-for-byte parity (stdout, exit code, repository state) over a curated + fuzzed corpus. No hand-classified numbers.\">\n\
         <title>zvcs &mdash; Git Parity / Port Report</title>\n\
         <link rel=\"preconnect\" href=\"https://fonts.googleapis.com\">\n\
         <link rel=\"preconnect\" href=\"https://fonts.gstatic.com\" crossorigin>\n\
         <link href=\"https://fonts.googleapis.com/css2?family=Orbitron:wght@400;600;700;900&family=Share+Tech+Mono&display=swap\" rel=\"stylesheet\">\n\
         <link rel=\"stylesheet\" href=\"hud-static.css\">\n\
         <link rel=\"stylesheet\" href=\"tutorial.css\">\n\
         <style>\n",
    );
    html.push_str(REPORT_CSS);
    html.push_str("\n</style>\n</head>\n<body>\n");

    // App shell + CRT overlays + header/toolbar + the color-scheme strip that
    // hud-theme.js fills (#hudSchemeGrid) and the toggle buttons it binds.
    html.push_str(
        "<div class=\"app tutorial-app\" id=\"docsApp\">\n\
         <div class=\"crt-scanline\" id=\"crtH\" aria-hidden=\"true\"></div>\n\
         <div class=\"crt-scanline-v\" id=\"crtV\" aria-hidden=\"true\"></div>\n\
         <header class=\"tutorial-header\">\n\
         <div class=\"tutorial-header-inner\">\n\
         <div>\n\
         <h1 class=\"tutorial-brand\">// ZVCS — GIT PARITY / PORT REPORT</h1>\n\
         <nav class=\"tutorial-crumbs\" aria-label=\"Breadcrumb\">\n\
         <a href=\"index.html\">Docs</a><span class=\"sep\">/</span>\
         <span class=\"current\">Port Report</span><span class=\"sep\">/</span>\
         <a href=\"https://github.com/MenkeTechnologies/zvcs\" target=\"_blank\" rel=\"noopener noreferrer\">GitHub</a>\n\
         </nav>\n",
    );
    let _ = write!(
        html,
        "<p class=\"docs-build-line\">Generated by <code>zvcs-parity --html</code> vs stock \
         <code>git {git_v}</code> · {date} · every figure measured at generation time, \
         nothing hand-classified</p>\n"
    );
    html.push_str(
        "</div>\n\
         <div class=\"tutorial-toolbar\">\n\
         <button type=\"button\" class=\"btn btn-secondary\" id=\"btnTheme\" title=\"Toggle light/dark\">Theme</button>\n\
         <button type=\"button\" class=\"btn btn-secondary active\" id=\"btnCrt\" title=\"CRT scanline overlay\">CRT</button>\n\
         <button type=\"button\" class=\"btn btn-secondary active\" id=\"btnNeon\" title=\"Neon border pulse\">Neon</button>\n\
         <a class=\"btn btn-secondary\" href=\"index.html\">Docs</a>\n\
         <a class=\"btn btn-secondary\" href=\"report.html\">Engineering Report</a>\n\
         </div>\n\
         </div>\n\
         </header>\n\
         <div class=\"hub-scheme-strip\">\n<div class=\"hub-scheme-strip-inner\">\n\
         <span class=\"hud-scheme-label\">// Color scheme</span>\n\
         <div class=\"scheme-grid\" id=\"hudSchemeGrid\"></div>\n\
         </div>\n</div>\n\
         <main class=\"tutorial-main\">\n\
         <h2 class=\"tutorial-title\"><span class=\"step-hash\">&gt;_</span>GIT PARITY / PORT REPORT</h2>\n",
    );

    let _ = write!(
        html,
        "<p class=\"tutorial-subtitle\"><b>Dispatch coverage</b> is empirical — the harness \
         runs each of stock git's {stock_n} main subcommands through the binary and counts \
         the ones that don't hit the \"not yet ported\" dispatch miss. <b>Parity</b> is a \
         differential test — for {scored} curated + fuzzed cases it compares zvcs's stdout, \
         exit code, and post-command repository state byte-for-byte against stock git, and \
         for every case that wrote to the repository it also hands both finished \
         repositories back to stock git (<code>fsck --strict</code>, <code>write-tree</code>) \
         and compares what stock makes of them — a port that writes what git cannot read \
         is as broken as one that prints the wrong thing. A case counts only when all of \
         them match. It does <b>not</b> assert per-command \
         feature completeness: a command with no corpus case is dispatched but untested, \
         listed below. A high parity over a narrow corpus is not full parity, so both \
         numbers are always shown together.</p>\n"
    );

    // Stat cards (HUD .stat-grid / .stat-card).
    html.push_str("<div class=\"stat-grid\">\n");
    let _ = write!(html, "<div class=\"stat-card\"><div class=\"stat-val\">{dispatched}/{stock_n}</div><div class=\"stat-label\">Dispatched</div></div>\n");
    let _ = write!(html, "<div class=\"stat-card\"><div class=\"stat-val\">{cov_pct:.0}%</div><div class=\"stat-label\">Dispatch coverage</div></div>\n");
    let _ = write!(html, "<div class=\"stat-card\"><div class=\"stat-val ok\">{parity}</div><div class=\"stat-label\">Parity ({matched}/{scored})</div></div>\n");
    let _ = write!(html, "<div class=\"stat-card\"><div class=\"stat-val {mm}\">{mismatches}</div><div class=\"stat-label\">Mismatches</div></div>\n", mm = if mismatches == 0 { "ok" } else { "bad" });
    let _ = write!(html, "<div class=\"stat-card\"><div class=\"stat-val accent\">{corpus_cmds}</div><div class=\"stat-label\">Cmds in corpus</div></div>\n");
    let _ = write!(html, "<div class=\"stat-card\"><div class=\"stat-val\">{cfg_ok}/{cfg_total}</div><div class=\"stat-label\">Config vars ref'd</div></div>\n");
    html.push_str("</div>\n");

    // Per-command parity table (HUD .file-table).
    let _ = write!(html, "<h3 class=\"section-h\">Per-command parity — {corpus_cmds} commands with corpus cases</h3>\n");
    html.push_str("<div class=\"controls\"><input id=\"q\" type=\"search\" placeholder=\"// filter commands…\" autocomplete=\"off\" spellcheck=\"false\"><span id=\"cnt\"></span></div>\n");
    html.push_str("<table class=\"file-table\" id=\"tbl\"><thead><tr><th>command</th><th>cases</th><th>match</th><th>out&ne;</th><th>exit&ne;</th><th>state&ne;</th><th title=\"stdout, exit code and post-state all agreed, and stock git still read the two finished repositories differently: the port wrote a structure git would not have\">interop&ne;</th><th title=\"zvcs did not reproduce its own output or post-state on a second run; counted as failures\">flaky</th>");
    if vskew_col {
        html.push_str("<th title=\"the two installed stock gits disagree here and zvcs reproduces the older one: a version difference, not a port defect. Counted as a failure against the git this port targets, never excluded from the denominator\">vskew</th>");
    }
    html.push_str("<th>parity</th></tr></thead><tbody>\n");
    html.push_str(&cmd_rows);
    html.push_str("</tbody></table>\n");

    let _ = write!(html, "<h3 class=\"section-h\">Dispatched, not yet in the parity corpus — {}</h3>\n", untested.len());
    html.push_str("<p class=\"muted\">These commands dispatch (a real code path exists) but have no differential-test case yet, so they are excluded from the parity number rather than counted as passing.</p>\n");
    let _ = write!(html, "<p class=\"chips\">{}</p>\n", list_cells(&untested));

    let _ = write!(html, "<h3 class=\"section-h\">Not dispatched — {}</h3>\n", missing.len());
    html.push_str("<p class=\"muted\">Stock git subcommands whose zvcs dispatch arm still hits \"not yet ported\".</p>\n");
    let _ = write!(html, "<p class=\"chips\">{}</p>\n", list_cells(&missing_refs));

    // Per-command option support matrix — every option in stock git's
    // `git <cmd> -h`, probed against the binary.
    let opt_total: usize = opts.values().map(|c| c.rows.len()).sum();
    let opt_ok: usize = opts.values().map(CmdOpts::supported).sum();
    let opt_cmds = opts.values().filter(|c| !c.rows.is_empty()).count();
    let opt_pct = if opt_total == 0 {
        0.0
    } else {
        100.0 * opt_ok as f64 / opt_total as f64
    };

    html.push_str("<hr class=\"section-rule\">\n");
    let _ = write!(
        html,
        "<h3 class=\"section-h\">Option support matrix — {opt_ok}/{opt_total} options across {opt_cmds} commands ({opt_pct:.0}%)</h3>\n"
    );
    html.push_str(
        "<p class=\"muted\">Every option stock git advertises in <code>git &lt;cmd&gt; -h</code>, \
         probed against the binary: <span class=\"yes\">✓</span> the flag is parsed, \
         <span class=\"no\">✗</span> it is rejected as unknown/unsupported. A parsed flag \
         is not proof the behavior is complete — that is what the parity corpus tests — only \
         that the flag is recognized. Click a command to expand its options.</p>\n",
    );
    html.push_str("<div class=\"controls\"><input id=\"q2\" type=\"search\" placeholder=\"// filter commands…\" autocomplete=\"off\" spellcheck=\"false\"><span id=\"cnt2\"></span></div>\n");
    html.push_str("<div id=\"optmatrix\">\n");
    for (cmd, co) in opts {
        if co.rows.is_empty() {
            continue;
        }
        let ok = co.supported();
        let tot = co.rows.len();
        let all = ok == tot;
        let _ = write!(
            html,
            "<details class=\"optcmd\" data-h=\"{c}\"><summary><span class=\"oc-cmd\">{c}</span>\
             <span class=\"oc-tally {cls}\">{ok}/{tot}</span></summary>\n\
             <table class=\"file-table\"><thead><tr><th>option</th><th>short</th><th>arg</th><th>zvcs</th></tr></thead><tbody>\n",
            c = esc(cmd),
            cls = if all { "ok" } else { "part" },
            ok = ok,
            tot = tot,
        );
        for r in &co.rows {
            let _ = write!(
                html,
                "<tr><td class=\"cmd\">{flag}</td><td>{short}</td><td>{arg}</td><td class=\"{cls}\">{mark}</td></tr>",
                flag = esc(&r.flag),
                short = r.short.as_deref().map(esc).unwrap_or_default(),
                arg = if r.takes_arg { "&lt;arg&gt;" } else { "" },
                cls = if r.supported { "ok" } else { "bad" },
                mark = if r.supported { "✓" } else { "✗" },
            );
        }
        html.push_str("</tbody></table></details>\n");
    }
    html.push_str("</div>\n");

    // Config-variable support matrix, grouped by section.
    let cfg_pct = if cfg_total == 0 {
        0.0
    } else {
        100.0 * cfg_ok as f64 / cfg_total as f64
    };
    // Group keys by their top-level section (the part before the first dot).
    let mut sections: BTreeMap<&str, Vec<(&String, bool)>> = BTreeMap::new();
    for (k, v) in cfg {
        let sec = k.split('.').next().unwrap_or(k);
        sections.entry(sec).or_default().push((k, *v));
    }

    html.push_str("<hr class=\"section-rule\">\n");
    let _ = write!(
        html,
        "<h3 class=\"section-h\">Config variable support — {cfg_ok}/{cfg_total} referenced in source ({cfg_pct:.0}%)</h3>\n"
    );
    html.push_str(
        "<p class=\"muted\">Every variable from stock <code>git help --config</code>, checked against the \
         source the <code>git</code> binary is built from — the extensions crate plus the vendored \
         gitoxide. <span class=\"yes\">✓</span> the key is referenced (read/honored somewhere), \
         <span class=\"no\">✗</span> no reference found. This is source evidence, not a behavioral \
         guarantee, and it undercounts keys gitoxide reaches through split section/name access rather \
         than a dotted literal. Grouped by section; click to expand.</p>\n",
    );
    html.push_str("<div class=\"controls\"><input id=\"q3\" type=\"search\" placeholder=\"// filter config sections + keys…\" autocomplete=\"off\" spellcheck=\"false\"><span id=\"cnt3\"></span></div>\n");
    html.push_str("<div id=\"cfgmatrix\">\n");
    for (sec, mut rows) in sections {
        rows.sort_by(|a, b| a.0.cmp(b.0));
        let ok = rows.iter().filter(|(_, v)| *v).count();
        let tot = rows.len();
        let all = ok == tot;
        let keys_h: String = rows
            .iter()
            .map(|(k, _)| k.to_lowercase())
            .collect::<Vec<_>>()
            .join(" ");
        let _ = write!(
            html,
            "<details class=\"optcmd\" data-h=\"{sec} {keys}\"><summary><span class=\"oc-cmd\">{sec}</span>\
             <span class=\"oc-tally {cls}\">{ok}/{tot}</span></summary>\n\
             <table class=\"file-table\"><thead><tr><th>variable</th><th>zvcs</th></tr></thead><tbody>\n",
            sec = esc(sec),
            keys = esc(&keys_h),
            cls = if all { "ok" } else { "part" },
        );
        for (k, v) in &rows {
            let _ = write!(
                html,
                "<tr><td class=\"cmd\">{key}</td><td class=\"{cls}\">{mark}</td></tr>",
                key = esc(k),
                cls = if *v { "ok" } else { "bad" },
                mark = if *v { "✓" } else { "✗" },
            );
        }
        html.push_str("</tbody></table></details>\n");
    }
    html.push_str("</div>\n");

    html.push_str("<hr class=\"section-rule\">\n<p class=\"muted\">Source of truth: the <code>zvcs-parity</code> harness (<code>src/parity</code>). Regenerate after any port work: <code>cargo run -p zvcs-parity -- --bin target/release/git --html docs/port_report.html</code>.</p>\n");
    html.push_str("</main>\n</div>\n<script src=\"hud-theme.js\"></script>\n<script>\n");
    html.push_str(REPORT_JS);
    html.push_str("\n</script>\n</body>\n</html>\n");

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).ok();
    }
    std::fs::write(path, html).with_context(|| format!("writing {}", path.display()))?;
    Ok(())
}

/// One git option and whether the zvcs binary recognizes it.
pub struct OptRow {
    /// The flag as probed — the canonical long form (`--message`) when git lists
    /// one, else the short form (`-m`).
    pub flag: String,
    /// The short alias for display, when the option has both forms.
    pub short: Option<String>,
    /// Whether the option takes a value (`<arg>` / `=<arg>` in git's usage).
    pub takes_arg: bool,
    /// True iff the zvcs binary parsed the flag instead of rejecting it as an
    /// unknown/unsupported option.
    pub supported: bool,
}

/// Every option stock git advertises for one command, with zvcs's support.
pub struct CmdOpts {
    pub rows: Vec<OptRow>,
}

impl CmdOpts {
    pub fn supported(&self) -> usize {
        self.rows.iter().filter(|r| r.supported).count()
    }
}

/// Parse the option list out of a `git <cmd> -h` dump.
///
/// git prints an indented option list beneath the usage synopsis: lines like
/// `    -m, --message <message>`. Synopsis and description lines are skipped —
/// only lines indented ≥4 spaces whose first non-space is `-` are option specs.
/// Both forms of each entry are captured; the long form is preferred for probing.
fn parse_options(text: &str) -> Vec<(String, Option<String>, bool)> {
    let mut seen = BTreeSet::new();
    let mut out = Vec::new();
    for line in text.lines() {
        let trimmed = line.trim_start();
        let indent = line.len() - trimmed.len();
        if indent < 4 || !trimmed.starts_with('-') {
            continue;
        }
        // The spec is everything before the 2+ space gap that precedes the
        // description (or the whole line when the description wraps to the next).
        let spec = match trimmed.find("  ") {
            Some(i) => &trimmed[..i],
            None => trimmed,
        };
        let takes_arg = spec.contains('<') || spec.contains('=');
        let mut long: Option<String> = None;
        let mut short: Option<String> = None;
        for tok in spec.split(',') {
            let tok = tok.trim();
            if let Some(rest) = tok.strip_prefix("--") {
                // `--[no-]quiet` → `--quiet`; stop at the first non-name byte.
                let rest = rest.replacen("[no-]", "", 1);
                let name: String = rest
                    .chars()
                    .take_while(|c| c.is_ascii_alphanumeric() || *c == '-')
                    .collect();
                if !name.is_empty() {
                    long = Some(format!("--{name}"));
                }
            } else if tok.starts_with('-') && tok.len() >= 2 {
                let c = tok.as_bytes()[1] as char;
                if c.is_ascii_alphanumeric() {
                    short = Some(format!("-{c}"));
                }
            }
        }
        if let Some(flag) = long.clone().or_else(|| short.clone()) {
            if seen.insert(flag.clone()) {
                out.push((flag, short, takes_arg));
            }
        }
    }
    out
}

/// The options stock git advertises for `cmd`, from `git <cmd> -h`.
fn git_options(cmd: &str) -> Vec<(String, Option<String>, bool)> {
    let Ok(mut stock) = crate::stock::command() else {
        return Vec::new();
    };
    let out = match stock.arg(cmd).arg("-h").output() {
        Ok(o) => o,
        Err(_) => return Vec::new(),
    };
    // git prints `-h` usage to stderr; a few plumbing commands use stdout.
    let mut text = String::from_utf8_lossy(&out.stderr).into_owned();
    text.push('\n');
    text.push_str(&String::from_utf8_lossy(&out.stdout));
    parse_options(&text)
}

/// Probe whether the zvcs binary recognizes `flag` for `cmd`: invoke
/// `<bin> <cmd> <flag>` in a throwaway repo and read the refusal. A flag the
/// parser rejects prints a distinctive "unsupported option / unsupported flag /
/// unknown option" (or the whole command is "not yet ported"); anything else —
/// a usage error, a needs-a-value error, or the command actually running — means
/// the flag was accepted. Bounded by a short timeout so a flag that starts real
/// work can't hang the probe (that still counts as recognized).
fn probe_supported(bin: &Path, home: &Path, cmd: &str, flag: &str, dir: &Path) -> bool {
    let mut c = Command::new(bin);
    env::harden(&mut c, home);
    c.current_dir(dir)
        .arg(cmd)
        .arg(flag)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = match c.spawn() {
        Ok(c) => c,
        Err(_) => return false,
    };
    let start = Instant::now();
    let timed_out = loop {
        if matches!(child.try_wait(), Ok(Some(_))) {
            break false;
        }
        if start.elapsed() >= Duration::from_millis(1500) {
            let _ = child.kill();
            let _ = child.wait();
            break true;
        }
        std::thread::sleep(Duration::from_millis(5));
    };
    let mut stderr = Vec::new();
    if let Some(mut h) = child.stderr.take() {
        let _ = h.read_to_end(&mut stderr);
    }
    // A flag that ran long enough to be killed was parsed and accepted.
    if timed_out {
        return true;
    }
    let err = String::from_utf8_lossy(&stderr);
    let rejected = err.contains("not yet ported")
        || err.contains("unsupported option")
        || err.contains("unsupported flag")
        || err.contains("unknown option")
        || err.contains("unknown switch");
    !rejected
}

/// Build the per-command option support matrix: for every dispatched command,
/// every option stock git advertises, probed against the zvcs binary.
///
/// Probes run across a worker pool, each worker owning its own instantiated repo
/// (`probe_root/w<k>`) so concurrent probes never race on one worktree. A
/// single-flag probe never deletes `.git`, so accumulated mutations across a
/// worker's commands don't corrupt classification.
pub fn option_matrix(
    bin: &Path,
    home: &Path,
    cmds: &[String],
    templates: &crate::fixture::Templates,
    probe_root: &Path,
) -> BTreeMap<String, CmdOpts> {
    let n_workers = std::thread::available_parallelism()
        .map(|n| n.get().saturating_sub(1).max(1))
        .unwrap_or(4)
        .min(cmds.len().max(1))
        .min(16);
    let next = AtomicUsize::new(0);
    let out: Mutex<BTreeMap<String, CmdOpts>> = Mutex::new(BTreeMap::new());

    std::thread::scope(|scope| {
        for w in 0..n_workers {
            let (next, out, cmds, templates) = (&next, &out, cmds, templates);
            let dir = probe_root.join(format!("w{w}"));
            scope.spawn(move || {
                let _ = std::fs::create_dir_all(&dir);
                let _ = templates.instantiate(crate::fixture::Shape::Linear, &dir);
                loop {
                    let i = next.fetch_add(1, Ordering::Relaxed);
                    if i >= cmds.len() {
                        break;
                    }
                    let cmd = &cmds[i];
                    let rows: Vec<OptRow> = git_options(cmd)
                        .into_iter()
                        .map(|(flag, short, takes_arg)| {
                            let supported = probe_supported(bin, home, cmd, &flag, &dir);
                            OptRow { flag, short, takes_arg, supported }
                        })
                        .collect();
                    out.lock().unwrap().insert(cmd.clone(), CmdOpts { rows });
                }
            });
        }
    });

    out.into_inner().unwrap()
}

/// The full list of git configuration variables, straight from
/// `git help --config` (camelCase `section.key` / `section.<name>.key`).
/// Derived at runtime so it tracks the installed git, never a hand list.
pub fn git_config_keys() -> Vec<String> {
    let Ok(mut stock) = crate::stock::command() else {
        return Vec::new();
    };
    let out = match stock.args(["help", "--config"]).output() {
        Ok(o) => o,
        Err(_) => return Vec::new(),
    };
    let text = String::from_utf8_lossy(&out.stdout);
    let mut keys: Vec<String> = text
        .lines()
        .map(str::trim)
        .filter(|l| {
            l.chars().next().is_some_and(|c| c.is_ascii_alphanumeric())
                && l.contains('.')
                && !l.chars().any(char::is_whitespace)
        })
        .map(str::to_string)
        .collect();
    keys.sort();
    keys.dedup();
    keys
}

/// True iff `key` (already lowercased) occurs in `hay` bounded by non-identifier
/// bytes on both sides — so `core.editor` does not match inside
/// `core.editorconfig`, which would overcount support.
fn referenced_flat(hay: &str, key: &str) -> bool {
    let bytes = hay.as_bytes();
    let is_ident = |b: u8| b.is_ascii_alphanumeric() || b == b'_';
    hay.match_indices(key).any(|(i, _)| {
        let before_ok = i == 0 || !is_ident(bytes[i - 1]);
        let end = i + key.len();
        let after_ok = end >= bytes.len() || !is_ident(bytes[end]);
        before_ok && after_ok
    })
}

/// For every git config variable, whether it is referenced anywhere in the
/// source the `git` binary is built from — the extensions crate plus the
/// vendored gitoxide. A referenced key is read/honored somewhere; this is source
/// evidence, not a behavioral guarantee, and it undercounts keys gitoxide reaches
/// through split section/subsection/name access rather than a dotted literal.
///
/// The whole tree is slurped once into a lowercased haystack; flat keys use a
/// boundary-checked substring test, `section.<name>.key` keys a wildcard regex.
pub fn config_support(keys: &[String], src_roots: &[std::path::PathBuf]) -> BTreeMap<String, bool> {
    let mut hay = String::new();
    let mut stack: Vec<std::path::PathBuf> = src_roots.to_vec();
    while let Some(p) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&p) else { continue };
        for e in rd.flatten() {
            let path = e.path();
            if path.is_dir() {
                stack.push(path);
            } else if path.extension().is_some_and(|x| x == "rs") {
                // Skip the files that *enumerate* config keys for documentation or
                // a dump — `git help --config` (help.rs) and `git bugreport`
                // (bugreport.rs) both embed the full key list, which would mark
                // every variable "referenced" and defeat the whole measurement.
                // We want keys the code READS to change behavior, not lists it prints.
                let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
                if name == "help.rs" || name == "bugreport.rs" {
                    continue;
                }
                if let Ok(s) = std::fs::read_to_string(&path) {
                    hay.push_str(&s.to_lowercase());
                    hay.push('\n');
                }
            }
        }
    }

    let ph = regex::Regex::new(r"<[^>]*>").unwrap();
    let mut out = BTreeMap::new();
    for key in keys {
        let lower = key.to_lowercase();
        let supported = if lower.contains('<') {
            // Replace each `<name>` placeholder with a non-dot identifier run.
            let mut pat = String::new();
            let mut last = 0;
            for m in ph.find_iter(&lower) {
                pat.push_str(&regex::escape(&lower[last..m.start()]));
                pat.push_str(r#"[^.\s"'/]+"#);
                last = m.end();
            }
            pat.push_str(&regex::escape(&lower[last..]));
            regex::Regex::new(&pat)
                .map(|re| re.is_match(&hay))
                .unwrap_or(false)
        } else {
            referenced_flat(&hay, &lower)
        };
        out.insert(key.clone(), supported);
    }
    out
}

/// Page-specific supplemental styles layered on the shared HUD chrome
/// (`hud-static.css` provides the color-scheme variables, the `.app` shell, the
/// toolbar buttons, and the scheme-strip; these classes are the report's own
/// stat cards, parity table, and chip lists). Everything keys off the HUD CSS
/// variables so all eight color schemes recolor the page.
const REPORT_CSS: &str = r#"
    .tutorial-main { max-width: 76rem; }
    .tutorial-subtitle b { color:var(--cyan); }
    /* Center the color-scheme strip like index.html / report.html (these live in
       the page style on every fleet page, not in hud-static.css). */
    .hub-scheme-strip { border-bottom:1px dashed var(--border);background:color-mix(in srgb, var(--bg-secondary) 85%, transparent);padding:0.55rem 1.5rem 0.65rem;position:relative; }
    .hub-scheme-strip-inner { max-width:76rem;margin:0 auto;display:flex;align-items:center;gap:0.85rem; }
    .hub-scheme-strip .hud-scheme-label { flex:0 0 auto;text-align:left; }
    .hub-scheme-strip .scheme-grid { flex:1 1 auto;display:grid;grid-template-columns:repeat(5,minmax(0,1fr));gap:6px; }
    @media (max-width:720px){ .hub-scheme-strip-inner{flex-direction:column;align-items:stretch}.hub-scheme-strip .scheme-grid{grid-template-columns:repeat(2,minmax(0,1fr))} }
    .section-h { font-family:'Orbitron',sans-serif;font-size:12px;font-weight:700;letter-spacing:1.5px;text-transform:uppercase;color:var(--accent);margin:2rem 0 0.7rem;border-bottom:1px dashed var(--border);padding-bottom:0.4rem; }
    .muted { color:var(--text-dim);font-size:12px;line-height:1.6;max-width:60rem; }
    .section-rule { border:none;border-top:1px dashed var(--border);margin:2.2rem 0 1.2rem; }
    .stat-grid { display:grid;grid-template-columns:repeat(auto-fill,minmax(11rem,1fr));gap:0.75rem;margin:1.2rem 0; }
    .stat-card { border:1px solid var(--border);border-top:3px solid var(--cyan);background:var(--bg-card);padding:1rem 1.2rem;border-radius:2px;text-align:center; }
    .stat-card .stat-val { font-family:'Orbitron',sans-serif;font-size:26px;font-weight:900;color:var(--cyan);line-height:1.1;text-shadow:0 0 20px var(--cyan-glow); }
    .stat-card .stat-val.accent { color:var(--accent);text-shadow:0 0 20px var(--accent-glow); }
    .stat-card .stat-val.ok { color:var(--green);text-shadow:0 0 18px var(--green-bg); }
    .stat-card .stat-val.bad { color:var(--red);text-shadow:none; }
    .stat-card .stat-label { font-family:'Orbitron',sans-serif;font-size:9px;font-weight:700;letter-spacing:1.5px;text-transform:uppercase;color:var(--text-muted);margin-top:0.5rem; }
    .controls { display:flex;gap:10px;align-items:center;margin:0.5rem 0 0.8rem;flex-wrap:wrap; }
    #q, #q2 { flex:0 1 26rem;min-width:12rem;max-width:100%;padding:8px 12px;background:var(--bg-card);border:1px solid var(--border);border-radius:3px;color:var(--text);font-family:'Share Tech Mono',ui-monospace,monospace;font-size:12px; }
    #q::placeholder, #q2::placeholder { color:var(--text-muted);opacity:0.8; }
    #q:focus, #q2:focus { outline:none;border-color:var(--cyan);box-shadow:0 0 0 2px var(--cyan-glow); }
    #cnt, #cnt2 { font-size:11px;color:var(--text-muted);white-space:nowrap;font-family:'Share Tech Mono',monospace; }
    .file-table { width:100%;border-collapse:collapse;margin:0.4rem 0;font-size:12px; }
    .file-table th { background:var(--bg-secondary);color:var(--cyan);font-family:'Orbitron',sans-serif;font-size:10px;font-weight:700;letter-spacing:1.2px;text-transform:uppercase;text-align:left;padding:7px 10px;border:1px solid var(--border); }
    .file-table td { padding:6px 10px;border:1px solid var(--border);color:var(--text-dim);vertical-align:middle;text-align:right; }
    .file-table td.cmd { text-align:left;font-family:'Share Tech Mono',monospace;color:var(--accent-light);font-weight:700;white-space:nowrap; }
    .file-table td.ok { color:var(--green);font-weight:700; }
    .file-table td.bad { color:var(--red);font-weight:800; }
    .file-table tr:hover td { background:var(--bg-hover); }
    .chips { line-height:2.1;margin:0.4rem 0 0.6rem; }
    .chip { display:inline-block;border:1px solid var(--border);background:var(--bg-card);color:var(--text-dim);border-radius:3px;padding:1px 7px;margin:0 0.25rem 0.35rem 0;font-family:'Share Tech Mono',monospace;font-size:11px; }
    .none { color:var(--green);font-weight:700; }
    .yes { color:var(--green);font-weight:700; } .no { color:var(--red);font-weight:700; }
    #optmatrix { margin-top:0.4rem; }
    .optcmd { border:1px solid var(--border);border-radius:2px;background:var(--bg-card);margin:0 0 0.35rem;overflow:hidden; }
    .optcmd > summary { cursor:pointer;list-style:none;display:flex;align-items:center;gap:0.6rem;padding:0.5rem 0.8rem;font-family:'Share Tech Mono',monospace; }
    .optcmd > summary::-webkit-details-marker { display:none; }
    .optcmd > summary::before { content:'▸';color:var(--accent);font-size:11px; }
    .optcmd[open] > summary::before { content:'▾'; }
    .optcmd[open] > summary { border-bottom:1px solid var(--border);background:var(--bg-secondary); }
    .oc-cmd { color:var(--accent-light);font-weight:700;flex:0 0 auto;min-width:11rem; }
    .oc-tally { font-family:'Orbitron',sans-serif;font-size:11px;font-weight:700;letter-spacing:1px; }
    .oc-tally.ok { color:var(--green); } .oc-tally.part { color:var(--accent); }
    .optcmd .file-table { margin:0; } .optcmd .file-table th { position:static; }
    .optcmd .file-table td:last-child { text-align:center;font-weight:800;font-size:14px; }
"#;

/// Client-side filter for the per-command table. No framework, no external
/// dependency — a search box that hides non-matching rows.
const REPORT_JS: &str = r#"
(function(){
 var q=document.getElementById('q'),cnt=document.getElementById('cnt'),tb=document.querySelector('#tbl tbody');
 if(q&&tb){
  var rows=[].slice.call(tb.querySelectorAll('tr'));
  function upd(){var t=(q.value||'').toLowerCase().trim(),n=0;rows.forEach(function(r){var h=r.getAttribute('data-h')||'';var show=!t||h.indexOf(t)>=0;r.style.display=show?'':'none';if(show)n++;});cnt.textContent=n+' / '+rows.length;}
  q.addEventListener('input',upd);upd();
 }
 function bindDetails(qid,cntid,wrapid){
  var q=document.getElementById(qid),cnt=document.getElementById(cntid),mx=document.getElementById(wrapid);
  if(!q||!mx)return;
  var cards=[].slice.call(mx.querySelectorAll('.optcmd'));
  function upd(){var t=(q.value||'').toLowerCase().trim(),n=0;cards.forEach(function(c){var h=c.getAttribute('data-h')||'';var show=!t||h.indexOf(t)>=0;c.style.display=show?'':'none';if(show)n++;if(t&&show)c.open=true;if(!t)c.open=false;});cnt.textContent=n+' / '+cards.length;}
  q.addEventListener('input',upd);upd();
 }
 bindDetails('q2','cnt2','optmatrix');
 bindDetails('q3','cnt3','cfgmatrix');
})();
"#;

#[cfg(test)]
mod tests {
    use super::Tally;
    use crate::runner::Verdict;

    /// Where each verdict lands in the two numbers that matter: the numerator
    /// (`matched`) and the denominator (`scored`).
    ///
    /// The zvcs flake is the one this pins down. It has to be *inside* the
    /// denominator — an exclusion the binary under test can trigger itself would
    /// pay a randomly-wrong port in removed cases — and it must never be inside
    /// the numerator. The two stock-side buckets stay outside both, and outside
    /// is only defensible because nothing zvcs does can reach them.
    #[test]
    fn a_flake_is_counted_as_a_failure_and_never_as_a_match() {
        let mut t = Tally::default();
        for v in [
            Verdict::Match,
            Verdict::StdoutDiff,
            Verdict::ZvcsNondeterministic,
            Verdict::Nondeterministic,
            Verdict::StockTimeout,
        ] {
            t.record(v);
        }
        assert_eq!(t.matched, 1);
        assert_eq!(t.zvcs_nondeterministic, 1);
        // match + stdout-diff + flake; the two stock-side buckets are excluded.
        assert_eq!(t.scored(), 3);
        // …but every case run is still visible in the total, so an exclusion can
        // never make a case disappear from the report.
        assert_eq!(t.total(), 5);
        assert!((t.pct() - 100.0 / 3.0).abs() < 1e-9);
    }

    /// A run that is all flake reports 0% parity, not 100% of an empty
    /// denominator — the shape of hole an exclusion would open.
    #[test]
    fn an_all_flake_command_scores_zero_not_a_vacuous_hundred() {
        let mut t = Tally::default();
        t.record(Verdict::ZvcsNondeterministic);
        t.record(Verdict::ZvcsNondeterministic);
        assert_eq!(t.scored(), 2);
        assert_eq!(t.pct(), 0.0);
    }

    /// The denominator and the "nothing could be measured" predicate must agree,
    /// verdict for verdict. They are written in two different files — `scored()`
    /// sums fields here, `is_unmeasurable` names verdicts there — and a new
    /// verdict added to one and forgotten in the other silently moves the parity
    /// number without anybody choosing to.
    ///
    /// The `match` is what keeps the list below honest: it is exhaustive, so a
    /// new variant fails to compile until it is listed here too.
    #[test]
    fn the_denominator_and_the_unmeasurable_predicate_agree() {
        let every = [
            Verdict::Match,
            Verdict::Unsupported,
            Verdict::StdoutDiff,
            Verdict::ExitDiff,
            Verdict::StateDiff,
            Verdict::InteropDiff,
            Verdict::StderrDiff,
            Verdict::Crash,
            Verdict::Hang,
            Verdict::ZvcsNondeterministic,
            Verdict::VersionSkew,
            Verdict::Nondeterministic,
            Verdict::StockTimeout,
        ];
        for v in every {
            match v {
                Verdict::Match
                | Verdict::Unsupported
                | Verdict::StdoutDiff
                | Verdict::ExitDiff
                | Verdict::StateDiff
                | Verdict::InteropDiff
                | Verdict::StderrDiff
                | Verdict::Crash
                | Verdict::Hang
                | Verdict::ZvcsNondeterministic
                | Verdict::VersionSkew
                | Verdict::Nondeterministic
                | Verdict::StockTimeout => {}
            }
            let mut t = Tally::default();
            t.record(v);
            assert_eq!(t.scored(), usize::from(!v.is_unmeasurable()), "{}", v.label());
            // Whatever the bucket, the case is still visible in the total.
            assert_eq!(t.total(), 1, "{}", v.label());
            // And only an actual match may reach the numerator.
            assert_eq!(t.matched, usize::from(v.is_match()), "{}", v.label());
        }
    }

    /// Where a version difference lands, and it is the same place a zvcs flake
    /// lands: inside the denominator, outside the numerator.
    ///
    /// This is the number somebody will want to argue about, so it is pinned.
    /// Excluding it would be an exclusion **the binary under test can trigger** —
    /// the condition is "the two gits disagree *and* the port reproduces the
    /// older one", and the second half is the port's own output, so a port that
    /// reproduced 2.50 on its hardest cases would shrink its own denominator and
    /// outscore one that aimed at 2.55 and missed. Counting it as a match would
    /// redefine the number as "agrees with some git somewhere", which degrades
    /// with every git installed on the machine. So it is a failure, and the
    /// forgiving number is printed on a line of its own instead.
    #[test]
    fn a_version_difference_is_counted_as_a_failure_and_never_as_a_match() {
        let mut t = Tally::default();
        t.record(Verdict::Match);
        t.record(Verdict::VersionSkew);
        assert_eq!(t.matched, 1);
        assert_eq!(t.version_skew, 1);
        // In the denominator, so the case cannot be removed from measurement…
        assert_eq!(t.scored(), 2);
        assert_eq!(t.total(), 2);
        // …and out of the numerator, so it cannot be paid for either.
        assert_eq!(t.pct(), 50.0);
    }

    /// A second oracle can never change the parity number of a run it did not
    /// reclassify — and when it does reclassify, the case moves between failure
    /// buckets and nothing else.
    ///
    /// The runner-side proof is `adjudicate`'s
    /// `the_second_oracle_cannot_move_the_parity_number`; this is the scoring
    /// half of the same property, which has to hold in `Tally` for the claim to
    /// mean anything: `stdout-diff` and `version-skew` have to be worth exactly
    /// the same to both numbers.
    #[test]
    fn reclassifying_a_failure_as_a_version_difference_moves_no_number() {
        let mut before = Tally::default();
        let mut after = Tally::default();
        for v in [Verdict::Match, Verdict::Match, Verdict::StdoutDiff] {
            before.record(v);
        }
        for v in [Verdict::Match, Verdict::Match, Verdict::VersionSkew] {
            after.record(v);
        }
        assert_eq!(before.matched, after.matched);
        assert_eq!(before.scored(), after.scored());
        assert_eq!(before.total(), after.total());
        assert_eq!(before.pct(), after.pct());
    }
}

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
use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::io::Read as _;
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

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
    pub nondeterministic: usize,
    pub hang: usize,
}

impl Tally {
    /// Every case run, including ones nothing could score.
    pub fn total(&self) -> usize {
        self.scored() + self.nondeterministic
    }

    /// Cases a byte comparison can actually judge — the parity denominator.
    ///
    /// Non-deterministic cases are excluded rather than counted as failures:
    /// stock git does not reproduce them itself, so no implementation could
    /// match, and scoring them against zvcs would understate parity as surely
    /// as passing them would overstate it. The count is always printed beside
    /// the percentage so the exclusion is visible, never inferred.
    pub fn scored(&self) -> usize {
        self.matched
            + self.unsupported
            + self.stdout_diff
            + self.exit_diff
            + self.state_diff
            + self.crash
            + self.hang
    }

    fn record(&mut self, v: Verdict) {
        match v {
            Verdict::Match => self.matched += 1,
            Verdict::Unsupported => self.unsupported += 1,
            Verdict::StdoutDiff => self.stdout_diff += 1,
            Verdict::ExitDiff => self.exit_diff += 1,
            Verdict::StateDiff => self.state_diff += 1,
            Verdict::Crash => self.crash += 1,
            Verdict::Hang => self.hang += 1,
            Verdict::Nondeterministic => self.nondeterministic += 1,
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
            "           unsupported={} stdout-diff={} exit-diff={} state-diff={} crash={} hang={}",
            self.overall.unsupported,
            self.overall.stdout_diff,
            self.overall.exit_diff,
            self.overall.state_diff,
            self.overall.crash,
            self.overall.hang
        );
        if self.overall.nondeterministic > 0 {
            println!(
                "           excluded={} (stock git does not reproduce these itself)",
                self.overall.nondeterministic
            );
        }

        println!("\n--- per subcommand ---");
        println!("{:<14} {:>6} {:>6} {:>7} {:>6} {:>6} {:>6} {:>7}", "cmd", "total", "match", "unsupp", "out", "exit", "state", "parity");
        for (cmd, t) in &self.by_cmd {
            println!(
                "{:<14} {:>6} {:>6} {:>7} {:>6} {:>6} {:>6} {:>6.1}%",
                cmd, t.scored(), t.matched, t.unsupported, t.stdout_diff, t.exit_diff, t.state_diff, t.pct()
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

/// Minimal HTML escape for the few dynamic strings the report interpolates
/// (command names, git version). Command names are `[a-z0-9-]`, so this only
/// ever matters for defense in depth.
fn esc(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}

/// Best-effort short git version (`2.55.0`) of the reference binary the corpus
/// was compared against.
fn git_version() -> String {
    Command::new("git")
        .arg("--version")
        .output()
        .ok()
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

    let mut cmd_rows = String::new();
    for (cmd, t) in &rows {
        let cls = if t.matched == t.scored() { "ok" } else { "bad" };
        let _ = write!(
            cmd_rows,
            "<tr data-h=\"{h}\"><td class=\"cmd\">{c}</td><td>{tot}</td><td>{m}</td>\
             <td>{o}</td><td>{e}</td><td>{s}</td><td class=\"{cls}\">{p}</td></tr>",
            h = esc(cmd),
            c = esc(cmd),
            tot = t.scored(),
            m = t.matched,
            o = t.stdout_diff,
            e = t.exit_diff,
            s = t.state_diff,
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
         exit code, and post-command repository state byte-for-byte against stock git, \
         counting a case only when all three match. It does <b>not</b> assert per-command \
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
    html.push_str("<table class=\"file-table\" id=\"tbl\"><thead><tr><th>command</th><th>cases</th><th>match</th><th>out&ne;</th><th>exit&ne;</th><th>state&ne;</th><th>parity</th></tr></thead><tbody>\n");
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
    let out = match Command::new("git").arg(cmd).arg("-h").output() {
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
        if start.elapsed() >= Duration::from_secs(5) {
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
    let out = match Command::new("git").args(["help", "--config"]).output() {
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

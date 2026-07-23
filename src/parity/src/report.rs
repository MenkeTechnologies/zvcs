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

/// Page-specific supplemental styles layered on the shared HUD chrome
/// (`hud-static.css` provides the color-scheme variables, the `.app` shell, the
/// toolbar buttons, and the scheme-strip; these classes are the report's own
/// stat cards, parity table, and chip lists). Everything keys off the HUD CSS
/// variables so all eight color schemes recolor the page.
const REPORT_CSS: &str = r#"
    .tutorial-main { max-width: 76rem; }
    .tutorial-subtitle b { color:var(--cyan); }
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
    .controls { display:flex;gap:10px;align-items:center;margin:0.5rem 0 0.8rem; }
    #q { flex:1;padding:8px 12px;background:var(--bg-card);border:1px solid var(--border);border-radius:3px;color:var(--text);font-family:'Share Tech Mono',ui-monospace,monospace;font-size:12px; }
    #q:focus { outline:none;border-color:var(--cyan); }
    #cnt { font-size:11px;color:var(--text-muted);white-space:nowrap; }
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
"#;

/// Client-side filter for the per-command table. No framework, no external
/// dependency — a search box that hides non-matching rows.
const REPORT_JS: &str = r#"
(function(){
 var q=document.getElementById('q'),cnt=document.getElementById('cnt'),tb=document.querySelector('#tbl tbody');
 if(!q||!tb)return;
 var rows=[].slice.call(tb.querySelectorAll('tr'));
 function upd(){var t=(q.value||'').toLowerCase().trim(),n=0;rows.forEach(function(r){var h=r.getAttribute('data-h')||'';var show=!t||h.indexOf(t)>=0;r.style.display=show?'':'none';if(show)n++;});cnt.textContent=n+' / '+rows.length;}
 q.addEventListener('input',upd);upd();
})();
"#;

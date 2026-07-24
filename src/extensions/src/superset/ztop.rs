//! `git ztop` — a live, htop-style monitor of the whole indexed fleet.
//!
//! It reads the daemon-maintained status cache (`repo_status`) once per frame —
//! instant, no live scan — so it stays usable across thousands of repos, exactly
//! the property that makes `git zdashboard` safe. Churn is tracked in-process:
//! when a repo's HEAD or dirty flag changes between frames (the daemon rewrote
//! its row) its churn score bumps and decays, like an htop CPU%% column, so the
//! fleet's activity sorts itself to the top.
//!
//! Ported from htoprs, like an htop should be: a **toast** (`StatusMsg`, 3s
//! auto-dismiss, `src/extensions/overlay.rs`), the **colorscheme** set
//! (`src/ported/crt.rs` `ColorScheme` — Default/Monochrome/Black-on-White/Light/
//! Midnight/Black-Night/Nord) with a **live picker** (htop F2 Setup), a **help**
//! overlay (F1), and **sort by any column** (htop F6) with an invert toggle and
//! the active column highlighted in the header. Rendering uses htoprs's own
//! `Buffer`/`Style` cell primitives (`set_cell`/`set_str`/`draw_box`) written
//! into a ratatui `Terminal`'s frame.

use std::collections::{HashMap, HashSet};
use std::io::{stdout, IsTerminal};
use std::process::{Command, ExitCode};
use std::time::{Duration, Instant};

use anyhow::Result;
use ratatui::backend::CrosstermBackend;
use ratatui::buffer::Buffer;
use ratatui::crossterm::{
    cursor,
    event::{self, Event, KeyCode, KeyEventKind, KeyModifiers},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::style::{Color, Modifier, Style};
use ratatui::Terminal;

use crate::superset::select::Selector;

/// Churn = how recently the daemon last rewrote a repo's status row
/// (`repo_status.updated_at`, which advances on commit/fetch/checkout), plus a
/// live burst when the commit id changes while you watch. `RECENCY_WINDOW` is how
/// long an update keeps a repo "hot" (fully lit → fades over this span), so a
/// just-committed repo rises to the top and stays there for a while — not the
/// 20-second fade the old per-frame counter gave. `CAP` maps a score onto the
/// churn bar's full width; `BUMP`/`DECAY` govern the live burst.
const RECENCY_WINDOW: f64 = 3600.0; // 1 hour: hot → cold
const CHURN_BUMP: f64 = 3.0;
const CHURN_DECAY: f64 = 0.85;
const CHURN_CAP: f64 = 6.0;
const CHURN_BAR_W: u16 = 8;

/// `git ztop [selectors] [--interval <secs>] [--once] [--mono]`.
pub fn ztop(args: &[String]) -> Result<ExitCode> {
    let (sel, rest) = Selector::parse(args);
    let mut interval = 2.0f64;
    let mut once = false;
    let mut force_mono = std::env::var_os("NO_COLOR").is_some();
    let mut i = 0;
    while i < rest.len() {
        match rest[i].as_str() {
            "--once" | "-1" => once = true,
            "--mono" | "--no-color" => force_mono = true,
            "--interval" | "-d" => {
                i += 1;
                if let Some(v) = rest.get(i).and_then(|s| s.parse::<f64>().ok()) {
                    interval = v.clamp(0.2, 60.0);
                }
            }
            _ => {}
        }
        i += 1;
    }

    let allowed = selector_set(&sel)?;
    // Start on the saved scheme (htop persists the Setup choice), or Monochrome
    // when color is off.
    let start = if force_mono { Scheme::Monochrome } else { load_scheme() };
    let mut app = App::new(start, Duration::from_secs_f64(interval));

    if once || !stdout().is_terminal() {
        app.sort = SortSpec { col: Col::State, desc: false };
        app.refresh(&allowed);
        app.print_once();
        return Ok(ExitCode::SUCCESS);
    }
    run_tui(&mut app, &allowed)
}

/// Resolve the selector to the set of workdir paths to show, or `None` for the
/// whole fleet. Matches `StatusRow.path` (`COALESCE(workdir, git_dir)`).
fn selector_set(sel: &Selector) -> Result<Option<HashSet<String>>> {
    let active = !sel.patterns.is_empty()
        || sel.dirty
        || sel.ahead
        || sel.behind
        || sel.claimed
        || sel.session.is_some();
    if !active {
        return Ok(None);
    }
    let set = sel
        .select()?
        .into_iter()
        .map(|(_, wd)| wd.display().to_string())
        .collect();
    Ok(Some(set))
}

// ---------------------------------------------------------------------------
// Columns + sorting (htop F6 "sort by").
// ---------------------------------------------------------------------------

/// The sortable table columns, in display order.
#[derive(Clone, Copy, PartialEq)]
enum Col {
    Churn,
    Repo,
    Head,
    State,
    Age,
}

const COLS: [Col; 5] = [Col::Churn, Col::Repo, Col::Head, Col::State, Col::Age];

impl Col {
    fn title(self) -> &'static str {
        match self {
            Col::Churn => "CHURN",
            Col::Repo => "REPOSITORY",
            Col::Head => "HEAD",
            Col::State => "STATE",
            Col::Age => "AGE",
        }
    }
    fn index(self) -> usize {
        COLS.iter().position(|c| *c == self).unwrap_or(0)
    }
}

/// The active sort: a column and a direction. `desc` puts the "most" at the top,
/// which for churn/age is the useful default.
#[derive(Clone, Copy)]
struct SortSpec {
    col: Col,
    desc: bool,
}

// ---------------------------------------------------------------------------
// Toast — ported from htoprs `StatusMsg` (src/extensions/overlay.rs).
// ---------------------------------------------------------------------------

struct StatusMsg {
    text: String,
    since: Instant,
}

impl StatusMsg {
    fn expired(&self) -> bool {
        self.since.elapsed().as_secs() >= 3
    }
}

// ---------------------------------------------------------------------------
// Overlays (htop F1 Help, F2 Setup/colors, F6 SortBy).
// ---------------------------------------------------------------------------

enum Overlay {
    None,
    Help,
    SortPick(usize),   // cursor over COLS
    SchemePick(usize), // cursor over SCHEMES; applied live as it moves
}

/// Per-repo live-burst accumulator: a decaying score bumped when the commit id
/// (`head_sha`) or dirty flag changes while ztop is watching. The steady-state
/// churn comes from `updated_at` recency; this just makes an in-view commit jump.
struct Churn {
    burst: f64,
    prev_sha: String,
    prev_dirty: bool,
}

/// One repo's raw status snapshot from the cache, including the fields
/// `list_status` does not expose (`head_sha`, `updated_at`). Read by ztop's own
/// query so it never has to touch the shared `db.rs` API.
struct Snap {
    path: String,
    dirty: bool,
    detached: bool,
    sync: String,
    head: String,
    head_sha: String,
    updated_at: i64,
}

/// Read every cached status row with `head_sha` + `updated_at`, resolving an
/// absolute path and dropping malformed (non-absolute) index entries. Falls back
/// to a `head_sha`-less query for an older schema.
fn read_snaps() -> Vec<Snap> {
    let Ok(conn) = crate::db::open_ro() else { return Vec::new() };
    let full = "SELECT r.workdir, r.git_dir, s.dirty, s.detached, s.sync, s.head, s.head_sha, s.updated_at \
                FROM repo_status s JOIN repos r ON r.id = s.repo_id";
    let fallback = "SELECT r.workdir, r.git_dir, s.dirty, s.detached, s.sync, s.head, '', s.updated_at \
                    FROM repo_status s JOIN repos r ON r.id = s.repo_id";
    for sql in [full, fallback] {
        let Ok(mut stmt) = conn.prepare(sql) else { continue };
        let mapped = stmt.query_map([], |r| {
            let wd: Option<String> = r.get(0)?;
            let gd: String = r.get(1)?;
            Ok(Snap {
                path: resolve_path(wd.as_deref(), &gd),
                dirty: r.get::<_, i64>(2)? != 0,
                detached: r.get::<_, i64>(3)? != 0,
                sync: r.get(4)?,
                head: r.get(5)?,
                head_sha: r.get::<_, Option<String>>(6)?.unwrap_or_default(),
                updated_at: r.get(7)?,
            })
        });
        if let Ok(it) = mapped {
            if let Ok(v) = it.collect::<rusqlite::Result<Vec<_>>>() {
                return v;
            }
        }
    }
    Vec::new()
}

/// Prefer an absolute worktree path, else the git dir without a trailing `/.git`.
/// A non-absolute value (e.g. a `.` / `./.git` index entry) yields `""`, which the
/// caller skips — better than displaying a meaningless `.` row.
fn resolve_path(workdir: Option<&str>, git_dir: &str) -> String {
    if let Some(wd) = workdir {
        if wd.starts_with('/') {
            return wd.to_string();
        }
    }
    if git_dir.starts_with('/') {
        return git_dir.trim_end_matches("/.git").trim_end_matches('/').to_string();
    }
    String::new()
}

/// One rendered repo row for the current frame.
#[derive(Clone)]
struct Row {
    path: String,
    head: String,
    dirty: bool,
    detached: bool,
    sync: String,
    churn: f64,
    age_secs: i64, // seconds since the daemon last updated this repo's status
}

#[derive(Default)]
struct Totals {
    repos: usize,
    cached: usize,
    dirty: usize,
    ahead: usize,
    behind: usize,
    diverged: usize,
    detached: usize,
    clean: usize,
    claims: usize,
    sessions: usize,
    queue: usize,
}

struct App {
    scheme: Scheme,
    theme: Theme,
    churn: HashMap<String, Churn>,
    status: Option<StatusMsg>,
    sort: SortSpec,
    overlay: Overlay,
    interval: Duration,
    scroll: usize,
    all: Vec<Row>,   // every row from the last DB read (pre-filter)
    rows: Vec<Row>,  // the filtered + sorted view actually shown
    filter: String,  // live `/` search query (case-insensitive path substring)
    searching: bool, // true while typing the query
    totals: Totals,
    daemon_up: bool,
}

impl App {
    fn new(scheme: Scheme, interval: Duration) -> Self {
        App {
            scheme,
            theme: scheme.theme(),
            churn: HashMap::new(),
            status: None,
            sort: SortSpec { col: Col::Churn, desc: true },
            overlay: Overlay::None,
            interval,
            scroll: 0,
            all: Vec::new(),
            rows: Vec::new(),
            filter: String::new(),
            searching: false,
            totals: Totals::default(),
            daemon_up: false,
        }
    }

    fn set_status(&mut self, msg: impl Into<String>) {
        self.status = Some(StatusMsg { text: msg.into(), since: Instant::now() });
    }

    fn set_scheme(&mut self, scheme: Scheme) {
        self.scheme = scheme;
        self.theme = scheme.theme();
    }

    fn refresh(&mut self, allowed: &Option<HashSet<String>>) {
        let snaps = read_snaps();
        let mut t = Totals::default();
        if let Ok(conn) = crate::db::open_ro() {
            t.repos = crate::db::list_repos(&conn).map(|v| v.len()).unwrap_or(0);
            let claim_list = crate::db::list_claims(&conn).unwrap_or_default();
            t.claims = claim_list.len();
            t.sessions = claim_list.iter().map(|(_, s, _)| s.as_str()).collect::<HashSet<_>>().len();
            t.queue = crate::db::list_jobs(&conn, 1000)
                .map(|j| j.iter().filter(|x| x.state == "queued" || x.state == "running").count())
                .unwrap_or(0);
        }

        let now = crate::date::now_seconds();
        for c in self.churn.values_mut() {
            c.burst *= CHURN_DECAY;
        }

        let mut changed = 0usize;
        let mut rows = Vec::new();
        for s in &snaps {
            if s.path.is_empty() {
                continue; // malformed (non-absolute) index entry — skip it
            }
            if let Some(a) = allowed {
                if !a.contains(&s.path) {
                    continue;
                }
            }
            t.cached += 1;

            // Live burst: the commit id (or dirty flag) changed while watching.
            let entry = self.churn.entry(s.path.clone()).or_insert_with(|| Churn {
                burst: 0.0,
                prev_sha: s.head_sha.clone(),
                prev_dirty: s.dirty,
            });
            if entry.prev_sha != s.head_sha || entry.prev_dirty != s.dirty {
                entry.burst += CHURN_BUMP;
                entry.prev_sha = s.head_sha.clone();
                entry.prev_dirty = s.dirty;
                changed += 1;
            }

            if s.dirty {
                t.dirty += 1;
            }
            if s.detached {
                t.detached += 1;
            }
            match s.sync.as_str() {
                "ahead" => t.ahead += 1,
                "behind" => t.behind += 1,
                "diverged" => t.diverged += 1,
                _ => {}
            }
            if !s.dirty && !s.detached && (s.sync == "up-to-date" || s.sync == "no-upstream") {
                t.clean += 1;
            }

            // Steady-state churn = how recently the daemon last touched this repo.
            let age = (now - s.updated_at).max(0);
            let recency = (1.0 - age as f64 / RECENCY_WINDOW).clamp(0.0, 1.0);
            let churn = recency * CHURN_CAP + entry.burst;

            rows.push(Row {
                path: s.path.clone(),
                head: s.head.clone(),
                dirty: s.dirty,
                detached: s.detached,
                sync: s.sync.clone(),
                churn,
                age_secs: age,
            });
        }

        if changed > 0 {
            self.set_status(format!("{changed} repo(s) changed"));
        }
        if self.churn.len() > snaps.len().saturating_mul(2) + 64 {
            let live: HashSet<&String> = snaps.iter().map(|s| &s.path).collect();
            self.churn.retain(|k, _| live.contains(k));
        }
        self.all = rows;
        self.totals = t;
        self.daemon_up = crate::superset::zdaemon::is_running();
        self.reproject();
    }

    /// Rebuild the shown `rows` from `all`: apply the `/` filter, then sort. Cheap
    /// (no DB read), so search keystrokes and sort changes call this directly.
    fn reproject(&mut self) {
        let needle = self.filter.to_lowercase();
        let mut rows: Vec<Row> = if needle.is_empty() {
            self.all.clone()
        } else {
            self.all
                .iter()
                .filter(|r| r.path.to_lowercase().contains(&needle))
                .cloned()
                .collect()
        };
        self.sort_rows(&mut rows);
        self.rows = rows;
        let max = self.rows.len().saturating_sub(1);
        if self.scroll > max {
            self.scroll = max;
        }
    }

    /// Sort by the active column (ascending key), then flip for `desc`.
    fn sort_rows(&self, rows: &mut [Row]) {
        rows.sort_by(|a, b| {
            let key = match self.sort.col {
                Col::Churn => a.churn.partial_cmp(&b.churn).unwrap_or(std::cmp::Ordering::Equal),
                Col::Repo => a.path.cmp(&b.path),
                Col::Head => a.head.cmp(&b.head),
                Col::State => severity_rank(a).cmp(&severity_rank(b)),
                // "age" = seconds since last change; smaller = more recent (top).
                Col::Age => a.age_secs.cmp(&b.age_secs),
            };
            key.then_with(|| a.path.cmp(&b.path))
        });
        if self.sort.desc {
            rows.reverse();
        }
    }

    fn resort(&mut self) {
        self.reproject();
    }

    fn set_sort(&mut self, col: Col) {
        // Re-selecting the same column flips direction (like htop).
        if self.sort.col == col {
            self.sort.desc = !self.sort.desc;
        } else {
            self.sort.col = col;
            // Churn: highest on top. Age: smallest (most recent) on top, which is
            // its ascending order — so only churn defaults to descending.
            self.sort.desc = matches!(col, Col::Churn);
        }
        self.resort();
        self.set_status(format!(
            "sort: {} {}",
            col.title().to_lowercase(),
            if self.sort.desc { "↓" } else { "↑" }
        ));
    }

    fn print_once(&self) {
        let t = &self.totals;
        println!(
            "zvcs ztop — {} repos ({} cached), daemon {}",
            t.repos,
            t.cached,
            if self.daemon_up { "up" } else { "down" }
        );
        println!(
            "  dirty {}  ahead {}  behind {}  diverged {}  detached {}  clean {}  queue {}",
            t.dirty, t.ahead, t.behind, t.diverged, t.detached, t.clean, t.queue
        );
        let width = self.rows.iter().map(|r| homify(&r.path).len()).max().unwrap_or(0);
        for r in &self.rows {
            let (state, _) = severity_label(r);
            let head = if r.head.is_empty() { "-" } else { &r.head };
            println!("{:<width$}  {:<9}  {}", homify(&r.path), state, head);
        }
        if t.cached < t.repos {
            println!("  (status cached for {}/{} — start the daemon or `git zstatus --all`)", t.cached, t.repos);
        }
    }
}

/// State severity for the `state` sort and coloring: lower = more urgent.
fn severity_rank(r: &Row) -> u8 {
    if r.detached {
        0
    } else if r.sync == "diverged" {
        1
    } else if r.dirty {
        2
    } else if r.sync == "behind" {
        3
    } else if r.sync == "ahead" {
        4
    } else if r.sync == "no-upstream" {
        6
    } else {
        5
    }
}

fn severity_label(r: &Row) -> (&'static str, u8) {
    let rank = severity_rank(r);
    let word = match rank {
        0 => "detached",
        1 => "diverged",
        2 => "dirty",
        3 => "behind",
        4 => "ahead",
        6 => "no-remote",
        _ => "clean",
    };
    (word, rank)
}

// ---------------------------------------------------------------------------
// Colorschemes — ported from htoprs `ColorScheme` (src/ported/crt.rs). Each is a
// small `Palette`; `Theme` derives the concrete `Style`s. Persisted to
// `zvcs.topscheme`, like htop's Setup.
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Debug)]
enum Scheme {
    Default,
    Monochrome,
    BlackOnWhite,
    LightTerminal,
    Midnight,
    BlackNight,
    BrokenGray,
    Nord,
}

const SCHEMES: [Scheme; 8] = [
    Scheme::Default, Scheme::Monochrome, Scheme::BlackOnWhite, Scheme::LightTerminal,
    Scheme::Midnight, Scheme::BlackNight, Scheme::BrokenGray, Scheme::Nord,
];

// htop ncurses base color numbers (crt.rs:120), for the exact scheme tables below.
const K: i16 = 0; // Black
const RD: i16 = 1; // Red
const GR: i16 = 2; // Green
const YL: i16 = 3; // Yellow
const BL: i16 = 4; // Blue
const MG: i16 = 5; // Magenta
const CY: i16 = 6; // Cyan
const WH: i16 = 7; // White
const DF: i16 = -1; // terminal default

/// One `CRT_colorSchemes` element: (fg, bg, bold), htop color numbers.
type E = (i16, i16, bool);

/// The ten elements ztop borrows from htop's per-scheme color table.
struct Elems {
    header_bar: E,   // PANEL_HEADER_FOCUS — the table header row
    function_bar: E, // FUNCTION_BAR — the footer key bar
    selection: E,    // PANEL_SELECTION_FOCUS — toast / accent
    meter_text: E,   // METER_TEXT — header labels
    meter_value: E,  // METER_VALUE — header values
    ok: E,           // METER_VALUE_OK
    warn: E,         // METER_VALUE_WARN
    error: E,        // METER_VALUE_ERROR
    process: E,      // PROCESS — data rows (carries the scheme background)
    megabytes: E,    // PROCESS_MEGABYTES — the churn bar
}

impl Scheme {
    fn name(self) -> &'static str {
        match self {
            Scheme::Default => "Default",
            Scheme::Monochrome => "Monochrome",
            Scheme::BlackOnWhite => "Black on White",
            Scheme::LightTerminal => "Light Terminal",
            Scheme::Midnight => "Midnight",
            Scheme::BlackNight => "Black Night",
            Scheme::BrokenGray => "Broken Gray",
            Scheme::Nord => "Nord",
        }
    }
    fn index(self) -> usize {
        SCHEMES.iter().position(|s| *s == self).unwrap_or(0)
    }
    fn from_name(s: &str) -> Option<Scheme> {
        SCHEMES.iter().copied().find(|c| c.name().eq_ignore_ascii_case(s))
    }
    fn theme(self) -> Theme {
        if self == Scheme::Monochrome {
            return Theme::monochrome();
        }
        Theme::from_elems(self.elems(), matches!(self, Scheme::BlackNight))
    }

    /// The exact `CRT_colorSchemes` values (htoprs `src/ported/crt.rs`) for the
    /// elements ztop uses. Broken Gray equals Default for all of them.
    fn elems(self) -> Elems {
        let (f, t) = (false, true);
        match self {
            Scheme::Default | Scheme::BrokenGray | Scheme::Monochrome => Elems {
                header_bar: (K, GR, f), function_bar: (K, CY, f), selection: (K, CY, f),
                meter_text: (CY, K, f), meter_value: (CY, K, t),
                ok: (GR, K, f), warn: (YL, K, t), error: (RD, K, t),
                process: (DF, DF, f), megabytes: (CY, K, f),
            },
            Scheme::BlackOnWhite => Elems {
                header_bar: (K, GR, f), function_bar: (K, CY, f), selection: (K, CY, f),
                meter_text: (BL, WH, f), meter_value: (K, WH, f),
                ok: (GR, WH, f), warn: (YL, WH, t), error: (RD, WH, t),
                process: (K, WH, f), megabytes: (BL, WH, f),
            },
            Scheme::LightTerminal => Elems {
                header_bar: (K, GR, f), function_bar: (K, CY, f), selection: (K, CY, f),
                meter_text: (BL, K, f), meter_value: (K, K, f),
                ok: (GR, K, f), warn: (YL, K, t), error: (RD, K, t),
                process: (K, K, f), megabytes: (BL, K, f),
            },
            Scheme::Midnight => Elems {
                header_bar: (K, CY, f), function_bar: (K, CY, f), selection: (K, WH, f),
                meter_text: (CY, BL, f), meter_value: (CY, BL, t),
                ok: (GR, BL, f), warn: (YL, K, t), error: (RD, BL, t),
                process: (WH, BL, f), megabytes: (CY, BL, f),
            },
            Scheme::BlackNight => Elems {
                header_bar: (K, GR, f), function_bar: (K, GR, f), selection: (K, CY, f),
                meter_text: (CY, K, f), meter_value: (GR, K, f),
                ok: (GR, K, f), warn: (YL, K, t), error: (RD, K, t),
                process: (CY, K, f), megabytes: (GR, K, t),
            },
            Scheme::Nord => Elems {
                header_bar: (K, CY, f), function_bar: (K, CY, f), selection: (K, CY, f),
                meter_text: (DF, DF, f), meter_value: (DF, DF, t),
                ok: (DF, DF, f), warn: (DF, DF, t), error: (DF, DF, t),
                process: (DF, DF, f), megabytes: (WH, K, t),
            },
        }
    }
}

/// Map an ncurses color number to a ratatui color, exactly as htop's `to_color`
/// resolves 0-7 (8=gray, -1=terminal default).
fn cn(n: i16) -> Color {
    match n {
        0 => Color::Black,
        1 => Color::Red,
        2 => Color::Green,
        3 => Color::Yellow,
        4 => Color::Blue,
        5 => Color::Magenta,
        6 => Color::Cyan,
        7 => Color::Gray,
        8 => Color::DarkGray,
        _ => Color::Reset,
    }
}

/// Build a `Style` from an htop element, applying htop's rule that a `Black`
/// background collapses to the terminal default in every scheme but Black Night.
fn mk((fg, bg, bold): E, blacknight: bool) -> Style {
    let bg = if !blacknight && bg == K { DF } else { bg };
    let mut s = Style::default().fg(cn(fg)).bg(cn(bg));
    if bold {
        s = s.add_modifier(Modifier::BOLD);
    }
    s
}

/// Concrete styles for one scheme.
struct Theme {
    header_label: Style,
    header_value: Style,
    header_bar: Style,   // table header row (PANEL_HEADER_FOCUS)
    function_bar: Style, // footer key bar (FUNCTION_BAR)
    row: Style,
    dim: Style,
    ok: Style,
    warn: Style,
    error: Style,
    behind: Style,
    diverged: Style,
    churn: Style,
    sort_col: Style, // active sort column: the header bar inverted
    accent_bg: Color,
    fg_on_accent: Color,
}

impl Theme {
    fn from_elems(e: Elems, bn: bool) -> Self {
        let row = mk(e.process, bn);
        let row_bg = row.bg.unwrap_or(Color::Reset);
        // Derived states not in htop's table share the scheme's row background.
        let on_row = |fg: i16, bold: bool| {
            let mut s = Style::default().fg(cn(fg)).bg(row_bg);
            if bold {
                s = s.add_modifier(Modifier::BOLD);
            }
            s
        };
        let hdr = mk(e.header_bar, bn);
        let sel = mk(e.selection, bn);
        Theme {
            header_label: mk(e.meter_text, bn),
            header_value: mk(e.meter_value, bn),
            header_bar: hdr,
            function_bar: mk(e.function_bar, bn),
            row,
            dim: on_row(8, false),
            ok: mk(e.ok, bn),
            warn: mk(e.warn, bn),
            error: mk(e.error, bn),
            behind: on_row(RD, false),
            diverged: on_row(MG, true),
            churn: mk(e.megabytes, bn),
            sort_col: Style::default()
                .fg(hdr.bg.unwrap_or(Color::Reset))
                .bg(hdr.fg.unwrap_or(Color::Reset)),
            accent_bg: sel.bg.unwrap_or(Color::Cyan),
            fg_on_accent: sel.fg.unwrap_or(Color::Black),
        }
    }

    fn monochrome() -> Self {
        let plain = Style::default();
        let b = plain.add_modifier(Modifier::BOLD);
        let rev = plain.add_modifier(Modifier::REVERSED);
        Theme {
            header_label: plain,
            header_value: b,
            header_bar: rev,
            function_bar: rev,
            row: plain,
            dim: plain.add_modifier(Modifier::DIM),
            ok: plain,
            warn: b,
            error: b,
            behind: b,
            diverged: b,
            churn: b,
            sort_col: plain.add_modifier(Modifier::UNDERLINED),
            accent_bg: Color::Reset,
            fg_on_accent: Color::Reset,
        }
    }

    fn state_style(&self, rank: u8) -> Style {
        match rank {
            0 => self.error,
            1 => self.diverged,
            2 => self.warn,
            3 => self.behind,
            4 => self.ok,
            _ => self.dim,
        }
    }
}

/// Read the persisted scheme (`zvcs.topscheme`), defaulting to Default.
fn load_scheme() -> Scheme {
    crate::config::global_config()
        .string("zvcs.topscheme")
        .and_then(|s| Scheme::from_name(&s.to_string()))
        .unwrap_or(Scheme::Default)
}

/// Persist the scheme choice to the global config (htop saves the Setup choice).
fn save_scheme(scheme: Scheme) {
    if let Ok(exe) = std::env::current_exe() {
        let _ = Command::new(exe)
            .args(["config", "--global", "zvcs.topscheme", scheme.name()])
            .status();
    }
}

// ---------------------------------------------------------------------------
// Rendering primitives — ported from htoprs src/extensions/overlay.rs.
// ---------------------------------------------------------------------------

fn set_cell(buf: &mut Buffer, x: u16, y: u16, ch: &str, s: Style) {
    let a = buf.area();
    if x < a.x + a.width && y < a.y + a.height {
        let c = &mut buf[(x, y)];
        c.set_symbol(ch);
        c.set_style(s);
    }
}

fn set_str(buf: &mut Buffer, x: u16, y: u16, s: &str, st: Style, mw: u16) {
    let aw = buf.area().x + buf.area().width;
    let ah = buf.area().y + buf.area().height;
    if y >= ah {
        return;
    }
    let mut cb = [0u8; 4];
    for (i, ch) in s.chars().enumerate() {
        let cx = x + i as u16;
        if cx >= x + mw || cx >= aw {
            break;
        }
        let c = &mut buf[(cx, y)];
        c.set_symbol(ch.encode_utf8(&mut cb));
        c.set_style(st);
    }
}

fn fill_row(buf: &mut Buffer, y: u16, st: Style) {
    let a = buf.area();
    for x in a.x..a.x + a.width {
        set_cell(buf, x, y, " ", st);
    }
}

/// Draw a centered bordered box, filled, and return its inner top-left. Ported
/// from htoprs `overlay::draw_box`.
fn draw_box(buf: &mut Buffer, bw: u16, bh: u16, bg: Style, border: Style) -> (u16, u16) {
    let area = buf.area();
    let bw = bw.min(area.width);
    let bh = bh.min(area.height);
    let x0 = (area.width.saturating_sub(bw)) / 2;
    let y0 = (area.height.saturating_sub(bh)) / 2;
    for y in y0..y0 + bh {
        for x in x0..x0 + bw {
            set_cell(buf, x, y, " ", bg);
        }
    }
    let x1 = x0 + bw - 1;
    let y1 = y0 + bh - 1;
    set_cell(buf, x0, y0, "╔", border);
    set_cell(buf, x1, y0, "╗", border);
    set_cell(buf, x0, y1, "╚", border);
    set_cell(buf, x1, y1, "╝", border);
    for x in x0 + 1..x1 {
        set_cell(buf, x, y0, "═", border);
        set_cell(buf, x, y1, "═", border);
    }
    for y in y0 + 1..y1 {
        set_cell(buf, x0, y, "║", border);
        set_cell(buf, x1, y, "║", border);
    }
    (x0 + 2, y0 + 1)
}

fn homify(path: &str) -> String {
    if let Some(home) = std::env::var_os("HOME") {
        let home = home.to_string_lossy();
        if !home.is_empty() {
            if let Some(rest) = path.strip_prefix(home.as_ref()) {
                return format!("~{rest}");
            }
        }
    }
    path.to_string()
}

fn fmt_ago(d: Duration) -> String {
    let s = d.as_secs();
    if s < 60 {
        format!("{s}s")
    } else if s < 3600 {
        format!("{}m", s / 60)
    } else if s < 86_400 {
        format!("{}h", s / 3600)
    } else {
        format!("{}d", s / 86_400)
    }
}

/// Column x-offsets and widths for the current terminal width.
struct Layout {
    churn_x: u16,
    repo_x: u16,
    repo_w: u16,
    head_x: u16,
    head_w: u16,
    state_x: u16,
    state_w: u16,
    age_x: u16,
    age_w: u16,
}

impl Layout {
    fn compute(cols: u16) -> Self {
        let head_w = 10u16;
        let state_w = 10u16;
        let age_w = 6u16;
        let repo_x = CHURN_BAR_W + 1;
        let fixed_right = head_w + 1 + state_w + 1 + age_w;
        let repo_w = cols.saturating_sub(repo_x + 1 + fixed_right).max(8);
        let head_x = repo_x + repo_w + 1;
        let state_x = head_x + head_w + 1;
        let age_x = state_x + state_w + 1;
        Layout { churn_x: 0, repo_x, repo_w, head_x, head_w, state_x, state_w, age_x, age_w }
    }
    fn col(&self, c: Col) -> (u16, u16) {
        match c {
            Col::Churn => (self.churn_x, CHURN_BAR_W),
            Col::Repo => (self.repo_x, self.repo_w),
            Col::Head => (self.head_x, self.head_w),
            Col::State => (self.state_x, self.state_w),
            Col::Age => (self.age_x, self.age_w),
        }
    }
}

fn render(buf: &mut Buffer, app: &App) {
    let area = buf.area();
    let (cols, rows) = (area.width, area.height);
    // Paint the whole surface in the theme row style first (clean background).
    for y in area.y..area.y + rows {
        fill_row(buf, y, app.theme.row);
    }
    if cols < 30 || rows < 6 {
        set_str(buf, 0, 0, "terminal too small", app.theme.error, cols);
        return;
    }
    let t = &app.theme;

    // Header.
    let daemon = if app.daemon_up { "up" } else { "down" };
    let daemon_style = if app.daemon_up { t.ok } else { t.error };
    set_str(buf, 0, 0, "ztop", t.header_value, 6);
    set_str(buf, 5, 0, &format!("— {} repos indexed ({} cached)", app.totals.repos, app.totals.cached), t.header_label, cols.saturating_sub(5));
    let daemon_txt = format!("daemon {daemon}");
    let dx = cols.saturating_sub(daemon_txt.len() as u16 + 1);
    set_str(buf, dx, 0, &daemon_txt, daemon_style, daemon_txt.len() as u16);

    let g = &app.totals;
    let counts = [
        ("dirty", g.dirty, t.warn),
        ("ahead", g.ahead, t.ok),
        ("behind", g.behind, t.behind),
        ("diverged", g.diverged, t.diverged),
        ("detached", g.detached, t.error),
        ("clean", g.clean, t.dim),
    ];
    let mut x = 0u16;
    for (label, n, style) in counts {
        let seg = format!("{label} ");
        set_str(buf, x, 1, &seg, t.header_label, cols.saturating_sub(x));
        x += seg.len() as u16;
        let num = format!("{n}  ");
        set_str(buf, x, 1, &num, style, cols.saturating_sub(x));
        x += num.len() as u16;
    }
    set_str(
        buf,
        0,
        2,
        &format!(
            "claims {}  sessions {}  queue {} active   scheme: {}   every {:.1}s",
            g.claims, g.sessions, g.queue, app.scheme.name(), app.interval.as_secs_f64()
        ),
        t.header_label,
        cols,
    );

    // Table header bar with the active sort column highlighted + an arrow.
    let lay = Layout::compute(cols);
    let hdr_y = 4u16;
    fill_row(buf, hdr_y, t.header_bar);
    for c in COLS {
        let (cx, cw) = lay.col(c);
        let active = c == app.sort.col;
        let style = if active { t.sort_col } else { t.header_bar };
        let arrow = if active {
            if app.sort.desc {
                " ↓"
            } else {
                " ↑"
            }
        } else {
            ""
        };
        set_str(buf, cx, hdr_y, &format!("{}{arrow}", c.title()), style, cw);
    }

    // Rows.
    let first_row = hdr_y + 1;
    let foot_y = rows - 1;
    let visible = foot_y.saturating_sub(first_row) as usize;
    let total = app.rows.len();
    for (i, row) in app.rows.iter().skip(app.scroll).take(visible).enumerate() {
        let y = first_row + i as u16;
        let filled = ((row.churn / CHURN_CAP).clamp(0.0, 1.0) * CHURN_BAR_W as f64).round() as u16;
        for c in 0..CHURN_BAR_W {
            let (ch, st) = if c < filled { ("█", t.churn) } else { ("·", t.dim) };
            set_cell(buf, lay.churn_x + c, y, ch, st);
        }
        set_str(buf, lay.repo_x, y, &homify(&row.path), t.row, lay.repo_w);
        let head = if row.head.is_empty() { "-" } else { &row.head };
        set_str(buf, lay.head_x, y, head, t.dim, lay.head_w);
        let (word, rank) = severity_label(row);
        set_str(buf, lay.state_x, y, word, t.state_style(rank), lay.state_w);
        set_str(buf, lay.age_x, y, &fmt_ago(Duration::from_secs(row.age_secs.max(0) as u64)), t.dim, lay.age_w);
    }

    // Function bar (htop-style key hints), or the live `/` search prompt.
    fill_row(buf, foot_y, t.function_bar);
    if app.searching {
        let prompt = format!(" Search: {}▏", app.filter);
        set_str(buf, 0, foot_y, &prompt, t.function_bar, cols);
    } else {
        let keys = " F1 Help  F2 Colors  F6 Sort  / Search  I Invert  q Quit ";
        set_str(buf, 0, foot_y, keys, t.function_bar, cols);
    }
    let right = if !app.filter.is_empty() {
        format!(" /{}/ {} shown ", app.filter, total)
    } else if total > visible {
        format!(" {}–{}/{} ", app.scroll + 1, (app.scroll + visible).min(total), total)
    } else {
        String::new()
    };
    if !right.is_empty() {
        let mx = cols.saturating_sub(right.len() as u16 + 1);
        set_str(buf, mx, foot_y, &right, t.function_bar, right.len() as u16);
    }

    // Toast (ported htoprs draw_status).
    if let Some(msg) = app.status.as_ref() {
        if !msg.expired() && matches!(app.overlay, Overlay::None) && !app.searching {
            let text = format!(" {} ", msg.text);
            let tw = text.chars().count() as u16;
            if tw < cols {
                let tx = (cols - tw) / 2;
                let ty = rows.saturating_sub(3);
                let style = Style::default().fg(t.fg_on_accent).bg(t.accent_bg).add_modifier(Modifier::BOLD);
                set_str(buf, tx, ty, &text, style, tw);
            }
        }
    }

    // Overlays on top.
    match &app.overlay {
        Overlay::None => {}
        Overlay::Help => render_help(buf, t),
        Overlay::SortPick(cur) => render_pick(buf, t, "Sort by", &COLS.iter().map(|c| c.title()).collect::<Vec<_>>(), *cur, Some(app.sort.col.index())),
        Overlay::SchemePick(cur) => render_pick(buf, t, "Color scheme (live)", &SCHEMES.iter().map(|s| s.name()).collect::<Vec<_>>(), *cur, Some(app.scheme.index())),
    }
}

// The help overlay's palette, matching htoprs's F1 help (overlay.rs `draw_help`,
// theme.rs help_* colors): dark box, indexed accents, keys highlighted, white
// descriptions. Fixed regardless of the active scheme, exactly as htop's help is.
const HELP_BG: Color = Color::Indexed(236);
const HELP_TITLE: Color = Color::Indexed(27);
const HELP_SECTION: Color = Color::Indexed(99);
const HELP_KEY: Color = Color::Indexed(48);
const HELP_HINT: Color = Color::Indexed(240);

/// The F1 help content: sections of `(key, description)`, laid out in two columns
/// like htop's keyboard-shortcuts screen.
const HELP_SECTIONS: &[(&str, &[(&str, &str)])] = &[
    ("GENERAL", &[("F1 h ?", "This help"), ("F2 C", "Color scheme"), ("q Esc", "Quit"), ("^C", "Quit")]),
    ("SEARCH", &[("/", "Search / filter"), ("Esc", "Clear search")]),
    ("SORT", &[("F6 s", "Sort by column"), (">", "Next column"), ("<", "Prev column"), ("I", "Invert order")]),
    ("NAVIGATE", &[("↑ ↓ k j", "Move up / down"), ("PgUp PgDn", "Page up / down"), ("g G", "Top / bottom")]),
    ("ACTIVITY", &[("churn", "recent activity"), ("bar", "brighter = newer"), ("daemon", "run git zdaemon")]),
];

/// Draw the F1 help — a centered box, bold title, bold section headers, keys in
/// the help-key color, descriptions in white; two columns. Ported style from
/// htoprs `overlay::draw_help`.
fn render_help(buf: &mut Buffer, _t: &Theme) {
    let bg = Style::default().fg(Color::White).bg(HELP_BG);
    let border = Style::default().fg(HELP_TITLE).bg(HELP_BG).add_modifier(Modifier::BOLD);
    let section_s = Style::default().fg(HELP_SECTION).bg(HELP_BG).add_modifier(Modifier::BOLD);
    let key_s = Style::default().fg(HELP_KEY).bg(HELP_BG);
    let desc_s = Style::default().fg(Color::White).bg(HELP_BG);
    let hint_s = Style::default().fg(HELP_HINT).bg(HELP_BG);

    // Distribute sections across two columns, balancing height.
    let mut cols: [Vec<&(&str, &[(&str, &str)])>; 2] = [Vec::new(), Vec::new()];
    let mut heights = [0usize; 2];
    for s in HELP_SECTIONS {
        let h = s.1.len() + 2; // header + rows + a blank
        let c = if heights[0] <= heights[1] { 0 } else { 1 };
        cols[c].push(s);
        heights[c] += h;
    }
    let content_h = heights[0].max(heights[1]) as u16;
    let col_w = 30u16;
    let bw = (col_w * 2 + 5).min(buf.area().width);
    let bh = (content_h + 6).min(buf.area().height);
    let (ix, iy) = draw_box(buf, bw, bh, bg, border);

    set_str(buf, ix, iy, "⌨  ZTOP — KEYBOARD SHORTCUTS", border, bw - 3);
    set_str(buf, ix, iy + 1, "live fleet monitor", hint_s, bw - 3);
    for (ci, col) in cols.iter().enumerate() {
        let cx = ix + ci as u16 * (col_w + 1);
        let mut y = iy + 3;
        for (name, keys) in col {
            set_str(buf, cx, y, name, section_s, col_w);
            y += 1;
            for (k, d) in keys.iter() {
                set_str(buf, cx, y, k, key_s, 11);
                set_str(buf, cx + 11, y, d, desc_s, col_w - 11);
                y += 1;
            }
            y += 1;
        }
    }
    set_str(buf, ix, iy + bh.saturating_sub(3), "press h or Esc to close", hint_s, bw - 3);
}

/// A generic centered picker list (sort columns / schemes), with the cursor and
/// the current selection marked.
fn render_pick(buf: &mut Buffer, t: &Theme, title: &str, items: &[&str], cursor: usize, current: Option<usize>) {
    let w = items.iter().map(|s| s.len()).chain(std::iter::once(title.len())).max().unwrap_or(16) as u16 + 8;
    let h = items.len() as u16 + 4;
    let bg = Style::default().fg(t.fg_on_accent).bg(t.accent_bg);
    let border = bg.add_modifier(Modifier::BOLD);
    let sel = Style::default().fg(t.accent_bg).bg(t.fg_on_accent).add_modifier(Modifier::BOLD);
    let (ix, iy) = draw_box(buf, w, h, bg, border);
    set_str(buf, ix, iy, title, border, w - 3);
    for (i, item) in items.iter().enumerate() {
        let mark = if Some(i) == current { "●" } else { " " };
        let text = format!(" {mark} {item}");
        let style = if i == cursor { sel } else { bg };
        // pad to box inner width so the selection bar spans the row
        let padded = format!("{text:<width$}", width = (w - 3) as usize);
        set_str(buf, ix, iy + 2 + i as u16, &padded, style, w - 3);
    }
}

// ---------------------------------------------------------------------------
// Terminal lifecycle + event loop.
// ---------------------------------------------------------------------------

struct TermGuard;
impl Drop for TermGuard {
    fn drop(&mut self) {
        let _ = disable_raw_mode();
        let _ = execute!(stdout(), LeaveAlternateScreen, cursor::Show);
    }
}

fn run_tui(app: &mut App, allowed: &Option<HashSet<String>>) -> Result<ExitCode> {
    enable_raw_mode()?;
    execute!(stdout(), EnterAlternateScreen, cursor::Hide)?;
    let _guard = TermGuard;
    let mut term = Terminal::new(CrosstermBackend::new(stdout()))?;
    event_loop(&mut term, app, allowed)
}

fn event_loop(
    term: &mut Terminal<CrosstermBackend<std::io::Stdout>>,
    app: &mut App,
    allowed: &Option<HashSet<String>>,
) -> Result<ExitCode> {
    app.refresh(allowed);
    loop {
        term.draw(|f| render(f.buffer_mut(), app))?;

        let deadline = Instant::now() + app.interval;
        let mut ticked = true;
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() || !event::poll(remaining)? {
                break;
            }
            if let Event::Key(k) = event::read()? {
                if k.kind != KeyEventKind::Press {
                    continue;
                }
                match handle_key(app, k.code, k.modifiers) {
                    KeyOutcome::Quit => return Ok(ExitCode::SUCCESS),
                    KeyOutcome::Redraw => {
                        ticked = false;
                        break;
                    }
                    KeyOutcome::Ignore => continue,
                }
            }
        }
        if ticked {
            app.refresh(allowed);
        }
    }
}

enum KeyOutcome {
    Quit,
    Redraw,
    Ignore,
}

fn handle_key(app: &mut App, code: KeyCode, mods: KeyModifiers) -> KeyOutcome {
    // Ctrl-C always quits, even mid-search or in an overlay.
    if code == KeyCode::Char('c') && mods.contains(KeyModifiers::CONTROL) {
        return KeyOutcome::Quit;
    }

    // Incremental `/` search input takes precedence over everything else.
    if app.searching {
        match code {
            KeyCode::Esc => {
                app.searching = false;
                app.filter.clear();
                app.reproject();
            }
            KeyCode::Enter => {
                app.searching = false;
                if !app.filter.is_empty() {
                    app.set_status(format!("filter: {} ({} match)", app.filter, app.rows.len()));
                }
            }
            KeyCode::Backspace => {
                app.filter.pop();
                app.scroll = 0;
                app.reproject();
            }
            KeyCode::Char(c) => {
                app.filter.push(c);
                app.scroll = 0;
                app.reproject();
            }
            _ => return KeyOutcome::Ignore,
        }
        return KeyOutcome::Redraw;
    }

    // Overlay-specific handling next.
    match app.overlay {
        Overlay::Help => {
            app.overlay = Overlay::None;
            return KeyOutcome::Redraw;
        }
        Overlay::SortPick(cur) => {
            match code {
                KeyCode::Up | KeyCode::Char('k') => app.overlay = Overlay::SortPick(cur.saturating_sub(1)),
                KeyCode::Down | KeyCode::Char('j') => app.overlay = Overlay::SortPick((cur + 1).min(COLS.len() - 1)),
                KeyCode::Enter => {
                    let col = COLS[cur];
                    app.overlay = Overlay::None;
                    app.set_sort(col);
                }
                KeyCode::Esc | KeyCode::Char('q') => app.overlay = Overlay::None,
                _ => return KeyOutcome::Ignore,
            }
            return KeyOutcome::Redraw;
        }
        Overlay::SchemePick(cur) => {
            match code {
                KeyCode::Up | KeyCode::Char('k') => {
                    let n = cur.saturating_sub(1);
                    app.overlay = Overlay::SchemePick(n);
                    app.set_scheme(SCHEMES[n]); // live preview
                }
                KeyCode::Down | KeyCode::Char('j') => {
                    let n = (cur + 1).min(SCHEMES.len() - 1);
                    app.overlay = Overlay::SchemePick(n);
                    app.set_scheme(SCHEMES[n]);
                }
                KeyCode::Enter => {
                    app.overlay = Overlay::None;
                    save_scheme(app.scheme);
                    app.set_status(format!("scheme: {}", app.scheme.name()));
                }
                KeyCode::Esc | KeyCode::Char('q') => app.overlay = Overlay::None,
                _ => return KeyOutcome::Ignore,
            }
            return KeyOutcome::Redraw;
        }
        Overlay::None => {}
    }

    // Base handling.
    match (code, mods) {
        (KeyCode::Char('q'), _) | (KeyCode::Esc, _) => return KeyOutcome::Quit,
        (KeyCode::Char('c'), KeyModifiers::CONTROL) => return KeyOutcome::Quit,
        (KeyCode::F(1), _) | (KeyCode::Char('h'), _) | (KeyCode::Char('?'), _) => app.overlay = Overlay::Help,
        (KeyCode::F(2), _) | (KeyCode::Char('C'), _) => app.overlay = Overlay::SchemePick(app.scheme.index()),
        (KeyCode::F(6), _) | (KeyCode::Char('s'), _) | (KeyCode::Char('S'), _) => app.overlay = Overlay::SortPick(app.sort.col.index()),
        (KeyCode::Char('>'), _) | (KeyCode::Char('.'), _) => {
            let n = (app.sort.col.index() + 1) % COLS.len();
            app.set_sort(COLS[n]);
        }
        (KeyCode::Char('<'), _) | (KeyCode::Char(','), _) => {
            let n = (app.sort.col.index() + COLS.len() - 1) % COLS.len();
            app.set_sort(COLS[n]);
        }
        (KeyCode::Char('I'), _) | (KeyCode::Char('i'), _) => {
            app.sort.desc = !app.sort.desc;
            app.resort();
            app.set_status(format!("order: {}", if app.sort.desc { "descending" } else { "ascending" }));
        }
        (KeyCode::Char('/'), _) => {
            app.searching = true;
            app.scroll = 0;
        }
        (KeyCode::Up, _) | (KeyCode::Char('k'), _) => app.scroll = app.scroll.saturating_sub(1),
        (KeyCode::Down, _) | (KeyCode::Char('j'), _) => {
            app.scroll = (app.scroll + 1).min(app.rows.len().saturating_sub(1))
        }
        (KeyCode::PageUp, _) => app.scroll = app.scroll.saturating_sub(10),
        (KeyCode::PageDown, _) => app.scroll = (app.scroll + 10).min(app.rows.len().saturating_sub(1)),
        (KeyCode::Char('g'), _) | (KeyCode::Home, _) => app.scroll = 0,
        (KeyCode::Char('G'), _) | (KeyCode::End, _) => app.scroll = app.rows.len().saturating_sub(1),
        _ => return KeyOutcome::Ignore,
    }
    KeyOutcome::Redraw
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(path: &str, dirty: bool, detached: bool, sync: &str, churn: f64) -> Row {
        Row {
            path: path.into(),
            head: "abc123".into(),
            dirty,
            detached,
            sync: sync.into(),
            churn,
            age_secs: 0,
        }
    }

    #[test]
    fn severity_orders_urgent_first() {
        assert!(severity_rank(&row("d", false, true, "up-to-date", 0.0)) < severity_rank(&row("v", false, false, "diverged", 0.0)));
        assert!(severity_rank(&row("v", false, false, "diverged", 0.0)) < severity_rank(&row("x", true, false, "up-to-date", 0.0)));
        assert_eq!(severity_label(&row("c", false, false, "up-to-date", 0.0)).0, "clean");
        assert_eq!(severity_label(&row("a", false, false, "ahead", 0.0)).0, "ahead");
    }

    #[test]
    fn churn_sort_desc_puts_most_active_on_top() {
        let app = App::new(Scheme::Monochrome, Duration::from_secs(2));
        let mut rows = vec![
            row("low", false, false, "up-to-date", 0.5),
            row("high", true, false, "up-to-date", 5.0),
            row("mid", false, false, "ahead", 2.0),
        ];
        app.sort_rows(&mut rows); // default: churn desc
        assert_eq!(rows[0].path, "high");
        assert_eq!(rows[2].path, "low");
    }

    #[test]
    fn sort_by_name_ascending() {
        let mut app = App::new(Scheme::Monochrome, Duration::from_secs(2));
        app.sort = SortSpec { col: Col::Repo, desc: false };
        let mut rows = vec![row("charlie", false, false, "up-to-date", 0.0), row("alpha", false, false, "up-to-date", 0.0), row("bravo", false, false, "up-to-date", 0.0)];
        app.sort_rows(&mut rows);
        assert_eq!(rows.iter().map(|r| r.path.as_str()).collect::<Vec<_>>(), ["alpha", "bravo", "charlie"]);
    }

    #[test]
    fn filter_narrows_by_path_substring_case_insensitively() {
        let mut app = App::new(Scheme::Monochrome, Duration::from_secs(2));
        app.all = vec![
            row("/x/cask-repo", false, false, "up-to-date", 0.0),
            row("/x/web-app", false, false, "up-to-date", 0.0),
            row("/x/CASK-other", false, false, "up-to-date", 0.0),
        ];
        app.filter = "cask".into();
        app.reproject();
        assert_eq!(app.rows.len(), 2, "matches cask-repo and CASK-other");
        app.filter.clear();
        app.reproject();
        assert_eq!(app.rows.len(), 3, "cleared filter shows all");
    }

    #[test]
    fn scheme_roundtrips_by_name() {
        for s in SCHEMES {
            assert_eq!(Scheme::from_name(s.name()), Some(s));
        }
        assert_eq!(Scheme::from_name("nonsense"), None);
    }

    #[test]
    fn toast_expires_after_three_seconds() {
        let m = StatusMsg { text: "x".into(), since: Instant::now() };
        assert!(!m.expired());
        let old = StatusMsg { text: "x".into(), since: Instant::now() - Duration::from_secs(4) };
        assert!(old.expired());
    }
}

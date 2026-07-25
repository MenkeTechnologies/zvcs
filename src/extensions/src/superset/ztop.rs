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
const RECENCY_WINDOW: f64 = 3600.0; // 1 hour: recency baseline fades to 0
const RECENCY_WEIGHT: f64 = 2.0; // recency fills at most 2/CAP (~1/3) of the bar,
                                 // so a recently-swept repo is NOT a full bar
const CHURN_BUMP: f64 = 6.0; // a live change (commit/dirty) → a full-height spike
const CHURN_DECAY: f64 = 0.90; // the spike fades over ~a minute at 2s frames
const CHURN_CAP: f64 = 6.0;
const CHURN_BAR_W: u16 = 8;

/// How many rows around the scroll position to live-refresh (attached/detached +
/// branch) each frame — enough to cover any terminal's visible window, small
/// enough that the cheap ref reads stay instant.
const LIVE_WINDOW: usize = 80;

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
    // Start on the saved theme (persisted like htoprs's chooser/editor), or the
    // monochrome theme when color is off.
    let (palette, label) = load_palette();
    let mut app = App::new(palette, label, force_mono, Duration::from_secs_f64(interval));

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
    pub(crate) fn index(self) -> usize {
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
    SortPick(usize),      // cursor over COLS
    ThemeChooser(usize),  // cursor over THEMES; applied live as it moves
    ThemeEditor(usize),   // cursor over the 6 palette channels; edits live
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
    git_dir: String,
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
                git_dir: gd,
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
    git_dir: String, // to live-refresh this row's HEAD state (fixes stale cache)
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
    palette: Palette,
    label: String, // active theme's display name, or "Custom"
    mono: bool,    // --mono / NO_COLOR: render the monochrome theme
    theme: Theme,
    restore: Option<(Palette, String, bool)>, // pre-open (palette,label,mono) for Esc
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
    full_path: bool, // `p`: show the full repo path vs just its basename
    totals: Totals,
    daemon_up: bool,
}

impl App {
    fn new(palette: Palette, label: String, mono: bool, interval: Duration) -> Self {
        let theme = if mono { Theme::monochrome() } else { Theme::from_palette(palette) };
        App {
            palette,
            label,
            mono,
            theme,
            restore: None,
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
            full_path: true,
            totals: Totals::default(),
            daemon_up: false,
        }
    }

    fn set_status(&mut self, msg: impl Into<String>) {
        self.status = Some(StatusMsg { text: msg.into(), since: Instant::now() });
    }

    /// Apply a palette live (turns color on if it was mono).
    fn set_palette(&mut self, palette: Palette, label: impl Into<String>) {
        self.palette = palette;
        self.label = label.into();
        self.mono = false;
        self.theme = Theme::from_palette(palette);
    }

    /// Remember the current theme before opening a chooser/editor, so Esc reverts.
    fn snapshot_theme(&mut self) {
        self.restore = Some((self.palette, self.label.clone(), self.mono));
    }

    /// Revert to the pre-open theme (chooser/editor Esc).
    fn revert_theme(&mut self) {
        if let Some((p, l, m)) = self.restore.take() {
            self.palette = p;
            self.label = l;
            self.mono = m;
            self.theme = if m { Theme::monochrome() } else { Theme::from_palette(p) };
        }
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

            // Churn = a small recency baseline (recently-touched repos sit a bit
            // up the list) plus a live change-spike that dominates and fades.
            let age = (now - s.updated_at).max(0);
            let recency = (1.0 - age as f64 / RECENCY_WINDOW).clamp(0.0, 1.0);
            let churn = recency * RECENCY_WEIGHT + entry.burst;

            rows.push(Row {
                path: s.path.clone(),
                git_dir: s.git_dir.clone(),
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
        self.live_refresh_window();
    }

    /// Live-refresh the HEAD state (attached/detached + branch) of the rows around
    /// the current scroll — the ones actually on screen — so a stale status-cache
    /// entry (e.g. a repo re-attached since the daemon last scanned it) never shows
    /// wrong. Cheap ref reads only (no worktree scan, no rev-walk), bounded to a
    /// small window, so it stays instant across thousands of repos.
    fn live_refresh_window(&mut self) {
        let end = (self.scroll + LIVE_WINDOW).min(self.rows.len());
        for row in &mut self.rows[self.scroll..end] {
            let Ok(repo) = gix::open(&row.git_dir) else { continue };
            let name = repo.head_name().ok().flatten();
            row.detached = name.is_none();
            row.head = match name {
                Some(n) => n.shorten().to_string(),
                None => match repo.head().ok().and_then(|mut h| h.try_peel_to_id().ok().flatten()) {
                    Some(id) => format!("detached@{}", id.to_hex_with_len(12)),
                    None => "(unborn)".into(),
                },
            };
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
// Colorschemes — htoprs's 31 themes (src/extensions/theme.rs). Each theme is a
// 6-color 256-index palette `(c1..c6)`; the UI is htop's DEFAULT element layout
// recolored by remapping the 8 ANSI slots onto the palette (src/extensions/
// colors.rs `remap_from_palette`). A live THEME CHOOSER (c / F2) and a palette
// EDITOR (~ / e) both persist to `zvcs.topscheme` / `zvcs.toppalette`, like
// htoprs's theme chooser + editor.
// ---------------------------------------------------------------------------

/// htop ncurses base color numbers, for the DEFAULT element layout below.
const K: i16 = 0; // Black
const RD: i16 = 1; // Red
const GR: i16 = 2; // Green
const YL: i16 = 3; // Yellow
const CY: i16 = 6; // Cyan
const MG: i16 = 5; // Magenta
const DF: i16 = -1; // terminal default

/// One element: (fg, bg, bold), htop color numbers.
type E = (i16, i16, bool);

/// The elements ztop uses, and htop's DEFAULT `CRT_colorSchemes` values for them
/// (crt.rs) — the structure a theme recolors.
struct Elems {
    header_bar: E,
    function_bar: E,
    selection: E,
    meter_text: E,
    meter_value: E,
    ok: E,
    warn: E,
    error: E,
    process: E,
    megabytes: E,
}

const DEFAULT_ELEMS: Elems = Elems {
    header_bar: (K, GR, false),
    function_bar: (K, CY, false),
    selection: (K, CY, false),
    meter_text: (CY, K, false),
    meter_value: (CY, K, true),
    ok: (GR, K, false),
    warn: (YL, K, true),
    error: (RD, K, true),
    process: (DF, DF, false),
    megabytes: (CY, K, false),
};

/// A theme's 6-color palette: 256-color indices `(c1..c6)`.
pub(crate) type Palette = [u8; 6];

/// htoprs's 31 built-in themes (theme.rs `ThemeName`).
#[derive(Clone, Copy, PartialEq, Debug)]
pub(crate) enum ThemeName {
    NeonSprawl, AcidRain, IceBreaker, SynthWave, RustBelt, GhostWire, RedSector,
    SakuraDen, DataStream, SolarFlare, NeonNoir, ChromeHeart, BladeRunner,
    VoidWalker, ToxicWaste, CyberFrost, PlasmaCore, SteelNerve, DarkSignal,
    GlitchPop, HoloShift, NightCity, DeepNet, LaserGrid, QuantumFlux, BioHazard,
    Darkwave, Overlock, Megacorp, Zaibatsu, Iftopcolor,
}

pub(crate) const THEMES: [ThemeName; 31] = [
    ThemeName::NeonSprawl, ThemeName::AcidRain, ThemeName::IceBreaker, ThemeName::SynthWave,
    ThemeName::RustBelt, ThemeName::GhostWire, ThemeName::RedSector, ThemeName::SakuraDen,
    ThemeName::DataStream, ThemeName::SolarFlare, ThemeName::NeonNoir, ThemeName::ChromeHeart,
    ThemeName::BladeRunner, ThemeName::VoidWalker, ThemeName::ToxicWaste, ThemeName::CyberFrost,
    ThemeName::PlasmaCore, ThemeName::SteelNerve, ThemeName::DarkSignal, ThemeName::GlitchPop,
    ThemeName::HoloShift, ThemeName::NightCity, ThemeName::DeepNet, ThemeName::LaserGrid,
    ThemeName::QuantumFlux, ThemeName::BioHazard, ThemeName::Darkwave, ThemeName::Overlock,
    ThemeName::Megacorp, ThemeName::Zaibatsu, ThemeName::Iftopcolor,
];

impl ThemeName {
    /// Display name (theme.rs `display_name`).
    pub(crate) fn display_name(self) -> &'static str {
        match self {
            ThemeName::NeonSprawl => "Neon Sprawl",
            ThemeName::AcidRain => "Acid Rain",
            ThemeName::IceBreaker => "Ice Breaker",
            ThemeName::SynthWave => "Synth Wave",
            ThemeName::RustBelt => "Rust Belt",
            ThemeName::GhostWire => "Ghost Wire",
            ThemeName::RedSector => "Red Sector",
            ThemeName::SakuraDen => "Sakura Den",
            ThemeName::DataStream => "Data Stream",
            ThemeName::SolarFlare => "Solar Flare",
            ThemeName::NeonNoir => "Neon Noir",
            ThemeName::ChromeHeart => "Chrome Heart",
            ThemeName::BladeRunner => "Blade Runner",
            ThemeName::VoidWalker => "Void Walker",
            ThemeName::ToxicWaste => "Toxic Waste",
            ThemeName::CyberFrost => "Cyber Frost",
            ThemeName::PlasmaCore => "Plasma Core",
            ThemeName::SteelNerve => "Steel Nerve",
            ThemeName::DarkSignal => "Dark Signal",
            ThemeName::GlitchPop => "Glitch Pop",
            ThemeName::HoloShift => "Holo Shift",
            ThemeName::NightCity => "Night City",
            ThemeName::DeepNet => "Deep Net",
            ThemeName::LaserGrid => "Laser Grid",
            ThemeName::QuantumFlux => "Quantum Flux",
            ThemeName::BioHazard => "Bio Hazard",
            ThemeName::Darkwave => "Darkwave",
            ThemeName::Overlock => "Overlock",
            ThemeName::Megacorp => "Megacorp",
            ThemeName::Zaibatsu => "Zaibatsu",
            ThemeName::Iftopcolor => "Iftopcolor",
        }
    }

    /// The exact 6-color palette (theme.rs `palette`).
    pub(crate) fn palette(self) -> Palette {
        match self {
            ThemeName::NeonSprawl => [27, 48, 135, 141, 63, 99],
            ThemeName::AcidRain => [28, 46, 34, 40, 22, 35],
            ThemeName::IceBreaker => [19, 39, 25, 33, 21, 32],
            ThemeName::SynthWave => [91, 177, 128, 134, 93, 97],
            ThemeName::RustBelt => [172, 214, 178, 220, 166, 130],
            ThemeName::GhostWire => [37, 50, 44, 87, 30, 23],
            ThemeName::RedSector => [160, 203, 196, 210, 124, 88],
            ThemeName::SakuraDen => [175, 218, 182, 225, 169, 132],
            ThemeName::DataStream => [22, 46, 28, 119, 34, 22],
            ThemeName::SolarFlare => [202, 220, 196, 213, 160, 125],
            ThemeName::NeonNoir => [201, 231, 93, 219, 57, 53],
            ThemeName::ChromeHeart => [250, 255, 246, 253, 243, 239],
            ThemeName::BladeRunner => [208, 37, 166, 73, 130, 23],
            ThemeName::VoidWalker => [55, 99, 54, 141, 92, 17],
            ThemeName::ToxicWaste => [118, 190, 154, 226, 82, 58],
            ThemeName::CyberFrost => [159, 195, 153, 189, 111, 67],
            ThemeName::PlasmaCore => [199, 213, 163, 207, 126, 89],
            ThemeName::SteelNerve => [68, 110, 60, 146, 24, 236],
            ThemeName::DarkSignal => [30, 43, 23, 79, 29, 16],
            ThemeName::GlitchPop => [201, 51, 226, 47, 196, 21],
            ThemeName::HoloShift => [123, 219, 159, 183, 87, 133],
            ThemeName::NightCity => [214, 227, 209, 223, 172, 94],
            ThemeName::DeepNet => [19, 33, 17, 75, 26, 16],
            ThemeName::LaserGrid => [46, 201, 51, 226, 196, 21],
            ThemeName::QuantumFlux => [135, 75, 171, 111, 98, 61],
            ThemeName::BioHazard => [148, 184, 106, 192, 64, 22],
            ThemeName::Darkwave => [53, 140, 89, 176, 127, 52],
            ThemeName::Overlock => [196, 208, 160, 214, 124, 52],
            ThemeName::Megacorp => [252, 39, 245, 81, 242, 236],
            ThemeName::Zaibatsu => [167, 216, 131, 224, 95, 52],
            ThemeName::Iftopcolor => [21, 46, 28, 48, 33, 19],
        }
    }

    pub(crate) fn index(self) -> usize {
        THEMES.iter().position(|t| *t == self).unwrap_or(0)
    }
    pub(crate) fn from_name(s: &str) -> Option<ThemeName> {
        THEMES.iter().copied().find(|t| t.display_name().eq_ignore_ascii_case(s))
    }
}

/// Remap an ANSI color number (0-8) onto a theme palette channel — htoprs
/// `remap_from_palette` (colors.rs): 0/8→c6, 1→c5, 2/6→c2, 3→c4, 4/5→c3, 7→c1.
fn remap(n: i16, p: Palette) -> u8 {
    let [c1, c2, c3, c4, c5, c6] = p;
    match n {
        0 | 8 => c6,
        1 => c5,
        2 | 6 => c2,
        3 => c4,
        4 | 5 => c3,
        7 => c1,
        _ => c6,
    }
}

/// An ANSI color number through the active palette → a 256-index ratatui color
/// (terminal-default for -1).
pub(crate) fn cn_themed(n: i16, p: Palette) -> Color {
    if (0..=8).contains(&n) {
        Color::Indexed(remap(n, p))
    } else {
        Color::Reset
    }
}

/// Build a `Style` from an element, recolored by the palette. A `Black`
/// background collapses to the terminal default (the theme never fills row bg).
fn mk_t((fg, bg, bold): E, p: Palette) -> Style {
    let bg = if bg == K { DF } else { bg };
    let mut s = Style::default().fg(cn_themed(fg, p)).bg(cn_themed(bg, p));
    if bold {
        s = s.add_modifier(Modifier::BOLD);
    }
    s
}

/// Concrete styles for the active palette.
struct Theme {
    header_label: Style,
    header_value: Style,
    header_bar: Style,
    function_bar: Style,
    row: Style,
    dim: Style,
    ok: Style,
    warn: Style,
    error: Style,
    behind: Style,
    diverged: Style,
    churn: Style,
    sort_col: Style,
    accent_bg: Color,
    fg_on_accent: Color,
}

impl Theme {
    fn from_palette(p: Palette) -> Self {
        let e = &DEFAULT_ELEMS;
        let hdr = mk_t(e.header_bar, p);
        let sel = mk_t(e.selection, p);
        let on_row = |n: i16, bold: bool| {
            let mut s = Style::default().fg(cn_themed(n, p));
            if bold {
                s = s.add_modifier(Modifier::BOLD);
            }
            s
        };
        Theme {
            header_label: mk_t(e.meter_text, p),
            header_value: mk_t(e.meter_value, p),
            header_bar: hdr,
            function_bar: mk_t(e.function_bar, p),
            row: mk_t(e.process, p),
            dim: on_row(8, false),
            ok: mk_t(e.ok, p),
            warn: mk_t(e.warn, p),
            error: mk_t(e.error, p),
            behind: on_row(RD, false),
            diverged: on_row(MG, true),
            churn: mk_t(e.megabytes, p),
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

/// Read the persisted theme: a custom palette when `zvcs.topscheme` is "Custom"
/// (palette in `zvcs.toppalette`), else the named theme, else Neon Sprawl.
pub(crate) fn load_palette() -> (Palette, String) {
    let cfg = crate::config::global_config();
    let name = cfg.string("zvcs.topscheme").map(|s| s.to_string()).unwrap_or_default();
    if name.eq_ignore_ascii_case("Custom") {
        if let Some(p) = cfg.string("zvcs.toppalette").and_then(|s| parse_palette(&s.to_string())) {
            return (p, "Custom".into());
        }
    }
    if let Some(tn) = ThemeName::from_name(&name) {
        return (tn.palette(), tn.display_name().into());
    }
    (ThemeName::NeonSprawl.palette(), ThemeName::NeonSprawl.display_name().into())
}

/// Parse a `c1,c2,...,c6` palette string.
fn parse_palette(s: &str) -> Option<Palette> {
    let v: Vec<u8> = s.split(',').filter_map(|x| x.trim().parse().ok()).collect();
    (v.len() == 6).then(|| [v[0], v[1], v[2], v[3], v[4], v[5]])
}

/// Persist the chosen theme (and, for a custom palette, its values), like
/// htoprs's theme chooser + editor save.
pub(crate) fn save_palette(label: &str, pal: Palette) {
    let Ok(exe) = std::env::current_exe() else { return };
    let _ = Command::new(&exe).args(["config", "--global", "zvcs.topscheme", label]).status();
    if label.eq_ignore_ascii_case("Custom") {
        let s = pal.iter().map(|n| n.to_string()).collect::<Vec<_>>().join(",");
        let _ = Command::new(&exe).args(["config", "--global", "zvcs.toppalette", &s]).status();
    }
}

// ---------------------------------------------------------------------------
// Rendering primitives — ported from htoprs src/extensions/overlay.rs.
// ---------------------------------------------------------------------------

pub(crate) fn set_cell(buf: &mut Buffer, x: u16, y: u16, ch: &str, s: Style) {
    let a = buf.area();
    if x < a.x + a.width && y < a.y + a.height {
        let c = &mut buf[(x, y)];
        c.set_symbol(ch);
        c.set_style(s);
    }
}

pub(crate) fn set_str(buf: &mut Buffer, x: u16, y: u16, s: &str, st: Style, mw: u16) {
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

pub(crate) fn fill_row(buf: &mut Buffer, y: u16, st: Style) {
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
            "claims {}  sessions {}  queue {} active   theme: {}   every {:.1}s",
            g.claims, g.sessions, g.queue, if app.mono { "Monochrome" } else { &app.label }, app.interval.as_secs_f64()
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
        let shown_path = if app.full_path {
            homify(&row.path)
        } else {
            std::path::Path::new(&row.path)
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| homify(&row.path))
        };
        set_str(buf, lay.repo_x, y, &shown_path, t.row, lay.repo_w);
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
        Overlay::Help => render_help(buf),
        Overlay::SortPick(cur) => render_pick(buf, t, "Sort by", &COLS.iter().map(|c| c.title()).collect::<Vec<_>>(), *cur, Some(app.sort.col.index())),
        Overlay::ThemeChooser(cur) => render_theme_chooser(buf, *cur),
        Overlay::ThemeEditor(chan) => render_theme_editor(buf, app.palette, *chan),
    }
}

/// The THEME CHOOSER — 31 named themes with a color swatch each, cursor
/// highlighted, live preview. Ported from htoprs's theme chooser.
pub(crate) fn render_theme_chooser(buf: &mut Buffer, cursor: usize) {
    let bg = Style::default().fg(Color::White).bg(HELP_BG);
    let border = Style::default().fg(HELP_TITLE).bg(HELP_BG).add_modifier(Modifier::BOLD);
    let sel = Style::default().fg(Color::Black).bg(HELP_KEY).add_modifier(Modifier::BOLD);
    let hint_s = Style::default().fg(HELP_HINT).bg(HELP_BG);

    let name_w = 16u16;
    let swatch_w = 6 * 2u16; // 6 palette colors, 2 cells each
    let inner = name_w + 1 + swatch_w;
    let bw = (inner + 4).min(buf.area().width);
    // Fit as many rows as the box allows; scroll to keep the cursor visible.
    let max_rows = buf.area().height.saturating_sub(6) as usize;
    let visible = max_rows.min(THEMES.len()).max(1);
    let top = cursor.saturating_sub(visible.saturating_sub(1)).min(THEMES.len().saturating_sub(visible));
    let bh = (visible as u16 + 4).min(buf.area().height);
    let (ix, iy) = draw_box(buf, bw, bh, bg, border);
    set_str(buf, ix, iy, "THEME CHOOSER", border, bw - 3);

    for (row, ti) in (top..top + visible).enumerate() {
        let name = THEMES[ti].display_name();
        let y = iy + 2 + row as u16;
        let ns = if ti == cursor { sel } else { bg };
        let padded = format!(" {name:<w$}", w = name_w as usize);
        set_str(buf, ix, y, &padded, ns, name_w + 1);
        // swatch: six palette colors as ██ blocks
        let pal = THEMES[ti].palette();
        for (i, c) in pal.iter().enumerate() {
            let sx = ix + name_w + 2 + i as u16 * 2;
            set_cell(buf, sx, y, "█", Style::default().fg(Color::Indexed(*c)).bg(HELP_BG));
            set_cell(buf, sx + 1, y, "█", Style::default().fg(Color::Indexed(*c)).bg(HELP_BG));
        }
    }
    set_str(buf, ix, iy + bh.saturating_sub(2), "j/k:nav  Enter:select  Esc:cancel", hint_s, bw - 3);
}

/// The palette EDITOR — the active theme's six channels, one selectable, with a
/// live swatch; ←/→ change the 256-color index, s saves it as a custom theme.
pub(crate) fn render_theme_editor(buf: &mut Buffer, pal: Palette, channel: usize) {
    let bg = Style::default().fg(Color::White).bg(HELP_BG);
    let border = Style::default().fg(HELP_TITLE).bg(HELP_BG).add_modifier(Modifier::BOLD);
    let sel = Style::default().fg(Color::Black).bg(HELP_KEY).add_modifier(Modifier::BOLD);
    let hint_s = Style::default().fg(HELP_HINT).bg(HELP_BG);
    let labels = ["c1 primary", "c2 accent", "c3 mid-a", "c4 mid-b", "c5 dim", "c6 dark/bg"];

    let bw = 34u16.min(buf.area().width);
    let bh = (labels.len() as u16 + 4).min(buf.area().height);
    let (ix, iy) = draw_box(buf, bw, bh, bg, border);
    set_str(buf, ix, iy, "THEME EDITOR", border, bw - 3);
    for (i, lab) in labels.iter().enumerate() {
        let y = iy + 2 + i as u16;
        let rs = if i == channel { sel } else { bg };
        set_str(buf, ix, y, &format!(" {lab:<12} {:>3} ", pal[i]), rs, bw - 3 - 3);
        set_cell(buf, ix + bw - 5, y, "█", Style::default().fg(Color::Indexed(pal[i])).bg(HELP_BG));
        set_cell(buf, ix + bw - 4, y, "█", Style::default().fg(Color::Indexed(pal[i])).bg(HELP_BG));
    }
    set_str(buf, ix, iy + bh.saturating_sub(2), "↑↓:chan ←→:value s:save Esc", hint_s, bw - 3);
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
    ("GENERAL", &[("F1 h ?", "This help"), ("q Esc", "Quit"), ("^C", "Quit")]),
    ("THEME", &[("F2 c", "Theme chooser"), ("~ e", "Theme editor")]),
    ("SEARCH", &[("/", "Search / filter"), ("Esc", "Clear search")]),
    ("SORT", &[("F6 s", "Sort by column"), (">", "Next column"), ("<", "Prev column"), ("I", "Invert order")]),
    ("NAVIGATE", &[("↑ ↓ k j", "Move up / down"), ("PgUp PgDn", "Page up / down"), ("g G", "Top / bottom"), ("p", "Full path / name")]),
    ("ACTIVITY", &[("churn", "recent activity"), ("bar", "brighter = newer"), ("daemon", "run git zdaemon")]),
];

/// Draw the F1 help — a centered box, bold title, bold section headers, keys in
/// the help-key color, descriptions in white; two columns. Ported style from
/// htoprs `overlay::draw_help`.
pub(crate) fn render_help(buf: &mut Buffer) {
    render_help_for(buf, "ZTOP", "live fleet monitor", HELP_SECTIONS);
}

/// The same help box for any tool that shares this theme layer, titled with the
/// tool it actually belongs to and listing ITS keys. zdashboard renders its own
/// section list through here — a help screen that names another program and
/// advertises bindings the running one does not have is worse than no help.
#[allow(clippy::type_complexity)] // sections table shape; a lifetime alias would over-constrain
pub(crate) fn render_help_for(
    buf: &mut Buffer,
    tool: &str,
    subtitle: &str,
    sections: &[(&str, &[(&str, &str)])],
) {
    let bg = Style::default().fg(Color::White).bg(HELP_BG);
    let border = Style::default().fg(HELP_TITLE).bg(HELP_BG).add_modifier(Modifier::BOLD);
    let section_s = Style::default().fg(HELP_SECTION).bg(HELP_BG).add_modifier(Modifier::BOLD);
    let key_s = Style::default().fg(HELP_KEY).bg(HELP_BG);
    let desc_s = Style::default().fg(Color::White).bg(HELP_BG);
    let hint_s = Style::default().fg(HELP_HINT).bg(HELP_BG);

    // Distribute sections across two columns, balancing height.
    let mut cols: [Vec<&(&str, &[(&str, &str)])>; 2] = [Vec::new(), Vec::new()];
    let mut heights = [0usize; 2];
    for s in sections {
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

    set_str(buf, ix, iy, &format!("⌨  {tool} — KEYBOARD SHORTCUTS"), border, bw - 3);
    set_str(buf, ix, iy + 1, subtitle, hint_s, bw - 3);
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
            // Only unmodified text enters the query. A terminal encodes Alt+<c>
            // as ESC followed by <c>, and crossterm delivers that as one
            // ALT-modified key — so treating every Char as literal input turns a
            // quick `Esc` then `h` into the character `h` appended to the filter,
            // leaving the user stuck in search with no way out that they can see.
            // Alt/Ctrl chords leave search first, then act.
            KeyCode::Char(c) if !mods.intersects(KeyModifiers::ALT | KeyModifiers::CONTROL) => {
                app.filter.push(c);
                app.scroll = 0;
                app.reproject();
            }
            KeyCode::Char(_) => {
                app.searching = false;
                app.filter.clear();
                app.reproject();
                return handle_key(app, code, mods - KeyModifiers::ALT);
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
        Overlay::ThemeChooser(cur) => {
            let n = THEMES.len();
            match code {
                KeyCode::Up | KeyCode::Char('k') => {
                    let c = (cur + n - 1) % n;
                    app.overlay = Overlay::ThemeChooser(c);
                    app.set_palette(THEMES[c].palette(), THEMES[c].display_name()); // live preview
                }
                KeyCode::Down | KeyCode::Char('j') => {
                    let c = (cur + 1) % n;
                    app.overlay = Overlay::ThemeChooser(c);
                    app.set_palette(THEMES[c].palette(), THEMES[c].display_name());
                }
                KeyCode::Enter => {
                    app.overlay = Overlay::None;
                    app.restore = None;
                    save_palette(&app.label, app.palette);
                    app.set_status(format!("theme: {}", app.label));
                }
                KeyCode::Esc | KeyCode::Char('q') => {
                    app.overlay = Overlay::None;
                    app.revert_theme();
                }
                _ => return KeyOutcome::Ignore,
            }
            return KeyOutcome::Redraw;
        }
        Overlay::ThemeEditor(chan) => {
            match code {
                KeyCode::Up | KeyCode::Char('k') => app.overlay = Overlay::ThemeEditor(chan.saturating_sub(1)),
                KeyCode::Down | KeyCode::Char('j') => app.overlay = Overlay::ThemeEditor((chan + 1).min(5)),
                KeyCode::Left | KeyCode::Char('-') | KeyCode::Char('_') => {
                    let mut p = app.palette;
                    p[chan] = p[chan].wrapping_sub(1);
                    app.set_palette(p, "Custom");
                }
                KeyCode::Right | KeyCode::Char('+') | KeyCode::Char('=') => {
                    let mut p = app.palette;
                    p[chan] = p[chan].wrapping_add(1);
                    app.set_palette(p, "Custom");
                }
                KeyCode::Char('s') | KeyCode::Enter => {
                    app.overlay = Overlay::None;
                    app.restore = None;
                    app.set_palette(app.palette, "Custom");
                    save_palette("Custom", app.palette);
                    app.set_status("saved custom theme");
                }
                KeyCode::Esc => {
                    app.overlay = Overlay::None;
                    app.revert_theme();
                }
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
        (KeyCode::F(2), _) | (KeyCode::Char('c'), _) => {
            app.snapshot_theme();
            let cur = ThemeName::from_name(&app.label).map(|t| t.index()).unwrap_or(0);
            app.overlay = Overlay::ThemeChooser(cur);
        }
        (KeyCode::Char('~'), _) | (KeyCode::Char('e'), _) => {
            app.snapshot_theme();
            app.overlay = Overlay::ThemeEditor(0);
        }
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
        (KeyCode::Char('p'), _) => {
            app.full_path = !app.full_path;
            app.set_status(if app.full_path { "full path" } else { "repo name" });
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

    fn test_app() -> App {
        App::new(ThemeName::NeonSprawl.palette(), "Neon Sprawl".into(), true, Duration::from_secs(2))
    }

    fn row(path: &str, dirty: bool, detached: bool, sync: &str, churn: f64) -> Row {
        Row {
            path: path.into(),
            git_dir: String::new(),
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
        let app = test_app();
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
        let mut app = test_app();
        app.sort = SortSpec { col: Col::Repo, desc: false };
        let mut rows = vec![row("charlie", false, false, "up-to-date", 0.0), row("alpha", false, false, "up-to-date", 0.0), row("bravo", false, false, "up-to-date", 0.0)];
        app.sort_rows(&mut rows);
        assert_eq!(rows.iter().map(|r| r.path.as_str()).collect::<Vec<_>>(), ["alpha", "bravo", "charlie"]);
    }

    #[test]
    fn filter_narrows_by_path_substring_case_insensitively() {
        let mut app = test_app();
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
    fn all_31_themes_roundtrip_and_have_full_palettes() {
        assert_eq!(THEMES.len(), 31, "all of htoprs's 31 themes");
        for t in THEMES {
            assert_eq!(ThemeName::from_name(t.display_name()), Some(t));
            assert_eq!(t.palette().len(), 6);
        }
        assert_eq!(ThemeName::from_name("nonsense"), None);
    }

    #[test]
    fn custom_palette_persists_and_parses() {
        assert_eq!(parse_palette("1,2,3,4,5,6"), Some([1, 2, 3, 4, 5, 6]));
        assert_eq!(parse_palette("bad"), None);
        // remap follows htoprs's channel mapping (accent = c2 on Green/Cyan slots).
        let p = [10, 20, 30, 40, 50, 60];
        assert_eq!(remap(2, p), 20);
        assert_eq!(remap(6, p), 20);
        assert_eq!(remap(7, p), 10);
        assert_eq!(remap(0, p), 60);
    }

    #[test]
    fn toast_expires_after_three_seconds() {
        let m = StatusMsg { text: "x".into(), since: Instant::now() };
        assert!(!m.expired());
        let old = StatusMsg { text: "x".into(), since: Instant::now() - Duration::from_secs(4) };
        assert!(old.expired());
    }
}

#[cfg(test)]
mod search_key_tests {
    use super::*;

    fn app() -> App {
        App::new(ThemeName::NeonSprawl.palette(), "Neon Sprawl".into(), true, Duration::from_secs(2))
    }

    #[test]
    fn plain_characters_type_into_the_query() {
        let mut a = app();
        handle_key(&mut a, KeyCode::Char('/'), KeyModifiers::NONE);
        assert!(a.searching, "'/' opens the incremental search");
        for c in "zdb".chars() {
            handle_key(&mut a, KeyCode::Char(c), KeyModifiers::NONE);
        }
        assert_eq!(a.filter, "zdb");
        assert!(a.searching, "typing stays in search");
    }

    /// A terminal encodes `Alt+h` — and an `Esc` immediately followed by `h` —
    /// as one ALT-modified key. Appending that to the query left the user stuck
    /// in search with an unexplained `h` in it, which is what "the h key doesn't
    /// work" looks like from the outside.
    #[test]
    fn an_alt_chord_leaves_search_and_acts() {
        let mut a = app();
        handle_key(&mut a, KeyCode::Char('/'), KeyModifiers::NONE);
        handle_key(&mut a, KeyCode::Char('h'), KeyModifiers::ALT);

        assert!(!a.searching, "the chord ends search");
        assert!(a.filter.is_empty(), "and does not leave a stray character behind");
        assert!(matches!(a.overlay, Overlay::Help), "then does what the key means");
    }

    #[test]
    fn escape_still_cancels_the_search() {
        let mut a = app();
        handle_key(&mut a, KeyCode::Char('/'), KeyModifiers::NONE);
        handle_key(&mut a, KeyCode::Char('x'), KeyModifiers::NONE);
        handle_key(&mut a, KeyCode::Esc, KeyModifiers::NONE);

        assert!(!a.searching);
        assert!(a.filter.is_empty());
        assert!(matches!(a.overlay, Overlay::None));
    }
}

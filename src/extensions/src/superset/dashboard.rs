//! `git zdashboard` — a live, tiled TUI that combines the fleet monitor, the
//! semantic event feed, and the fleet command feed on one screen.
//!
//! Four tiles, refreshed together: a **fleet** table (every indexed repo, most
//! recently active first, read from the daemon status cache with the on-screen
//! rows' HEAD state live-refreshed so nothing is stale), a **processes** tile
//! (`zppid`: each invoking ppid, its commit tally, live/dead state, and last-commit
//! age), the **events** feed (`zevents`: commits / reconciles / status changes),
//! and the **commands** feed (`zcommands`: every git command run across the machine,
//! with the agent that ran it). A header row carries the aggregate totals. It shares ztop's theme
//! system: `c` opens the 31-scheme chooser, `~`/`e` the palette editor, F1 the
//! help overlay. The non-interactive path (`--once` / `--json` / a non-tty
//! stdout) keeps the instant text summary.
//!
//! Interactive cursor (mouse ported from iftoprs, plus keyboard). One tile is
//! focused and its selected row is highlighted with a background — the cursor.
//! `↑`/`↓` (or `j`/`k`, PageUp/Down, `g`/`G`) move the cursor within the focused
//! tile; `Tab` cycles tiles and `←`/`→` switch columns. The mouse drives the same
//! cursor: hovering moves it to the row under the pointer (the highlight follows the
//! mouse), the wheel steps it, a left-click selects, and a right-click selects and
//! opens an opaque per-row detail popup. Feeds (events/commands) stay pinned to the
//! newest row until you scroll up. The focused tile's title shows `▸ … [sel/len]`.
//! Panes are resizable (ported from zmax): **click and drag a divider** (the border
//! between panes) to move it under the cursor, or use the keys `H`/`L` (vertical
//! column divider), `J`/`K` (focused column's horizontal divider), `=` to reset —
//! each clamped so no pane drops below `MIN_W × MIN_H`.

use std::io::{stdout, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::{Duration, Instant};

use anyhow::Result;
use ratatui::backend::CrosstermBackend;
use ratatui::buffer::Buffer;
use ratatui::crossterm::{
    cursor,
    event::{
        self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEventKind, KeyModifiers,
        MouseButton, MouseEvent, MouseEventKind,
    },
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::style::{Modifier, Style};
use ratatui::Terminal;

use crate::superset::ztop::{
    cn_themed, fill_row, load_palette, render_help, render_theme_chooser, render_theme_editor,
    save_palette, set_cell, set_str, Palette, ThemeName, THEMES,
};

const POLL: Duration = Duration::from_millis(700);
const FEED: usize = 200; // rows pulled per feed before the tile clips them

/// Concrete styles for the dashboard, derived from the active theme palette via
/// the same remap ztop uses (so both recolor identically). ANSI slots: accent=6,
/// green=2, yellow=3, red=1, magenta=5, blue=4, gray=8, black=0, default=-1.
struct Colors {
    title: Style,
    border: Style,
    label: Style,
    row: Style,
    dim: Style,
    ok: Style,
    warn: Style,
    error: Style,
    behind: Style,
    diverged: Style,
    bar: Style,
    cyan: Style,
    green: Style,
    magenta: Style,
    blue: Style,
    /// Background overlay for the row under the mouse cursor (hover highlight).
    hl_bg: ratatui::style::Color,
    /// Solid, opaque background for the right-click detail popup.
    popup_bg: ratatui::style::Color,
}

impl Colors {
    fn from_theme(pal: Palette, mono: bool) -> Self {
        if mono {
            return Colors::mono();
        }
        let c = |n: i16| cn_themed(n, pal);
        let f = |n: i16| Style::default().fg(c(n));
        let b = |n: i16| Style::default().fg(c(n)).add_modifier(Modifier::BOLD);
        Colors {
            title: b(6),
            border: f(6),
            label: f(6),
            row: Style::default(),
            dim: f(8),
            ok: f(2),
            warn: b(3),
            error: b(1),
            behind: f(1),
            diverged: b(5),
            bar: Style::default().fg(c(0)).bg(c(6)),
            cyan: f(6),
            green: f(2),
            magenta: f(5),
            blue: f(4),
            hl_bg: c(8), // gray: a subtle selection background under the cursor
            popup_bg: c(0), // black: an opaque backing for the right-click popup
        }
    }
    fn mono() -> Self {
        let p = Style::default();
        let b = p.add_modifier(Modifier::BOLD);
        let rev = p.add_modifier(Modifier::REVERSED);
        Colors {
            title: b,
            border: p,
            label: p,
            row: p,
            dim: p.add_modifier(Modifier::DIM),
            ok: p,
            warn: b,
            error: b,
            behind: b,
            diverged: b,
            bar: rev,
            cyan: p,
            green: p,
            magenta: p,
            blue: p,
            hl_bg: ratatui::style::Color::DarkGray,
            popup_bg: ratatui::style::Color::Black,
        }
    }
}

/// A theme overlay open over the dashboard.
enum Overlay {
    None,
    Help,
    Chooser(usize),
    Editor(usize),
}

/// A rectangle in the terminal grid, for mouse hit-testing the tiles.
#[derive(Clone, Copy)]
struct Rect {
    x: u16,
    y: u16,
    w: u16,
    h: u16,
}

impl Rect {
    fn contains(&self, col: u16, row: u16) -> bool {
        col >= self.x && col < self.x + self.w && row >= self.y && row < self.y + self.h
    }

    /// Inner content area, matching [`tile`]'s border inset (`x+2, y+1, w-4, h-2`).
    fn inner(&self) -> (u16, u16, u16, u16) {
        (self.x + 2, self.y + 1, self.w.saturating_sub(4), self.h.saturating_sub(2))
    }
}

/// The four tile rectangles, derived from the terminal size the same way [`render`]
/// lays them out — shared with the mouse handler so a click maps to a tile.
struct Layout {
    fleet: Rect,
    procs: Rect,
    events: Rect,
    commands: Rect,
}

/// Minimum pane width/height a divider may leave on either side — the floor the
/// resize clamp respects, ported from zmax's `grow_to`/`resize_*` (which never take
/// a sibling below `min`).
const MIN_W: u16 = 12;
const MIN_H: u16 = 3;

/// Adjustable split offsets (in cells) from the default layout, driven by the resize
/// keys (ported from zmax's `resize_horizontal`/`resize_vertical` delta model).
/// `col` shifts the vertical column divider; `lrow`/`rrow` shift the left and right
/// columns' horizontal dividers. Always clamped in [`layout`] so no pane vanishes.
#[derive(Default, Clone, Copy)]
struct Splits {
    col: i16,
    lrow: i16,
    rrow: i16,
}

/// Which divider a mouse drag is moving.
#[derive(Clone, Copy, PartialEq)]
enum Divider {
    Col,      // vertical seam between the left and right columns
    LeftRow,  // horizontal seam between fleet and procs
    RightRow, // horizontal seam between events and commands
}

/// The default (un-resized) width of the left column and heights of the left/right
/// top panes, for `cols × rows`. The resize deltas are offsets from these.
fn split_defaults(cols: u16, body_h: u16) -> (u16, u16, u16) {
    (cols / 2, body_h - body_h / 3, body_h / 2)
}

/// The divider (and the grab offset from its edge) under the point `(col, row)`, if
/// any — a press here starts a drag-to-resize (ported from zmax's `split_divider_at`
/// + `resize_drag`). The seam is the two-cell border between adjacent tiles.
fn divider_at(l: &Layout, col: u16, row: u16) -> Option<(Divider, i16)> {
    let top = l.fleet.y;
    let left_w = l.fleet.w;
    let body_bottom = l.commands.y + l.commands.h;
    // Vertical column seam: x in {left_w-1, left_w}, anywhere down the body.
    if (col == left_w || col + 1 == left_w) && row >= top && row < body_bottom {
        return Some((Divider::Col, col as i16 - left_w as i16));
    }
    // Left column's fleet/procs seam.
    let ldiv = l.procs.y;
    if col < left_w && (row == ldiv || row + 1 == ldiv) {
        return Some((Divider::LeftRow, row as i16 - ldiv as i16));
    }
    // Right column's events/commands seam.
    let rdiv = l.commands.y;
    if col >= left_w && (row == rdiv || row + 1 == rdiv) {
        return Some((Divider::RightRow, row as i16 - rdiv as i16));
    }
    None
}

/// Clamp an absolute split position to `[min, max]` and return it as an offset from
/// `default` (the value stored in [`Splits`]).
fn set_split_abs(default: u16, target: i32, min: u16, max: u16) -> i16 {
    (target.clamp(min as i32, max as i32) - default as i32) as i16
}

/// Compute the tile layout for `cols × rows` with the given resize `splits`, or
/// `None` when the terminal is too small (matching [`render`]'s minimum). The split
/// deltas are applied and clamped so every pane keeps at least `MIN_W × MIN_H`.
fn layout(cols: u16, rows: u16, s: Splits) -> Option<Layout> {
    if cols < 40 || rows < 12 {
        return None;
    }
    let top = 3u16;
    let body_h = (rows - 1).saturating_sub(top);
    let (def_lw, def_fh, def_eh) = split_defaults(cols, body_h);
    let clampw = |v: i32| v.clamp(MIN_W as i32, (cols - MIN_W) as i32) as u16;
    let clamph = |v: i32| v.clamp(MIN_H as i32, (body_h - MIN_H) as i32) as u16;
    let left_w = clampw(def_lw as i32 + s.col as i32);
    let right_w = cols - left_w;
    let fleet_h = clamph(def_fh as i32 + s.lrow as i32);
    let proc_h = body_h - fleet_h;
    let ev_h = clamph(def_eh as i32 + s.rrow as i32);
    let cmd_h = body_h - ev_h;
    Some(Layout {
        fleet: Rect { x: 0, y: top, w: left_w, h: fleet_h },
        procs: Rect { x: 0, y: top + fleet_h, w: left_w, h: proc_h },
        events: Rect { x: left_w, y: top, w: right_w, h: ev_h },
        commands: Rect { x: left_w, y: top + ev_h, w: right_w, h: cmd_h },
    })
}

/// A right-click detail popup: the lines to show and where the click landed.
struct Tooltip {
    lines: Vec<String>,
    col: u16,
    row: u16,
}

/// The rectangle of tile index `t`.
fn tile_rect(l: &Layout, t: usize) -> Rect {
    match t {
        FLEET => l.fleet,
        PROCS => l.procs,
        EVENTS => l.events,
        _ => l.commands,
    }
}

/// Which tile index (if any) the point `(col, row)` falls in.
fn tile_at(l: &Layout, col: u16, row: u16) -> Option<usize> {
    (0..4).find(|&t| tile_rect(l, t).contains(col, row))
}

// ---------------------------------------------------------------------------
// Data.
// ---------------------------------------------------------------------------

struct FleetRow {
    path: String,
    git_dir: String,
    head: String,
    dirty: bool,
    detached: bool,
    sync: String,
    age_secs: i64,
}

struct CmdRec {
    ts: i64,
    pid: u32,
    ppid: i32,
    cwd: String,
    argv: String,
}

/// One row of the per-process commit tile (`zppid`): the responsible process's pid,
/// command name and cwd, its commit tally, whether it is still live, and how long
/// since it last committed.
struct ProcRow {
    ppid: i64,
    cmd: String,
    cwd: String,
    commits: i64,
    alive: bool,
    age_secs: i64,
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
    procs: usize,
    /// The most productive tracked process: `(ppid, commits)`, if any.
    top_ppid: Option<(i64, i64)>,
}

struct Dash {
    totals: Totals,
    fleet: Vec<FleetRow>,
    events: Vec<crate::db::EventRow>,
    commands: Vec<CmdRec>,
    procs: Vec<ProcRow>,
    daemon_up: bool,
    // Theme state (shared with ztop).
    pal: Palette,
    label: String,
    mono: bool,
    colors: Colors,
    overlay: Overlay,
    restore: Option<(Palette, String, bool)>,
    // Interactive cursor. One `TileView` (selected row + scroll top) per tile, plus
    // which tile is focused. The focused tile's selected row is the highlighted
    // cursor; arrow keys move it within the focused tile, Tab switches tiles, and the
    // mouse (hover / scroll / click) drives the same selection.
    view: [TileView; 4],
    focus: usize,
    /// Per-tile "follow the newest row" flag. Feeds (events/commands) start pinned to
    /// the tail and re-pin whenever the cursor is on the last row; scrolling up
    /// unpins. Lists (fleet/procs) ignore it.
    follow: [bool; 4],
    /// User pane-resize offsets from the default layout.
    splits: Splits,
    /// An in-progress divider drag: which divider, and the grab offset from its edge.
    drag: Option<(Divider, i16)>,
    tooltip: Option<Tooltip>,
}

/// Tile indices into [`Dash::view`], and the tile the mouse/keys act on.
const FLEET: usize = 0;
const PROCS: usize = 1;
const EVENTS: usize = 2;
const COMMANDS: usize = 3;

/// Per-tile view: the selected data row (the cursor) and the first visible row
/// (scroll top). The visible window is `[top, top + inner_height)`.
#[derive(Default, Clone, Copy)]
struct TileView {
    sel: usize,
    top: usize,
}

impl Dash {
    fn new() -> Self {
        let (pal, label) = load_palette();
        let mono = std::env::var_os("NO_COLOR").is_some();
        Dash {
            totals: Totals::default(),
            fleet: Vec::new(),
            events: Vec::new(),
            commands: Vec::new(),
            procs: Vec::new(),
            daemon_up: false,
            pal,
            label,
            mono,
            colors: Colors::from_theme(pal, mono),
            overlay: Overlay::None,
            restore: None,
            view: [TileView::default(); 4],
            focus: FLEET,
            follow: [true; 4],
            splits: Splits::default(),
            drag: None,
            tooltip: None,
        }
    }

    /// Move a divider so its edge follows the mouse absolutely (`cursor − grab
    /// offset`), clamped so no pane drops below the minimum — zmax's drag model.
    /// `l` is the current layout (only its invariants — body height, top, defaults —
    /// are read, so the stale-vs-new split doesn't matter).
    fn apply_drag(&mut self, l: &Layout, div: Divider, offset: i16, col: u16, row: u16, cols: u16) {
        let top = l.fleet.y;
        let body_h = l.fleet.h + l.procs.h;
        let (def_lw, def_fh, def_eh) = split_defaults(cols, body_h);
        match div {
            Divider::Col => {
                let target = col as i32 - offset as i32;
                self.splits.col = set_split_abs(def_lw, target, MIN_W, cols - MIN_W);
            }
            Divider::LeftRow => {
                let target = row as i32 - offset as i32 - top as i32; // fleet height
                self.splits.lrow = set_split_abs(def_fh, target, MIN_H, body_h - MIN_H);
            }
            Divider::RightRow => {
                let target = row as i32 - offset as i32 - top as i32; // events height
                self.splits.rrow = set_split_abs(def_eh, target, MIN_H, body_h - MIN_H);
            }
        }
    }

    /// The data-row count of tile `t`.
    fn tile_len(&self, t: usize) -> usize {
        match t {
            FLEET => self.fleet.len(),
            PROCS => self.procs.len(),
            EVENTS => self.events.len(),
            _ => self.commands.len(),
        }
    }

    /// Keep every tile's selection/scroll in range as the data changes between
    /// refreshes, and re-pin any following feed to its newest row. Needs the tile
    /// heights, so it runs from the render loop where the terminal size is known.
    fn reconcile_views(&mut self, l: &Layout) {
        for t in 0..4 {
            let len = self.tile_len(t);
            let ih = tile_rect(l, t).inner().3 as usize;
            let v = &mut self.view[t];
            if len == 0 {
                *v = TileView::default();
                continue;
            }
            // A pinned feed tracks the newest row.
            if (t == EVENTS || t == COMMANDS) && self.follow[t] {
                v.sel = len - 1;
            }
            v.sel = v.sel.min(len - 1);
            // Keep the selection inside the window.
            if v.sel < v.top {
                v.top = v.sel;
            } else if ih > 0 && v.sel >= v.top + ih {
                v.top = v.sel + 1 - ih;
            }
            v.top = v.top.min(len.saturating_sub(ih));
        }
    }

    /// Move the focused tile's cursor by `delta` rows, scrolling to keep it visible.
    fn move_sel(&mut self, delta: i32, ih: usize) {
        let t = self.focus;
        let len = self.tile_len(t);
        if len == 0 {
            return;
        }
        let v = &mut self.view[t];
        v.sel = (v.sel as i32 + delta).clamp(0, len as i32 - 1) as usize;
        if v.sel < v.top {
            v.top = v.sel;
        } else if ih > 0 && v.sel >= v.top + ih {
            v.top = v.sel + 1 - ih;
        }
        // Feeds re-pin to the tail only when the cursor sits on the last row.
        if t == EVENTS || t == COMMANDS {
            self.follow[t] = v.sel + 1 >= len;
        }
    }

    fn set_palette(&mut self, pal: Palette, label: impl Into<String>) {
        self.pal = pal;
        self.label = label.into();
        self.mono = false;
        self.colors = Colors::from_theme(pal, false);
    }

    fn snapshot_theme(&mut self) {
        self.restore = Some((self.pal, self.label.clone(), self.mono));
    }

    fn revert_theme(&mut self) {
        if let Some((p, l, m)) = self.restore.take() {
            self.pal = p;
            self.label = l;
            self.mono = m;
            self.colors = Colors::from_theme(p, m);
        }
    }

    /// Re-read all three sources. `visible_fleet` is how many fleet rows the tile
    /// can show — only those get their HEAD state live-refreshed (cheap).
    fn refresh(&mut self, visible_fleet: usize) {
        let now = crate::date::now_seconds();
        let mut t = Totals::default();
        let mut fleet = Vec::new();
        if let Ok(conn) = crate::db::open_ro() {
            t.repos = crate::db::list_repos(&conn).map(|v| v.len()).unwrap_or(0);
            let sql = "SELECT COALESCE(r.workdir, r.git_dir), r.git_dir, s.dirty, s.detached, s.sync, s.head, s.updated_at \
                       FROM repo_status s JOIN repos r ON r.id = s.repo_id ORDER BY s.updated_at DESC";
            if let Ok(mut stmt) = conn.prepare(sql) {
                let rows = stmt.query_map([], |r| {
                    let path: String = r.get(0)?;
                    Ok(FleetRow {
                        path,
                        git_dir: r.get(1)?,
                        dirty: r.get::<_, i64>(2)? != 0,
                        detached: r.get::<_, i64>(3)? != 0,
                        sync: r.get(4)?,
                        head: r.get(5)?,
                        age_secs: (now - r.get::<_, i64>(6)?).max(0),
                    })
                });
                if let Ok(it) = rows {
                    for row in it.flatten() {
                        if !row.path.starts_with('/') {
                            continue;
                        }
                        t.cached += 1;
                        if row.dirty {
                            t.dirty += 1;
                        }
                        if row.detached {
                            t.detached += 1;
                        }
                        match row.sync.as_str() {
                            "ahead" => t.ahead += 1,
                            "behind" => t.behind += 1,
                            "diverged" => t.diverged += 1,
                            _ => {}
                        }
                        if !row.dirty && !row.detached && (row.sync == "up-to-date" || row.sync == "no-upstream") {
                            t.clean += 1;
                        }
                        fleet.push(row);
                    }
                }
            }
            let claim_list = crate::db::list_claims(&conn).unwrap_or_default();
            t.claims = claim_list.len();
            t.sessions = claim_list
                .iter()
                .map(|(_, s, _)| s.as_str())
                .collect::<std::collections::HashSet<_>>()
                .len();
            t.queue = crate::db::list_jobs(&conn, 1000)
                .map(|j| j.iter().filter(|x| x.state == "queued" || x.state == "running").count())
                .unwrap_or(0);
            // Per-process commit tallies (`zppid`): count of tracked processes and
            // the busiest one (list_ppids is ordered commits-desc, so first = top).
            let ppids = crate::db::list_ppids(&conn).unwrap_or_default();
            t.procs = ppids.len();
            t.top_ppid = ppids.first().map(|p| (p.ppid, p.commits));
            self.procs = ppids
                .into_iter()
                .map(|p| ProcRow {
                    alive: crate::superset::zppid::is_alive(p.ppid),
                    age_secs: (now - p.last_seen).max(0),
                    ppid: p.ppid,
                    cmd: p.cmd,
                    cwd: p.cwd,
                    commits: p.commits,
                })
                .collect();
            self.events = crate::db::events_recent(&conn, FEED, None, None).unwrap_or_default();
        }

        for row in fleet.iter_mut().take(visible_fleet) {
            if let Ok(repo) = gix::open(&row.git_dir) {
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

        self.totals = t;
        self.fleet = fleet;
        self.commands = read_commands(FEED);
        self.daemon_up = crate::superset::zdaemon::is_running();
        // Selection/scroll are reconciled against the new data in the render loop,
        // where the tile heights are known (see `reconcile_views`).
    }
}

fn commands_log() -> PathBuf {
    crate::superset::zdaemon::zvcs_home().join("commands.log")
}

fn read_commands(n: usize) -> Vec<CmdRec> {
    let path = commands_log();
    let Ok(len) = std::fs::metadata(&path).map(|m| m.len()) else { return Vec::new() };
    let Ok(mut f) = std::fs::File::open(&path) else { return Vec::new() };
    let want = (n as u64 * 256).min(len);
    let _ = f.seek(SeekFrom::Start(len - want));
    let mut buf = Vec::new();
    if f.read_to_end(&mut buf).is_err() {
        return Vec::new();
    }
    let text = String::from_utf8_lossy(&buf);
    let mut out: Vec<CmdRec> = text.lines().filter_map(parse_cmd).collect();
    let start = out.len().saturating_sub(n);
    out.drain(..start);
    out
}

fn parse_cmd(line: &str) -> Option<CmdRec> {
    let mut it = line.splitn(5, '\t');
    Some(CmdRec {
        ts: it.next()?.parse().ok()?,
        pid: it.next()?.parse().ok()?,
        ppid: it.next()?.parse().ok()?,
        cwd: it.next()?.to_string(),
        argv: it.next()?.to_string(),
    })
}

// ---------------------------------------------------------------------------
// Rendering.
// ---------------------------------------------------------------------------

fn basename(path: &str) -> String {
    Path::new(path).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| path.to_string())
}

fn hms(ts: i64) -> String {
    let t = ts as libc::time_t;
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    if unsafe { libc::localtime_r(&t, &mut tm) }.is_null() {
        return "--:--:--".to_string();
    }
    format!("{:02}:{:02}:{:02}", tm.tm_hour, tm.tm_min, tm.tm_sec)
}

fn fmt_ago(secs: i64) -> String {
    let s = secs.max(0);
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

fn tile(buf: &mut Buffer, x: u16, y: u16, w: u16, h: u16, title: &str, c: &Colors) -> (u16, u16, u16, u16) {
    if w < 2 || h < 2 {
        return (x, y, 0, 0);
    }
    let (x1, y1) = (x + w - 1, y + h - 1);
    let bs = c.border;
    set_cell(buf, x, y, "┌", bs);
    set_cell(buf, x1, y, "┐", bs);
    set_cell(buf, x, y1, "└", bs);
    set_cell(buf, x1, y1, "┘", bs);
    for cx in x + 1..x1 {
        set_cell(buf, cx, y, "─", bs);
        set_cell(buf, cx, y1, "─", bs);
    }
    for cy in y + 1..y1 {
        set_cell(buf, x, cy, "│", bs);
        set_cell(buf, x1, cy, "│", bs);
    }
    set_str(buf, x + 2, y, &format!(" {title} "), c.title, w.saturating_sub(4));
    (x + 2, y + 1, w.saturating_sub(4), h.saturating_sub(2))
}

fn render(buf: &mut Buffer, d: &Dash) {
    let c = &d.colors;
    let area = buf.area();
    let (cols, rows) = (area.width, area.height);
    for y in area.y..area.y + rows {
        fill_row(buf, y, c.row);
    }
    if cols < 40 || rows < 12 {
        set_str(buf, 0, 0, "terminal too small for the dashboard", c.error, cols);
        return;
    }

    let t = &d.totals;
    set_str(buf, 0, 0, "zvcs dashboard", c.title, 16);
    set_str(buf, 16, 0, &format!("— {} repos ({} cached)", t.repos, t.cached), c.label, cols.saturating_sub(16));
    let daemon = if d.daemon_up { "daemon up" } else { "daemon down" };
    let dtxt = format!("{daemon}  {}", hms(crate::date::now_seconds()));
    set_str(buf, cols.saturating_sub(dtxt.len() as u16 + 1), 0, &dtxt, if d.daemon_up { c.ok } else { c.error }, dtxt.len() as u16);

    let counts = [
        ("dirty", t.dirty, c.warn),
        ("ahead", t.ahead, c.ok),
        ("behind", t.behind, c.behind),
        ("diverged", t.diverged, c.diverged),
        ("detached", t.detached, c.error),
        ("clean", t.clean, c.dim),
    ];
    let mut x = 0u16;
    for (label, n, style) in counts {
        let seg = format!("{label} ");
        set_str(buf, x, 1, &seg, c.label, cols.saturating_sub(x));
        x += seg.len() as u16;
        let num = format!("{n}  ");
        set_str(buf, x, 1, &num, style, cols.saturating_sub(x));
        x += num.len() as u16;
    }
    let top_seg = match t.top_ppid {
        Some((ppid, n)) if n > 0 => format!("  top ppid {ppid} ({n}c)"),
        _ => String::new(),
    };
    set_str(buf, 0, 2, &format!("claims {}  sessions {}  queue {} active  procs {}{}   theme: {}", t.claims, t.sessions, t.queue, t.procs, top_seg, if d.mono { "Monochrome" } else { &d.label }), c.label, cols);

    let foot = rows - 1;
    // The tile rectangles come from the shared layout so the mouse handler hit-tests
    // exactly what is drawn. The fleet sits on top of the per-process tile in the
    // left column; events over commands on the right.
    let Some(l) = layout(cols, rows, d.splits) else { return };
    render_fleet(buf, l.fleet, d);
    render_procs(buf, l.procs, d);
    render_events(buf, l.events, d);
    render_commands(buf, l.commands, d);

    fill_row(buf, foot, c.bar);
    set_str(buf, 0, foot, " q Quit  ↑↓ Move  Tab Tile  drag/HJKL Resize (= reset)  right-click Info  c Colors  F1 Help ", c.bar, cols);

    match &d.overlay {
        Overlay::None => {}
        Overlay::Help => render_help(buf),
        Overlay::Chooser(cur) => render_theme_chooser(buf, *cur),
        Overlay::Editor(chan) => render_theme_editor(buf, d.pal, *chan),
    }
    // The right-click detail popup sits above the tiles but below theme overlays.
    if matches!(d.overlay, Overlay::None) {
        if let Some(tt) = &d.tooltip {
            render_tooltip(buf, tt, cols, rows, c);
        }
    }
}

/// A tile title with a `▸` marker when it is focused and a `[sel/len]` position, so
/// the cursor's tile and place in the list are always visible.
fn tile_title(base: &str, t: usize, d: &Dash) -> String {
    let mark = if d.focus == t { "▸ " } else { "" };
    let len = d.tile_len(t);
    if len == 0 {
        format!("{mark}{base}")
    } else {
        format!("{mark}{base}  [{}/{}]", d.view[t].sel + 1, len)
    }
}

/// Paint a solid background across a row's cells, keeping the glyphs/foregrounds
/// already drawn — the cursor highlight (ported from iftoprs's `select_bg` overlay).
fn highlight_row(buf: &mut Buffer, x: u16, y: u16, w: u16, bg: ratatui::style::Color) {
    let a = *buf.area();
    for cx in x..x + w {
        if cx < a.x + a.width && y < a.y + a.height {
            buf[(cx, y)].set_bg(bg);
        }
    }
}

fn render_fleet(buf: &mut Buffer, r: Rect, d: &Dash) {
    let c = &d.colors;
    let v = d.view[FLEET];
    let (ix, iy, iw, ih) = tile(buf, r.x, r.y, r.w, r.h, &tile_title("FLEET · most active", FLEET, d), c);
    let top = v.top.min(d.fleet.len());
    let end = (top + ih as usize).min(d.fleet.len());
    for (i, row) in d.fleet[top..end].iter().enumerate() {
        let cy = iy + i as u16;
        let (word, style) = state_of(row, c);
        let namew = iw.saturating_sub(20);
        set_str(buf, ix, cy, &basename(&row.path), c.row, namew);
        set_str(buf, ix + namew + 1, cy, word, style, 10);
        set_str(buf, ix + namew + 12, cy, &fmt_ago(row.age_secs), c.dim, 6);
        if d.focus == FLEET && top + i == v.sel {
            highlight_row(buf, ix, cy, iw, c.hl_bg);
        }
    }
}

fn render_procs(buf: &mut Buffer, r: Rect, d: &Dash) {
    let c = &d.colors;
    let v = d.view[PROCS];
    let (ix, iy, iw, ih) = tile(buf, r.x, r.y, r.w, r.h, &tile_title("PROCESSES · commits by ppid", PROCS, d), c);
    if d.procs.is_empty() {
        set_str(buf, ix, iy, "no commits recorded yet", c.dim, iw);
        return;
    }
    // pid | cmd | commits | live/dead | age, with the cwd basename filling whatever
    // is left. commits/state/age are anchored from the right edge so they never clip;
    // pid+cmd+cwd share the rest. set_str clips each field to its width.
    let top = v.top.min(d.procs.len());
    let end = (top + ih as usize).min(d.procs.len());
    for (i, p) in d.procs[top..end].iter().enumerate() {
        let cy = iy + i as u16;
        let (state, sstyle) = if p.alive { ("live", c.ok) } else { ("dead", c.dim) };
        set_str(buf, ix, cy, &p.ppid.to_string(), c.cyan, 8);
        set_str(buf, ix + 9, cy, &p.cmd, c.magenta, 14);
        let restx = ix + 24;
        let restw = iw.saturating_sub(24 + 18);
        set_str(buf, restx, cy, &basename(&crate::superset::zppid::tilde(&p.cwd)), c.dim, restw);
        set_str(buf, ix + iw.saturating_sub(18), cy, &format!("{}c", p.commits), c.green, 6);
        set_str(buf, ix + iw.saturating_sub(11), cy, state, sstyle, 5);
        set_str(buf, ix + iw.saturating_sub(5), cy, &fmt_ago(p.age_secs), c.dim, 5);
        if d.focus == PROCS && top + i == v.sel {
            highlight_row(buf, ix, cy, iw, c.hl_bg);
        }
    }
}

fn state_of(row: &FleetRow, c: &Colors) -> (&'static str, Style) {
    if row.detached {
        ("detached", c.error)
    } else if row.sync == "diverged" {
        ("diverged", c.diverged)
    } else if row.dirty {
        ("dirty", c.warn)
    } else if row.sync == "behind" {
        ("behind", c.behind)
    } else if row.sync == "ahead" {
        ("ahead", c.ok)
    } else {
        ("clean", c.dim)
    }
}

fn render_events(buf: &mut Buffer, r: Rect, d: &Dash) {
    let c = &d.colors;
    let v = d.view[EVENTS];
    let (ix, iy, iw, ih) = tile(buf, r.x, r.y, r.w, r.h, &tile_title("EVENTS · commits · reconciles · status", EVENTS, d), c);
    let top = v.top.min(d.events.len());
    let end = (top + ih as usize).min(d.events.len());
    for (i, e) in d.events[top..end].iter().enumerate() {
        let cy = iy + i as u16;
        let (glyph, style) = match e.kind.as_str() {
            "commit" => ("●", c.green),
            "reconcile" => ("↻", c.magenta),
            "status" => ("◑", c.warn),
            "stage" => ("+", c.blue),
            _ => ("•", c.dim),
        };
        set_str(buf, ix, cy, &hms(e.ts), c.dim, 8);
        set_cell(buf, ix + 9, cy, glyph, style);
        set_str(buf, ix + 11, cy, &event_repo(e), c.cyan, 18);
        set_str(buf, ix + 30, cy, &e.detail.clone().unwrap_or_default(), c.row, iw.saturating_sub(30));
        if d.focus == EVENTS && top + i == v.sel {
            highlight_row(buf, ix, cy, iw, c.hl_bg);
        }
    }
}

fn event_repo(e: &crate::db::EventRow) -> String {
    let src = e.workdir.clone().or_else(|| e.git_dir.clone()).unwrap_or_default();
    basename(&src).trim_end_matches(".git").to_string()
}

fn render_commands(buf: &mut Buffer, r: Rect, d: &Dash) {
    let c = &d.colors;
    let v = d.view[COMMANDS];
    let (ix, iy, iw, ih) = tile(buf, r.x, r.y, r.w, r.h, &tile_title("COMMANDS · every git across the fleet", COMMANDS, d), c);
    if d.commands.is_empty() {
        set_str(buf, ix, iy, "run `git zcommands` once to enable command logging", c.dim, iw);
        return;
    }
    let top = v.top.min(d.commands.len());
    let end = (top + ih as usize).min(d.commands.len());
    for (i, cmd) in d.commands[top..end].iter().enumerate() {
        let cy = iy + i as u16;
        set_str(buf, ix, cy, &hms(cmd.ts), c.dim, 8);
        set_str(buf, ix + 9, cy, &format!("{}\u{2190}{}", cmd.pid, cmd.ppid), c.dim, 13);
        set_str(buf, ix + 23, cy, &basename(&cmd.cwd), c.cyan, 14);
        set_str(buf, ix + 38, cy, "git", c.green, 3);
        set_str(buf, ix + 42, cy, &cmd.argv, c.row, iw.saturating_sub(42));
        if d.focus == COMMANDS && top + i == v.sel {
            highlight_row(buf, ix, cy, iw, c.hl_bg);
        }
    }
}

/// Draw the right-click detail popup: a bordered box of `tt.lines` placed near the
/// click, clamped so it stays fully on screen.
fn render_tooltip(buf: &mut Buffer, tt: &Tooltip, cols: u16, rows: u16, c: &Colors) {
    if tt.lines.is_empty() {
        return;
    }
    let inner_w = tt.lines.iter().map(|l| l.chars().count()).max().unwrap_or(0).min(cols as usize - 4) as u16;
    let bw = inner_w + 2; // + borders
    let bh = tt.lines.len() as u16 + 2;
    // Prefer just below-right of the click, clamped into the screen.
    let bx = tt.col.min(cols.saturating_sub(bw));
    let by = (tt.row + 1).min(rows.saturating_sub(bh));
    let (x1, y1) = (bx + bw - 1, by + bh - 1);
    // Every cell of the popup carries the opaque `popup_bg`, so nothing shows through.
    let bg = c.popup_bg;
    let border = c.title.bg(bg);
    let text = c.cyan.bg(bg);
    // Fill the whole box solid first (opaque backing).
    for yy in by..=y1 {
        for xx in bx..=x1 {
            set_cell(buf, xx, yy, " ", text);
        }
    }
    set_cell(buf, bx, by, "┌", border);
    set_cell(buf, x1, by, "┐", border);
    set_cell(buf, bx, y1, "└", border);
    set_cell(buf, x1, y1, "┘", border);
    for cx in bx + 1..x1 {
        set_cell(buf, cx, by, "─", border);
        set_cell(buf, cx, y1, "─", border);
    }
    for (i, line) in tt.lines.iter().enumerate() {
        let cy = by + 1 + i as u16;
        set_cell(buf, bx, cy, "│", border);
        set_cell(buf, x1, cy, "│", border);
        set_str(buf, bx + 1, cy, line, text, inner_w);
    }
}

// ---------------------------------------------------------------------------
// Lifecycle + input.
// ---------------------------------------------------------------------------

struct TermGuard;
impl Drop for TermGuard {
    fn drop(&mut self) {
        let _ = disable_raw_mode();
        let _ = execute!(stdout(), DisableMouseCapture, LeaveAlternateScreen, cursor::Show);
    }
}

pub fn run() -> Result<ExitCode> {
    enable_raw_mode()?;
    execute!(stdout(), EnterAlternateScreen, EnableMouseCapture, cursor::Hide)?;
    let _guard = TermGuard;
    let mut term = Terminal::new(CrosstermBackend::new(stdout()))?;
    let mut dash = Dash::new();
    loop {
        let (cols, rows) = ratatui::crossterm::terminal::size().unwrap_or((80, 24));
        dash.refresh(rows.saturating_sub(6) as usize);
        // Reconcile the cursor/scroll against the new data (needs the tile heights).
        if let Some(l) = layout(cols, rows, dash.splits) {
            dash.reconcile_views(&l);
        }
        term.draw(|f| render(f.buffer_mut(), &dash))?;

        let deadline = Instant::now() + POLL;
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() || !event::poll(remaining)? {
                break;
            }
            match event::read()? {
                Event::Key(k) => {
                    if k.kind != KeyEventKind::Press {
                        continue;
                    }
                    if handle_key(&mut dash, k.code, k.modifiers) {
                        return Ok(ExitCode::SUCCESS);
                    }
                    break; // redraw immediately after a handled key
                }
                Event::Mouse(m) => {
                    let (cols, rows) =
                        ratatui::crossterm::terminal::size().unwrap_or((80, 24));
                    // Only redraw when the event changed something — hover `Moved`
                    // events flood in and must not trigger a redraw storm.
                    if handle_mouse(&mut dash, m, cols, rows) {
                        break;
                    }
                }
                _ => {}
            }
        }
    }
}

/// Route one mouse event: scroll the tile under the cursor, or (right-click) open a
/// detail tooltip for the row there. Ported from iftoprs's `handle_mouse` — any
/// click dismisses an open tooltip; while a theme overlay is up the scroll wheel
/// cycles it, exactly as the keyboard would. Returns `true` when the view changed
/// and needs a redraw, so hover `Moved` events (which return `false`) don't churn.
fn handle_mouse(d: &mut Dash, m: MouseEvent, cols: u16, rows: u16) -> bool {
    let dismissed = matches!(m.kind, MouseEventKind::Down(_)) && d.tooltip.take().is_some();

    if let Overlay::Chooser(cur) = d.overlay {
        let n = THEMES.len();
        match m.kind {
            MouseEventKind::ScrollDown => {
                let c = (cur + 1) % n;
                d.overlay = Overlay::Chooser(c);
                d.set_palette(THEMES[c].palette(), THEMES[c].display_name());
            }
            MouseEventKind::ScrollUp => {
                let c = (cur + n - 1) % n;
                d.overlay = Overlay::Chooser(c);
                d.set_palette(THEMES[c].palette(), THEMES[c].display_name());
            }
            MouseEventKind::Down(MouseButton::Left) => {
                d.overlay = Overlay::None;
                d.restore = None;
                save_palette(&d.label, d.pal);
            }
            MouseEventKind::Down(_) => {
                d.overlay = Overlay::None;
                d.revert_theme();
            }
            _ => return false,
        }
        return true;
    }
    if !matches!(d.overlay, Overlay::None) {
        return dismissed; // help/editor: keyboard-driven
    }

    let Some(l) = layout(cols, rows, d.splits) else { return dismissed };
    let changed = match m.kind {
        // Wheel moves the cursor in the tile under the pointer (iftoprs select_next/prev).
        MouseEventKind::ScrollDown => scroll_select(d, &l, m.column, m.row, 1),
        MouseEventKind::ScrollUp => scroll_select(d, &l, m.column, m.row, -1),
        // Left press on a divider begins a resize drag; otherwise it moves the cursor.
        MouseEventKind::Down(MouseButton::Left) => {
            if let Some(dv) = divider_at(&l, m.column, m.row) {
                d.drag = Some(dv);
                false
            } else {
                point_select(d, &l, m.column, m.row)
            }
        }
        // A left drag moves the grabbed divider (following the cursor absolutely); with
        // no divider grabbed it extends the selection to the row under the pointer.
        MouseEventKind::Drag(MouseButton::Left) => {
            if let Some((div, off)) = d.drag {
                d.apply_drag(&l, div, off, m.column, m.row, cols);
                true
            } else {
                point_select(d, &l, m.column, m.row)
            }
        }
        // Releasing ends any divider drag.
        MouseEventKind::Up(_) => d.drag.take().is_some(),
        // Bare hover must NOT move the cursor (matches iftoprs, which only tracks a
        // hover position and never disturbs the selection). It just dismisses an open
        // tooltip — redraw only if one was actually up, so idle hover doesn't churn.
        MouseEventKind::Moved => d.tooltip.take().is_some(),
        MouseEventKind::Down(MouseButton::Right) => {
            point_select(d, &l, m.column, m.row);
            open_tooltip(d, &l, m.column, m.row);
            true
        }
        _ => false,
    };
    changed || dismissed
}

/// Move focus to the tile the pointer is over and set its cursor to the row under
/// the pointer, keeping it in view. Returns whether focus or selection changed (so a
/// click on the current row causes no redraw). Used by left-click and right-click.
fn point_select(d: &mut Dash, l: &Layout, col: u16, row: u16) -> bool {
    let Some(t) = tile_at(l, col, row) else { return false };
    let (_, iy, _, ih) = tile_rect(l, t).inner();
    let len = d.tile_len(t);
    // Over the border/title, or an empty tile → just focus it.
    if len == 0 || !(row >= iy && row < iy + ih) {
        let changed = d.focus != t;
        d.focus = t;
        return changed;
    }
    let idx = (d.view[t].top + (row - iy) as usize).min(len - 1);
    let changed = d.focus != t || d.view[t].sel != idx;
    d.focus = t;
    d.view[t].sel = idx;
    if t == EVENTS || t == COMMANDS {
        d.follow[t] = idx + 1 >= len;
    }
    changed
}

/// Focus the tile under the pointer and move its cursor by `delta` (wheel).
fn scroll_select(d: &mut Dash, l: &Layout, col: u16, row: u16, delta: i32) -> bool {
    let Some(t) = tile_at(l, col, row) else { return false };
    d.focus = t;
    let ih = tile_rect(l, t).inner().3 as usize;
    d.move_sel(delta, ih);
    true
}

/// Open a detail tooltip for the row the pointer is over, mapping the click back to
/// the record through the same scroll top the renderer used.
fn open_tooltip(d: &mut Dash, l: &Layout, col: u16, row: u16) {
    let Some(t) = tile_at(l, col, row) else { return };
    let (_, iy, _, ih) = tile_rect(l, t).inner();
    if !(row >= iy && row < iy + ih) {
        return;
    }
    let idx = d.view[t].top + (row - iy) as usize;
    let lines = match t {
        FLEET => d.fleet.get(idx).map(|r| {
            vec![
                format!("repo:  {}", crate::superset::zppid::tilde(&r.path)),
                format!("head:  {}   [{}]", r.head, state_of(r, &d.colors).0),
                format!("sync:  {}   {} ago", if r.sync.is_empty() { "—" } else { &r.sync }, fmt_ago(r.age_secs)),
                format!("git:   {}", crate::superset::zppid::tilde(&r.git_dir)),
            ]
        }),
        PROCS => d.procs.get(idx).map(|p| {
            vec![
                format!("pid:      {}   [{}]", p.ppid, if p.alive { "live" } else { "dead" }),
                format!("command:  {}", p.cmd),
                format!("commits:  {}   {} ago", p.commits, fmt_ago(p.age_secs)),
                format!("cwd:      {}", crate::superset::zppid::tilde(&p.cwd)),
            ]
        }),
        EVENTS => d.events.get(idx).map(|e| {
            vec![
                format!("{}  {}  {}", hms(e.ts), e.kind, event_repo(e)),
                format!("detail: {}", e.detail.clone().unwrap_or_else(|| "—".into())),
                format!("{}..{}", short_sha(&e.sha_before), short_sha(&e.sha_after)),
            ]
        }),
        _ => d.commands.get(idx).map(|cmd| {
            vec![
                format!("{}  pid {}\u{2190}{}", hms(cmd.ts), cmd.pid, cmd.ppid),
                format!("cwd:  {}", crate::superset::zppid::tilde(&cmd.cwd)),
                format!("git {}", cmd.argv),
            ]
        }),
    };
    if let Some(lines) = lines {
        d.tooltip = Some(Tooltip { lines, col, row });
    }
}

/// Short SHA (12) for tooltips; a placeholder for a null/absent id.
fn short_sha(s: &Option<String>) -> String {
    match s {
        Some(s) if s.len() >= 12 => s[..12].to_string(),
        Some(s) if !s.is_empty() => s.clone(),
        _ => "—".into(),
    }
}

/// Handle one key. Returns true to quit. Theme overlays (chooser/editor/help)
/// mirror ztop's controls so the two feel identical.
fn handle_key(d: &mut Dash, code: KeyCode, mods: KeyModifiers) -> bool {
    if code == KeyCode::Char('c') && mods.contains(KeyModifiers::CONTROL) {
        return true;
    }
    match d.overlay {
        Overlay::Help => {
            d.overlay = Overlay::None;
            return false;
        }
        Overlay::Chooser(cur) => {
            let n = THEMES.len();
            match code {
                KeyCode::Up | KeyCode::Char('k') => {
                    let c = (cur + n - 1) % n;
                    d.overlay = Overlay::Chooser(c);
                    d.set_palette(THEMES[c].palette(), THEMES[c].display_name());
                }
                KeyCode::Down | KeyCode::Char('j') => {
                    let c = (cur + 1) % n;
                    d.overlay = Overlay::Chooser(c);
                    d.set_palette(THEMES[c].palette(), THEMES[c].display_name());
                }
                KeyCode::Enter => {
                    d.overlay = Overlay::None;
                    d.restore = None;
                    save_palette(&d.label, d.pal);
                }
                KeyCode::Esc | KeyCode::Char('q') => {
                    d.overlay = Overlay::None;
                    d.revert_theme();
                }
                _ => {}
            }
            return false;
        }
        Overlay::Editor(chan) => {
            match code {
                KeyCode::Up | KeyCode::Char('k') => d.overlay = Overlay::Editor(chan.saturating_sub(1)),
                KeyCode::Down | KeyCode::Char('j') => d.overlay = Overlay::Editor((chan + 1).min(5)),
                KeyCode::Left | KeyCode::Char('-') => {
                    let mut p = d.pal;
                    p[chan] = p[chan].wrapping_sub(1);
                    d.set_palette(p, "Custom");
                }
                KeyCode::Right | KeyCode::Char('+') | KeyCode::Char('=') => {
                    let mut p = d.pal;
                    p[chan] = p[chan].wrapping_add(1);
                    d.set_palette(p, "Custom");
                }
                KeyCode::Char('s') | KeyCode::Enter => {
                    d.overlay = Overlay::None;
                    d.restore = None;
                    save_palette("Custom", d.pal);
                }
                KeyCode::Esc => {
                    d.overlay = Overlay::None;
                    d.revert_theme();
                }
                _ => {}
            }
            return false;
        }
        Overlay::None => {}
    }

    let ih = focus_ih(d);
    match (code, mods) {
        (KeyCode::Char('q'), _) | (KeyCode::Esc, _) => return true,
        (KeyCode::F(1), _) | (KeyCode::Char('?'), _) => d.overlay = Overlay::Help,
        (KeyCode::F(2), _) | (KeyCode::Char('c'), _) => {
            d.snapshot_theme();
            let cur = ThemeName::from_name(&d.label).map(|t| t.index()).unwrap_or(0);
            d.overlay = Overlay::Chooser(cur);
        }
        (KeyCode::Char('~'), _) | (KeyCode::Char('e'), _) => {
            d.snapshot_theme();
            d.overlay = Overlay::Editor(0);
        }
        // Move the cursor within the focused tile.
        (KeyCode::Down, _) | (KeyCode::Char('j'), _) => d.move_sel(1, ih),
        (KeyCode::Up, _) | (KeyCode::Char('k'), _) => d.move_sel(-1, ih),
        (KeyCode::PageDown, _) | (KeyCode::Char('d'), KeyModifiers::CONTROL) => {
            d.move_sel(ih as i32, ih)
        }
        (KeyCode::PageUp, _) | (KeyCode::Char('u'), KeyModifiers::CONTROL) => {
            d.move_sel(-(ih as i32), ih)
        }
        (KeyCode::Home, _) | (KeyCode::Char('g'), _) => d.move_sel(i32::MIN / 2, ih),
        (KeyCode::End, _) | (KeyCode::Char('G'), _) => d.move_sel(i32::MAX / 2, ih),
        // Switch which tile the cursor is in.
        (KeyCode::Tab, _) => d.focus = (d.focus + 1) % 4,
        (KeyCode::BackTab, _) => d.focus = (d.focus + 3) % 4,
        (KeyCode::Left, _) | (KeyCode::Right, _) => d.focus ^= 2, // toggle column
        // Resize panes (Shift+H/J/K/L). H/L move the vertical column divider; J/K
        // move the focused column's horizontal divider. `=` resets the layout.
        (KeyCode::Char('L'), _) => resize_panes(d, 2, 0),
        (KeyCode::Char('H'), _) => resize_panes(d, -2, 0),
        (KeyCode::Char('J'), _) => resize_panes(d, 0, 1),
        (KeyCode::Char('K'), _) => resize_panes(d, 0, -1),
        (KeyCode::Char('='), _) => d.splits = Splits::default(),
        _ => {}
    }
    false
}

/// Resize the panes by moving a divider, ported from zmax's delta-resize model
/// (`resize_horizontal`/`resize_vertical`): `dx` shifts the vertical column divider
/// (+ widens the left column), `dy` shifts the *focused* column's horizontal divider
/// (+ grows its top pane). Each delta is folded into the stored offset and re-clamped
/// against the live terminal size so a pane never drops below `MIN_W × MIN_H`.
fn resize_panes(d: &mut Dash, dx: i16, dy: i16) {
    let (cols, rows) = ratatui::crossterm::terminal::size().unwrap_or((80, 24));
    if cols < 40 || rows < 12 {
        return;
    }
    let body_h = (rows - 1).saturating_sub(3);
    let (def_lw, def_fh, def_eh) = split_defaults(cols, body_h);
    if dx != 0 {
        d.splits.col = clamp_delta(def_lw, d.splits.col, dx, MIN_W, cols - MIN_W);
    }
    if dy != 0 {
        let left = d.focus == FLEET || d.focus == PROCS;
        let (default, slot) = if left {
            (def_fh, &mut d.splits.lrow)
        } else {
            (def_eh, &mut d.splits.rrow)
        };
        *slot = clamp_delta(default, *slot, dy, MIN_H, body_h - MIN_H);
    }
}

/// Fold `delta` into `cur_delta` (an offset from `default`) and clamp the resulting
/// absolute split position to `[min, max]`, returning the new offset.
fn clamp_delta(default: u16, cur_delta: i16, delta: i16, min: u16, max: u16) -> i16 {
    let want = default as i32 + cur_delta as i32 + delta as i32;
    (want.clamp(min as i32, max as i32) - default as i32) as i16
}

/// The inner (content) height of the currently-focused tile, read from the live
/// terminal size — the step size for keyboard cursor moves.
fn focus_ih(d: &Dash) -> usize {
    let (cols, rows) = ratatui::crossterm::terminal::size().unwrap_or((80, 24));
    layout(cols, rows, d.splits).map(|l| tile_rect(&l, d.focus).inner().3 as usize).unwrap_or(10)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn command_line_parses_five_fields() {
        let c = parse_cmd("1700000000\t42\t7\t/x/repo\tpush origin main").unwrap();
        assert_eq!(c.ts, 1700000000);
        assert_eq!(c.pid, 42);
        assert_eq!(c.ppid, 7);
        assert_eq!(c.cwd, "/x/repo");
        assert_eq!(c.argv, "push origin main");
        assert!(parse_cmd("junk").is_none());
    }

    #[test]
    fn basename_and_state() {
        assert_eq!(basename("/a/b/zpwr"), "zpwr");
        let c = Colors::mono();
        let detached = FleetRow { path: "/x".into(), git_dir: "/x/.git".into(), head: "h".into(), dirty: false, detached: true, sync: "up-to-date".into(), age_secs: 0 };
        assert_eq!(state_of(&detached, &c).0, "detached");
        let dirty = FleetRow { path: "/y".into(), git_dir: "/y/.git".into(), head: "h".into(), dirty: true, detached: false, sync: "up-to-date".into(), age_secs: 0 };
        assert_eq!(state_of(&dirty, &c).0, "dirty");
    }

    #[test]
    fn layout_tiles_cover_the_body_without_overlap() {
        let l = layout(120, 40, Splits::default()).expect("layout for a normal terminal");
        // Left column = fleet stacked on procs; right column = events over commands.
        assert_eq!(l.fleet.x, 0);
        assert_eq!(l.procs.x, 0);
        assert_eq!(l.events.x, l.fleet.w);
        assert_eq!(l.procs.y, l.fleet.y + l.fleet.h); // procs directly below fleet
        assert_eq!(l.commands.y, l.events.y + l.events.h); // commands below events
        // Too-small terminal has no layout (matches render's guard).
        assert!(layout(20, 8, Splits::default()).is_none());
    }

    #[test]
    fn rect_contains_is_half_open() {
        let r = Rect { x: 10, y: 5, w: 4, h: 3 };
        assert!(r.contains(10, 5)); // top-left corner
        assert!(r.contains(13, 7)); // bottom-right interior
        assert!(!r.contains(14, 5)); // one past the right edge
        assert!(!r.contains(10, 8)); // one past the bottom edge
    }

    #[test]
    fn move_sel_scrolls_and_clamps() {
        let mut d = Dash::new();
        d.focus = FLEET;
        // 25 fleet rows, a 10-row tile.
        d.fleet = (0..25)
            .map(|i| FleetRow {
                path: format!("/r{i}"),
                git_dir: format!("/r{i}/.git"),
                head: "h".into(),
                dirty: false,
                detached: false,
                sync: String::new(),
                age_secs: 0,
            })
            .collect();
        d.move_sel(1, 10);
        assert_eq!(d.view[FLEET].sel, 1);
        assert_eq!(d.view[FLEET].top, 0); // still in view
        d.move_sel(20, 10);
        assert_eq!(d.view[FLEET].sel, 21);
        assert_eq!(d.view[FLEET].top, 12); // scrolled to keep the cursor visible
        d.move_sel(100, 10);
        assert_eq!(d.view[FLEET].sel, 24); // clamped to the last row
        d.move_sel(-100, 10);
        assert_eq!(d.view[FLEET].sel, 0); // clamped to the first row
        assert_eq!(d.view[FLEET].top, 0);
    }

    #[test]
    fn render_paints_cursor_highlight_and_opaque_tooltip() {
        let area = ratatui::layout::Rect::new(0, 0, 120, 40);
        let l = layout(120, 40, Splits::default()).unwrap();

        // Selected fleet row must carry the highlight background.
        let mut d = Dash::new();
        d.fleet = (0..6)
            .map(|i| FleetRow {
                path: format!("/repo{i}"),
                git_dir: format!("/repo{i}/.git"),
                head: "main".into(),
                dirty: false,
                detached: false,
                sync: "up-to-date".into(),
                age_secs: 0,
            })
            .collect();
        d.focus = FLEET;
        d.view[FLEET].sel = 2;
        d.view[FLEET].top = 0;
        let mut buf = Buffer::empty(area);
        render(&mut buf, &d);
        let (ix, iy, _, _) = l.fleet.inner();
        let sel_y = iy + 2;
        assert_eq!(
            buf[(ix, sel_y)].style().bg,
            Some(d.colors.hl_bg),
            "the selected row must be highlighted (cursor)"
        );
        assert_ne!(
            buf[(ix, iy)].style().bg,
            Some(d.colors.hl_bg),
            "a non-selected row must not be highlighted"
        );

        // A tooltip must paint an opaque background — no cell in its box left transparent.
        let mut d2 = Dash::new();
        d2.tooltip = Some(Tooltip { lines: vec!["repo: /x/y".into(), "head: main".into()], col: 20, row: 15 });
        let mut buf2 = Buffer::empty(area);
        render(&mut buf2, &d2);
        let opaque = (0..40u16).any(|y| (0..120u16).any(|x| buf2[(x, y)].style().bg == Some(d2.colors.popup_bg)));
        assert!(opaque, "the right-click tooltip must have a solid (opaque) background");
    }

    #[test]
    fn resize_moves_dividers_and_respects_minimums() {
        let base = layout(120, 40, Splits::default()).unwrap();
        // A positive col delta widens the left column; the right column shifts right.
        let wider = layout(120, 40, Splits { col: 10, lrow: 0, rrow: 0 }).unwrap();
        assert_eq!(wider.fleet.w, base.fleet.w + 10);
        assert_eq!(wider.events.x, base.events.x + 10);
        // Extreme deltas clamp so neither side drops below MIN_W.
        let maxed = layout(120, 40, Splits { col: 10_000, lrow: 0, rrow: 0 }).unwrap();
        assert_eq!(maxed.fleet.w, 120 - MIN_W);
        assert_eq!(maxed.events.w, MIN_W);
        let mined = layout(120, 40, Splits { col: -10_000, lrow: 0, rrow: 0 }).unwrap();
        assert_eq!(mined.fleet.w, MIN_W);
        // Row divider: lrow grows the fleet pane and shrinks procs, staying adjacent.
        let taller = layout(120, 40, Splits { col: 0, lrow: 5, rrow: 0 }).unwrap();
        assert_eq!(taller.fleet.h, base.fleet.h + 5);
        assert_eq!(taller.procs.h, base.procs.h - 5);
        assert_eq!(taller.procs.y, taller.fleet.y + taller.fleet.h);
    }

    #[test]
    fn divider_hit_test_and_drag_resize() {
        let l = layout(120, 40, Splits::default()).unwrap();
        let left_w = l.fleet.w;
        let mid = l.fleet.y + 5;
        // The vertical column seam is grabbable at x in {left_w-1, left_w}.
        assert!(matches!(divider_at(&l, left_w, mid), Some((Divider::Col, _))));
        assert!(matches!(divider_at(&l, left_w - 1, mid), Some((Divider::Col, _))));
        // A cell inside a tile is not a divider; the fleet/procs seam is LeftRow.
        assert!(divider_at(&l, 5, mid).is_none());
        assert!(matches!(divider_at(&l, 5, l.procs.y), Some((Divider::LeftRow, _))));

        // Grab the column divider and drag it to x = 40 → left column becomes 40 wide.
        let mut d = Dash::new();
        let (div, off) = divider_at(&l, left_w, mid).unwrap();
        d.apply_drag(&l, div, off, 40, mid, 120);
        assert_eq!(layout(120, 40, d.splits).unwrap().fleet.w, 40);
        // Dragging past the edge clamps to the minimum pane width.
        d.apply_drag(&l, div, off, 500, mid, 120);
        assert_eq!(layout(120, 40, d.splits).unwrap().fleet.w, 120 - MIN_W);
    }

    #[test]
    fn clamp_delta_bounds_the_offset() {
        assert_eq!(clamp_delta(60, 0, 5, 12, 108), 5);
        assert_eq!(clamp_delta(60, 0, 1000, 12, 108), 48); // lands on max (108) → offset 48
        assert_eq!(clamp_delta(60, 0, -1000, 12, 108), -48); // lands on min (12) → offset -48
    }

    #[test]
    fn feed_follow_pins_to_the_newest_row() {
        let mut d = Dash::new();
        let l = layout(120, 40, Splits::default()).unwrap();
        d.events = (0..5).map(|i| crate::db::EventRow {
            id: i, ts: 0, kind: "commit".into(), git_dir: None, workdir: None,
            detail: None, sha_before: None, sha_after: None,
        }).collect();
        d.reconcile_views(&l);
        assert_eq!(d.view[EVENTS].sel, 4); // pinned to newest
        // Grow the feed → still pinned to the new newest.
        d.events.push(crate::db::EventRow {
            id: 5, ts: 0, kind: "commit".into(), git_dir: None, workdir: None,
            detail: None, sha_before: None, sha_after: None,
        });
        d.reconcile_views(&l);
        assert_eq!(d.view[EVENTS].sel, 5);
    }
}

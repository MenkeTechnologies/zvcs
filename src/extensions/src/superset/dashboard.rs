//! `git zdashboard` — a live, tiled TUI that combines the fleet monitor, the
//! semantic event feed, and the fleet command feed on one screen.
//!
//! Three tiles, refreshed together: a **fleet** table (every indexed repo, most
//! recently active first, read from the daemon status cache with the on-screen
//! rows' HEAD state live-refreshed so nothing is stale), the **events** feed
//! (`zevents`: commits / reconciles / status changes), and the **commands** feed
//! (`zcommands`: every git command run across the machine, with the agent that
//! ran it). A header row carries the aggregate totals. It shares ztop's theme
//! system: `c` opens the 31-scheme chooser, `~`/`e` the palette editor, F1 the
//! help overlay. The non-interactive path (`--once` / `--json` / a non-tty
//! stdout) keeps the instant text summary.

use std::io::{stdout, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
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

struct Dash {
    totals: Totals,
    fleet: Vec<FleetRow>,
    events: Vec<crate::db::EventRow>,
    commands: Vec<CmdRec>,
    daemon_up: bool,
    // Theme state (shared with ztop).
    pal: Palette,
    label: String,
    mono: bool,
    colors: Colors,
    overlay: Overlay,
    restore: Option<(Palette, String, bool)>,
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
            daemon_up: false,
            pal,
            label,
            mono,
            colors: Colors::from_theme(pal, mono),
            overlay: Overlay::None,
            restore: None,
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
    set_str(buf, 0, 2, &format!("claims {}  sessions {}  queue {} active   theme: {}", t.claims, t.sessions, t.queue, if d.mono { "Monochrome" } else { &d.label }), c.label, cols);

    let top = 3u16;
    let foot = rows - 1;
    let body_h = foot.saturating_sub(top);
    let left_w = cols / 2;
    let right_x = left_w;
    let right_w = cols - left_w;
    let ev_h = body_h / 2;
    let cmd_h = body_h - ev_h;

    render_fleet(buf, 0, top, left_w, body_h, d);
    render_events(buf, right_x, top, right_w, ev_h, d);
    render_commands(buf, right_x, top + ev_h, right_w, cmd_h, d);

    fill_row(buf, foot, c.bar);
    set_str(buf, 0, foot, " q Quit   c Colors   ~ Edit   F1 Help   live — fleet · events · commands ", c.bar, cols);

    match &d.overlay {
        Overlay::None => {}
        Overlay::Help => render_help(buf),
        Overlay::Chooser(cur) => render_theme_chooser(buf, *cur),
        Overlay::Editor(chan) => render_theme_editor(buf, d.pal, *chan),
    }
}

fn render_fleet(buf: &mut Buffer, x: u16, y: u16, w: u16, h: u16, d: &Dash) {
    let c = &d.colors;
    let (ix, iy, iw, ih) = tile(buf, x, y, w, h, "FLEET · most active", c);
    for (i, row) in d.fleet.iter().take(ih as usize).enumerate() {
        let cy = iy + i as u16;
        let (word, style) = state_of(row, c);
        let namew = iw.saturating_sub(20);
        set_str(buf, ix, cy, &basename(&row.path), c.row, namew);
        set_str(buf, ix + namew + 1, cy, word, style, 10);
        set_str(buf, ix + namew + 12, cy, &fmt_ago(row.age_secs), c.dim, 6);
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

fn render_events(buf: &mut Buffer, x: u16, y: u16, w: u16, h: u16, d: &Dash) {
    let c = &d.colors;
    let (ix, iy, iw, ih) = tile(buf, x, y, w, h, "EVENTS · commits · reconciles · status", c);
    let take = ih as usize;
    let start = d.events.len().saturating_sub(take);
    for (i, e) in d.events[start..].iter().enumerate() {
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
    }
}

fn event_repo(e: &crate::db::EventRow) -> String {
    let src = e.workdir.clone().or_else(|| e.git_dir.clone()).unwrap_or_default();
    basename(&src).trim_end_matches(".git").to_string()
}

fn render_commands(buf: &mut Buffer, x: u16, y: u16, w: u16, h: u16, d: &Dash) {
    let c = &d.colors;
    let (ix, iy, iw, ih) = tile(buf, x, y, w, h, "COMMANDS · every git across the fleet", c);
    if d.commands.is_empty() {
        set_str(buf, ix, iy, "run `git zcommands` once to enable command logging", c.dim, iw);
        return;
    }
    let take = ih as usize;
    let start = d.commands.len().saturating_sub(take);
    for (i, cmd) in d.commands[start..].iter().enumerate() {
        let cy = iy + i as u16;
        set_str(buf, ix, cy, &hms(cmd.ts), c.dim, 8);
        set_str(buf, ix + 9, cy, &format!("{}\u{2190}{}", cmd.pid, cmd.ppid), c.dim, 13);
        set_str(buf, ix + 23, cy, &basename(&cmd.cwd), c.cyan, 14);
        set_str(buf, ix + 38, cy, "git", c.green, 3);
        set_str(buf, ix + 42, cy, &cmd.argv, c.row, iw.saturating_sub(42));
    }
}

// ---------------------------------------------------------------------------
// Lifecycle + input.
// ---------------------------------------------------------------------------

struct TermGuard;
impl Drop for TermGuard {
    fn drop(&mut self) {
        let _ = disable_raw_mode();
        let _ = execute!(stdout(), LeaveAlternateScreen, cursor::Show);
    }
}

pub fn run() -> Result<ExitCode> {
    enable_raw_mode()?;
    execute!(stdout(), EnterAlternateScreen, cursor::Hide)?;
    let _guard = TermGuard;
    let mut term = Terminal::new(CrosstermBackend::new(stdout()))?;
    let mut dash = Dash::new();
    loop {
        let visible = ratatui::crossterm::terminal::size().map(|(_, h)| h.saturating_sub(6) as usize).unwrap_or(40);
        dash.refresh(visible);
        term.draw(|f| render(f.buffer_mut(), &dash))?;

        let deadline = Instant::now() + POLL;
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() || !event::poll(remaining)? {
                break;
            }
            if let Event::Key(k) = event::read()? {
                if k.kind != KeyEventKind::Press {
                    continue;
                }
                if handle_key(&mut dash, k.code, k.modifiers) {
                    return Ok(ExitCode::SUCCESS);
                }
                break; // redraw immediately after a handled key
            }
        }
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

    match (code, mods) {
        (KeyCode::Char('q'), _) | (KeyCode::Esc, _) => return true,
        (KeyCode::F(1), _) | (KeyCode::Char('h'), _) | (KeyCode::Char('?'), _) => d.overlay = Overlay::Help,
        (KeyCode::F(2), _) | (KeyCode::Char('c'), _) => {
            d.snapshot_theme();
            let cur = ThemeName::from_name(&d.label).map(|t| t.index()).unwrap_or(0);
            d.overlay = Overlay::Chooser(cur);
        }
        (KeyCode::Char('~'), _) | (KeyCode::Char('e'), _) => {
            d.snapshot_theme();
            d.overlay = Overlay::Editor(0);
        }
        _ => {}
    }
    false
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
}

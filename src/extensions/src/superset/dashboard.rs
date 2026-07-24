//! `git zdashboard` — a live, tiled TUI that combines the fleet monitor, the
//! semantic event feed, and the fleet command feed on one screen.
//!
//! Three tiles, refreshed together: a **fleet** table (every indexed repo, most
//! recently active first, read from the daemon status cache with the on-screen
//! rows' HEAD state live-refreshed so nothing is stale), the **events** feed
//! (`zevents`: commits / reconciles / status changes), and the **commands** feed
//! (`zcommands`: every git command run across the machine, with the agent that
//! ran it). A header row carries the aggregate totals. The non-interactive path
//! (`--once`, `--json`, or a non-tty stdout) keeps the old instant text summary.

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

use crate::superset::ztop::{fill_row, set_cell, set_str};

const POLL: Duration = Duration::from_millis(700);
const FEED: usize = 200; // rows pulled per feed before the tile clips them

/// A fixed palette (htop DEFAULT-flavored): cyan chrome, htop's semantic accents.
struct Palette {
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

impl Palette {
    fn new() -> Self {
        let b = |c: Color| Style::default().fg(c).add_modifier(Modifier::BOLD);
        Palette {
            title: b(Color::Cyan),
            border: Style::default().fg(Color::Cyan),
            label: Style::default().fg(Color::Cyan),
            row: Style::default().fg(Color::Gray),
            dim: Style::default().fg(Color::Indexed(240)),
            ok: Style::default().fg(Color::Green),
            warn: b(Color::Yellow),
            error: b(Color::Red),
            behind: Style::default().fg(Color::Red),
            diverged: b(Color::Magenta),
            bar: Style::default().fg(Color::Black).bg(Color::Cyan),
            cyan: Style::default().fg(Color::Cyan),
            green: Style::default().fg(Color::Green),
            magenta: Style::default().fg(Color::Magenta),
            blue: Style::default().fg(Color::Blue),
        }
    }
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
}

impl Dash {
    fn empty() -> Self {
        Dash { totals: Totals::default(), fleet: Vec::new(), events: Vec::new(), commands: Vec::new(), daemon_up: false }
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
                            continue; // skip malformed index entries
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

        // Live-refresh the on-screen fleet rows' HEAD state (kills stale detached).
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

/// The last `n` command-log records, oldest-first.
fn read_commands(n: usize) -> Vec<CmdRec> {
    let path = commands_log();
    let Ok(len) = std::fs::metadata(&path).map(|m| m.len()) else { return Vec::new() };
    let Ok(mut f) = std::fs::File::open(&path) else { return Vec::new() };
    // Read only the tail (256 bytes per line is plenty).
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

/// Draw a bordered tile at `(x,y)` of size `w×h` with a title; return the inner
/// `(x, y, w, h)` content rectangle.
fn tile(buf: &mut Buffer, x: u16, y: u16, w: u16, h: u16, title: &str, p: &Palette) -> (u16, u16, u16, u16) {
    if w < 2 || h < 2 {
        return (x, y, 0, 0);
    }
    let (x1, y1) = (x + w - 1, y + h - 1);
    let bs = p.border;
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
    set_str(buf, x + 2, y, &format!(" {title} "), p.title, w.saturating_sub(4));
    (x + 2, y + 1, w.saturating_sub(4), h.saturating_sub(2))
}

fn render(buf: &mut Buffer, d: &Dash, p: &Palette) {
    let area = buf.area();
    let (cols, rows) = (area.width, area.height);
    for y in area.y..area.y + rows {
        fill_row(buf, y, p.row);
    }
    if cols < 40 || rows < 12 {
        set_str(buf, 0, 0, "terminal too small for the dashboard", p.error, cols);
        return;
    }

    // Header (rows 0..3).
    let t = &d.totals;
    set_str(buf, 0, 0, "zvcs dashboard", p.title, 16);
    set_str(buf, 16, 0, &format!("— {} repos ({} cached)", t.repos, t.cached), p.label, cols.saturating_sub(16));
    let daemon = if d.daemon_up { "daemon up" } else { "daemon down" };
    let dtxt = format!("{daemon}  {}", hms(crate::date::now_seconds()));
    set_str(buf, cols.saturating_sub(dtxt.len() as u16 + 1), 0, &dtxt, if d.daemon_up { p.ok } else { p.error }, dtxt.len() as u16);

    let counts = [
        ("dirty", t.dirty, p.warn),
        ("ahead", t.ahead, p.ok),
        ("behind", t.behind, p.behind),
        ("diverged", t.diverged, p.diverged),
        ("detached", t.detached, p.error),
        ("clean", t.clean, p.dim),
    ];
    let mut x = 0u16;
    for (label, n, style) in counts {
        let seg = format!("{label} ");
        set_str(buf, x, 1, &seg, p.label, cols.saturating_sub(x));
        x += seg.len() as u16;
        let num = format!("{n}  ");
        set_str(buf, x, 1, &num, style, cols.saturating_sub(x));
        x += num.len() as u16;
    }
    set_str(buf, 0, 2, &format!("claims {}  sessions {}  queue {} active", t.claims, t.sessions, t.queue), p.label, cols);

    // Body tiles.
    let top = 3u16;
    let foot = rows - 1;
    let body_h = foot.saturating_sub(top);
    let left_w = cols / 2;
    let right_x = left_w;
    let right_w = cols - left_w;
    let ev_h = body_h / 2;
    let cmd_h = body_h - ev_h;

    render_fleet(buf, 0, top, left_w, body_h, d, p);
    render_events(buf, right_x, top, right_w, ev_h, d, p);
    render_commands(buf, right_x, top + ev_h, right_w, cmd_h, d, p);

    // Footer.
    fill_row(buf, foot, p.bar);
    set_str(buf, 0, foot, " q Quit   live — fleet · events · commands · reads the daemon status cache ", p.bar, cols);
}

fn render_fleet(buf: &mut Buffer, x: u16, y: u16, w: u16, h: u16, d: &Dash, p: &Palette) {
    let (ix, iy, iw, ih) = tile(buf, x, y, w, h, "FLEET · most active", p);
    for (i, row) in d.fleet.iter().take(ih as usize).enumerate() {
        let cy = iy + i as u16;
        let (word, style) = state_of(row, p);
        let namew = iw.saturating_sub(20);
        set_str(buf, ix, cy, &basename(&row.path), p.row, namew);
        set_str(buf, ix + namew + 1, cy, word, style, 10);
        set_str(buf, ix + namew + 12, cy, &fmt_ago(row.age_secs), p.dim, 6);
    }
}

/// The salient state word + style for a fleet row.
fn state_of<'a>(row: &FleetRow, p: &'a Palette) -> (&'static str, Style) {
    if row.detached {
        ("detached", p.error)
    } else if row.sync == "diverged" {
        ("diverged", p.diverged)
    } else if row.dirty {
        ("dirty", p.warn)
    } else if row.sync == "behind" {
        ("behind", p.behind)
    } else if row.sync == "ahead" {
        ("ahead", p.ok)
    } else {
        ("clean", p.dim)
    }
}

fn render_events(buf: &mut Buffer, x: u16, y: u16, w: u16, h: u16, d: &Dash, p: &Palette) {
    let (ix, iy, iw, ih) = tile(buf, x, y, w, h, "EVENTS · commits · reconciles · status", p);
    // Newest last, so the freshest sits at the bottom of the tile.
    let take = ih as usize;
    let start = d.events.len().saturating_sub(take);
    for (i, e) in d.events[start..].iter().enumerate() {
        let cy = iy + i as u16;
        let (glyph, style) = match e.kind.as_str() {
            "commit" => ("●", p.green),
            "reconcile" => ("↻", p.magenta),
            "status" => ("◑", p.warn),
            "stage" => ("+", p.blue),
            _ => ("•", p.dim),
        };
        set_str(buf, ix, cy, &hms(e.ts), p.dim, 8);
        set_cell(buf, ix + 9, cy, glyph, style);
        let repo = event_repo(e);
        set_str(buf, ix + 11, cy, &repo, p.cyan, 18);
        let detail = e.detail.clone().unwrap_or_default();
        set_str(buf, ix + 30, cy, &detail, p.row, iw.saturating_sub(30));
    }
}

fn event_repo(e: &crate::db::EventRow) -> String {
    let src = e.workdir.clone().or_else(|| e.git_dir.clone()).unwrap_or_default();
    let base = basename(&src);
    base.trim_end_matches(".git").to_string()
}

fn render_commands(buf: &mut Buffer, x: u16, y: u16, w: u16, h: u16, d: &Dash, p: &Palette) {
    let (ix, iy, iw, ih) = tile(buf, x, y, w, h, "COMMANDS · every git across the fleet", p);
    if d.commands.is_empty() {
        set_str(buf, ix, iy, "run `git zcommands` once to enable command logging", p.dim, iw);
        return;
    }
    let take = ih as usize;
    let start = d.commands.len().saturating_sub(take);
    for (i, c) in d.commands[start..].iter().enumerate() {
        let cy = iy + i as u16;
        set_str(buf, ix, cy, &hms(c.ts), p.dim, 8);
        let who = format!("{}\u{2190}{}", c.pid, c.ppid);
        set_str(buf, ix + 9, cy, &who, p.dim, 13);
        set_str(buf, ix + 23, cy, &basename(&c.cwd), p.cyan, 14);
        set_str(buf, ix + 38, cy, "git", p.green, 3);
        set_str(buf, ix + 42, cy, &c.argv, p.row, iw.saturating_sub(42));
    }
}

// ---------------------------------------------------------------------------
// Lifecycle.
// ---------------------------------------------------------------------------

struct TermGuard;
impl Drop for TermGuard {
    fn drop(&mut self) {
        let _ = disable_raw_mode();
        let _ = execute!(stdout(), LeaveAlternateScreen, cursor::Show);
    }
}

/// Run the interactive tiled dashboard.
pub fn run() -> Result<ExitCode> {
    enable_raw_mode()?;
    execute!(stdout(), EnterAlternateScreen, cursor::Hide)?;
    let _guard = TermGuard;
    let mut term = Terminal::new(CrosstermBackend::new(stdout()))?;
    let p = Palette::new();
    let mut dash = Dash::empty();
    loop {
        // Fleet tile height ≈ body height; refresh with a generous visible count.
        let visible = ratatui::crossterm::terminal::size().map(|(_, h)| h.saturating_sub(6) as usize).unwrap_or(40);
        dash.refresh(visible);
        term.draw(|f| render(f.buffer_mut(), &dash, &p))?;

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
                match (k.code, k.modifiers) {
                    (KeyCode::Char('q'), _) | (KeyCode::Esc, _) => return Ok(ExitCode::SUCCESS),
                    (KeyCode::Char('c'), KeyModifiers::CONTROL) => return Ok(ExitCode::SUCCESS),
                    _ => {}
                }
            }
        }
    }
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
        let p = Palette::new();
        let detached = FleetRow { path: "/x".into(), git_dir: "/x/.git".into(), head: "h".into(), dirty: false, detached: true, sync: "up-to-date".into(), age_secs: 0 };
        assert_eq!(state_of(&detached, &p).0, "detached");
        let dirty = FleetRow { path: "/y".into(), git_dir: "/y/.git".into(), head: "h".into(), dirty: true, detached: false, sync: "up-to-date".into(), age_secs: 0 };
        assert_eq!(state_of(&dirty, &p).0, "dirty");
    }
}

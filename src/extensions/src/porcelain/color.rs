//! git's status coloring: the `color.status` / `color.ui` enablement rules, the
//! per-slot color config (`color.status.<slot>`), and a faithful port of git's
//! `color.c` color-spec parser so a custom slot value renders byte-for-byte the
//! same SGR sequence git would emit.
//!
//! Only `git status`'s human formats (long and short) colorize; the porcelain
//! formats and `-z` never do, matching git — the caller passes `porcelain = true`
//! to force [`StatusColors::disabled`].

use std::io::IsTerminal;

/// git's reset sequence — `ESC [ m`, not `ESC [ 0 m`.
const RESET: &str = "\x1b[m";

/// The `color.status.<slot>` slots this port colors, with git's built-in default
/// spec for each (empty string = git's "no color" default for that slot).
#[derive(Clone, Copy)]
pub(crate) enum Slot {
    /// `header` — section headers and hints. git's default is uncolored.
    Header,
    /// `added` / `updated` — staged changes. Default green.
    Added,
    /// `changed` — unstaged worktree changes. Default red.
    Changed,
    /// `untracked` — untracked (and, as git does, ignored) paths. Default red.
    Untracked,
    /// `unmerged` — conflicted paths. Default red.
    Unmerged,
    /// `nobranch` — the detached-HEAD short header. Default red.
    Nobranch,
    /// `localBranch` — the current branch name / ahead count in `-b`. Default green.
    LocalBranch,
    /// `remoteBranch` — the upstream name / behind count in `-b`. Default red.
    RemoteBranch,
    /// `branch` (git's `WT_STATUS_ONBRANCH`) — the branch name in the long-format
    /// `On branch <name>` header and the object name in `HEAD detached at <sha>`.
    /// git's default is uncolored.
    Branch,
}

impl Slot {
    /// The `color.status.<name>` config key.
    fn config_key(self) -> &'static str {
        match self {
            Slot::Header => "color.status.header",
            Slot::Added => "color.status.added",
            Slot::Changed => "color.status.changed",
            Slot::Untracked => "color.status.untracked",
            Slot::Unmerged => "color.status.unmerged",
            Slot::Nobranch => "color.status.nobranch",
            Slot::LocalBranch => "color.status.localBranch",
            Slot::RemoteBranch => "color.status.remoteBranch",
            Slot::Branch => "color.status.branch",
        }
    }

    /// git's built-in default spec for the slot; `""` means "no color".
    fn default_spec(self) -> &'static str {
        match self {
            Slot::Header | Slot::Branch => "",
            Slot::Added | Slot::LocalBranch => "green",
            Slot::Changed
            | Slot::Untracked
            | Slot::Unmerged
            | Slot::Nobranch
            | Slot::RemoteBranch => "red",
        }
    }
}

/// The resolved SGR sequences for every status slot, or a disabled instance whose
/// [`StatusColors::paint`] is the identity.
pub(crate) struct StatusColors {
    enabled: bool,
    header: String,
    added: String,
    changed: String,
    untracked: String,
    unmerged: String,
    nobranch: String,
    local_branch: String,
    remote_branch: String,
    branch: String,
}

impl StatusColors {
    /// Colors turned off — every `paint` returns its input unchanged.
    pub(crate) fn disabled() -> Self {
        StatusColors {
            enabled: false,
            header: String::new(),
            added: String::new(),
            changed: String::new(),
            untracked: String::new(),
            unmerged: String::new(),
            nobranch: String::new(),
            local_branch: String::new(),
            remote_branch: String::new(),
            branch: String::new(),
        }
    }

    /// Resolve status coloring against `repo`'s config. `porcelain` forces the
    /// disabled instance (git never colors the machine formats). Otherwise the
    /// `color.status` value — falling back to `color.ui`, default `auto` — decides:
    /// `always` on, `never`/false off, `auto`/true on only when stdout is a
    /// terminal and `TERM` is not `dumb`, exactly as git's `want_color`.
    pub(crate) fn resolve(repo: &gix::Repository, porcelain: bool) -> Self {
        if porcelain || !want_color(repo) {
            return Self::disabled();
        }
        let snapshot = repo.config_snapshot();
        let slot = |s: Slot| -> String {
            let spec = snapshot
                .string(s.config_key())
                .map(|v| v.to_string())
                .unwrap_or_else(|| s.default_spec().to_string());
            // A spec git accepts but this port cannot render falls back to the
            // built-in default rather than to no color, so the file still stands out.
            parse_color_spec(&spec)
                .or_else(|| parse_color_spec(s.default_spec()))
                .unwrap_or_default()
        };
        StatusColors {
            enabled: true,
            header: slot(Slot::Header),
            added: slot(Slot::Added),
            changed: slot(Slot::Changed),
            untracked: slot(Slot::Untracked),
            unmerged: slot(Slot::Unmerged),
            nobranch: slot(Slot::Nobranch),
            local_branch: slot(Slot::LocalBranch),
            remote_branch: slot(Slot::RemoteBranch),
            branch: slot(Slot::Branch),
        }
    }

    fn sgr(&self, slot: Slot) -> &str {
        match slot {
            Slot::Header => &self.header,
            Slot::Added => &self.added,
            Slot::Changed => &self.changed,
            Slot::Untracked => &self.untracked,
            Slot::Unmerged => &self.unmerged,
            Slot::Nobranch => &self.nobranch,
            Slot::LocalBranch => &self.local_branch,
            Slot::RemoteBranch => &self.remote_branch,
            Slot::Branch => &self.branch,
        }
    }

    /// Wrap `text` in the slot's color, or return it unchanged when coloring is off
    /// or the slot resolved to no color (git emits neither the SGR nor the reset in
    /// that case).
    pub(crate) fn paint(&self, slot: Slot, text: &str) -> String {
        let sgr = self.sgr(slot);
        if !self.enabled || sgr.is_empty() {
            text.to_string()
        } else {
            format!("{sgr}{text}{RESET}")
        }
    }
}

/// git's `want_color` for the status slot: read `color.status`, fall back to
/// `color.ui` (git's default is `auto`), then apply the tri-state.
fn want_color(repo: &gix::Repository) -> bool {
    let snapshot = repo.config_snapshot();
    let raw = snapshot
        .string("color.status")
        .or_else(|| snapshot.string("color.ui"))
        .map(|v| v.to_string());
    match raw.as_deref() {
        Some("always") => true,
        // git treats `true`/`yes`/`on`/`auto`/`1` (and an unset value) as auto.
        None | Some("auto" | "true" | "yes" | "on" | "1" | "") => auto_color(),
        // `never`/`false`/`no`/`off`/`0` — and anything else — turn color off.
        _ => false,
    }
}

/// git's `check_auto_color`: color on `auto` only when stdout is a terminal and the
/// terminal is not `dumb`.
fn auto_color() -> bool {
    if !std::io::stdout().is_terminal() {
        return false;
    }
    match std::env::var("TERM") {
        Ok(term) => !term.is_empty() && term != "dumb",
        Err(_) => false,
    }
}

// ---------------------------------------------------------------------------
// color-spec parser (git's color.c)
// ---------------------------------------------------------------------------

/// Parse a git color spec (`"green"`, `"bold red"`, `"brightblue"`, `"#ff0000"`,
/// `"216"`, `"ul"`, `"no-bold"`, …) into the SGR sequence git's `color_output`
/// would emit, or `None` for a spec git rejects. An empty / `"normal"` spec that
/// selects no color and no attributes yields `Some("")` — the caller renders that
/// as "leave text unpainted", matching git.
fn parse_color_spec(spec: &str) -> Option<String> {
    let mut attrs: Vec<String> = Vec::new();
    let mut fg: Option<Color> = None;
    let mut bg: Option<Color> = None;

    for word in spec.split_whitespace() {
        if let Some(code) = parse_attr(word) {
            attrs.push(code);
            continue;
        }
        let color = parse_color(word)?;
        if fg.is_none() {
            fg = Some(color);
        } else if bg.is_none() {
            bg = Some(color);
        } else {
            // A third color is a spec error, exactly as git's parser reports.
            return None;
        }
    }

    let mut codes = attrs;
    if let Some(c) = fg {
        if let Some(code) = c.sgr(false) {
            codes.push(code);
        }
    }
    if let Some(c) = bg {
        if let Some(code) = c.sgr(true) {
            codes.push(code);
        }
    }
    if codes.is_empty() {
        Some(String::new())
    } else {
        Some(format!("\x1b[{}m", codes.join(";")))
    }
}

/// A parsed color: `Normal` selects no code for its ground; the rest map to the
/// ANSI/256/RGB SGR encodings.
enum Color {
    /// `normal` — leave this ground's color untouched (no code).
    Normal,
    /// `default` — the terminal's default color (39 fg / 49 bg).
    Default,
    /// A basic ANSI color 0..=7.
    Ansi(u8),
    /// A bright ANSI color 8..=15 (`90+`/`100+`).
    Bright(u8),
    /// A 256-palette index.
    C256(u8),
    /// A 24-bit color.
    Rgb(u8, u8, u8),
}

impl Color {
    /// The SGR body for this color as a foreground (`bg = false`) or background.
    fn sgr(&self, bg: bool) -> Option<String> {
        let (base, ext) = if bg { (40u8, 48u8) } else { (30u8, 38u8) };
        let bright_base = if bg { 100u8 } else { 90u8 };
        match self {
            Color::Normal => None,
            Color::Default => Some((base + 9).to_string()),
            Color::Ansi(v) => Some((base + v).to_string()),
            Color::Bright(v) => Some((bright_base + (v - 8)).to_string()),
            Color::C256(n) => Some(format!("{ext};5;{n}")),
            Color::Rgb(r, g, b) => Some(format!("{ext};2;{r};{g};{b}")),
        }
    }
}

/// git's `parse_color`: a color name, `default`/`normal`, a `bright`-prefixed name,
/// a 0..=255 palette index, or a `#rrggbb` value.
fn parse_color(word: &str) -> Option<Color> {
    let lower = word.to_ascii_lowercase();
    match lower.as_str() {
        "normal" => return Some(Color::Normal),
        "default" => return Some(Color::Default),
        _ => {}
    }
    // A `#rrggbb` 24-bit color.
    if let Some(hex) = lower.strip_prefix('#') {
        if hex.len() == 6 && hex.bytes().all(|b| b.is_ascii_hexdigit()) {
            let r = u8::from_str_radix(&hex[0..2], 16).ok()?;
            let g = u8::from_str_radix(&hex[2..4], 16).ok()?;
            let b = u8::from_str_radix(&hex[4..6], 16).ok()?;
            return Some(Color::Rgb(r, g, b));
        }
        return None;
    }
    // A `bright<name>` color (git's shorthand for the 8..=15 range).
    if let Some(rest) = lower.strip_prefix("bright") {
        let idx = basic_color_index(rest)?;
        return Some(Color::Bright(idx + 8));
    }
    if let Some(idx) = basic_color_index(&lower) {
        return Some(Color::Ansi(idx));
    }
    // A bare palette index: 0..=7 basic, 8..=15 bright, 16..=255 the 256-palette.
    if let Ok(n) = lower.parse::<u16>() {
        return match n {
            0..=7 => Some(Color::Ansi(n as u8)),
            8..=15 => Some(Color::Bright(n as u8)),
            16..=255 => Some(Color::C256(n as u8)),
            _ => None,
        };
    }
    None
}

/// The 0..=7 index of a basic ANSI color name, or `None`.
fn basic_color_index(name: &str) -> Option<u8> {
    Some(match name {
        "black" => 0,
        "red" => 1,
        "green" => 2,
        "yellow" => 3,
        "blue" => 4,
        "magenta" => 5,
        "cyan" => 6,
        "white" => 7,
        _ => return None,
    })
}

/// git's `parse_attr`: an attribute name (`bold`, `dim`, `italic`, `ul`, `blink`,
/// `reverse`, `strike`), `reset`, or a `no`/`no-` negation, returning the SGR code.
/// `None` for a word that is not an attribute (the caller then tries a color).
fn parse_attr(word: &str) -> Option<String> {
    let lower = word.to_ascii_lowercase();
    if lower == "reset" {
        return Some("0".to_string());
    }
    // A `no`/`no-` prefix turns the attribute off with git's reset code.
    let (name, negate) = match lower.strip_prefix("no-").or_else(|| lower.strip_prefix("no")) {
        Some(rest) => (rest, true),
        None => (lower.as_str(), false),
    };
    let on = match name {
        "bold" => 1,
        "dim" => 2,
        "italic" => 3,
        "ul" => 4,
        "blink" => 5,
        "reverse" => 7,
        "strike" => 9,
        _ => return None,
    };
    let code = if negate {
        // git's off codes: bold and dim share 22, the rest are value + 20.
        if on == 1 || on == 2 {
            22
        } else {
            on + 20
        }
    } else {
        on
    };
    Some(code.to_string())
}

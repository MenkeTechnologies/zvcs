//! git's `color.*` configuration: the enablement rules (`color.ui`, the
//! per-command `color.<cmd>` overrides and `color.pager`), a faithful port of
//! git's `color.c` color-spec parser so a custom slot value renders
//! byte-for-byte the same SGR sequence git would emit, and the per-command slot
//! tables (`color.status.*`, `color.grep.*`, `color.decorate.*`,
//! `color.branch.*`, `color.transport.*`, `color.push.*`) the renderers paint
//! with.
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
    /// `updated` (also spelled `added`) — staged changes. Default green.
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
    /// The `color.status.<name>` config key. git's `WT_STATUS_UPDATED` slot is
    /// named `updated`; `added` is its second accepted spelling and is resolved
    /// separately in [`StatusColors::resolve`].
    fn config_key(self) -> &'static str {
        match self {
            Slot::Header => "color.status.header",
            Slot::Added => "color.status.updated",
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
    /// terminal (or a `color.pager` pager) and `TERM` is not `dumb`, exactly as
    /// git's `want_color`.
    pub(crate) fn resolve(repo: &gix::Repository, porcelain: bool) -> Self {
        if porcelain || !want_color_stdout(repo, "status") {
            return Self::disabled();
        }
        let snapshot = repo.config_snapshot();
        let slot = |s: Slot| -> String {
            let spec = match s {
                // git's `parse_status_slot` accepts both `updated` and `added` for
                // `WT_STATUS_UPDATED`; whichever the config sets last wins.
                Slot::Added => status_updated_spec(&snapshot),
                _ => snapshot.string(s.config_key()).map(|v| v.to_string()),
            }
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

/// The spec for git's `WT_STATUS_UPDATED` slot, which is reachable under two
/// names: `color.status.updated` and `color.status.added`. git applies whichever
/// assignment its config callback sees last, so this walks the merged config in
/// occurrence order (system, global, local, `-c`) and returns the value of the
/// last of the two names to appear, or `None` when neither is set.
fn status_updated_spec(snapshot: &gix::config::Snapshot<'_>) -> Option<String> {
    let file = snapshot.plumbing();
    let mut winner: Option<&'static str> = None;
    for section in file.sections() {
        let header = section.header();
        if !header.name().eq_ignore_ascii_case(b"color")
            || header
                .subsection_name()
                .is_none_or(|s| !s.eq_ignore_ascii_case(b"status"))
        {
            continue;
        }
        for name in section.value_names() {
            if name.eq_ignore_ascii_case("updated") {
                winner = Some("updated");
            } else if name.eq_ignore_ascii_case("added") {
                winner = Some("added");
            }
        }
    }
    let key = winner?;
    snapshot
        .string(&format!("color.status.{key}"))
        .map(|v| v.to_string())
}

// ---------------------------------------------------------------------------
// enablement (git's `want_color` / `check_auto_color`)
// ---------------------------------------------------------------------------

/// git's `want_color` for a command that writes to stdout: read `color.<section>`,
/// fall back to `color.ui` (git's default is `auto`), then apply the tri-state.
pub(crate) fn want_color_stdout(repo: &gix::Repository, section: &str) -> bool {
    let snapshot = repo.config_snapshot();
    let raw = snapshot
        .string(&format!("color.{section}"))
        .or_else(|| snapshot.string("color.ui"))
        .map(|v| v.to_string());
    match colorbool(raw.as_deref()) {
        ColorBool::Always => true,
        ColorBool::Never => false,
        ColorBool::Auto => auto_color_stdout(repo),
    }
}

/// git's `want_color_stderr` for the diagnostics slots (`color.advice`,
/// `color.push`, `color.transport`). These read only their own section — stock
/// git does not consult `color.ui` for them — and their `auto` state is decided
/// by stderr, not stdout.
pub(crate) fn want_color_stderr(repo: Option<&gix::Repository>, section: &str) -> bool {
    let raw = repo.and_then(|r| {
        r.config_snapshot()
            .string(&format!("color.{section}"))
            .map(|v| v.to_string())
    });
    match colorbool(raw.as_deref()) {
        ColorBool::Always => true,
        ColorBool::Never => false,
        ColorBool::Auto => std::io::stderr().is_terminal() && !terminal_is_dumb(),
    }
}

/// git's `git_config_colorbool`: `always`/`never`/`auto` verbatim, a boolean-true
/// value (and an unset key) meaning `auto`, and anything else meaning `never`.
enum ColorBool {
    Always,
    Never,
    Auto,
}

/// Classify a raw `color.*` value the way git's `git_config_colorbool` does.
fn colorbool(raw: Option<&str>) -> ColorBool {
    match raw {
        Some(v) if v.eq_ignore_ascii_case("always") => ColorBool::Always,
        Some(v) if v.eq_ignore_ascii_case("never") => ColorBool::Never,
        None => ColorBool::Auto,
        // git funnels everything else through `git_config_bool`: true means
        // `auto`, false means off.
        Some(v) => {
            if config_bool(v) {
                ColorBool::Auto
            } else {
                ColorBool::Never
            }
        }
    }
}

/// git's `git_parse_maybe_bool_text`: `true`/`yes`/`on`/`auto` and any non-zero
/// integer are true; `false`/`no`/`off`, `0` and the empty value are false.
fn config_bool(value: &str) -> bool {
    if value.is_empty() {
        return false;
    }
    for t in ["true", "yes", "on", "auto"] {
        if value.eq_ignore_ascii_case(t) {
            return true;
        }
    }
    for f in ["false", "no", "off"] {
        if value.eq_ignore_ascii_case(f) {
            return false;
        }
    }
    value.parse::<i64>().map(|n| n != 0).unwrap_or(false)
}

/// git's `check_auto_color(1)`: color on `auto` when stdout is a terminal, or when
/// output is going to a pager that `color.pager` (default true) allows to receive
/// color — in both cases only if the terminal is not `dumb`.
fn auto_color_stdout(repo: &gix::Repository) -> bool {
    let to_terminal = std::io::stdout().is_terminal() || (pager_in_use() && pager_use_color(repo));
    to_terminal && !terminal_is_dumb()
}

/// git's `pager_in_use`: the `GIT_PAGER_IN_USE` environment flag, parsed as a
/// config boolean. `pager::maybe_setup` sets it when it installs the pager.
fn pager_in_use() -> bool {
    match std::env::var("GIT_PAGER_IN_USE") {
        Ok(v) => config_bool(&v),
        Err(_) => false,
    }
}

/// git's `pager_use_color`, from `color.pager` — whether output that is being
/// piped into the pager may still carry ANSI. Defaults to true.
fn pager_use_color(repo: &gix::Repository) -> bool {
    repo.config_snapshot().boolean("color.pager").unwrap_or(true)
}

/// git's `is_terminal_dumb`: an unset `TERM`, or a `TERM` of exactly `dumb`,
/// suppresses `auto` color.
fn terminal_is_dumb() -> bool {
    match std::env::var("TERM") {
        Ok(term) => term == "dumb",
        Err(_) => true,
    }
}

// ---------------------------------------------------------------------------
// per-command slot tables
// ---------------------------------------------------------------------------

/// Resolve a `<key>` color slot to its SGR sequence, falling back to git's
/// built-in `default_spec` when unset and to the default again when the user's
/// spec is one git accepts but this port cannot render.
pub(crate) fn slot(snapshot: &gix::config::Snapshot<'_>, key: &str, default_spec: &str) -> String {
    let spec = snapshot
        .string(key)
        .map(|v| v.to_string())
        .unwrap_or_else(|| default_spec.to_string());
    parse_color_spec(&spec)
        .or_else(|| parse_color_spec(default_spec))
        .unwrap_or_default()
}

/// The `color.grep.<slot>` table (git's `grep.c` `color_grep_slots`), resolved to
/// SGR sequences. Empty strings are git's "no color" defaults for that slot, and
/// the renderer emits neither the SGR nor a reset for them.
pub(crate) struct GrepColors {
    /// `context` — non-matching text on `-A`/`-B`/`-C` context lines.
    pub(crate) context: String,
    /// `filename` — the filename prefix. Default magenta.
    pub(crate) filename: String,
    /// `function` — the `-p` function-name line.
    pub(crate) function: String,
    /// `lineNumber` — the `-n` line-number prefix. Default green.
    pub(crate) line_number: String,
    /// `column` — the `--column` column-number prefix. Default green.
    pub(crate) column: String,
    /// `matchContext` — matched text on a context line. Default bold red.
    pub(crate) match_context: String,
    /// `matchSelected` — matched text on a selected line. Default bold red.
    pub(crate) match_selected: String,
    /// `selected` — non-matching text on a selected line.
    pub(crate) selected: String,
    /// `separator` — the `:`/`-`/`=` field separators and the `--` hunk separator.
    /// Default cyan.
    pub(crate) separator: String,
}

impl GrepColors {
    /// git's `init_grep_defaults` table, used when there is no repository (and so
    /// no config) to read — `git grep --no-index` outside a work tree.
    pub(crate) fn defaults() -> Self {
        GrepColors {
            context: String::new(),
            filename: "\x1b[35m".into(),
            function: String::new(),
            line_number: "\x1b[32m".into(),
            column: "\x1b[32m".into(),
            match_context: "\x1b[1;31m".into(),
            match_selected: "\x1b[1;31m".into(),
            selected: String::new(),
            separator: "\x1b[36m".into(),
        }
    }

    /// Read every `color.grep.<slot>` from `repo`. `color.grep.match` sets both
    /// match slots at once (git's `grep_config` writes it into both); an explicit
    /// `matchContext`/`matchSelected` is more specific and wins over it.
    pub(crate) fn resolve(repo: &gix::Repository) -> Self {
        let snapshot = repo.config_snapshot();
        let both = snapshot
            .string("color.grep.match")
            .map(|v| v.to_string())
            .filter(|spec| parse_color_spec(spec).is_some())
            .unwrap_or_else(|| "bold red".to_string());
        GrepColors {
            context: slot(&snapshot, "color.grep.context", ""),
            filename: slot(&snapshot, "color.grep.filename", "magenta"),
            function: slot(&snapshot, "color.grep.function", ""),
            line_number: slot(&snapshot, "color.grep.lineNumber", "green"),
            column: slot(&snapshot, "color.grep.column", "green"),
            match_context: slot(&snapshot, "color.grep.matchContext", &both),
            match_selected: slot(&snapshot, "color.grep.matchSelected", &both),
            selected: slot(&snapshot, "color.grep.selected", ""),
            separator: slot(&snapshot, "color.grep.separator", "cyan"),
        }
    }
}

/// The `color.decorate.<slot>` table (git's `log-tree.c` `decoration_colors`) plus
/// the `color.diff.commit` color git paints the decoration punctuation and the
/// commit object name with.
pub(crate) struct DecorateColors {
    /// `HEAD` — the `HEAD` entry. Default bold cyan.
    pub(crate) head: String,
    /// `branch` — a local branch. Default bold green.
    pub(crate) branch: String,
    /// `remoteBranch` — a remote-tracking branch. Default bold red.
    pub(crate) remote_branch: String,
    /// `tag` — the `tag: ` prefix and the tag name. Default bold yellow.
    pub(crate) tag: String,
    /// `stash` — the `refs/stash` entry. Default bold magenta.
    pub(crate) stash: String,
    /// git's `DECORATION_NONE`, the bare reset any other ref is shown in. It has
    /// no config key of its own; it is empty here only when coloring is off.
    pub(crate) none: String,
    /// `color.diff.commit` — the ` (`, `, ` and `)` punctuation around the
    /// decoration list, and the commit object name it follows. Default yellow.
    pub(crate) commit: String,
}

impl DecorateColors {
    /// Read git's decoration slots from `repo`.
    pub(crate) fn resolve(repo: &gix::Repository) -> Self {
        let snapshot = repo.config_snapshot();
        DecorateColors {
            head: slot(&snapshot, "color.decorate.HEAD", "bold cyan"),
            branch: slot(&snapshot, "color.decorate.branch", "bold green"),
            remote_branch: slot(&snapshot, "color.decorate.remoteBranch", "bold red"),
            tag: slot(&snapshot, "color.decorate.tag", "bold yellow"),
            stash: slot(&snapshot, "color.decorate.stash", "bold magenta"),
            none: RESET.to_string(),
            commit: slot(&snapshot, "color.diff.commit", "yellow"),
        }
    }

    /// The uncolored table — every slot empty, so a caller that paints
    /// unconditionally emits plain text.
    pub(crate) fn disabled() -> Self {
        DecorateColors {
            head: String::new(),
            branch: String::new(),
            remote_branch: String::new(),
            tag: String::new(),
            stash: String::new(),
            none: String::new(),
            commit: String::new(),
        }
    }
}

/// The colors `git push` paints its ref-status report with. git splits these
/// across two independent switches: the per-ref summary field is `color.transport`
/// / `color.transport.rejected`, and the trailing `error: failed to push some
/// refs` line is `color.push` / `color.push.error`. Neither consults `color.ui`,
/// and both are `auto` against stderr.
pub(crate) struct PushColors {
    /// `color.transport.rejected` — the ` ! [rejected]` summary field. Default red.
    /// Empty when `color.transport` is off.
    pub(crate) rejected: String,
    /// `color.push.error` — the `error: failed to push some refs to …` line.
    /// Default red. Empty when `color.push` is off.
    pub(crate) error: String,
}

impl PushColors {
    /// Resolve both switches against `repo`'s config and stderr's tty state.
    pub(crate) fn resolve(repo: Option<&gix::Repository>) -> Self {
        let spec = |section: &str, key: &str, default_spec: &str| -> String {
            if !want_color_stderr(repo, section) {
                return String::new();
            }
            match repo {
                Some(r) => slot(&r.config_snapshot(), key, default_spec),
                None => parse_color_spec(default_spec).unwrap_or_default(),
            }
        };
        PushColors {
            rejected: spec("transport", "color.transport.rejected", "red"),
            error: spec("push", "color.push.error", "red"),
        }
    }

    /// Wrap `text` in `sgr`, or return it unchanged when the slot is off. git
    /// emits neither the SGR nor the reset when the color is disabled.
    pub(crate) fn paint(sgr: &str, text: &str) -> String {
        if sgr.is_empty() {
            text.to_string()
        } else {
            format!("{sgr}{text}{RESET}")
        }
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
pub(crate) fn parse_color_spec(spec: &str) -> Option<String> {
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

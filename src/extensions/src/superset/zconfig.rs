//! `git zconfig` — inspect and toggle the `[zvcs]` daemon settings from the CLI,
//! so the autonomy features (auto-sync, status maintenance, MRU file-watch,
//! hooks, …) can be turned on/off without hand-editing `~/.gitconfig`.
//!
//! Reads resolve through the same cascade the daemon sees (repo snapshot when
//! inside a repo, else the global+system cascade). Writes go to the per-user
//! global config (`~/.gitconfig`) — the file the daemon reads on start/reload —
//! via zvcs's own `config --global` porcelain. After a change, a *running*
//! daemon is reloaded so the toggle takes effect at once; a stopped daemon
//! picks it up on next start (toggling never spawns a daemon).

use anyhow::{bail, Result};
use std::process::{Command, ExitCode};

/// Kind of a setting's value: a boolean switch, or a non-negative count where
/// `0` disables the loop it gates.
#[derive(Clone, Copy, PartialEq)]
enum Kind {
    Bool,
    Count,
}

/// One toggleable `[zvcs]` setting. `key` is the name after the `zvcs.` prefix;
/// `default` is the effective value when unset (as the daemon computes it), so
/// the listing shows real state, not "unset". `gated` marks a switch that `all
/// off` should turn off — every boolean plus the two counts whose `0` disables a
/// loop; a plain debounce like `interval` is left alone. This table is the
/// single source of truth for what `git zconfig` lists, sets, and validates.
struct Setting {
    key: &'static str,
    kind: Kind,
    default: &'static str,
    gated: bool,
    desc: &'static str,
}

const SETTINGS: &[Setting] = &[
    Setting { key: "autoreconcile", kind: Kind::Bool,  default: "off", gated: true,  desc: "auto-sync: reconcile submodules to origin/main on change" },
    Setting { key: "autobump",      kind: Kind::Bool,  default: "off", gated: true,  desc: "forward-only submodule gitlink bumps + commit" },
    Setting { key: "autocrawl",     kind: Kind::Bool,  default: "off", gated: true,  desc: "crawl zvcs.crawlroots into the index on daemon start" },
    Setting { key: "autostatus",    kind: Kind::Bool,  default: "off", gated: true,  desc: "recompute a repo's status cache when it changes" },
    Setting { key: "autohook",      kind: Kind::Bool,  default: "off", gated: true,  desc: "fire each repo's zvcs.hook on change" },
    Setting { key: "autodups",      kind: Kind::Bool,  default: "off", gated: true,  desc: "fan a commit out to local duplicate checkouts" },
    Setting { key: "statusinterval",kind: Kind::Count, default: "10",  gated: true,  desc: "status-cache backstop sweep, seconds (0 disables)" },
    Setting { key: "watchmru",      kind: Kind::Count, default: "512", gated: true,  desc: "file-watch the N most-recently-used repos (0 disables)" },
    Setting { key: "interval",      kind: Kind::Count, default: "30",  gated: false, desc: "autonomy debounce, seconds (always on)" },
];

/// The setting names `git zconfig <name>` accepts, plus the `all` pseudo-name —
/// the second-token completion vocabulary for the repl. Sourced from [`SETTINGS`]
/// so it can never drift from what the verb actually sets.
pub fn setting_names() -> Vec<&'static str> {
    let mut v: Vec<&'static str> = SETTINGS.iter().map(|s| s.key).collect();
    v.push("all");
    v
}

/// The value vocabulary offered after a `git zconfig <name>` token, for repl
/// completion: booleans (and the `all` pseudo-name) take on/off, and every named
/// setting also accepts `default` (revert). A bare count otherwise has no fixed
/// vocabulary beyond `default`. Empty for an unknown name.
pub fn value_hints(name: &str) -> &'static [&'static str] {
    if name == "all" {
        return &["on", "off"];
    }
    match SETTINGS.iter().find(|s| s.key == name) {
        Some(s) if s.kind == Kind::Bool => &["on", "off", "default"],
        Some(_) => &["default"], // a count takes a number, or `default` to revert
        None => &[],
    }
}

const USAGE: &str = "\
usage: git zconfig                       list every setting and its value
       git zconfig <name>                show one setting
       git zconfig <name> on|off         toggle a boolean feature
       git zconfig <name> <count>        set a numeric knob (0 disables)
       git zconfig <name> default        revert to the built-in default
       git zconfig all on|off            flip every autonomy switch at once";

pub fn zconfig(args: &[String]) -> Result<ExitCode> {
    match args {
        [] => list(),
        [name] if name == "-h" || name == "--help" => {
            println!("{USAGE}");
            Ok(ExitCode::SUCCESS)
        }
        [name] => show(name),
        [name, value] if name == "all" => set_all(value),
        [name, value] => set(name, value),
        _ => {
            eprintln!("{USAGE}");
            Ok(ExitCode::FAILURE)
        }
    }
}

/// `git zconfig` — every setting, its effective value, and whether it is set in
/// your config (`*`) or showing the built-in default.
fn list() -> Result<ExitCode> {
    println!("zvcs daemon settings (written to the global config, ~/.gitconfig):\n");
    let width = SETTINGS.iter().map(|s| s.key.len()).max().unwrap_or(0);
    for s in SETTINGS {
        let (val, is_set) = effective(s);
        let mark = if is_set { '*' } else { ' ' };
        println!("  {mark} {:width$}  {:<5}  {}", s.key, val, s.desc, width = width);
    }
    println!(
        "\n* = set in your config; unmarked rows show the built-in default.\n\
         Set with: git zconfig <name> on|off | <count> | default   (git zconfig -h)\n\
         Toggling every autonomy switch: git zconfig all on|off"
    );
    Ok(ExitCode::SUCCESS)
}

/// `git zconfig <name>` — one setting's value, source, and description.
fn show(name: &str) -> Result<ExitCode> {
    let s = lookup(name)?;
    let (val, is_set) = effective(s);
    let src = if is_set { "set in ~/.gitconfig" } else { "default (unset)" };
    println!("zvcs.{} = {}   ({})\n{}", s.key, val, src, s.desc);
    Ok(ExitCode::SUCCESS)
}

/// `git zconfig <name> <value>` — validate, write to the global config, and
/// reload a running daemon so the change takes effect immediately.
fn set(name: &str, value: &str) -> Result<ExitCode> {
    let s = lookup(name)?;
    if value == "default" || value == "--unset" {
        unset_key(s.key)?;
        println!("zvcs.{} reverted to default ({})", s.key, s.default);
        return apply();
    }
    let normalized = normalize(s, value)?;
    write_key(s.key, &normalized)?;
    println!("zvcs.{} = {}", s.key, display_value(s, &normalized));
    apply()
}

/// `git zconfig all on|off` — flip every gated autonomy switch at once. `off`
/// also zeroes the two background loops (`statusinterval`, `watchmru`) so the
/// daemon truly idles; `on` restores their defaults. Non-gated knobs like
/// `interval` (a debounce that is always active) are left untouched.
fn set_all(value: &str) -> Result<ExitCode> {
    let on = match value {
        "on" | "true" | "1" | "yes" | "enable" => true,
        "off" | "false" | "0" | "no" | "disable" => false,
        _ => bail!("git zconfig all expects on|off, got `{value}`"),
    };
    for s in SETTINGS.iter().filter(|s| s.gated) {
        let v = match s.kind {
            Kind::Bool => if on { "true" } else { "false" }.to_string(),
            // Gated counts: on → their default cadence, off → 0 (disabled).
            Kind::Count => if on { s.default.to_string() } else { "0".to_string() },
        };
        write_key(s.key, &v)?;
    }
    println!("all zvcs autonomy switches turned {}", if on { "on" } else { "off" });
    apply()
}

/// Resolve a setting name; error listing valid names on a typo.
fn lookup(name: &str) -> Result<&'static Setting> {
    SETTINGS.iter().find(|s| s.key == name).ok_or_else(|| {
        let names: Vec<&str> = SETTINGS.iter().map(|s| s.key).collect();
        anyhow::anyhow!("unknown setting `{name}`; valid: {}", names.join(", "))
    })
}

/// Normalize a user value to its gitconfig form, or reject it.
fn normalize(s: &Setting, value: &str) -> Result<String> {
    match s.kind {
        Kind::Bool => match value {
            "on" | "true" | "1" | "yes" | "enable" => Ok("true".into()),
            "off" | "false" | "0" | "no" | "disable" => Ok("false".into()),
            _ => bail!("`{}` is a switch; use on|off (got `{value}`)", s.key),
        },
        Kind::Count => match value.parse::<u64>() {
            Ok(n) => Ok(n.to_string()),
            Err(_) => bail!("`{}` takes a non-negative count (got `{value}`)", s.key),
        },
    }
}

/// The effective value as the daemon would compute it, and whether it is set
/// explicitly. Booleans render on/off; counts render as-is.
fn effective(s: &Setting) -> (String, bool) {
    match s.kind {
        Kind::Bool => match crate::config::config_bool(&format!("zvcs.{}", s.key)) {
            Some(b) => (if b { "on" } else { "off" }.into(), true),
            None => (s.default.into(), false),
        },
        Kind::Count => match config_int(&format!("zvcs.{}", s.key)) {
            Some(n) => (n.to_string(), true),
            None => (s.default.into(), false),
        },
    }
}

/// A configured value's display form (booleans as on/off for consistency).
fn display_value(s: &Setting, normalized: &str) -> String {
    if s.kind == Kind::Bool {
        if normalized == "true" { "on".into() } else { "off".into() }
    } else {
        normalized.to_string()
    }
}

/// Read an integer `[zvcs]` key with the same repo-then-global cascade as
/// [`crate::config::config_bool`].
fn config_int(key: &str) -> Option<i64> {
    match crate::setup::discover() {
        Ok(repo) => repo.config_snapshot().integer(key),
        Err(_) => crate::config::global_config().integer(key).ok().flatten(),
    }
}

/// Write `zvcs.<key> = <val>` to the global config via zvcs's own porcelain.
fn write_key(key: &str, val: &str) -> Result<()> {
    let exe = crate::hosted::git_exe()?;
    let ok = Command::new(exe)
        .args(["config", "--global", &format!("zvcs.{key}"), val])
        .status()?
        .success();
    if !ok {
        bail!("failed to write zvcs.{key}");
    }
    Ok(())
}

/// Unset `zvcs.<key>` from the global config; a missing key (git rc 5) is fine.
fn unset_key(key: &str) -> Result<()> {
    let exe = crate::hosted::git_exe()?;
    let status = Command::new(exe)
        .args(["config", "--global", "--unset", &format!("zvcs.{key}")])
        .status()?;
    if !status.success() && status.code() != Some(5) {
        bail!("failed to unset zvcs.{key}");
    }
    Ok(())
}

/// Reload a running daemon so a just-written setting takes effect. Never starts
/// one — a stopped daemon reads the new value on its next start.
fn apply() -> Result<ExitCode> {
    if super::zdaemon::is_running() {
        let exe = crate::hosted::git_exe()?;
        let _ = Command::new(exe).args(["zdaemon", "reload"]).status();
        println!("(reloaded the running daemon)");
    } else {
        println!("(daemon not running — applies on next start)");
    }
    Ok(ExitCode::SUCCESS)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_setting_normalizes_its_own_default() {
        // The listed default must be a value the setting actually accepts —
        // guards the table against a default that `normalize` would reject.
        for s in SETTINGS {
            let input = s.default;
            assert!(normalize(s, input).is_ok(), "{} rejects its own default", s.key);
        }
    }

    #[test]
    fn bool_and_count_reject_wrong_kind() {
        let b = lookup("autoreconcile").unwrap();
        let c = lookup("statusinterval").unwrap();
        assert!(normalize(b, "7").is_err(), "a switch must reject a number");
        assert!(normalize(c, "on").is_err(), "a count must reject on/off");
        assert!(normalize(b, "on").is_ok());
        assert!(normalize(c, "0").is_ok());
    }

    #[test]
    fn unknown_setting_is_rejected() {
        assert!(lookup("nope").is_err());
    }
}

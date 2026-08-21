//! Port of `git_log_config()` and `git_format_config()`
//! (builtin/log.c:462-524 and :978-1136) — the config callbacks the commit-listing
//! commands install on top of the diff chain.
//!
//! ```text
//! git_format_config     format.headers/suffix/to/cc/numbered/attach/thread/…
//!   └─ git_log_config   format.pretty/subjectPrefix/…, log.*, color.decorate.<slot>
//!        └─ git_diff_ui_config   (crate::diff_config)
//! ```
//!
//! `log`, `show` and `whatchanged` install `git_log_config`; `format-patch`
//! installs `git_format_config`, which is `git_log_config` plus its own keys plus
//! one short-circuit.
//!
//! # The short-circuit is the reason this module exists
//!
//! `git_format_config` swallows four keys before they can reach the diff chain
//! (builtin/log.c:1005-1008 and :1131-1133):
//!
//! ```c
//! if (!strcmp(var, "diff.color") || !strcmp(var, "color.diff") ||
//!     !strcmp(var, "color.ui") || !strcmp(var, "diff.submodule")) {
//!         return 0;
//! }
//! …
//! /* ignore some porcelain config which would otherwise be parsed by
//!  * git_diff_ui_config(), via git_log_config() … */
//! if (!strcmp(var, "diff.noprefix"))
//!         return 0;
//! ```
//!
//! So `format-patch` accepts values that `log` — one link further out on the same
//! chain — refuses. Measured against git 2.55.0:
//!
//! ```text
//! $ git -c color.ui=bogus format-patch -1 --stdout      # exit 0
//! $ git -c color.ui=bogus log -1                        # fatal: bad boolean config value …
//! $ git -c diff.relative=bogus format-patch -1 --stdout # fatal (not short-circuited)
//! ```
//!
//! Wiring `format-patch` to the plain diff-UI validator would have refused all
//! four, which is why the callbacks are modelled rather than approximated.
//!
//! # Keys read here but resolved elsewhere
//!
//! `log.date` and `format.pretty` are `git_config_string` at this point — they
//! cannot fail here. Their familiar refusals (`unknown date format bogus`,
//! `invalid --pretty format: x`) come later, from `cmd_log_init()` after option
//! parsing, and this port already produces them there. Only the valueless
//! spelling is a config-time error for either.

use crate::config::{ConfigValue, walk_config};
use crate::default_config::{DefaultConfig, ObjectCreationMode, Rejection};
use crate::diff_config::git_diff_ui_config;

fn defaults() -> DefaultConfig {
    DefaultConfig {
        object_creation_mode: ObjectCreationMode::Renames,
        sparse_expect_files_outside_of_patterns: false,
    }
}

/// `repo_config(the_repository, git_log_config, &cfg)` — `log`, `show`,
/// `whatchanged` (builtin/log.c:538, :675, :792, :837).
pub fn validate_log(repo: &gix::Repository) -> Result<(), Rejection> {
    let mut out = defaults();
    for v in walk_config(repo) {
        git_log_config(&v, &mut out)?;
    }
    Ok(())
}

/// `repo_config(the_repository, git_format_config, &cfg)` — `format-patch`
/// (builtin/log.c:2101).
pub fn validate_format(repo: &gix::Repository) -> Result<(), Rejection> {
    let mut out = defaults();
    for v in walk_config(repo) {
        git_format_config(&v, &mut out)?;
    }
    Ok(())
}

/// `git_format_config()` (builtin/log.c:978-1136).
fn git_format_config(v: &ConfigValue, out: &mut DefaultConfig) -> Result<(), Rejection> {
    let key = v.key.as_str();
    match key {
        // builtin/log.c:983-988 — the one arm that `die()`s on a missing value
        // instead of using `config_error_nonbool`, so it has no origin clause.
        "format.headers" => {
            if v.value.is_none() {
                return Err(Rejection::Die("format.headers without value".to_string()));
            }
        }
        // builtin/log.c:989-1004, 1046-1050, 1055-1058, 1063-1070 — plain strings.
        "format.suffix"
        | "format.to"
        | "format.cc"
        | "format.signature"
        | "format.commitlistformat"
        | "format.outputdirectory" => {
            string_value(v)?;
        }
        // builtin/log.c:1005-1008 — the short-circuit. These four never reach the
        // diff chain from `format-patch`.
        "diff.color" | "color.diff" | "color.ui" | "diff.submodule" => {}
        // builtin/log.c:1131-1133 — the fifth, added separately with its own
        // comment in the C.
        "diff.noprefix" => {}
        // builtin/log.c:1009-1017 — `auto` (strcasecmp) or a boolean.
        "format.numbered" | "format.coverletter" => {
            if !v.value.as_deref().is_some_and(|s| s.eq_ignore_ascii_case("auto")) {
                bool_value(v, key)?;
            }
        }
        // builtin/log.c:1018-1030 — every value is legal, the valueless form
        // included (it means the git version string).
        "format.attach" => {}
        // builtin/log.c:1031-1041 — `deep`/`shallow` (strcasecmp) or a boolean.
        "format.thread" => {
            let word = v.value.as_deref().unwrap_or("");
            if !(v.value.is_some()
                && (word.eq_ignore_ascii_case("deep") || word.eq_ignore_ascii_case("shallow")))
            {
                bool_value(v, key)?;
            }
        }
        // builtin/log.c:1042-1045, 1085-1088, 1105-1108 — plain booleans.
        "format.signoff" | "format.forceinbodyfrom" | "format.mboxrd" => {
            bool_value(v, key)?;
        }
        // builtin/log.c:1051-1054 — `git_config_pathname`.
        "format.signaturefile" => {
            let raw = string_value(v)?;
            crate::default_config::expand_path(&raw)?;
        }
        // builtin/log.c:1071-1077 — `whenAble` (strcasecmp) or a boolean.
        "format.useautobase" => {
            if !v
                .value
                .as_deref()
                .is_some_and(|s| s.eq_ignore_ascii_case("whenAble"))
            {
                bool_value(v, key)?;
            }
        }
        // builtin/log.c:1078-1084, 1089-1098 — `git_parse_maybe_bool` where a
        // non-boolean is a *legal* value with a different meaning (an ident
        // string, a notes ref), so nothing can be rejected.
        "format.from" | "format.notes" => {}
        // builtin/log.c:1099-1104 → `parse_cover_from_description`
        // (builtin/log.c:942-956), which `die()`s with the value first.
        "format.coverfromdescription" => {
            if let Some(raw) = v.value.as_deref() {
                if !matches!(raw, "default" | "none" | "message" | "subject" | "auto") {
                    return Err(Rejection::Die(format!(
                        "{raw}: invalid cover from description mode"
                    )));
                }
            }
        }
        // builtin/log.c:1109-1122 — a boolean whose refusal is `die_message()`
        // plus an `advise()`, i.e. the usual fatal line followed by two `hint:`
        // lines. `git_parse_maybe_bool(NULL)` is 1, so the valueless form is fine.
        "format.noprefix" => {
            if let Some(raw) = v.value.as_deref() {
                if crate::optint::maybe_bool(raw).is_none() {
                    eprintln!("fatal: bad boolean config value '{raw}' for '{key}'");
                    eprintln!("hint: '{key}' used to accept any value and treat that as 'true'.");
                    eprintln!("hint: Now it only accepts boolean values, like what 'diff.noprefix' does.");
                    return Err(Rejection::Silent);
                }
            }
        }
        _ => return git_log_config(v, out),
    }
    Ok(())
}

/// `git_log_config()` (builtin/log.c:462-524).
fn git_log_config(v: &ConfigValue, out: &mut DefaultConfig) -> Result<(), Rejection> {
    let key = v.key.as_str();
    match key {
        // builtin/log.c:468-475 — plain strings. `format.pretty`'s
        // `invalid --pretty format` refusal happens after option parsing, not here.
        "format.pretty" | "format.subjectprefix" => {
            string_value(v)?;
        }
        // builtin/log.c:476-479.
        "format.filenamemaxlength" => {
            int_value(v, key)?;
        }
        // builtin/log.c:480-495, 502-517 — plain booleans.
        "format.encodeemailheaders"
        | "log.abbrevcommit"
        | "log.showroot"
        | "log.follow"
        | "log.mailmap"
        | "log.showsignature" => {
            bool_value(v, key)?;
        }
        // builtin/log.c:488-491 — `git_config_string`; `unknown date format` comes
        // from `parse_date_format()` later.
        "log.date" => {
            string_value(v)?;
        }
        // builtin/log.c:492-497 — `parse_decoration_style` returning -1 is
        // *silently* turned into 0 ("maybe warn?" says the comment), so an
        // unreadable value can never fail.
        "log.decorate" => {}
        // builtin/log.c:498-502 → `diff_merges_config`, which returns -1 for an
        // unknown mode with no `error()` of its own — leaving the origin line as
        // the whole diagnostic.
        "log.diffmerges" => {
            let raw = string_value(v)?;
            if !DIFF_MERGES_MODES.contains(&raw.as_str()) {
                return Err(Rejection::Reported {
                    errors: Vec::new(),
                    fatal: v.origin.die_linenr(key),
                });
            }
        }
        _ => {
            // builtin/log.c:513-514 → `parse_decorate_color_config`
            // (log-tree.c:69-77): a slot outside the table returns before the
            // value is read.
            if let Some(slot) = key.strip_prefix("color.decorate.") {
                if !COLOR_DECORATE_SLOTS
                    .iter()
                    .any(|s| slot.eq_ignore_ascii_case(s))
                {
                    return Ok(());
                }
                let raw = string_value(v)?;
                if crate::porcelain::color::parse_color_spec(&raw).is_none() {
                    return Err(reported(v, vec![format!("invalid color value: {raw}")]));
                }
                return Ok(());
            }
            // builtin/log.c:522 — everything else is the diff UI chain.
            return git_diff_ui_config(v, out);
        }
    }
    Ok(())
}

/// `color_decorate_slots[]` (log-tree.c:51-58), matched case-insensitively by
/// `LOOKUP_CONFIG`.
const COLOR_DECORATE_SLOTS: &[&str] = &[
    "branch",
    "remoteBranch",
    "tag",
    "stash",
    "HEAD",
    "grafted",
];

/// The modes `diff_merges_config()` (diff-merges.c) accepts for `log.diffMerges`.
/// Anything else is the silent -1 that leaves only the origin line.
const DIFF_MERGES_MODES: &[&str] = &[
    "off",
    "none",
    "on",
    "first-parent",
    "1",
    "separate",
    "m",
    "combined",
    "c",
    "dense-combined",
    "cc",
    "remerge",
    "r",
];

/// `git_config_bool()` (config.c:1292-1298), with `NULL` meaning true.
fn bool_value(v: &ConfigValue, key: &str) -> Result<bool, Rejection> {
    let Some(raw) = v.value.as_deref() else {
        return Ok(true);
    };
    crate::optint::maybe_bool(raw)
        .ok_or_else(|| Rejection::Die(format!("bad boolean config value '{raw}' for '{key}'")))
}

/// `git_config_string()` (config.c:1300-1306).
fn string_value(v: &ConfigValue) -> Result<String, Rejection> {
    match v.value.as_deref() {
        Some(raw) => Ok(raw.to_string()),
        None => Err(reported(v, vec![format!("missing value for '{}'", v.key)])),
    }
}

/// `git_config_int()` (config.c:1226-1232) through `die_bad_number`.
fn int_value(v: &ConfigValue, key: &str) -> Result<i64, Rejection> {
    let raw = v.value.as_deref().unwrap_or("");
    crate::config::parse_config_int(raw).map_err(|reason| {
        Rejection::Die(format!(
            "bad numeric config value '{raw}' for '{key}'{}: {reason}",
            v.origin.bad_number_clause()
        ))
    })
}

/// Any `return error(...)` arm: the `error:` lines, then the origin-named fatal.
fn reported(v: &ConfigValue, errors: Vec<String>) -> Rejection {
    Rejection::Reported {
        errors,
        fatal: v.origin.die_linenr(&v.key),
    }
}

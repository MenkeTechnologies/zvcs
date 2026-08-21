//! Port of `git_status_config()` and `git_commit_config()`
//! (builtin/commit.c:1451-1535 and :1669-1696) — the config callbacks `status`
//! and `commit` install.
//!
//! Both end by delegating: `git_commit_config` finishes
//! `return git_status_config(k, v, ctx, s)`, and `git_status_config` finishes
//! `return git_diff_ui_config(k, v, ctx, NULL)` (builtin/commit.c:1535). So
//! `git status` validates the whole diff-UI chain and everything below it, which
//! is why `git -c diff.context=-1 status` refuses to run at all:
//!
//! ```text
//! $ git -c diff.context=-1 status
//! fatal: unable to parse 'diff.context' from command-line config
//! ```
//!
//! # The two conditional arms
//!
//! `diff.renameLimit` and `diff.renames` are read here only *while the
//! corresponding field is still unset*:
//!
//! ```c
//! if (!strcmp(k, "diff.renamelimit")) {
//!         if (s->rename_limit == -1)
//!                 s->rename_limit = git_config_int(k, v, ctx->kvi);
//!         return 0;
//! }
//! if (!strcmp(k, "status.renamelimit")) {
//!         s->rename_limit = git_config_int(k, v, ctx->kvi);
//!         return 0;
//! }
//! ```
//!
//! The guard is not just precedence — it decides whether a later value is
//! *validated at all*. Once anything has written the field, a subsequent
//! `diff.renameLimit` is skipped before its value is parsed, so a garbage value
//! there is silently accepted. All four corners were measured against git 2.55.0:
//!
//! ```text
//! $ git -c status.renameLimit=5 -c diff.renameLimit=bogus status   # exit 0
//! $ git -c diff.renameLimit=bogus -c status.renameLimit=5 status   # fatal
//! $ git -c diff.renameLimit=5     -c diff.renameLimit=bogus status # exit 0
//! $ git -c diff.renameLimit=-1    -c diff.renameLimit=bogus status # fatal
//! ```
//!
//! The third and fourth differ because the guard tests for `-1` specifically:
//! after `diff.renameLimit=-1` the field is *still* -1, so the next occurrence is
//! read. `diff.renames` behaves the same way against `s->detect_rename`, whose
//! unset value is also -1 and whose `false` is 0 — so
//! `-c diff.renames=false -c diff.renames=bogus status` is clean.
//!
//! Under `git diff` neither guard exists (`git_diff_ui_config` assigns
//! unconditionally), so the same pair is fatal there. That asymmetry is real and
//! is what this module exists to reproduce.

use crate::config::{ConfigValue, walk_config};
use crate::default_config::{DefaultConfig, ObjectCreationMode, Rejection};
use crate::diff_config::git_diff_ui_config;

/// The pieces of `struct wt_status` whose *previous* value changes whether the
/// next occurrence of a key is even parsed.
struct Status {
    /// `s->rename_limit`, -1 until something sets it.
    rename_limit: i64,
    /// `s->detect_rename`, -1 until something sets it.
    detect_rename: i64,
    /// The `git_default_config` tail of the chain.
    defaults: DefaultConfig,
}

impl Status {
    fn new() -> Self {
        Status {
            rename_limit: -1,
            detect_rename: -1,
            defaults: DefaultConfig {
                object_creation_mode: ObjectCreationMode::Renames,
                sparse_expect_files_outside_of_patterns: false,
            },
        }
    }
}

/// `repo_config(r, git_status_config, &s)` — what `cmd_status` installs through
/// `status_init_config()` (builtin/commit.c:1608).
pub fn validate_status(repo: &gix::Repository) -> Result<(), Rejection> {
    let mut st = Status::new();
    for v in walk_config(repo) {
        git_status_config(&v, &mut st)?;
    }
    Ok(())
}

/// `repo_config(r, git_commit_config, &s)` — what `cmd_commit` installs.
pub fn validate_commit(repo: &gix::Repository) -> Result<(), Rejection> {
    let mut st = Status::new();
    for v in walk_config(repo) {
        git_commit_config(&v, &mut st)?;
    }
    Ok(())
}

/// `git_commit_config()` (builtin/commit.c:1669-1696).
fn git_commit_config(v: &ConfigValue, st: &mut Status) -> Result<(), Rejection> {
    let key = v.key.as_str();
    match key {
        // `git_config_pathname`, so the valueless form and an unexpandable
        // `~user`. Whether the file *exists* is not checked here — `commit` opens
        // it later and reports its own failure then.
        "commit.template" => {
            let raw = string_value(v)?;
            crate::default_config::expand_path(&raw)?;
            return Ok(());
        }
        "commit.status" | "commit.gpgsign" => {
            bool_value(v, key)?;
            return Ok(());
        }
        // `git_config_string`. The `Invalid cleanup mode` refusal is *not* here:
        // `get_cleanup_mode()` runs after option parsing, so a bogus value reaches
        // it only on a commit that gets that far.
        "commit.cleanup" => {
            string_value(v)?;
            return Ok(());
        }
        // `git_config_bool_or_int`: the boolean words first, then an integer that
        // can die through `die_bad_number`.
        "commit.verbose" => {
            bool_or_int(v, key)?;
            return Ok(());
        }
        _ => {}
    }
    git_status_config(v, st)
}

/// `git_status_config()` (builtin/commit.c:1451-1535).
fn git_status_config(v: &ConfigValue, st: &mut Status) -> Result<(), Rejection> {
    let key = v.key.as_str();

    // builtin/commit.c:1457-1458 → `git_column_config(k, v, "status", …)`
    // (column.c:328-343), which recognises `column.ui` and the one key named
    // after this command, and ignores every other `column.*`.
    if key.starts_with("column.") {
        if key == "column.ui" || key == "column.status" {
            let raw = string_value(v)?;
            if let Err(message) = crate::porcelain::column::validate_config_value(&raw) {
                // column.c:323-324 prints the token's own reason and then
                // `invalid column.<key> mode <value>`, naming the key without its
                // `column.` prefix.
                let name = key.trim_start_matches("column.");
                return Err(reported(
                    v,
                    vec![message, format!("invalid column.{name} mode {raw}")],
                ));
            }
        }
        return Ok(());
    }

    match key {
        // builtin/commit.c:1459-1466 — `git_config_bool_or_int`.
        "status.submodulesummary" => {
            bool_or_int(v, key)?;
        }
        // builtin/commit.c:1467-1493, 1503-1506 — the plain booleans.
        "status.short"
        | "status.branch"
        | "status.aheadbehind"
        | "status.showstash"
        | "status.displaycommentprefix"
        | "status.relativepaths" => {
            bool_value(v, key)?;
        }
        // builtin/commit.c:1486-1489 — `git_config_colorbool`, under either name.
        "status.color" | "color.status" => {
            if let Some(word) = v.value.as_deref() {
                if ["never", "always", "auto"]
                    .iter()
                    .any(|w| word.eq_ignore_ascii_case(w))
                {
                    return Ok(());
                }
            }
            bool_value(v, key)?;
        }
        // builtin/commit.c:1507-1515 → `parse_untracked_setting_name`
        // (builtin/commit.c:1189-1214). The boolean grammar runs first, so `1` is
        // `normal` and `off` is `no`; the three remaining words are `strcmp`.
        "status.showuntrackedfiles" => {
            let raw = v.value.as_deref().unwrap_or("");
            let word = match crate::optint::maybe_bool(raw) {
                Some(false) => "no",
                Some(true) => "normal",
                // `git_parse_maybe_bool(NULL)` is 1, i.e. `normal`; the valueless
                // form never reaches the `strcmp`s.
                None if v.value.is_none() => "normal",
                None => raw,
            };
            if !matches!(word, "no" | "normal" | "all") {
                return Err(reported(
                    v,
                    vec![format!("Invalid untracked files mode '{raw}'")],
                ));
            }
        }
        // builtin/commit.c:1516-1520 — read only while the field is unset; see
        // the module header for why that decides whether it is validated at all.
        "diff.renamelimit" => {
            if st.rename_limit == -1 {
                st.rename_limit = int_value(v, key)?;
            }
        }
        // builtin/commit.c:1521-1524 — unconditional.
        "status.renamelimit" => {
            st.rename_limit = int_value(v, key)?;
        }
        // builtin/commit.c:1525-1533 → `git_config_rename` (diff.c:211-218).
        "diff.renames" => {
            if st.detect_rename == -1 {
                st.detect_rename = config_rename(v, key)?;
            }
        }
        "status.renames" => {
            st.detect_rename = config_rename(v, key)?;
        }
        _ => {
            // builtin/commit.c:1494-1502 — the colour slots, under either
            // spelling. A slot outside the table returns before the value is read.
            for prefix in ["status.color.", "color.status."] {
                let Some(slot) = key.strip_prefix(prefix) else {
                    continue;
                };
                if !COLOR_STATUS_SLOTS.iter().any(|s| slot.eq_ignore_ascii_case(s)) {
                    return Ok(());
                }
                let raw = string_value(v)?;
                if crate::porcelain::color::parse_color_spec(&raw).is_none() {
                    return Err(reported(v, vec![format!("invalid color value: {raw}")]));
                }
                return Ok(());
            }
            // builtin/commit.c:1535 — everything else is the diff UI chain.
            return git_diff_ui_config(v, &mut st.defaults);
        }
    }
    Ok(())
}

/// `color_status_slots[]` (builtin/commit.c:93-103) plus the `added` alias
/// `parse_status_slot` checks first (:1445-1446). Compared with `strcasecmp`.
const COLOR_STATUS_SLOTS: &[&str] = &[
    "added",
    "header",
    "updated",
    "changed",
    "untracked",
    "noBranch",
    "unmerged",
    "localBranch",
    "remoteBranch",
    "branch",
];

/// `git_config_bool()` (config.c:1292-1298), with `NULL` meaning true.
fn bool_value(v: &ConfigValue, key: &str) -> Result<bool, Rejection> {
    let Some(raw) = v.value.as_deref() else {
        return Ok(true);
    };
    crate::optint::maybe_bool(raw)
        .ok_or_else(|| Rejection::Die(format!("bad boolean config value '{raw}' for '{key}'")))
}

/// `git_config_bool_or_int()` (config.c:1280-1290): the *text* boolean first —
/// `git_parse_maybe_bool_text`, so `1` is a number rather than a word — and then
/// `git_config_int`, which dies on anything the number grammar cannot read.
fn bool_or_int(v: &ConfigValue, key: &str) -> Result<i64, Rejection> {
    let raw = v.value.as_deref().unwrap_or("");
    if v.value.is_none() {
        // `git_parse_maybe_bool_text(NULL)` is 1.
        return Ok(1);
    }
    if let Some(b) = crate::optint::maybe_bool_text(raw) {
        return Ok(i64::from(b));
    }
    int_value(v, key)
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

/// `git_config_rename()` (diff.c:211-218), returning the value the caller stores
/// so the `-1` guard can see it move: `DIFF_DETECT_RENAME` is 1,
/// `DIFF_DETECT_COPY` is 2, and a false boolean is 0.
fn config_rename(v: &ConfigValue, key: &str) -> Result<i64, Rejection> {
    let Some(raw) = v.value.as_deref() else {
        return Ok(1);
    };
    if raw.eq_ignore_ascii_case("copies") || raw.eq_ignore_ascii_case("copy") {
        return Ok(2);
    }
    Ok(i64::from(bool_value(v, key)?))
}

/// Any `return error(...)` arm: the `error:` lines, then the origin-named fatal.
fn reported(v: &ConfigValue, errors: Vec<String>) -> Rejection {
    Rejection::Reported {
        errors,
        fatal: v.origin.die_linenr(&v.key),
    }
}

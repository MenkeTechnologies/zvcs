//! The per-command config callbacks that sit on top of `git_default_config()`
//! for commands outside the diff/status/log family: `grep_cmd_config`
//! (builtin/grep.c:297-327) and the `grep_config` it wraps (grep.c:59-111),
//! `git_blame_config` (builtin/blame.c:714-805), `git_fetch_config`
//! (builtin/fetch.c:115-177) and `repack_config` (builtin/repack.c:55-113).
//!
//! Each is the same shape as [`crate::diff_config`]: a chain that ends in
//! `git_default_config`, walked once per configured value in parse order. Which
//! link reports first is therefore decided by the config, not by the chain, and
//! that was measured for each of them against git 2.55.0:
//!
//! ```text
//! $ git -c blame.showRoot=bogus -c core.ignorecase=bogus blame f
//! fatal: bad boolean config value 'bogus' for 'blame.showroot'
//! $ git -c core.ignorecase=bogus -c blame.showRoot=bogus blame f
//! fatal: bad boolean config value 'bogus' for 'core.ignorecase'
//! ```
//!
//! # `grep_config` is read twice, by two different sets of commands
//!
//! `git grep` installs `grep_cmd_config`, which is `grep_config` plus
//! `git_color_config` plus `git_default_config` plus two keys of its own — one
//! chain, so the config order decides.
//!
//! The *commit-listing* commands read the same `grep.*` keys through a
//! **separate** `repo_config(grep_config, …)` that `repo_init_revisions()` runs
//! for `--grep`/`-S`, and that pass happens **after** the command's own callback
//! has finished. So for `log` the reporting order is fixed rather than
//! config-ordered, and the grep key always loses:
//!
//! ```text
//! $ git -c grep.patternType=bogus -c log.showRoot=bogus log -1
//! fatal: bad boolean config value 'bogus' for 'log.showroot'
//! $ git -c log.showRoot=bogus -c grep.patternType=bogus log -1
//! fatal: bad boolean config value 'bogus' for 'log.showroot'
//! ```
//!
//! [`validate_grep_only`] is that second pass; the dispatcher runs it after the
//! primary callback for the verbs measured to reach it (`log`, `show`,
//! `whatchanged`, `format-patch`, `range-diff`) and not for the ones that do not
//! (`rev-list`, `shortlog`, `diff-tree`, `diff`, `status`).
//!
//! # Deliberately not ported
//!
//! `userdiff_config()`, which `grep_config` and `git_blame_config` both call, is
//! left out for the reason given in [`crate::diff_config`]: its refusals are the
//! platform regex library's messages and there is no user-driver machinery here
//! for a valid value to steer.

use crate::config::{ConfigValue, walk_config};
use crate::default_config::{DefaultConfig, ObjectCreationMode, Rejection, git_default_config};
use crate::diff_config::git_color_config;

fn defaults() -> DefaultConfig {
    DefaultConfig {
        object_creation_mode: ObjectCreationMode::Renames,
        sparse_expect_files_outside_of_patterns: false,
    }
}

/// `repo_config(the_repository, grep_cmd_config, &opt)` — `git grep`
/// (builtin/grep.c:1182).
pub fn validate_grep(repo: &gix::Repository) -> Result<(), Rejection> {
    let mut out = defaults();
    for v in walk_config(repo) {
        grep_cmd_config(&v, &mut out)?;
    }
    Ok(())
}

/// `repo_config(r, grep_config, &revs->grep_filter)` — the second pass
/// `repo_init_revisions()` runs for the commit-listing commands, which validates
/// the `grep.*` keys and nothing else.
pub fn validate_grep_only(repo: &gix::Repository) -> Result<(), Rejection> {
    for v in walk_config(repo) {
        grep_config(&v)?;
    }
    Ok(())
}

/// `repo_config(r, git_blame_config, &output_option)` — `blame` and `annotate`.
pub fn validate_blame(repo: &gix::Repository) -> Result<(), Rejection> {
    let mut out = defaults();
    for v in walk_config(repo) {
        git_blame_config(&v, &mut out)?;
    }
    Ok(())
}

/// `repo_config(r, git_fetch_config, &fetch_config)` — `fetch`.
pub fn validate_fetch(repo: &gix::Repository) -> Result<(), Rejection> {
    let mut out = defaults();
    for v in walk_config(repo) {
        git_fetch_config(&v, &mut out)?;
    }
    Ok(())
}

/// `repo_config(r, repack_config, &ctx)` — `repack`.
pub fn validate_repack(repo: &gix::Repository) -> Result<(), Rejection> {
    let mut out = defaults();
    for v in walk_config(repo) {
        repack_config(&v, &mut out)?;
    }
    Ok(())
}

/// `repo_config(the_repository, git_checkout_config, opts)` — `checkout`,
/// `switch` and `restore`, all three through `checkout_main()`
/// (builtin/checkout.c:1879).
pub fn validate_checkout(repo: &gix::Repository) -> Result<(), Rejection> {
    let mut out = defaults();
    for v in walk_config(repo) {
        git_checkout_config(&v, &mut out)?;
    }
    Ok(())
}

/// `repo_config(the_repository, git_branch_config, &sorting_options)` —
/// `git branch` (builtin/branch.c:795), after `-h` and before `parse_options()`,
/// so a refused value stops a delete or a rename as surely as a listing.
///
/// Measured against git 2.55.0:
///
/// ```text
/// $ git -c submodule.recurse=abc branch -D nosuch
/// fatal: bad boolean config value 'abc' for 'submodule.recurse'
/// $ git -c submodule.recurse=abc -c color.ui=bogus branch
/// fatal: bad boolean config value 'abc' for 'submodule.recurse'
/// $ git -c color.ui=bogus -c submodule.recurse=abc branch
/// fatal: bad boolean config value 'bogus' for 'color.ui'
/// ```
pub fn validate_branch(repo: &gix::Repository) -> Result<(), Rejection> {
    let mut out = defaults();
    for v in walk_config(repo) {
        git_branch_config(&v, &mut out)?;
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// branch
// ---------------------------------------------------------------------------

/// `git_branch_config()` (builtin/branch.c:84-123).
///
/// ```c
/// if (!strcmp(var, "branch.sort")) {
///         if (!value)
///                 return config_error_nonbool(var);
///         ...
/// }
/// if (starts_with(var, "column."))
///         return git_column_config(var, value, "branch", &colopts);
/// if (!strcmp(var, "color.branch")) {
///         branch_use_color = git_config_colorbool(var, value);
///         return 0;
/// }
/// if (skip_prefix(var, "color.branch.", &slot_name)) {
///         int slot = LOOKUP_CONFIG(color_branch_slots, slot_name);
///         if (slot < 0)
///                 return 0;
///         if (!value)
///                 return config_error_nonbool(var);
///         return color_parse(value, branch_colors[slot]);
/// }
/// if (!strcmp(var, "submodule.recurse")) { ... git_config_bool ... }
/// if (!strcasecmp(var, "submodule.propagateBranches")) { ... git_config_bool ... }
///
/// if (git_color_config(var, value, cb) < 0)
///         return -1;
///
/// return git_default_config(var, value, ctx, cb);
/// ```
fn git_branch_config(v: &ConfigValue, out: &mut DefaultConfig) -> Result<(), Rejection> {
    let key = v.key.as_str();
    if key == "branch.sort" {
        string_value(v)?;
        return Ok(());
    }
    // `git_column_config()` (column.c:328-343): `column.ui` and the key named
    // after the command; every other `column.*` is ignored.
    if key.starts_with("column.") {
        if key == "column.ui" || key == "column.branch" {
            let raw = string_value(v)?;
            if let Err(message) = crate::porcelain::column::validate_config_value(&raw) {
                let name = key.trim_start_matches("column.");
                return Err(reported(
                    v,
                    vec![message, format!("invalid column.{name} mode {raw}")],
                ));
            }
        }
        return Ok(());
    }
    if key == "color.branch" {
        colorbool(v, key)?;
        return Ok(());
    }
    if let Some(slot) = key.strip_prefix("color.branch.") {
        // `LOOKUP_CONFIG` is `strcasecmp` over `color_branch_slots[]`.
        if !crate::porcelain::branch::COLOR_SLOTS
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
    if key == "submodule.recurse" || key.eq_ignore_ascii_case("submodule.propagatebranches") {
        bool_value(v, key)?;
        return Ok(());
    }
    git_color_config(v)?;
    git_default_config(v, out)
}

// ---------------------------------------------------------------------------
// checkout / switch / restore
// ---------------------------------------------------------------------------

/// `git_checkout_config()` (builtin/checkout.c:1277-1297).
///
/// ```c
/// if (!strcmp(var, "diff.ignoresubmodules")) {
///         if (!value)
///                 return config_error_nonbool(var);
///         handle_ignore_submodules_arg(&opts->diff_options, value);
///         return 0;
/// }
/// if (!strcmp(var, "checkout.guess")) {
///         opts->dwim_new_local_branch = git_config_bool(var, value);
///         return 0;
/// }
/// if (starts_with(var, "submodule."))
///         return git_default_submodule_config(var, value, NULL);
/// return git_xmerge_config(var, value, ctx, NULL);
/// ```
///
/// Note the `submodule.` arm returns without reaching `git_default_config`, and
/// that `git_default_submodule_config()` (submodule.c:216-225) reads only
/// `submodule.recurse`, as a boolean: `submodule.recurse=abc` stops all three
/// verbs before they parse their options.
fn git_checkout_config(v: &ConfigValue, out: &mut DefaultConfig) -> Result<(), Rejection> {
    let key = v.key.as_str();
    if key == "diff.ignoresubmodules" {
        let raw = string_value(v)?;
        // `handle_ignore_submodules_arg()` (submodule.c:429-445): `strcmp`
        // against four words, and `die()` for anything else.
        if !["all", "untracked", "dirty", "none"].contains(&raw.as_str()) {
            return Err(Rejection::Die(format!("bad --ignore-submodules argument: {raw}")));
        }
        return Ok(());
    }
    if key == "checkout.guess" {
        bool_value(v, key)?;
        return Ok(());
    }
    if key.starts_with("submodule.") {
        if key == "submodule.recurse" {
            bool_value(v, key)?;
        }
        return Ok(());
    }
    git_xmerge_config(v, out)
}

/// `git_xmerge_config()` (xdiff-interface.c:342-355), with the
/// `parse_conflict_style_name()` table (xdiff-interface.c:312-326) inline: three
/// exact, case-sensitive names.
fn git_xmerge_config(v: &ConfigValue, out: &mut DefaultConfig) -> Result<(), Rejection> {
    if v.key == "merge.conflictstyle" {
        let raw = string_value(v)?;
        if !["diff3", "zdiff3", "merge"].contains(&raw.as_str()) {
            return Err(reported(
                v,
                vec![format!("unknown style '{raw}' given for '{}'", v.key)],
            ));
        }
        return Ok(());
    }
    git_default_config(v, out)
}

// ---------------------------------------------------------------------------
// grep
// ---------------------------------------------------------------------------

/// `grep_cmd_config()` (builtin/grep.c:297-327).
///
/// The C runs `grep_config`, then `git_color_config`, then `git_default_config`,
/// and only *afterwards* looks at its own two keys — but it accumulates the
/// failures into `st` instead of returning early, so the first arm that fails
/// still decides. Running them in source order reproduces that.
fn grep_cmd_config(v: &ConfigValue, out: &mut DefaultConfig) -> Result<(), Rejection> {
    grep_config(v)?;
    git_color_config(v)?;
    git_default_config(v, out)?;

    // builtin/grep.c:307-321. `grep.threads` is read here rather than in
    // `grep_config` because the thread count is the builtin's, not the matcher's.
    // A negative value dies with its own message; this port is single-threaded,
    // so the `no threads support` warning at :318 never applies.
    if v.key == "grep.threads" {
        let n = int_value(v, "grep.threads")?;
        if n < 0 {
            return Err(Rejection::Die(format!(
                "invalid number of threads specified ({n}) for grep.threads"
            )));
        }
    }
    // builtin/grep.c:323-324.
    if v.key == "submodule.recurse" {
        bool_value(v, "submodule.recurse")?;
    }
    Ok(())
}

/// `grep_config()` (grep.c:59-111).
fn grep_config(v: &ConfigValue) -> Result<(), Rejection> {
    let key = v.key.as_str();
    match key {
        // grep.c:68-90 — the plain booleans.
        "grep.extendedregexp" | "grep.linenumber" | "grep.column" | "grep.fullname" => {
            bool_value(v, key)?;
            return Ok(());
        }
        // grep.c:73-76 → `parse_pattern_type_arg` (grep.c:38-51), which `die()`s
        // with `bad <var> argument: <value>` — naming the variable, since the same
        // function serves `--<opt>`.
        "grep.patterntype" => {
            let raw = v.value.as_deref().unwrap_or("");
            if !matches!(raw, "default" | "basic" | "extended" | "fixed" | "perl") {
                return Err(Rejection::Die(format!("bad {key} argument: {raw}")));
            }
            return Ok(());
        }
        // grep.c:92-93 — `git_config_colorbool`. Note the missing `return 0`: a
        // valid `color.grep` falls into the slot tests below, where it does not
        // match either.
        "color.grep" => {
            colorbool(v, key)?;
            return Ok(());
        }
        // grep.c:94-98 — `match` is an alias that sets two slots, so it is legal
        // even though the slot table does not list it.
        "color.grep.match" => {
            let raw = string_value(v)?;
            if crate::porcelain::color::parse_color_spec(&raw).is_none() {
                return Err(reported(v, vec![format!("invalid color value: {raw}")]));
            }
            return Ok(());
        }
        _ => {}
    }

    // grep.c:99-109. This is the one colour-slot arm in git that refuses an
    // *unknown* slot rather than ignoring it: `if (i < 0) return -1`, with no
    // `error()` of its own, so the whole diagnostic is the origin line.
    if let Some(slot) = key.strip_prefix("color.grep.") {
        if !COLOR_GREP_SLOTS.iter().any(|s| slot.eq_ignore_ascii_case(s)) {
            return Err(Rejection::Reported {
                errors: Vec::new(),
                fatal: v.origin.die_linenr(key),
            });
        }
        let raw = string_value(v)?;
        if crate::porcelain::color::parse_color_spec(&raw).is_none() {
            return Err(reported(v, vec![format!("invalid color value: {raw}")]));
        }
    }
    Ok(())
}

/// `color_grep_slots[]` (grep.c:26-36), matched case-insensitively.
/// `match` is handled before the table because it is an alias for two of these.
const COLOR_GREP_SLOTS: &[&str] = &[
    "context",
    "filename",
    "function",
    "lineNumber",
    "column",
    "matchContext",
    "matchSelected",
    "selected",
    "separator",
];

// ---------------------------------------------------------------------------
// blame
// ---------------------------------------------------------------------------

/// `git_blame_config()` (builtin/blame.c:714-805).
fn git_blame_config(v: &ConfigValue, out: &mut DefaultConfig) -> Result<(), Rejection> {
    let key = v.key.as_str();
    match key {
        // builtin/blame.c:717-732, 751-758 — the plain booleans.
        "blame.showroot"
        | "blame.blankboundary"
        | "blame.showemail"
        | "blame.markunblamablelines"
        | "blame.markignoredlines" => {
            bool_value(v, key)?;
            return Ok(());
        }
        // builtin/blame.c:733-738 — `git_config_string` then `parse_date_format`,
        // which dies with its own `unknown date format` message. That parser lives
        // in `crate::date`; only the valueless refusal belongs here.
        "blame.date" => {
            string_value(v)?;
            return Ok(());
        }
        // builtin/blame.c:739-750 — `git_config_pathname`.
        "blame.ignorerevsfile" => {
            let raw = string_value(v)?;
            crate::default_config::expand_path(&raw)?;
            return Ok(());
        }
        // builtin/blame.c:759-768. Both of these only *warn* on a value they
        // cannot read, and `crate::porcelain::blame` already emits git's exact
        // two-line `color.blame.repeatedLines` diagnostic and the
        // `blame.coloring` warning at its own read — for the same two verbs this
        // gate covers, so repeating them here would double every one. They are
        // deliberately not handled: the gate is only the earlier of two readers,
        // and for a key whose whole output is a warning the later reader is
        // indistinguishable from the earlier one.
        "color.blame.repeatedlines" | "color.blame.highlightrecent" => {
            return Ok(());
        }
        // builtin/blame.c:770-785 — three words, `strcmp`; an unknown one warns
        // (from `porcelain::blame`, as above) but the *valueless* spelling is
        // `config_error_nonbool`, which only this gate produces early enough:
        //
        //     error: missing value for 'blame.coloring'
        //     fatal: bad config variable 'blame.coloring' in file '.git/config' at line 9
        "blame.coloring" => {
            string_value(v)?;
            return Ok(());
        }
        // builtin/blame.c:787-797 — the same arm `git_diff_ui_config` has, with
        // the same two messages.
        "diff.algorithm" => {
            let raw = string_value(v)?;
            const ALGORITHMS: [&str; 5] =
                ["myers", "default", "minimal", "patience", "histogram"];
            if !ALGORITHMS.iter().any(|a| raw.eq_ignore_ascii_case(a)) {
                return Err(reported(
                    v,
                    vec![format!("unknown value for config '{key}': {raw}")],
                ));
            }
            return Ok(());
        }
        // builtin/blame.c:799 → `git_diff_heuristic_config` (diff.c:287-293).
        "diff.indentheuristic" => {
            bool_value(v, key)?;
            return Ok(());
        }
        _ => {}
    }
    // builtin/blame.c:803.
    git_default_config(v, out)
}

// ---------------------------------------------------------------------------
// fetch
// ---------------------------------------------------------------------------

/// `git_fetch_config()` (builtin/fetch.c:115-177).
fn git_fetch_config(v: &ConfigValue, out: &mut DefaultConfig) -> Result<(), Rejection> {
    let key = v.key.as_str();
    match key {
        // builtin/fetch.c:120-145 — the plain booleans, `submodule.recurse`
        // included (it is read as a boolean here and as a mode elsewhere).
        "fetch.all"
        | "fetch.prune"
        | "fetch.prunetags"
        | "fetch.showforcedupdates"
        | "submodule.recurse" => {
            bool_value(v, key)?;
            return Ok(());
        }
        // builtin/fetch.c:147-149 → `parse_submodule_fetchjobs`
        // (submodule-config.c:441-450).
        "submodule.fetchjobs" => {
            let n = int_value(v, key)?;
            if n < 0 {
                return Err(Rejection::Die(
                    "negative values not allowed for submodule.fetchJobs".to_string(),
                ));
            }
            return Ok(());
        }
        // builtin/fetch.c:150-153 → `parse_fetch_recurse_submodules_arg`
        // (submodule-config.c:419-439): the full boolean grammar first, then
        // `on-demand`, and a `die()` naming the variable for anything else.
        "fetch.recursesubmodules" => {
            let raw = v.value.as_deref().unwrap_or("");
            if v.value.is_some()
                && crate::optint::maybe_bool(raw).is_none()
                && raw != "on-demand"
            {
                return Err(Rejection::Die(format!("bad {key} argument: {raw}")));
            }
            return Ok(());
        }
        // builtin/fetch.c:155-162.
        "fetch.parallel" => {
            let n = int_value(v, key)?;
            if n < 0 {
                return Err(Rejection::Die(
                    "fetch.parallel cannot be negative".to_string(),
                ));
            }
            return Ok(());
        }
        // builtin/fetch.c:164-174 — `full`/`compact` with `strcasecmp`, and note
        // the missing `return 0`: a valid value falls through to
        // `git_default_config`, which does not match it either.
        "fetch.output" => {
            let raw = string_value(v)?;
            if !raw.eq_ignore_ascii_case("full") && !raw.eq_ignore_ascii_case("compact") {
                return Err(Rejection::Die(format!(
                    "invalid value for 'fetch.output': '{raw}'"
                )));
            }
        }
        _ => {}
    }
    // builtin/fetch.c:176.
    git_default_config(v, out)
}

// ---------------------------------------------------------------------------
// repack
// ---------------------------------------------------------------------------

/// `repack_config()` (builtin/repack.c:55-113).
fn repack_config(v: &ConfigValue, out: &mut DefaultConfig) -> Result<(), Rejection> {
    let key = v.key.as_str();
    match key {
        // builtin/repack.c:61-81, 98-101 — the plain booleans.
        "repack.usedeltabaseoffset"
        | "repack.packkeptobjects"
        | "repack.writebitmaps"
        | "pack.writebitmaps"
        | "repack.usedeltaislands"
        | "repack.updateserverinfo"
        | "repack.midxmustcontaincruft" => {
            bool_value(v, key)?;
            return Ok(());
        }
        // builtin/repack.c:82-97 — these are `git_config_string` because they are
        // forwarded to `pack-objects` as text, so only the valueless form fails.
        "repack.cruftwindow"
        | "repack.cruftwindowmemory"
        | "repack.cruftdepth"
        | "repack.cruftthreads" => {
            string_value(v)?;
            return Ok(());
        }
        // builtin/repack.c:102-111.
        "repack.midxsplitfactor" | "repack.midxnewlayerthreshold" => {
            int_value(v, key)?;
            return Ok(());
        }
        _ => {}
    }
    // builtin/repack.c:112.
    git_default_config(v, out)
}

// ---------------------------------------------------------------------------
// gc
// ---------------------------------------------------------------------------

/// `gc_config()` (builtin/gc.c:176-233) — the one reader in this module that is
/// **not** a callback.
///
/// It is a fixed sequence of targeted `repo_config_get_*` lookups followed by a
/// single `repo_config(the_repository, git_default_config, NULL)` at the end
/// (builtin/gc.c:232). Two consequences, both measured against git 2.55.0:
///
/// * a `gc.*` key beats a `core.*` one no matter which comes first in the config,
///   because the whole `gc.*` block runs before the default walk starts:
///
///   ```text
///   $ git -c core.ignorecase=bogus -c gc.auto=bogus gc --auto
///   fatal: bad numeric config value 'bogus' for 'gc.auto': invalid unit
///   ```
///
/// * within the block the *source order* decides, not the config order:
///
///   ```text
///   $ git -c gc.auto=bogus -c gc.packRefs=bogus gc --auto
///   fatal: bad boolean config value 'bogus' for 'gc.packrefs'
///   $ git -c gc.autoPackLimit=bogus -c gc.auto=bogus gc --auto
///   fatal: bad numeric config value 'bogus' for 'gc.auto': invalid unit
///   ```
///
/// so the lookups below are written in the C's order and must stay that way.
///
/// Each lookup is last-value-wins ([`crate::config::config_int`] and friends),
/// which is the opposite of the callback readers in the rest of this module.
///
/// `gc.maxCruftSize`, `gc.logExpiry` and `gc.repackFilter*` are read again by
/// [`crate::porcelain::gc`], which needs their values; validating them here as
/// well changes nothing observable because the gate dies first with the identical
/// message and status, and it is what puts them in the right order relative to
/// the keys around them.
pub fn validate_gc(repo: &gix::Repository) -> Result<(), Rejection> {
    // One walk for the whole block: every lookup below is last-value-wins over the
    // same merged config, and walking it once per key would re-read and re-parse
    // every config file seventeen times.
    let all = walk_config(repo);
    let last = |key: &str| -> Option<ConfigValue> {
        all.iter().filter(|v| v.key == key).next_back().cloned()
    };

    // builtin/gc.c:182-187 — `notbare` or a boolean.
    if let Some(v) = last("gc.packrefs") {
        if v.value.as_deref() != Some("notbare") {
            bool_value(&v, "gc.packrefs")?;
        }
    }

    // builtin/gc.c:189-191. The `&&` short-circuits, so the second key is only
    // read when the first resolved to "never" — which is why
    // `gc.reflogExpireUnreachable=bogus` alone runs clean under stock git.
    if expiry_is_never(last("gc.reflogexpire"), "gc.reflogexpire")? {
        expiry_is_never(last("gc.reflogexpireunreachable"), "gc.reflogexpireunreachable")?;
    }

    // builtin/gc.c:193-197.
    for key in [
        "gc.aggressivewindow",
        "gc.aggressivedepth",
        "gc.auto",
        "gc.autopacklimit",
    ] {
        if let Some(v) = last(key) {
            int_value(&v, key)?;
        }
    }
    for key in ["gc.autodetach", "gc.cruftpacks"] {
        if let Some(v) = last(key) {
            bool_value(&v, key)?;
        }
    }

    // builtin/gc.c:199 and :216-220 — the byte-sized values.
    if let Some(v) = last("gc.maxcruftsize") {
        ulong_value(&v, "gc.maxcruftsize")?;
    }

    // builtin/gc.c:201-214 — three `repo_config_get_expiry` reads in a row.
    for key in ["gc.pruneexpire", "gc.worktreepruneexpire", "gc.logexpiry"] {
        check_expiry(last(key), key)?;
    }

    for key in [
        "gc.bigpackthreshold",
        "pack.deltacachesize",
        "core.deltabasecachelimit",
    ] {
        if let Some(v) = last(key) {
            ulong_value(&v, key)?;
        }
    }

    // builtin/gc.c:222-230 — `repo_config_get_string`, so only the valueless form.
    for key in ["gc.repackfilter", "gc.repackfilterto"] {
        if let Some(v) = last(key) {
            string_value(&v)?;
        }
    }

    // builtin/gc.c:232.
    let mut out = defaults();
    for v in &all {
        git_default_config(v, &mut out)?;
    }
    Ok(())
}

/// `gc_config_is_timestamp_never()` (builtin/gc.c:113-124): whether the key is set
/// to a moment that resolves to zero, dying if `parse_expiry_date` cannot read it.
///
/// ```c
/// if (!repo_config_get_value(the_repository, var, &value) && value) {
///         if (parse_expiry_date(value, &expire))
///                 die(_("failed to parse '%s' value '%s'"), var, value);
///         return expire == 0;
/// }
/// return 0;
/// ```
///
/// Note that this is `parse_expiry_date`, not the `approxidate` comparison
/// [`check_expiry`] runs — the same section, two different notions of a bad date.
fn expiry_is_never(value: Option<ConfigValue>, key: &str) -> Result<bool, Rejection> {
    let Some(v) = value else {
        return Ok(false);
    };
    let Some(raw) = v.value.as_deref() else {
        return Ok(false);
    };
    match crate::date::parse_expiry_date(raw) {
        Some(when) => Ok(when == 0),
        None => Err(Rejection::Die(format!(
            "failed to parse '{key}' value '{raw}'"
        ))),
    }
}

/// `repo_config_get_expiry()` (config.c:2468-2481):
///
/// ```c
/// int ret = repo_config_get_string(r, key, output);
/// if (ret) return ret;
/// if (strcmp(*output, "now")) {
///         timestamp_t now = approxidate("now");
///         if (approxidate(*output) >= now)
///                 git_die_config(r, key, _("Invalid %s: '%s'"), key, *output);
/// }
/// ```
///
/// The literal `now` is a special case; everything else has to resolve to a
/// moment strictly in the past. That is wider than "unparseable", because
/// `approxidate()` answers *now* for anything it cannot read — so `bogus`, an
/// empty value, `false` and `all` are all refused while `never`, `1.day.ago` and
/// `2 weeks ago` are accepted.
fn check_expiry(value: Option<ConfigValue>, key: &str) -> Result<(), Rejection> {
    let Some(v) = value else {
        return Ok(());
    };
    let raw = string_value(&v)?;
    if raw != "now" && crate::date::approxidate(&raw) >= crate::date::now_seconds() {
        return Err(reported(&v, vec![format!("Invalid {key}: '{raw}'")]));
    }
    Ok(())
}

/// `git_config_ulong()` (config.c:1253-1259) through `die_bad_number`.
fn ulong_value(v: &ConfigValue, key: &str) -> Result<u64, Rejection> {
    let raw = v.value.as_deref().unwrap_or("");
    crate::config::parse_config_ulong(raw).map_err(|reason| {
        Rejection::Die(format!(
            "bad numeric config value '{raw}' for '{key}'{}: {reason}",
            v.origin.bad_number_clause()
        ))
    })
}

// ---------------------------------------------------------------------------
// The value readers
// ---------------------------------------------------------------------------

/// `git_config_bool()` (config.c:1292-1298), with `NULL` meaning true.
fn bool_value(v: &ConfigValue, key: &str) -> Result<bool, Rejection> {
    let Some(raw) = v.value.as_deref() else {
        return Ok(true);
    };
    crate::optint::maybe_bool(raw)
        .ok_or_else(|| Rejection::Die(format!("bad boolean config value '{raw}' for '{key}'")))
}

/// `git_config_colorbool()` (color.c:383-403).
fn colorbool(v: &ConfigValue, key: &str) -> Result<(), Rejection> {
    if let Some(word) = v.value.as_deref() {
        if ["never", "always", "auto"]
            .iter()
            .any(|w| word.eq_ignore_ascii_case(w))
        {
            return Ok(());
        }
    }
    bool_value(v, key).map(|_| ())
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

// ---------------------------------------------------------------------------
// merge
// ---------------------------------------------------------------------------

/// `repo_config(the_repository, git_merge_config, &merge_log_config)` —
/// `cmd_merge` (builtin/merge.c:1400), after `show_usage_with_options_if_asked()`
/// and before `parse_options()`, so it refuses `--abort`, `--quit` and
/// `--continue` exactly as it refuses a merge.
///
/// Measured against git 2.55.0 in a repository with a conflicted merge:
///
/// ```text
/// $ git -c diff.renameLimit=always merge --abort
/// fatal: bad numeric config value 'always' for 'diff.renamelimit': invalid unit
/// $ git -c color.ui=bogus -c merge.autostash=bogus merge --quit
/// fatal: bad boolean config value 'bogus' for 'color.ui'
/// $ git -c merge.autostash=bogus -c color.ui=bogus merge --quit
/// fatal: bad boolean config value 'bogus' for 'merge.autostash'
/// ```
pub fn validate_merge(repo: &gix::Repository) -> Result<(), Rejection> {
    let mut out = defaults();
    for v in walk_config(repo) {
        git_merge_config(&v, &mut out)?;
    }
    Ok(())
}

/// `git_merge_config()` (builtin/merge.c:661-741), keeping only the arms that
/// can refuse a value.
///
/// `branch.<current>.mergeoptions` returns before anything else and is split by
/// `porcelain::merge` itself. `merge.stat`/`merge.diffstat` and `merge.ff`
/// accept any value ("A setting from a future?"). `merge.verifysignatures`,
/// `merge.stat` and `gpg.mintrustlevel` do not return, so they still fall
/// through to `fmt_merge_msg_config` and `git_diff_ui_config`, which claim none
/// of them.
fn git_merge_config(v: &ConfigValue, out: &mut DefaultConfig) -> Result<(), Rejection> {
    let key = v.key.as_str();
    match key {
        "merge.verifysignatures" => {
            bool_value(v, key)?;
        }
        "pull.twohead" | "pull.octopus" | "commit.cleanup" => {
            string_value(v)?;
            return Ok(());
        }
        "merge.ff" => return Ok(()),
        "merge.defaulttoupstream" | "commit.gpgsign" | "merge.autostash" => {
            bool_value(v, key)?;
            return Ok(());
        }
        _ => {}
    }
    fmt_merge_msg_config(v, out)?;
    crate::diff_config::git_diff_ui_config(v, out)
}

/// `fmt_merge_msg_config()` (fmt-merge-msg.c:26-52).
///
/// ```c
/// if (!strcmp(key, "merge.log") || !strcmp(key, "merge.summary")) {
///         int is_bool;
///         *merge_log_config = git_config_bool_or_int(key, value, ctx->kvi, &is_bool);
///         if (!is_bool && *merge_log_config < 0)
///                 return error("%s: negative length %s", key, value);
///         ...
/// } else if (!strcmp(key, "merge.branchdesc")) {
///         use_branch_desc = git_config_bool(key, value);
/// } else if (!strcmp(key, "merge.suppressdest")) {
///         if (!value)
///                 return config_error_nonbool(key);
///         ...
/// } else {
///         return git_default_config(key, value, ctx, cb);
/// }
/// ```
fn fmt_merge_msg_config(v: &ConfigValue, out: &mut DefaultConfig) -> Result<(), Rejection> {
    let key = v.key.as_str();
    match key {
        "merge.log" | "merge.summary" => {
            let Some(raw) = v.value.as_deref() else {
                return Ok(());
            };
            // `git_parse_maybe_bool_text` first, so `1` is a length, not a word.
            if crate::optint::maybe_bool_text(raw).is_none() && int_value(v, key)? < 0 {
                return Err(reported(v, vec![format!("{key}: negative length {raw}")]));
            }
            Ok(())
        }
        "merge.branchdesc" => bool_value(v, key).map(|_| ()),
        "merge.suppressdest" => string_value(v).map(|_| ()),
        _ => git_default_config(v, out),
    }
}

// ---------------------------------------------------------------------------
// merge-recursive
// ---------------------------------------------------------------------------

/// `init_merge_options()` → `merge_recursive_config()` (merge-recursive.c:3847-3877)
/// — what `git merge-recursive`, its `-ours`/`-theirs` aliases and
/// `git merge-subtree` run as `cmd_merge_recursive`'s first statement, ahead of
/// `-h` and the `argc < 4` usage line.
///
/// Targeted lookups come first, each dying on a value it cannot read; then
/// `git_config(git_xmerge_config)` walks every value. Measured against git 2.55.0:
///
/// ```text
/// $ git -c merge.conflictStyle=bogus -c merge.verbosity=bogus merge-recursive
/// fatal: bad numeric config value 'bogus' for 'merge.verbosity': invalid unit
/// $ git -c core.createObject=bogus -c merge.conflictStyle=bogus merge-subtree
/// fatal: invalid mode for object creation: bogus
/// $ git -c merge.conflictStyle=bogus -c core.createObject=bogus merge-subtree
/// error: unknown style 'bogus' given for 'merge.conflictstyle'
/// fatal: unable to parse 'merge.conflictstyle' from command-line config
/// ```
///
/// `merge.directoryRenames` is read too but never refuses (the C ignores values
/// it does not know, "from future versions of git").
pub fn validate_merge_recursive(repo: &gix::Repository) -> Result<(), Rejection> {
    // `git_config_get_int()`: `die_bad_number`, with its ` in file` clause.
    for key in ["merge.verbosity", "diff.renamelimit", "merge.renamelimit"] {
        crate::config::config_int(repo, key).map_err(Rejection::Die)?;
    }
    // `git_config_get_bool("merge.renormalize")`, and `git_config_rename()`
    // (diff.c:191-198) for the two rename keys: `copies`/`copy` first, then
    // `git_config_bool`, whose refusal carries no origin.
    for key in ["merge.renormalize", "diff.renames", "merge.renames"] {
        let Some((raw, _)) = crate::config::last_value_with_origin(repo, key) else {
            continue;
        };
        let copies = key != "merge.renormalize"
            && (raw.eq_ignore_ascii_case("copies") || raw.eq_ignore_ascii_case("copy"));
        if !copies && crate::optint::maybe_bool(&raw).is_none() {
            return Err(Rejection::Die(format!("bad boolean config value '{raw}' for '{key}'")));
        }
    }
    let mut out = defaults();
    for v in walk_config(repo) {
        git_xmerge_config(&v, &mut out)?;
    }
    Ok(())
}

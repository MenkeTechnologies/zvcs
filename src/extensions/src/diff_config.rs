//! Port of `git_diff_ui_config()` / `git_diff_basic_config()` /
//! `git_diff_heuristic_config()` (diff.c:287-562) and `git_color_config()`
//! (color.c:443-451) — the config callbacks the diff-producing commands install
//! instead of `git_default_config()`.
//!
//! # The chain
//!
//! These are not independent readers; each one *ends* by calling the next, so a
//! command that installs the outermost callback validates every key all the way
//! down:
//!
//! ```text
//! git_diff_ui_config      diff.color/colorMoved/context/renames/relative/…
//!   └─ git_color_config   color.ui
//!   └─ git_diff_basic_config   diff.renameLimit, color.diff.<slot>, diff.wsErrorHighlight, diff.dirstat
//!        └─ git_diff_heuristic_config   diff.indentHeuristic
//!        └─ git_default_config          core.*, push.*, advice.*, … (crate::default_config)
//! ```
//!
//! That composition is why the whole chain has to run over each value in turn
//! rather than as several separate walks: `configset_iter()` calls the installed
//! callback once per value, and the callback is the *whole* chain. Two bad keys in
//! one config are reported in config order, not in chain order. Verified against
//! git 2.55.0:
//!
//! ```text
//! $ git -c core.ignorecase=bogus -c diff.relative=bogus diff
//! fatal: bad boolean config value 'bogus' for 'core.ignorecase'
//! $ git -c diff.relative=bogus -c core.ignorecase=bogus diff
//! fatal: bad boolean config value 'bogus' for 'diff.relative'
//! ```
//!
//! # Which commands install which callback
//!
//! Three different entry points, and the difference is observable — every row
//! below was measured with git 2.55.0 in a one-commit repository:
//!
//! | callback | refuses `diff.context=-1` | refuses `diff.renameLimit=bogus` | refuses `color.ui=bogus` |
//! |---|---|---|---|
//! | `git_diff_ui_config` (`diff`, `log`, `show`, `status`, `commit`, `format-patch`, `whatchanged`, `range-diff`) | yes | yes | yes |
//! | `git_diff_basic_config` (`diff-files`, `diff-index`, `diff-tree`, `stash`, `merge-tree`, `cherry-pick`, `revert`) | no | yes | no |
//! | `git_color_default_config` (`branch`, `add`, `clean`, `grep`, `tag`, `show-branch`) | no | no | yes |
//!
//! The plumbing row is the point of `git_diff_basic_config` existing at all:
//! diff.c:276-279 says the UI-layer defaults "should never affect" the
//! core-level commands, so `diff-files` reads the rename limit but not
//! `diff.renames`, and colours nothing.
//!
//! # What is honored, and what is only validated
//!
//! As in [`crate::default_config`], a *valid* value changes nothing here — every
//! one of these keys is read again by the verb that needs it. What this module
//! adds is git's refusal, in git's position, with git's bytes. Two arms are
//! exceptions in that they also *warn* on a value git accepts (`diff.dirstat`,
//! `diff.submodule`), and those warnings are emitted here because parse time is
//! where git emits them.
//!
//! # Deliberately not ported
//!
//! `userdiff_config()` (userdiff.c), which `git_diff_basic_config` calls between
//! the rename limit and the colour slots, owns the `diff.<driver>.funcname`,
//! `.xfuncname`, `.wordregex`, `.binary`, `.textconv`, `.cachetextconv`,
//! `.command` and `.algorithm` keys of a *user-defined driver*. Its refusals are
//! regex-compilation failures whose text comes from the platform's regex library,
//! so reproducing them means reproducing that library's messages; and this port
//! has no user-driver machinery for a valid value to steer. It is left out rather
//! than approximated.

use crate::config::{ConfigValue, walk_config};
use crate::default_config::{DefaultConfig, ObjectCreationMode, Rejection, git_default_config};

/// A fresh accumulator for the `git_default_config` tail of the chain.
fn defaults() -> DefaultConfig {
    DefaultConfig {
        object_creation_mode: ObjectCreationMode::Renames,
        sparse_expect_files_outside_of_patterns: false,
    }
}

/// `repo_config(r, git_diff_ui_config, NULL)` — the callback `diff`, `log`,
/// `show`, `format-patch`, `whatchanged` and `range-diff` install, and the one
/// `git_status_config` falls through to.
pub fn validate_ui(repo: &gix::Repository) -> Result<(), Rejection> {
    let mut out = defaults();
    for v in walk_config(repo) {
        git_diff_ui_config(&v, &mut out)?;
    }
    Ok(())
}

/// Whether this process has already printed `diff.submodule`'s
/// unknown-format warning.
///
/// git prints it once because it reads the key once, inside the config callback.
/// This port has a *second* reader — `porcelain::diff`'s own `diff.submodule`
/// lookup, which predates this module and warns again at the point it resolves
/// the format. Without this latch `git -c diff.submodule=bogus diff` prints the
/// warning twice where git prints it once, while `status`, `log`, `show` and
/// `diff-tree` (which have no second reader) print it not at all. The latch keeps
/// the first warning and drops the rest, which is git's count for every one of
/// those verbs.
///
/// It becomes dead the moment `porcelain::diff` stops warning for itself; nothing
/// else depends on it.
static SUBMODULE_WARNED: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

/// Claim the right to print `diff.submodule`'s warning; false if it is already
/// spoken for.
///
/// The dispatcher calls this ahead of the gate for a verb that warns for itself,
/// which hands that verb the one warning git would print and keeps the gate
/// quiet.
pub fn claim_submodule_warning() -> bool {
    !SUBMODULE_WARNED.swap(true, std::sync::atomic::Ordering::Relaxed)
}

/// `repo_config(r, git_diff_basic_config, NULL)` — the plumbing callback, which
/// skips the UI arms and `color.ui` entirely.
pub fn validate_basic(repo: &gix::Repository) -> Result<(), Rejection> {
    let mut out = defaults();
    for v in walk_config(repo) {
        git_diff_basic_config(&v, &mut out)?;
    }
    Ok(())
}

/// `repo_config(r, git_color_default_config, NULL)` (color.c) — `git_color_config`
/// followed by `git_default_config`, which is what the colouring porcelain that
/// produces no diff installs.
pub fn validate_color(repo: &gix::Repository) -> Result<(), Rejection> {
    let mut out = defaults();
    for v in walk_config(repo) {
        git_color_config(&v)?;
        git_default_config(&v, &mut out)?;
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// git_diff_ui_config() — diff.c:360-476
// ---------------------------------------------------------------------------

/// One value through the UI callback and everything it chains to.
pub(crate) fn git_diff_ui_config(
    v: &ConfigValue,
    out: &mut DefaultConfig,
) -> Result<(), Rejection> {
    let key = v.key.as_str();
    match key {
        // diff.c:362-365 — `git_config_colorbool`, so the three words first and
        // then a boolean that can die.
        "diff.color" | "color.diff" => {
            colorbool(v, key)?;
            return Ok(());
        }
        // diff.c:366-372 → `parse_color_moved` (diff.c:296-336).
        "diff.colormoved" => {
            parse_color_moved(v)?;
            return Ok(());
        }
        // diff.c:373-381 → `parse_color_moved_ws` (diff.c:338-374), which reports
        // *every* bad token before it fails.
        "diff.colormovedws" => {
            let raw = string_value(v)?;
            parse_color_moved_ws(v, &raw)?;
            return Ok(());
        }
        // diff.c:382-394. A negative value is a plain `return -1` with no
        // `error()` of its own, so the refusal is the origin line alone:
        //
        //     $ git -c diff.context=-1 diff
        //     fatal: unable to parse 'diff.context' from command-line config
        "diff.context" | "diff.interhunkcontext" => {
            if int_value(v, key)? < 0 {
                return Err(Rejection::Reported {
                    errors: Vec::new(),
                    fatal: v.origin.die_linenr(key),
                });
            }
            return Ok(());
        }
        // diff.c:395-398 → `git_config_rename` (diff.c:211-218).
        "diff.renames" => {
            config_rename(v, key)?;
            return Ok(());
        }
        // diff.c:399-414, 421-424, 435-438 — the plain booleans.
        "diff.autorefreshindex"
        | "diff.mnemonicprefix"
        | "diff.noprefix"
        | "diff.relative"
        | "diff.trustexitcode" => {
            bool_value(v, key)?;
            return Ok(());
        }
        // diff.c:415-420, 433-434, 439-440 — the plain strings.
        "diff.srcprefix" | "diff.dstprefix" | "diff.external" | "diff.wordregex" => {
            string_value(v)?;
            return Ok(());
        }
        // diff.c:425-432.
        "diff.statnamewidth" | "diff.statgraphwidth" => {
            int_value(v, key)?;
            return Ok(());
        }
        // diff.c:441-445 — `git_config_pathname`, so the valueless form and a
        // `~user` that cannot be expanded. The *file* is not opened until the
        // diff runs, which is why `diff.orderFile=/nonexistent` is fatal at diff
        // time (`failed to read orderfile`) rather than here.
        "diff.orderfile" => {
            let raw = string_value(v)?;
            crate::default_config::expand_path(&raw)?;
            return Ok(());
        }
        // diff.c:447-451 → `handle_ignore_submodules_arg` (submodule.c:429-449),
        // which `die()`s rather than reporting. Note the message says
        // `--ignore-submodules`, naming the *option* even though the value came
        // from configuration — that is the C, verbatim.
        //
        // Note also the missing `return 0` in this arm: a valid value falls
        // through to the `diff.submodule` test below, which does not match.
        "diff.ignoresubmodules" => {
            let raw = string_value(v)?;
            if !matches!(raw.as_str(), "all" | "untracked" | "dirty" | "none") {
                return Err(Rejection::Die(format!(
                    "bad --ignore-submodules argument: {raw}"
                )));
            }
        }
        // diff.c:453-460 → `parse_submodule_params` (diff.c:194-209): an unknown
        // format only *warns*, and the run continues with the previous format.
        "diff.submodule" => {
            let raw = string_value(v)?;
            if !matches!(raw.as_str(), "log" | "short" | "diff") && claim_submodule_warning() {
                eprintln!("warning: Unknown value for 'diff.submodule' config variable: '{raw}'");
            }
            return Ok(());
        }
        // diff.c:462-470 → `parse_algorithm_value` (diff.c:220-237), `strcasecmp`
        // against four names plus `default` as a synonym for `myers`.
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
        _ => {}
    }

    // diff.c:472-475: `git_color_config` and then the basic callback.
    git_color_config(v)?;
    git_diff_basic_config(v, out)
}

// ---------------------------------------------------------------------------
// git_diff_basic_config() — diff.c:478-562
// ---------------------------------------------------------------------------

/// One value through the plumbing callback and everything it chains to.
pub(crate) fn git_diff_basic_config(
    v: &ConfigValue,
    out: &mut DefaultConfig,
) -> Result<(), Rejection> {
    let key = v.key.as_str();

    // diff.c:482-485.
    if key == "diff.renamelimit" {
        int_value(v, key)?;
        return Ok(());
    }

    // diff.c:489-497 — `parse_diff_color_slot` (diff.c:134-140) over
    // `color_diff_slots[]` plus the `plain` alias, then `config_error_nonbool`
    // and `color_parse`. A slot outside the table returns 0 *before* the value is
    // looked at, so `color.diff.nosuchslot=nosuchcolor` runs clean.
    for prefix in ["diff.color.", "color.diff."] {
        let Some(slot) = key.strip_prefix(prefix) else {
            continue;
        };
        if !COLOR_DIFF_SLOTS.iter().any(|s| slot.eq_ignore_ascii_case(s)) {
            return Ok(());
        }
        let raw = string_value(v)?;
        if crate::porcelain::color::parse_color_spec(&raw).is_none() {
            return Err(reported(v, vec![format!("invalid color value: {raw}")]));
        }
        return Ok(());
    }

    // diff.c:499-511 → `parse_ws_error_highlight` (diff.c:249-274).
    if key == "diff.wserrorhighlight" {
        let raw = string_value(v)?;
        if !parse_ws_error_highlight(&raw) {
            return Err(reported(
                v,
                vec![format!("unknown value for config '{key}': {raw}")],
            ));
        }
        return Ok(());
    }

    // diff.c:513-520. Two spellings, and the second one is not a legal config key
    // for git's own parser to *write*, only to read.
    if key == "diff.suppressblankempty" || key == "diff.suppress-blank-empty" {
        bool_value(v, key)?;
        return Ok(());
    }

    // diff.c:522-533 → `parse_dirstat_params` (diff.c:141-192), which collects
    // every bad parameter into one `warning()` and never fails the run.
    if key == "diff.dirstat" {
        let raw = string_value(v)?;
        let errors = parse_dirstat_params(&raw);
        if !errors.is_empty() {
            // `warning(_("Found errors in 'diff.dirstat' config variable:\n%s"),
            // errmsg.buf)` — the format string carries the newline after the
            // colon, every accumulated reason already ends in one, and
            // `vreportf()` adds one more of its own. So the block ends in a blank
            // line, which is two `\n` after the last reason. Checked byte for
            // byte against git 2.55.0.
            eprint!(
                "warning: Found errors in 'diff.dirstat' config variable:\n{}\n",
                errors.join("")
            );
        }
        return Ok(());
    }

    // diff.c:535-536 → `git_diff_heuristic_config` (diff.c:287-293), then
    // diff.c:538 → `git_default_config`.
    git_diff_heuristic_config(v)?;
    git_default_config(v, out)
}

/// `git_diff_heuristic_config()` (diff.c:287-293): one boolean.
///
/// Note that this one has no `return 0` inside the `if`, so it always falls
/// through to `return 0` at the end — which is why a valid `diff.indentHeuristic`
/// still reaches `git_default_config` afterwards in the C. Nothing under `core.`
/// or the other default sections can also be `diff.indentheuristic`, so the
/// fall-through has no observable effect and this returns instead.
fn git_diff_heuristic_config(v: &ConfigValue) -> Result<(), Rejection> {
    if v.key == "diff.indentheuristic" {
        bool_value(v, "diff.indentheuristic")?;
    }
    Ok(())
}

/// `git_color_config()` (color.c:443-451): `color.ui`, and nothing else.
pub(crate) fn git_color_config(v: &ConfigValue) -> Result<(), Rejection> {
    if v.key == "color.ui" {
        colorbool(v, "color.ui")?;
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// The value parsers these arms need
// ---------------------------------------------------------------------------

/// `color_diff_slots[]` (diff.c:107-130) plus the `plain` alias
/// `parse_diff_color_slot` checks first (diff.c:136-137). Compared with
/// `strcasecmp`, so the camelCase here is documentation.
const COLOR_DIFF_SLOTS: &[&str] = &[
    "plain",
    "context",
    "meta",
    "frag",
    "old",
    "new",
    "commit",
    "whitespace",
    "func",
    "oldMoved",
    "oldMovedAlternative",
    "oldMovedDimmed",
    "oldMovedAlternativeDimmed",
    "newMoved",
    "newMovedAlternative",
    "newMovedDimmed",
    "newMovedAlternativeDimmed",
    "contextDimmed",
    "oldDimmed",
    "newDimmed",
    "contextBold",
    "oldBold",
    "newBold",
];

/// `parse_color_moved()` (diff.c:296-336).
///
/// The boolean grammar comes **first** — `git_parse_maybe_bool(arg)`, which is
/// the full word-then-integer grammar — so `1`, `on` and `0x10` are all
/// `COLOR_MOVED_DEFAULT` and `0`/`off` is `COLOR_MOVED_NO`. Only when that
/// answers -1 do the six mode names get a look, and those are `strcmp`, so
/// `Zebra` is not one of them. A valueless key is `git_parse_maybe_bool(NULL)`,
/// which is 1, so it is accepted.
fn parse_color_moved(v: &ConfigValue) -> Result<(), Rejection> {
    let Some(raw) = v.value.as_deref() else {
        return Ok(());
    };
    if crate::optint::maybe_bool(raw).is_some() {
        return Ok(());
    }
    const MODES: [&str; 7] = [
        "no",
        "plain",
        "blocks",
        "zebra",
        "default",
        "dimmed-zebra",
        "dimmed_zebra",
    ];
    if MODES.contains(&raw) {
        return Ok(());
    }
    Err(reported(
        v,
        vec![
            "color moved setting must be one of 'no', 'default', 'blocks', 'zebra', \
             'dimmed-zebra', 'plain'"
                .to_string(),
        ],
    ))
}

/// `parse_color_moved_ws()` (diff.c:338-374).
///
/// Two things make this arm different from every other closed set here: it
/// reports **each** unknown token rather than stopping at the first, and it has a
/// second error for a combination in which every token is individually valid.
/// Both `error()` lines are emitted before the single `fatal:`, so a value with
/// two bad tokens produces three lines. The split is on `,` with each token
/// trimmed (`STRING_LIST_SPLIT_TRIM`).
fn parse_color_moved_ws(v: &ConfigValue, raw: &str) -> Result<(), Rejection> {
    let mut errors = Vec::new();
    let mut allow_indentation_change = false;
    let mut whitespace_flags = false;
    for token in raw.split(',') {
        match token.trim() {
            "no" => {
                // `ret = 0` — the running set is *reset*, not extended.
                allow_indentation_change = false;
                whitespace_flags = false;
            }
            "ignore-space-change" | "ignore-space-at-eol" | "ignore-all-space" => {
                whitespace_flags = true;
            }
            "allow-indentation-change" => allow_indentation_change = true,
            other => errors.push(format!(
                "unknown color-moved-ws mode '{other}', possible values are \
                 'ignore-space-change', 'ignore-space-at-eol', 'ignore-all-space', \
                 'allow-indentation-change'"
            )),
        }
    }
    if allow_indentation_change && whitespace_flags {
        errors.push(
            "color-moved-ws: allow-indentation-change cannot be combined with other \
             whitespace modes"
                .to_string(),
        );
    }
    if errors.is_empty() {
        return Ok(());
    }
    Err(reported(v, errors))
}

/// `parse_ws_error_highlight()` (diff.c:249-274) — true when git can read the
/// whole value.
///
/// `parse_one_token` (diff.c:239-247) matches a token only when what follows it
/// is the end of the string or a comma, so `newx` is not `new` with junk: it is
/// an unknown token. An empty value parses (the `while (*arg)` never runs).
fn parse_ws_error_highlight(raw: &str) -> bool {
    if raw.is_empty() {
        return true;
    }
    raw.split(',')
        .all(|t| matches!(t, "none" | "default" | "all" | "new" | "old" | "context"))
}

/// `parse_dirstat_params()` (diff.c:141-192): the `  <reason>\n` lines it
/// accumulates for a value git will still accept.
///
/// A token starting with a digit is a cut-off percentage read by `strtoul` and an
/// optional single fractional digit; anything left over after that is
/// `Failed to parse dirstat cut-off percentage`. A token that is neither a known
/// word nor digit-initial is `Unknown dirstat parameter`. Note the two-space
/// indent and the trailing newline: they are part of each `strbuf_addf`, and the
/// `warning()` wrapper adds none of its own.
fn parse_dirstat_params(raw: &str) -> Vec<String> {
    let mut errors = Vec::new();
    if raw.is_empty() {
        return errors;
    }
    for p in raw.split(',') {
        match p {
            "changes" | "lines" | "files" | "noncumulative" | "cumulative" => continue,
            _ => {}
        }
        let first = p.as_bytes().first().copied();
        if !first.is_some_and(|b| b.is_ascii_digit()) {
            errors.push(format!("  Unknown dirstat parameter '{p}'\n"));
            continue;
        }
        // `strtoul(p, &end, 10)`, then an optional `.<digit><digits…>` tail.
        let digits = p.len() - p.trim_start_matches(|c: char| c.is_ascii_digit()).len();
        let rest = &p[digits..];
        let rest = match rest.strip_prefix('.') {
            Some(frac) if frac.starts_with(|c: char| c.is_ascii_digit()) => {
                frac.trim_start_matches(|c: char| c.is_ascii_digit())
            }
            _ => rest,
        };
        if !rest.is_empty() {
            errors.push(format!(
                "  Failed to parse dirstat cut-off percentage '{p}'\n"
            ));
        }
    }
    errors
}

/// `git_config_colorbool()` (color.c:383-403): `never`/`always`/`auto` matched
/// with `strcasecmp` and guarded by `if (value)`, then `git_config_bool`, which
/// is the only thing here that can die.
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

/// `git_config_rename()` (diff.c:211-218): a `NULL` value is `DIFF_DETECT_RENAME`,
/// `copies`/`copy` are `strcasecmp`, everything else is a boolean.
fn config_rename(v: &ConfigValue, key: &str) -> Result<(), Rejection> {
    let Some(raw) = v.value.as_deref() else {
        return Ok(());
    };
    if raw.eq_ignore_ascii_case("copies") || raw.eq_ignore_ascii_case("copy") {
        return Ok(());
    }
    bool_value(v, key).map(|_| ())
}

// The four readers below are the same `git_config_*` primitives
// `crate::default_config` uses; they are re-stated here rather than shared
// because `Rejection` is the only thing they have in common and the C spells each
// call out at its own site.

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
        None => Err(reported(
            v,
            vec![format!("missing value for '{}'", v.key)],
        )),
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

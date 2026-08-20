//! `git --list-cmds=<group>[,<group>...]` — the machine-readable command
//! listing every shell completion drives off.
//!
//! Port of `git.c`'s `list_cmds()` (git.c:78-124) and of the `parseopt`
//! special case its caller keeps out of that function (git.c:325-337). The
//! option is a *query* global: it runs inside `handle_options()`, before a
//! subcommand exists, prints one group per `,`-separated token and exits.
//!
//! ### Why the answer is derived, never transcribed
//!
//! git answers `builtins` from the `commands[]` table it dispatches through
//! (`list_builtins()`, git.c:702-715) and `main`/`others` from
//! `load_command_list()`, a live `$PATH`/exec-path scan. Both are *facts about
//! the running binary*, not documentation. Completion scripts treat them that
//! way: `contrib/completion/git-completion.bash:1261` builds its whole command
//! set from `--list-cmds=main,others,alias,nohelpers` and then offers those
//! names to the user, so a name in the list that the binary cannot run is a
//! completion that fails, and a name missing from it is a command the user
//! cannot tab to.
//!
//! This port therefore derives the same three groups from
//! [`crate::dispatch::PORCELAIN_VERBS`] / [`crate::dispatch::SUPERSET_VERBS`] —
//! the tables `dispatch::run` actually matches on — plus the same exec-path and
//! `$PATH` scans (`help::load_command_list`). A literal copy of git's list would
//! be wrong in both directions the moment either table moved: it would advertise
//! stock verbs this binary does not serve and hide every `z*` verb it does.
//!
//! The same derivation is why `others` answers differently here than stock does
//! on this machine. Stock lists every `git-z*` dashed link `git zdashed`
//! installed as an *external* command, because they are not builtins of its
//! binary; they are builtins of this one, so they appear under `builtins` and
//! `main` instead and `exclude_cmds()` keeps them out of `others`. Neither
//! listing loses a name — `main,others` covers the same set both ways.
//!
//! The documentation-driven groups are the opposite case and are handled the
//! opposite way. `list-<category>` answers from `command-list.txt`
//! (`help.c:394-416`), which classifies git's *documented* command set — `gitk`
//! and `scalar` are listed there and are not builtins of anything. Those groups
//! are read out of the same `git help -a` / `git help -g` tables this port
//! already prints ([`crate::porcelain::help::topics`]), so a category listing
//! and the manual listing can never disagree.
//!
//! ### Groups
//!
//! | token | source | git |
//! |---|---|---|
//! | `builtins` | dispatch tables | `list_builtins(&list, 0, 0)` |
//! | `main` | dispatch tables + exec-path scan | `list_all_main_cmds()` |
//! | `others` | `$PATH` scan | `list_all_other_cmds()` |
//! | `nohelpers` | filter: drop names containing `--` | `exclude_helpers_from_list()` |
//! | `alias` | `alias.<name>` config | `list_aliases()` |
//! | `config` | `completion.commands` config | `list_cmds_by_config()` |
//! | `deprecated` | [`DEPRECATED`] ∩ dispatch tables | `list_builtins(&list, DEPRECATED, 0)` |
//! | `list-<cat>` | `git help -a`/`-g` tables | `list_cmds_by_category()` |
//! | `parseopt` | [`PARSEOPT_VERBS`] | `list_builtins(&list, 0, NO_PARSEOPT)` |
//!
//! There is **no negation syntax**. `--list-cmds=no-main` is not "everything but
//! main": `match_token()` (git.c:71-76) is an exact length-and-bytes compare, so
//! stock 2.55.0 answers `fatal: unsupported command listing type 'no-main'` and
//! exits 128, and so does this port. `nohelpers` reads like a negation but is a
//! *filter over what earlier tokens accumulated*, which is why order matters:
//! `--list-cmds=nohelpers,main` prints the full `main` list, `main,nohelpers`
//! prints it with the `--` helpers removed.

use crate::dispatch;
use crate::porcelain::help;
use std::process::ExitCode;

/// The builtins git flags `DEPRECATED` in its `commands[]` table (git.c), which
/// is the whole content of `--list-cmds=deprecated`. Intersected with the
/// dispatch tables below, so a name this port stops serving leaves the listing
/// with it rather than being advertised as a deprecated-but-present command.
const DEPRECATED: &[&str] = &["pack-redundant", "whatchanged"];

/// The verbs that answer `--git-completion-helper`, which is the only thing
/// `--list-cmds=parseopt` is used for: `git-completion.bash:3812` caches this
/// list as `__git_cmds_with_parseopt_helper` and asks *exactly* these commands
/// to enumerate their own options.
///
/// git's answer is "every builtin whose entry lacks `NO_PARSEOPT`", i.e. every
/// builtin driven by `parse_options()`, because `parse_options()` is what
/// implements `--git-completion-helper` for free. This port has no such shared
/// implementation — its option sweeps are per-command — and no verb answers the
/// flag today:
///
/// ```text
/// $ git log --git-completion-helper
/// zvcs: log: unsupported flag "--git-completion-helper"
/// ```
///
/// So the honest answer is the empty list, and `git --list-cmds=parseopt`
/// prints nothing. Naming verbs here that cannot answer would be worse than
/// printing nothing: the completion script would then run the helper on them
/// and paste this port's error text into the user's option list. Add a verb to
/// this table only together with a working `--git-completion-helper` for it;
/// `listcmds.rs`'s test asserts every name listed here really answers.
const PARSEOPT_VERBS: &[&str] = &[];

/// The `command-list.txt` attribute groups that have no heading in `git help -a`
/// and so cannot be read back out of the tables this port prints.
///
/// `main_categories[]` (help.c:45-57) is what `git help -a` walks, and it lists
/// neither `CAT_synchelpers` nor the `complete` attribute — the first because
/// those six helpers are deliberately kept out of the manual listing, the
/// second because `complete` is an *extra* attribute layered on commands that
/// already appear under their type heading (`git-completion.bash` uses it to
/// offer non-porcelain commands like `fsck` and `reflog`). Both are transcribed
/// from `command-list.txt` (git 2.55.0), the same way this port already
/// transcribes `git help -a`'s own tables, and move only when git's file does.
const SYNCHELPERS: &[&str] =
    &["http-fetch", "http-push", "receive-pack", "shell", "upload-archive", "upload-pack"];

/// Commands carrying `command-list.txt`'s `complete` attribute — the ones
/// `git-completion.bash` offers even though they are not `mainporcelain`.
/// See [`SYNCHELPERS`] for why this cannot be derived from `git help -a`.
const COMPLETE: &[&str] = &[
    "apply",
    "blame",
    "cherry",
    "config",
    "difftool",
    "fsck",
    "help",
    "instaweb",
    "mergetool",
    "prune",
    "reflog",
    "refs",
    "remote",
    "repack",
    "replace",
    "request-pull",
    "send-email",
    "show-branch",
    "stage",
    "whatchanged",
];

/// `command-list.txt`'s type attributes paired with the heading `git help -a`
/// files them under, which is how a `list-<category>` request is answered from
/// the tables this port already prints. The headings are
/// `main_categories[]`/`common_categories[]` (help.c:34-57) verbatim; the two
/// documentation categories keep git's `drop_prefix()` behaviour implicitly,
/// because the tables store the user-facing topic name (`attributes`, not
/// `gitattributes`) to begin with.
const CATEGORY_SECTIONS: &[(&str, &str)] = &[
    ("mainporcelain", "Main Porcelain Commands"),
    ("ancillarymanipulators", "Ancillary Commands / Manipulators"),
    ("ancillaryinterrogators", "Ancillary Commands / Interrogators"),
    ("foreignscminterface", "Interacting with Others"),
    ("plumbingmanipulators", "Low-level Commands / Manipulators"),
    ("plumbinginterrogators", "Low-level Commands / Interrogators"),
    ("synchingrepositories", "Low-level Commands / Syncing Repositories"),
    ("purehelpers", "Low-level Commands / Internal Helpers"),
    ("userinterfaces", "User-facing repository, command and file interfaces"),
    ("developerinterfaces", "Developer-facing file formats, protocols and other interfaces"),
    ("guide", help::GUIDES_SECTION),
];

/// The five `common_categories[]` attributes (help.c:34-44), whose members are
/// the groups `git help` prints with no `-a`. Their headings are the sentences
/// in that listing, so they are matched by heading exactly as the type
/// categories are.
const COMMON_CATEGORY_SECTIONS: &[(&str, &str)] = &[
    ("init", "start a working area (see also: git help tutorial)"),
    ("worktree", "work on the current change (see also: git help everyday)"),
    ("info", "examine the history and state (see also: git help revisions)"),
    ("history", "grow, mark and tweak your common history"),
    ("remote", "collaborate (see also: git help workflows)"),
];

/// Every verb [`crate::dispatch::run`] serves, sorted — this port's
/// `list_builtins(&list, 0, 0)`.
///
/// git's `commands[]` holds only the C builtins; the scripted commands
/// (`git-archimport`, `git-cvsimport`, `git-submodule`, …) are separate
/// executables in its exec-path and reach the listing through the `main` scan
/// instead. This binary serves all of them in-process, so they are builtins
/// *here* and belong in this group — as do the `z*` verbs, which no other git
/// has. Both differences are additive: every stock builtin is still listed.
fn builtins() -> Vec<String> {
    let mut out: Vec<String> = dispatch::PORCELAIN_VERBS
        .iter()
        .chain(dispatch::SUPERSET_VERBS.iter())
        .map(|v| (*v).to_string())
        .collect();
    out.sort();
    out.dedup();
    out
}

/// `list_all_main_cmds()`: the builtins plus the `git-*` files in the exec-path,
/// sorted and de-duplicated (`load_command_list()`'s `main_cmds`).
///
/// The union with [`builtins`] is what keeps this consistent with that group
/// *before* `git zshadow` has been run. Once the shadow is installed the
/// exec-path scan finds a `git-z*` link for every superset verb and would list
/// them anyway; without it, a completion built from `main,others` — which is how
/// `git-completion.bash:1261` builds its command set — would offer the git verbs
/// and silently omit every `z*` verb this binary dispatches.
fn main_cmds() -> Vec<String> {
    let mut out: std::collections::BTreeSet<String> = help::load_command_list().0;
    out.extend(builtins());
    out.into_iter().collect()
}

/// `list_all_other_cmds()`: the `git-*` files on `$PATH` that this installation
/// does not itself provide (`load_command_list()`'s `other_cmds`, which already
/// has `exclude_cmds()` applied).
///
/// The superset verbs are subtracted for the same reason git subtracts its own
/// builtins: `git zdashed` installs a `git-zstatus` link, and a command served
/// in-process is not an external one — listing it in both groups would make
/// `main,others` offer it twice.
fn other_cmds() -> Vec<String> {
    help::load_command_list()
        .1
        .into_iter()
        .filter(|name| !dispatch::SUPERSET_VERBS.contains(&name.as_str()))
        .collect()
}

/// One `list-<category>` request: the members of `command-list.txt`'s
/// `<category>` attribute, in the order the tables list them (git's
/// `command_list[]` is one alphabetical file, so a filtered category comes out
/// alphabetical either way).
///
/// Returns `None` for a category git does not know, which is the caller's cue
/// to `die` with git's `unsupported command listing type` — `list_cmds_by_category()`
/// (help.c:406-407) dies with the *category* alone, not the rest of the spec.
fn category(cat: &str) -> Option<Vec<String>> {
    if cat == "synchelpers" {
        return Some(SYNCHELPERS.iter().map(|s| (*s).to_string()).collect());
    }
    if cat == "complete" {
        return Some(COMPLETE.iter().map(|s| (*s).to_string()).collect());
    }
    let section = CATEGORY_SECTIONS
        .iter()
        .chain(COMMON_CATEGORY_SECTIONS.iter())
        .find(|(name, _)| *name == cat)
        .map(|(_, section)| *section)?;

    let mut members = match help::common_group(section) {
        Some(members) => members,
        None => help::topics().into_iter().filter(|t| t.section == section).map(|t| t.name).collect(),
    };
    members.sort_by_key(|name| command_list_key(name));
    Some(members)
}

/// The key `command-list.txt` is sorted by, which is the order
/// `list_cmds_by_category()` walks the generated `command_list[]` in.
///
/// The file is one C-sorted column of **manual page** names — `git-add`,
/// `gitattributes`, `gitk` — while the tables this port reads a category out of
/// hold the *topic* name `git help` takes (`add`, `attributes`, `gitk`), sorted
/// under that spelling instead. Sorting by the page name recovers the file's
/// order, which is why `gitk` and `scalar` come after `git-worktree` in the
/// listing while `git help -a` prints them among the `g`s and `s`s (that listing
/// sorts by topic, help.c's `print_cmd_by_category()`).
///
/// `scalar` is the one command git documents without the `git-` prefix — the
/// only entry in `command-list.txt` whose first column does not begin with
/// `git`, checked against git 2.55.0's file.
fn command_list_key(name: &str) -> String {
    match name {
        "scalar" => name.to_string(),
        _ if name.starts_with("git") => name.to_string(),
        _ => format!("git-{name}"),
    }
}

/// `exclude_helpers_from_list()` (git.c:59-69): drop every accumulated name
/// containing `--`. That is the whole test in the C — `strstr(…, "--")` — so it
/// catches `submodule--helper` and `credential-cache--daemon` by spelling, not
/// by any recorded property.
fn exclude_helpers(list: &mut Vec<String>) {
    list.retain(|name| !name.contains("--"));
}

/// `list_cmds_by_config()` (help.c:418-441): `completion.commands` is a
/// space-separated edit script over what the earlier tokens accumulated. A bare
/// word adds a command, a `-`-prefixed word removes one, and the list is sorted
/// and de-duplicated first so the edits apply to a canonical set.
///
/// Unset — the common case — leaves the list untouched, which is why
/// `--list-cmds=config` on its own prints nothing at all.
fn apply_completion_commands(list: &mut Vec<String>) {
    let Some(spec) = help::completion_commands() else {
        return;
    };
    list.sort();
    list.dedup();
    for token in spec.split(' ').filter(|t| !t.is_empty()) {
        match token.strip_prefix('-') {
            Some(drop) => list.retain(|name| name != drop),
            None => {
                if let Err(pos) = list.binary_search(&token.to_string()) {
                    list.insert(pos, token.to_string());
                }
            }
        }
    }
}

/// `--list-cmds=parseopt`, which git answers inside `handle_options()` rather
/// than through `list_cmds()` (git.c:327-334) and formats differently: the names
/// are printed with `printf("%s ", …)`, so they are space-separated with a
/// trailing space and **no** newline. Reproduced byte for byte, including the
/// empty output [`PARSEOPT_VERBS`] currently produces.
pub fn parseopt() -> ExitCode {
    crate::trace2::cmd_name("_query_");
    let names: Vec<&str> =
        PARSEOPT_VERBS.iter().copied().filter(|v| dispatch::is_verb(v)).collect();
    print!("{}", names.iter().map(|n| format!("{n} ")).collect::<String>());
    ExitCode::SUCCESS
}

/// `list_cmds()` (git.c:78-124): walk the `,`-separated spec left to right,
/// letting each token append to (or filter) one shared list, then print the
/// list one name per line.
///
/// The two failure spellings are git's own. A bad top-level token dies with the
/// **rest of the spec** — `die(_("…'%s'"), spec)` is handed the pointer that
/// still carries everything from the failing token onward — while a bad
/// `list-<cat>` dies with the bare category, because that message comes from
/// `list_cmds_by_category()`. Both are `die`, so both exit 128:
///
/// ```text
/// $ git --list-cmds=bogus,main
/// fatal: unsupported command listing type 'bogus,main'
/// $ git --list-cmds=list-bogus,main
/// fatal: unsupported command listing type 'bogus'
/// ```
pub fn list_cmds(spec: &str) -> ExitCode {
    crate::trace2::cmd_name("_query_");

    let mut list: Vec<String> = Vec::new();
    let mut rest = spec;
    while !rest.is_empty() {
        let (token, tail) = match rest.split_once(',') {
            Some((token, tail)) => (token, tail),
            None => (rest, ""),
        };
        match token {
            "builtins" => list.extend(builtins()),
            "main" => list.extend(main_cmds()),
            "others" => list.extend(other_cmds()),
            "nohelpers" => exclude_helpers(&mut list),
            "alias" => list.extend(help::alias_names()),
            "config" => apply_completion_commands(&mut list),
            "deprecated" => {
                list.extend(
                    DEPRECATED.iter().filter(|v| dispatch::is_verb(v)).map(|v| (*v).to_string()),
                );
            }
            _ => {
                let Some(cat) = token.strip_prefix("list-").filter(|c| !c.is_empty()) else {
                    return die_unsupported(rest);
                };
                let Some(members) = category(cat) else {
                    return die_unsupported(cat);
                };
                list.extend(members);
            }
        }
        rest = tail;
    }

    let mut out = String::new();
    for name in &list {
        out.push_str(name);
        out.push('\n');
    }
    print!("{out}");
    ExitCode::SUCCESS
}

/// `die(_("unsupported command listing type '%s'"), …)` — git's exit 128.
fn die_unsupported(what: &str) -> ExitCode {
    eprintln!("fatal: unsupported command listing type '{what}'");
    ExitCode::from(crate::fatal::EXIT_FATAL)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every verb advertised as answering `--git-completion-helper` must be a
    /// verb this binary dispatches at all. The stronger claim — that it really
    /// answers the flag — is asserted by the `list_cmds` integration test, which
    /// can run the binary.
    #[test]
    fn parseopt_verbs_are_dispatched() {
        for verb in PARSEOPT_VERBS {
            assert!(dispatch::is_verb(verb), "{verb} is listed in PARSEOPT_VERBS but not dispatched");
        }
    }

    /// The category table must name headings that exist in the tables `git help`
    /// prints; a typo would silently answer an empty list.
    #[test]
    fn every_category_resolves_to_a_non_empty_section() {
        for (name, _) in CATEGORY_SECTIONS.iter().chain(COMMON_CATEGORY_SECTIONS.iter()) {
            let members = category(name).unwrap_or_else(|| panic!("{name} unknown"));
            assert!(!members.is_empty(), "category {name} resolved to no commands");
        }
    }
}

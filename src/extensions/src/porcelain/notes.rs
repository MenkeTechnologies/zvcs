use anyhow::{anyhow, bail, Result};
use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::process::ExitCode;

use gix::bstr::{BStr, BString, ByteSlice};
use gix::hash::ObjectId;
use gix::objs::tree::{EntryKind, EntryMode};
use gix::refs::transaction::{Change, LogChange, PreviousValue, RefEdit, RefLog};
use gix::refs::Target;

use crate::cstdio::println;

/// `git notes` — add or inspect object notes.
///
/// Notes live in their own commit history (`refs/notes/commits` by default),
/// whose tree maps the hex id of an annotated object to a blob holding the note
/// text. That mapping is stored with a progressive byte-based fanout (`ab/cd…`),
/// and git re-derives the fanout depth on every write from a density heuristic
/// in `notes.c:determine_fanout()`. Both the 16-way nibble trie git builds and
/// that heuristic are ported here, so the tree — and therefore the commit id —
/// matches stock git for the same note set, at any note count.
///
/// Supported subcommands (stdout, exit code and resulting objects/refs match
/// stock git):
///   * `list [<object>]`                — all notes, or the note blob id of one
///   * `add [-f] [<object>]`            — with `-m`/`-F`/`-C` supplying the text
///   * `append [<object>]`              — with `-m`/`-F`/`-C`
///   * `copy [-f] <from> [<to>]`        — including `--stdin` and `--for-rewrite`
///   * `edit [<object>]`                — with `-m`/`-F`/`-C` (git's deprecated form)
///   * `show [<object>]`                — the note text verbatim
///   * `remove [--ignore-missing] [--stdin] [<object>...]`
///   * `prune [-n] [-v]`                — drop notes whose object is gone
///   * `merge [-s <strategy>] <ref>`, `merge --commit`, `merge --abort`
///   * `get-ref`
///
/// Top-level parsing mirrors git's `parse_options()`: `--ref`/`--no-ref` (and
/// any unambiguous prefix of either) are the only options, `--` ends option
/// parsing *and* subcommand recognition, `-h` prints the usage block on stdout,
/// and every unknown option, missing option value or unknown subcommand exits
/// 129 with the usage block on stderr.
///
/// Supported options: `--ref=<ref>` (before the subcommand, as git requires),
/// `-f`/`--force`, `-m`/`--message`, `-F`/`--file` (incl. `-` for stdin),
/// `-C`/`--reuse-message`, `--allow-empty`, `--separator[=<sep>]`,
/// `--no-separator`, `--stripspace`/`--no-stripspace`, `--ignore-missing`,
/// `--stdin`, `--for-rewrite=<cmd>`, and the merge strategies
/// `ours`/`theirs`/`union`/`cat_sort_uniq`/`manual`.
/// `GIT_NOTES_REF` and `core.notesRef` are honoured with git's precedence, and
/// `merge` without `-s` takes its strategy from `notes.<name>.mergeStrategy`
/// then `notes.mergeStrategy`. `merge`'s operand goes through
/// `expand_loose_notes_ref()`, so a name that already resolves to an object is
/// taken as written and only one that resolves to nothing gets the
/// `refs/notes/` prefix.
///
/// `copy --for-rewrite=<cmd>` and every other rewriting command share
/// [`RewriteCfg`], the port of `notes-utils.c`'s `notes_rewrite_cfg`:
/// `notes.rewrite.<cmd>`, `notes.rewriteRef`/`GIT_NOTES_REWRITE_REF` (globs
/// expanded against the ref store) and `notes.rewriteMode`/
/// `GIT_NOTES_REWRITE_MODE`, with the environment half suppressing the config
/// half rather than merely outranking it.
///
/// The editor round-trip is ported: `prepare_note_data()` writes
/// `$GIT_DIR/NOTES_EDITMSG` with the message so far (or, for `edit`, the note
/// being re-edited), git's commented template and a commented
/// `git show --stat --no-notes <object>`, opens `git_editor()` on it and
/// stripspaces what comes back — so a bare `edit`/`add`/`append`, `-e` and
/// `-c`/`--reedit-message` all work, and a dumb terminal with no editor
/// configured still reports git's `Terminal is dumb, but EDITOR unset` followed
/// by `please supply the note contents using either -m or -F option` (128).
/// A bare `add` on an object that already has a note re-enters `edit`, as
/// git's `argv[0] = "edit"` redirect does.
///
/// The manual merge strategy writes each conflict through a port of
/// `ll_merge()`: the `merge` and `conflict-marker-size` attributes of the
/// object's hex name pick the built-in text, union or binary driver or a
/// `merge.<name>.driver` command; its stdout, exit code and staged-merge state
/// match stock git.
pub fn notes(args: &[String]) -> Result<ExitCode> {
    // `dispatch::run` hands us the arguments after the `notes` verb, so `args`
    // starts at the first top-level option.
    let mut override_ref: Option<String> = None;
    let mut i = 0;
    // git registers the subcommands as options, so a `--` both ends option
    // parsing and disables subcommand recognition for what follows.
    let mut dashdash = false;

    while i < args.len() {
        let a = args[i].as_str();
        if a == "--" {
            dashdash = true;
            i += 1;
            break;
        }
        // parse_options_step() tests `--help-all` with a `strcmp()` of its own,
        // ahead of parse_long_opt(): the name never abbreviates and never takes
        // an `=<value>`, and it is not reached past the `--` handled above. This
        // table has no `PARSE_OPT_HIDDEN` entry, so `USAGE_FULL` renders the
        // same block `-h` prints.
        if a == "-h" || a == "--help-all" {
            let mut buf = Vec::new();
            print_usage(&mut buf)?;
            return Ok(super::show_usage(&String::from_utf8_lossy(&buf)));
        }
        // A lone `-` is a non-option, and so is anything not starting with `-`.
        if !a.starts_with('-') || a == "-" {
            break;
        }

        let Some(body) = a.strip_prefix("--") else {
            // No short option exists at this level, so the first character
            // after the dash is always the one git names.
            let switch = a[1..].chars().next().unwrap_or(' ');
            return top_usage(&format!("unknown switch `{switch}'"));
        };
        let (name, value) = match body.split_once('=') {
            Some((n, v)) => (n, Some(v)),
            None => (body, None),
        };

        // git's parse-options accepts any unambiguous prefix of a long name,
        // and `--ref` is the only one here, negation included.
        if is_prefix_of(name, "ref") {
            override_ref = Some(match value {
                Some(v) => v.to_string(),
                None => {
                    i += 1;
                    match args.get(i) {
                        Some(v) => v.clone(),
                        None => return missing_value("--ref"),
                    }
                }
            });
        } else if name
            .strip_prefix("no-")
            .is_some_and(|n| is_prefix_of(n, "ref"))
        {
            if value.is_some() {
                return top_usage("option `no-ref' takes no value");
            }
            // `--no-ref` clears the override, falling back to git's own
            // environment/config precedence rather than pinning a ref.
            override_ref = None;
        } else {
            return top_usage(&format!("unknown option `{body}'"));
        }
        i += 1;
    }

    let sub = match args.get(i) {
        // After `--` nothing is a subcommand any more, so a name that would
        // otherwise dispatch is reported as unknown instead.
        Some(s) if dashdash => return top_usage(&format!("unknown subcommand: `{s}'")),
        Some(s) => s.as_str(),
        None => "list",
    };
    let sub_args: &[String] = if i < args.len() { &args[i + 1..] } else { &[] };

    // `create_notes_commit()` hands `commit_tree()` no author and no committer
    // (notes-utils.c:39-41), so each notes commit takes both from
    // `git_*_info(IDENT_STRICT)` (commit.c:1709-1714): the verdict is taken here,
    // before the identity the objects are written with is injected, and reported
    // by [`notes_commit`] once the notes tree is written — a subcommand that commits
    // nothing never asks.
    let mut repo = crate::setup::discover()?;
    crate::settle_commit_identity(&mut repo);
    let notes_ref = resolve_notes_ref(&repo, override_ref.as_deref());

    match sub {
        "list" => list(&repo, &notes_ref, sub_args),
        "show" => show(&repo, &notes_ref, sub_args),
        "get-ref" => {
            // `get_ref()` runs a `parse_options()` of its own over an empty
            // table before it counts what is left, so a dashed word is an
            // unknown option rather than an extra argument — and `-h` is a
            // question the empty table still answers.
            let sub_args = match normalize_sub_args(sub_args, &EMPTY_TABLE) {
                Ok(v) => v,
                Err(code) => return Ok(code),
            };
            let mut operands = 0;
            let mut literal = false;
            for a in &sub_args {
                if literal {
                    operands += 1;
                    continue;
                }
                if a == "--" {
                    literal = true;
                    continue;
                }
                if is_help(a, "") {
                    return Ok(sub_help(GET_REF_USAGE, &[]));
                }
                if a.starts_with('-') && a != "-" {
                    return sub_usage(&unknown_opt(a), GET_REF_USAGE, &[]);
                }
                operands += 1;
            }
            if operands != 0 {
                return sub_usage("too many arguments", GET_REF_USAGE, &[]);
            }
            println!("{notes_ref}");
            Ok(ExitCode::SUCCESS)
        }
        "add" => add(&repo, &notes_ref, sub_args),
        "append" => append(&repo, &notes_ref, sub_args),
        "copy" => copy(&repo, &notes_ref, sub_args),
        "remove" => remove(&repo, &notes_ref, sub_args),
        "prune" => prune(&repo, &notes_ref, sub_args),
        "edit" => edit(&repo, &notes_ref, sub_args),
        "merge" => merge(&repo, &notes_ref, sub_args),
        _ => top_usage(&format!("unknown subcommand: `{sub}'")),
    }
}

/// True when `s` is a non-empty prefix of `full` — git's long-option prefix
/// matching, which is unambiguous here because `--ref` is the only long option.
fn is_prefix_of(s: &str, full: &str) -> bool {
    !s.is_empty() && full.starts_with(s)
}

/// `git_notes_usage[]` plus the one top-level option, as
/// `usage_with_options()` lays it out.
const NOTES_USAGE: &[&str] = &[
    "git notes [--ref <notes-ref>] [list [<object>]]",
    "git notes [--ref <notes-ref>] add [-f] [--allow-empty] [--[no-]separator|--separator=<paragraph-break>] [--[no-]stripspace] [-m <msg> | -F <file> | (-c | -C) <object>] [<object>] [-e]",
    "git notes [--ref <notes-ref>] copy [-f] <from-object> <to-object>",
    "git notes [--ref <notes-ref>] append [--allow-empty] [--[no-]separator|--separator=<paragraph-break>] [--[no-]stripspace] [-m <msg> | -F <file> | (-c | -C) <object>] [<object>] [-e]",
    "git notes [--ref <notes-ref>] edit [--allow-empty] [<object>]",
    "git notes [--ref <notes-ref>] show [<object>]",
    "git notes [--ref <notes-ref>] merge [-v | -q] [-s <strategy>] <notes-ref>",
    "git notes merge --commit [-v | -q]",
    "git notes merge --abort [-v | -q]",
    "git notes [--ref <notes-ref>] remove [<object>...]",
    "git notes [--ref <notes-ref>] prune [-n] [-v]",
    "git notes [--ref <notes-ref>] get-ref",
];

fn print_usage(out: &mut impl std::io::Write) -> Result<()> {
    for (n, l) in NOTES_USAGE.iter().enumerate() {
        writeln!(out, "{} {l}", if n == 0 { "usage:" } else { "   or:" })?;
    }
    writeln!(out)?;
    writeln!(out, "    --[no-]ref <notes-ref>")?;
    writeln!(out, "                          use notes from <notes-ref>")?;
    writeln!(out)?;
    Ok(())
}

/// A top-level usage error: `error:` then the whole usage block on stderr,
/// exit 129.
fn top_usage(msg: &str) -> Result<ExitCode> {
    eprintln!("error: {msg}");
    print_usage(&mut std::io::stderr())?;
    Ok(ExitCode::from(129))
}

// ---------------------------------------------------------------------------
// notes ref
// ---------------------------------------------------------------------------

/// The notes ref in git's precedence order: `--ref` (expanded to a full name),
/// then `GIT_NOTES_REF`, then `core.notesRef`, then `refs/notes/commits`. Only
/// the `--ref` value is expanded — git passes it through `expand_notes_ref()`
/// before exporting it, and takes the environment and config values verbatim.
pub(crate) fn resolve_notes_ref(repo: &gix::Repository, override_ref: Option<&str>) -> String {
    if let Some(r) = override_ref {
        return expand_notes_ref(r);
    }
    // `notes.c:1007` takes the environment through `xstrdup_or_null(getenv())`,
    // which is NULL only when the variable is unset: `GIT_NOTES_REF=` is a set,
    // empty notes ref, and the `core.notesref` lookup at `notes.c:1009` is then
    // skipped. The empty name reaches `init_notes_check()` (`builtin/notes.c:427`)
    // and every subcommand refuses it as outside `refs/notes/`.
    if let Some(env) = std::env::var_os("GIT_NOTES_REF") {
        return env.to_string_lossy().into_owned();
    }
    // `repo_config_get_string(repo, "core.notesref", …)` (notes.c:1009), which dies
    // through `git_die_config()` on a valueless key.
    crate::config::config_get_string(Some(repo), "core.notesref")
        .unwrap_or_else(|| "refs/notes/commits".to_string())
}

/// `notes.c:expand_notes_ref()` — a bare name becomes `refs/notes/<name>`, a
/// `notes/`-prefixed name gains `refs/`, a full name is left alone.
pub(crate) fn expand_notes_ref(name: &str) -> String {
    if name.starts_with("refs/notes/") {
        name.to_string()
    } else if name.starts_with("notes/") {
        format!("refs/{name}")
    } else {
        format!("refs/notes/{name}")
    }
}

// ---------------------------------------------------------------------------
// the notes tree
// ---------------------------------------------------------------------------

/// The loaded contents of a notes tree: the note mapping plus any entries that
/// do not follow the note naming convention, which git preserves verbatim.
pub(crate) struct Notes {
    /// annotated object id → note blob id, ordered as git emits them.
    pub(crate) map: BTreeMap<ObjectId, ObjectId>,
    /// (full path, mode, id) for entries that are not notes, sorted by path.
    non_notes: Vec<(BString, EntryMode, ObjectId)>,
}

/// Read the notes ref and load the tree it points at (empty when unborn).
pub(crate) fn load(repo: &gix::Repository, notes_ref: &str) -> Result<(Notes, Option<ObjectId>)> {
    let tip = match repo.try_find_reference(notes_ref) {
        Ok(Some(r)) => Some(r.into_fully_peeled_id()?.detach()),
        Ok(None) => None,
        // A notes ref that is not even a valid name — `--ref=` expands to the
        // bare `refs/notes/` — reads as absent, the way git's `read_ref()` does.
        Err(gix::reference::find::Error::Find(
            gix::refs::file::find::Error::RefnameValidation(_),
        )) => None,
        Err(e) => return Err(e.into()),
    };
    let notes = match tip {
        // `get_tree_entry(&object_oid, "")` (notes.c:1048-1050) peels what the
        // ref names to a tree, and dies when it cannot: a notes ref pointing at
        // a blob is not an empty notes tree.
        Some(tip) if repo.find_object(tip).ok().and_then(|o| o.peel_to_tree().ok()).is_none() => {
            eprintln!("fatal: Failed to read notes tree referenced by {notes_ref} ({tip})");
            return Err(crate::parseopt::silent(128));
        }
        Some(tip) => load_from_commit(repo, tip)?,
        None => Notes {
            map: BTreeMap::new(),
            non_notes: Vec::new(),
        },
    };
    Ok((notes, tip))
}

/// Load the notes tree carried by a specific notes commit.
fn load_from_commit(repo: &gix::Repository, commit: ObjectId) -> Result<Notes> {
    let tree_id = repo.find_commit(commit)?.tree_id()?.detach();
    let mut notes = Notes {
        map: BTreeMap::new(),
        non_notes: Vec::new(),
    };
    let hex_len = repo.object_hash().len_in_hex();
    load_subtree(repo, tree_id, "", hex_len, &mut notes)?;
    notes.non_notes.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(notes)
}

/// `notes.c:load_subtree()` — classify every entry of one fanout level.
///
/// `prefix` is the hex already consumed by the enclosing fanout directories, so
/// a name of the remaining hex length is a note and a two-character name is the
/// next fanout level. Anything else — and anything of the right length but the
/// wrong object type or not hex — is a non-note kept as-is.
fn load_subtree(
    repo: &gix::Repository,
    tree_id: ObjectId,
    prefix: &str,
    hex_len: usize,
    out: &mut Notes,
) -> Result<()> {
    // Materialise the entries so the borrow on the tree's data ends before the
    // recursive lookups, which need their own buffer.
    let entries: Vec<(EntryMode, String, ObjectId)> = repo
        .find_tree(tree_id)?
        .decode()?
        .entries
        .iter()
        .map(|e| (e.mode, e.filename.to_string(), e.oid.to_owned()))
        .collect();

    for (mode, name, oid) in entries {
        // git tests the two lengths in this order, so at the deepest level a
        // two-character name is read as the note, never as another fanout.
        let remaining = hex_len - prefix.len();
        if name.len() == remaining {
            if mode.is_blob() && is_hex(&name) {
                if let Ok(key) = ObjectId::from_hex(format!("{prefix}{name}").as_bytes()) {
                    out.map.insert(key, oid);
                    continue;
                }
            }
        } else if name.len() == 2 && mode.is_tree() && is_hex(&name) {
            load_subtree(repo, oid, &format!("{prefix}{name}"), hex_len, out)?;
            continue;
        }
        // git rebuilds the non-note's full path from the fanout it was found
        // under, which is exactly `prefix` split back into `xy/` components.
        let mut path = String::new();
        for pair in prefix.as_bytes().chunks(2) {
            path.push_str(std::str::from_utf8(pair).unwrap_or_default());
            path.push('/');
        }
        path.push_str(&name);
        out.non_notes.push((BString::from(path), mode, oid));
    }
    Ok(())
}

fn is_hex(s: &str) -> bool {
    !s.is_empty() && s.bytes().all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
}

// ---------------------------------------------------------------------------
// display: the `Notes:` block `log`, `show` and `format-patch` render
// ---------------------------------------------------------------------------

/// git's `GIT_NOTES_DEFAULT_REF` — the one ref whose block has no `(<name>)`.
pub(crate) const DEFAULT_REF: &str = "refs/notes/commits";

/// Which notes trees to display: a port of `struct display_notes_opt`.
#[derive(Clone)]
pub(crate) struct DisplayOpt {
    /// `use_default_notes`: `-1` until asked for, `1` once `--notes` with no ref
    /// (or a config default) turned the default tree on.
    use_default: i32,
    /// The `--notes=<ref>` values, already through [`expand_notes_ref`].
    extra_refs: Vec<String>,
    /// git's `rev.show_notes`.
    pub(crate) show: bool,
    /// git's `rev.show_notes_given`: whether any `--notes`/`--no-notes` was seen,
    /// which is what decides if the format's default still applies.
    pub(crate) given: bool,
}

impl Default for DisplayOpt {
    fn default() -> Self {
        DisplayOpt {
            use_default: -1,
            extra_refs: Vec::new(),
            show: false,
            given: false,
        }
    }
}

impl DisplayOpt {
    /// `enable_default_display_notes()`: `--notes`/`--show-notes` with no ref.
    pub(crate) fn enable_default(&mut self) {
        self.use_default = 1;
        self.show = true;
    }

    /// `enable_ref_display_notes()`: `--notes=<ref>`. An explicit ref suppresses
    /// both the default tree and the configured display refs.
    pub(crate) fn enable_ref(&mut self, name: &str) {
        self.extra_refs.push(expand_notes_ref(name));
        self.show = true;
    }

    /// The `--show-notes=<ref>` spelling, which is *not* `--notes=<ref>`:
    ///
    /// ```c
    /// if (starts_with(arg, "--show-notes=") &&
    ///     revs->notes_opt.use_default_notes < 0)
    ///         revs->notes_opt.use_default_notes = 1;
    /// enable_ref_display_notes(&revs->notes_opt, &revs->show_notes, optarg);
    /// ```
    ///
    /// (revision.c.) The deprecated spelling keeps the default tree alongside the
    /// ref it names, so `--show-notes=other` prints both blocks.
    pub(crate) fn enable_ref_show(&mut self, name: &str) {
        if self.use_default < 0 {
            self.use_default = 1;
        }
        self.enable_ref(name);
    }

    /// `if (!rev->show_notes_given && (!rev->pretty_given || w.notes))
    /// rev->show_notes = 1;` (builtin/log.c): the format asked for notes, which
    /// turns the *display* on and nothing else — `use_default_notes` keeps
    /// whatever `--no-standard-notes` or a `--notes=<ref>` left it at.
    pub(crate) fn show_only(&mut self) {
        self.show = true;
    }

    /// `--standard-notes`: `revs->notes_opt.use_default_notes = 1`, which adds the
    /// default tree back beside an explicit `--notes=<ref>` without turning the
    /// display on by itself.
    pub(crate) fn standard(&mut self) {
        self.use_default = 1;
    }

    /// `--no-standard-notes`: `use_default_notes = 0`. Unlike every other notes
    /// spelling it does not set `show_notes_given`, so a `%N` in the format still
    /// turns the display on — with no default tree to show.
    pub(crate) fn no_standard(&mut self) {
        self.use_default = 0;
    }

    /// `disable_display_notes()`: `--no-notes` forgets every ref asked for.
    pub(crate) fn disable(&mut self) {
        self.use_default = -1;
        self.extra_refs.clear();
        self.show = false;
    }
}

/// One loaded notes tree, with the ref it came from (which names the block).
pub(crate) struct Tree {
    ref_name: String,
    notes: Notes,
}

/// Port of `load_display_notes()` (notes.c).
///
/// The default tree is consulted when `--notes` was given without a ref (or when
/// nothing narrowed the selection), and only then are `GIT_NOTES_DISPLAY_REF`
/// and `notes.displayRef` added — an explicit `--notes=<ref>` suppresses both.
pub(crate) fn load_display(repo: &gix::Repository, opt: &DisplayOpt) -> Result<Vec<Tree>> {
    if !opt.show {
        return Ok(Vec::new());
    }
    let mut refs: Vec<String> = Vec::new();
    if opt.use_default > 0 || (opt.use_default == -1 && opt.extra_refs.is_empty()) {
        refs.push(resolve_notes_ref(repo, None));
        match std::env::var("GIT_NOTES_DISPLAY_REF") {
            Ok(v) => {
                for name in v.split(':').filter(|s| !s.is_empty()) {
                    add_by_glob(repo, &mut refs, name)?;
                }
            }
            // `repo_config(the_repository, notes_display_config, …)`: the
            // callback runs once per configured value, in configuration order,
            // so one `-c notes.displayRef=<ref>` is one value (and one warning),
            // and a valueless spelling is `config_error_nonbool()`, which
            // `configset_iter()` turns into `git_die_config_linenr()` for that
            // occurrence (notes.c:987-999, config.c:1654-1673).
            Err(_) => {
                for v in crate::config::walk_config(repo) {
                    if v.key != "notes.displayref" {
                        continue;
                    }
                    let Some(value) = v.value.as_deref() else {
                        eprintln!("error: missing value for '{}'", v.key);
                        crate::git_fatal!("{}", v.origin.die_linenr(&v.key));
                    };
                    add_by_glob(repo, &mut refs, value)?;
                }
            }
        }
    }
    for r in &opt.extra_refs {
        add_by_glob(repo, &mut refs, r)?;
    }

    let mut trees = Vec::new();
    for r in refs {
        let (notes, _) = load(repo, &r)?;
        trees.push(Tree {
            ref_name: r,
            notes,
        });
    }
    Ok(trees)
}

/// Port of `string_list_add_refs_by_glob()`: a pattern is expanded against the
/// ref store in name order, a plain name that resolves to nothing is warned
/// about but still listed, and duplicates are dropped either way.
fn add_by_glob(repo: &gix::Repository, refs: &mut Vec<String>, name: &str) -> Result<()> {
    let push = |refs: &mut Vec<String>, candidate: String| {
        if !refs.contains(&candidate) {
            refs.push(candidate);
        }
    };
    if name.contains(['*', '?', '[']) {
        let mut matched: Vec<String> = Vec::new();
        for r in repo.references()?.all()?.filter_map(std::result::Result::ok) {
            let full = r.name().as_bstr().to_str_lossy().into_owned();
            if super::log::wildmatch(name.as_bytes(), full.as_bytes()) {
                matched.push(full);
            }
        }
        matched.sort();
        for m in matched {
            push(refs, m);
        }
        return Ok(());
    }
    // `string_list_add_refs_by_glob()` → `string_list_add_one_ref()`: a name that
    // does not resolve — including one that is not a valid refname at all, which
    // is what the empty `--notes=` expands to — is warned about and kept, so the
    // walk simply finds no notes under it.
    let found = match repo.try_find_reference(name) {
        Ok(found) => found.is_some(),
        Err(gix::reference::find::Error::Find(
            gix::refs::file::find::Error::RefnameValidation(_),
        )) => false,
        Err(e) => return Err(e.into()),
    };
    if !found {
        eprintln!("warning: notes ref {name} is invalid");
    }
    push(refs, name.to_owned());
    Ok(())
}

/// Port of `format_display_notes()`/`format_note()` (notes.c).
///
/// Each tree that carries a note for `object` contributes a bare newline, a
/// `Notes[ (<ref>)]:` header, and the note's lines indented four spaces. The
/// leading newline is what renders as the blank line above the block when the
/// message before it already ended in one — and as no blank at all after a
/// `--pretty=oneline` subject, which is exactly git's own spacing.
///
/// `raw` is `%N`'s expansion: the note text alone, no header and no indent.
///
/// This is [`format_display_encoded`] with no output encoding, i.e. the note
/// bytes exactly as stored.
pub(crate) fn format_display(
    repo: &gix::Repository,
    trees: &[Tree],
    object: ObjectId,
    raw: bool,
) -> Result<Vec<u8>> {
    format_display_encoded(repo, trees, object, raw, "")
}

/// [`format_display`] with `format_note()`'s `output_encoding` (notes.c:1305-1312):
/// a note is stored as UTF-8, so under any other non-empty encoding its text is
/// re-coded from UTF-8 first — and left as stored when that conversion fails.
pub(crate) fn format_display_encoded(
    repo: &gix::Repository,
    trees: &[Tree],
    object: ObjectId,
    raw: bool,
    output_encoding: &str,
) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    for t in trees {
        let Some(note_id) = t.notes.map.get(&object) else {
            continue;
        };
        let Ok(blob) = repo.find_object(*note_id) else {
            continue;
        };
        if blob.kind != gix::object::Kind::Blob {
            continue;
        }
        // `reencode_string(msg, output_encoding, utf8)` and then
        // `msglen = strlen(msg)` (notes.c:1305-1312): both ends are C strings, so a
        // UTF-16 or UTF-32 note ends at its first NUL.
        let c_string = |s: &[u8]| s.split(|&b| b == 0).next().unwrap_or_default().to_vec();
        let reencoded = (!output_encoding.is_empty()
            && !super::mailinfo::is_utf8_name(output_encoding))
        .then(|| super::mailinfo::reencode(&c_string(&blob.data), "utf-8", output_encoding))
        .flatten()
        .map(|out| c_string(&out));
        let mut msg = reencoded.as_deref().unwrap_or(blob.data.as_slice());
        // "we will end the annotation by a newline anyway"
        if msg.last() == Some(&b'\n') {
            msg = &msg[..msg.len() - 1];
        }
        if !raw {
            let label = t.ref_name.as_str();
            if label == DEFAULT_REF {
                out.extend_from_slice(b"\nNotes:\n");
            } else {
                let short = label.strip_prefix("refs/").unwrap_or(label).to_owned();
                let short = short.strip_prefix("notes/").unwrap_or(&short);
                write!(out, "\nNotes ({short}):\n")?;
            }
        }
        // `for (msg_p = msg; msg_p < msg + msglen; msg_p += linelen + 1)`
        // (notes.c:1328): each line ends at a newline or at `msglen`, so an empty
        // note prints no line and a newline left at the end opens none.
        let mut lines: Vec<&[u8]> = msg.split(|&b| b == b'\n').collect();
        if lines.last().is_some_and(|l| l.is_empty()) {
            lines.pop();
        }
        for line in lines {
            if !raw {
                out.extend_from_slice(b"    ");
            }
            out.extend_from_slice(line);
            out.push(b'\n');
        }
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// the 16-way nibble trie and git's fanout heuristic
// ---------------------------------------------------------------------------

enum Node {
    Leaf(ObjectId),
    Internal(Box<Trie>),
}

struct Trie {
    slots: [Option<Node>; 16],
}

impl Trie {
    fn new() -> Self {
        Trie {
            slots: std::array::from_fn(|_| None),
        }
    }

    /// `notes.c:note_tree_insert()` for the fully-loaded case: a slot holds a
    /// single leaf until a second key collides on that nibble, at which point it
    /// becomes an internal node holding both one nibble deeper. The resulting
    /// shape depends only on the key set, not on insertion order.
    fn insert(&mut self, n: usize, key: ObjectId) {
        let i = nibble(n, &key) as usize;
        match self.slots[i].take() {
            None => self.slots[i] = Some(Node::Leaf(key)),
            Some(Node::Internal(mut sub)) => {
                sub.insert(n + 1, key);
                self.slots[i] = Some(Node::Internal(sub));
            }
            Some(Node::Leaf(other)) => {
                if other == key {
                    self.slots[i] = Some(Node::Leaf(key));
                    return;
                }
                let mut sub = Box::new(Trie::new());
                sub.insert(n + 1, other);
                sub.insert(n + 1, key);
                self.slots[i] = Some(Node::Internal(sub));
            }
        }
    }
}

/// The `n`th nibble of `key`, most significant first — `notes.c:GET_NIBBLE()`.
fn nibble(n: usize, key: &ObjectId) -> u8 {
    let bytes = key.as_bytes();
    (bytes[n >> 1] >> ((!n & 0x01) << 2)) & 0x0f
}

/// `notes.c:determine_fanout()`.
///
/// Each on-disk fanout level spans two trie levels, so the heuristic only fires
/// on even levels at or above the current fanout depth: if every one of the 16
/// slots is an internal node there are plenty of notes below, and the fanout
/// deepens by one.
fn determine_fanout(tree: &Trie, n: usize, fanout: usize) -> usize {
    if n % 2 == 1 || n > 2 * fanout {
        return fanout;
    }
    for slot in &tree.slots {
        if !matches!(slot, Some(Node::Internal(_))) {
            return fanout;
        }
    }
    fanout + 1
}

/// `notes.c:for_each_note_helper()` — walk the trie in nibble order, recomputing
/// the fanout at each level, and emit the on-disk path of every note.
fn emit(
    tree: &Trie,
    n: usize,
    fanout: usize,
    map: &BTreeMap<ObjectId, ObjectId>,
    out: &mut Vec<(String, EntryKind, ObjectId)>,
) {
    let fanout = determine_fanout(tree, n, fanout);
    for slot in &tree.slots {
        match slot {
            Some(Node::Internal(sub)) => emit(sub, n + 1, fanout, map, out),
            Some(Node::Leaf(key)) => {
                let note = map[key];
                out.push((fanout_path(key, fanout), EntryKind::Blob, note));
            }
            None => {}
        }
    }
}

/// `notes.c:construct_path_with_fanout()` — `fanout` leading hex pairs become
/// directory components, the remaining hex is the file name.
fn fanout_path(key: &ObjectId, fanout: usize) -> String {
    let hex = key.to_hex().to_string();
    let mut path = String::with_capacity(hex.len() + fanout);
    for i in 0..fanout {
        path.push_str(&hex[2 * i..2 * i + 2]);
        path.push('/');
    }
    path.push_str(&hex[2 * fanout..]);
    path
}

/// Write `notes` out as a tree, reproducing `notes.c:write_notes_tree()`.
///
/// Non-notes are woven in by path, with a note winning over a non-note of the
/// same path — git's `write_each_non_note_until()` rule.
fn write_tree(repo: &gix::Repository, notes: &Notes) -> Result<ObjectId> {
    let mut entries: Vec<(String, EntryKind, ObjectId)> = Vec::new();
    if !notes.map.is_empty() {
        let mut trie = Trie::new();
        for key in notes.map.keys() {
            trie.insert(0, *key);
        }
        emit(&trie, 0, 0, &notes.map, &mut entries);
    }

    let note_paths: std::collections::HashSet<String> =
        entries.iter().map(|(p, _, _)| p.clone()).collect();
    let mut all: Vec<(String, EntryKind, ObjectId)> = notes
        .non_notes
        .iter()
        .filter_map(|(path, mode, id)| {
            let path = path.to_str_lossy().into_owned();
            (!note_paths.contains(&path)).then_some((path, mode.kind(), *id))
        })
        .collect();
    all.extend(entries);

    let mut editor =
        gix::objs::tree::Editor::new(gix::objs::Tree::empty(), &repo.objects, repo.object_hash());
    for (path, kind, id) in &all {
        editor.upsert(path.split('/').map(BStr::new), *kind, *id)?;
    }
    Ok(editor.write(|tree| repo.write_object(tree).map(|id| id.detach()))?)
}

/// `create_notes_commit()`'s `commit_tree()` (notes-utils.c:39-41): the commit is
/// written only once `git_author_info(IDENT_STRICT)` and
/// `git_committer_info(IDENT_STRICT)` accept the identity, and their `die()` comes
/// after the notes tree it would have pointed at is already in the object store.
fn notes_commit(
    repo: &gix::Repository,
    message: String,
    tree: ObjectId,
    parents: impl IntoIterator<Item = impl Into<ObjectId>>,
) -> Result<ObjectId> {
    let exported = EXPORTED_AUTHOR.get();
    if let Some(refusal) = crate::commit_tree_refusal(exported.is_some()) {
        return Err(refusal.clone().into_fatal());
    }
    Ok(match exported {
        Some(author) => {
            let committer = repo
                .committer()
                .ok_or_else(|| anyhow!("committer identity unknown"))??;
            repo.new_commit_as(committer, author.to_ref(&mut gix::date::parse::TimeBuf::default()), message, tree, parents)?
                .id()
                .detach()
        }
        None => repo.new_commit(message, tree, parents)?.id().detach(),
    })
}

/// The author `git commit` exported for the rest of the process.
///
/// `determine_author_info()` ends in `export_one("GIT_AUTHOR_NAME", …)`,
/// `"GIT_AUTHOR_EMAIL"` and `"GIT_AUTHOR_DATE"` (builtin/commit.c:686-688), so
/// every later `git_author_info()` in the same process — the notes commit
/// `commit_post_rewrite()` writes for `--amend` among them — takes the author of
/// the commit just made, the amended commit's original author and date included,
/// rather than the environment the command started with.
static EXPORTED_AUTHOR: std::sync::OnceLock<gix::actor::Signature> = std::sync::OnceLock::new();

/// [`EXPORTED_AUTHOR`]'s setter, for `git commit`.
pub(crate) fn export_author(author: gix::actor::Signature) {
    let _ = EXPORTED_AUTHOR.set(author);
}

/// `notes-utils.c:commit_notes()` — write the tree, commit it on top of the
/// current notes ref (a root commit when the ref is unborn), and move the ref
/// with git's `notes: `-prefixed reflog message.
fn commit_notes(
    repo: &gix::Repository,
    notes_ref: &str,
    notes: &Notes,
    parent: Option<ObjectId>,
    msg: &str,
) -> Result<()> {
    let tree_id = write_tree(repo, notes)?;
    let commit = notes_commit(repo, format!("{msg}\n"), tree_id, parent)?;
    // `refs_update_ref(…, t->update_ref, &commit_oid, NULL, 0,
    // UPDATE_REFS_DIE_ON_ERR)` (notes-utils.c:64-66): no expected old value and
    // no `REF_NO_DEREF`, so a symbolic notes ref moves its referent and both
    // reflogs record the change; a name `check_refname_format()` refuses dies
    // with refs.c's `refusing to update ref with bad name`.
    let Ok(name) = notes_ref.try_into() else {
        eprintln!(
            "fatal: update_ref failed for ref '{notes_ref}': refusing to update ref with bad name '{notes_ref}'"
        );
        return Err(crate::parseopt::silent(128));
    };
    repo.edit_reference(RefEdit {
        change: Change::Update {
            log: LogChange {
                mode: RefLog::AndReference,
                force_create_reflog: false,
                message: format!("notes: {msg}").into(),
            },
            expected: PreviousValue::Any,
            new: Target::Object(commit),
        },
        name,
        deref: true,
    })?;
    Ok(())
}

// ---------------------------------------------------------------------------
// notes-cache.c — a notes tree used as a memo rather than as annotation
// ---------------------------------------------------------------------------

/// Port of `struct notes_cache` (notes-cache.h): a notes tree keyed by *blob* id and
/// used as a content-addressed cache. `diff.<driver>.cachetextconv` is its only user
/// in git — `userdiff_get_textconv()` (userdiff.c:432-439) builds one named
/// `textconv/<driver>` whose validity string is the converter command itself.
///
/// It differs from the annotation trees the `git notes` subcommands write in three
/// ways, all of them in `notes_cache_write()` (notes-cache.c:50):
///
/// * the commit is always a **root** commit — `commit_tree(…, NULL, …)` — so a
///   rewritten cache does not chain onto the old one;
/// * its message is the validity string verbatim, with no trailing newline, which is
///   what `notes_cache_match_validity()` reads back with `%s`; and
/// * the ref moves with `UPDATE_REFS_QUIET_ON_ERR` and no expected old value, under
///   the reflog message `update notes cache`.
pub(crate) struct Cache {
    notes_ref: String,
    /// `c->validity`: the converter command. A cache whose ref names a different one
    /// is not this cache and is discarded rather than read.
    validity: String,
    notes: Notes,
}

impl Cache {
    /// `notes_cache_init()` (notes-cache.c:35): load the tree only when the ref's
    /// commit subject is the validity string, and start empty otherwise —
    /// `NOTES_INIT_EMPTY`. A converter whose command changed therefore silently
    /// invalidates every entry it wrote before.
    pub(crate) fn init(repo: &gix::Repository, name: &str, validity: &str) -> Result<Self> {
        let notes_ref = format!("refs/notes/{name}");
        let (notes, tip) = load(repo, &notes_ref)?;
        // `notes_cache_match_validity()` (notes-cache.c:9): `format_commit_message(…,
        // "%s", …)` then `strbuf_trim()`, i.e. the trimmed subject line.
        let valid = match tip {
            Some(tip) => repo
                .find_commit(tip)
                .ok()
                .and_then(|c| c.message().ok().map(|m| m.summary().to_string()))
                .is_some_and(|s| s.trim() == validity),
            None => false,
        };
        Ok(Self {
            notes_ref,
            validity: validity.to_owned(),
            notes: if valid {
                notes
            } else {
                Notes { map: BTreeMap::new(), non_notes: Vec::new() }
            },
        })
    }

    /// `notes_cache_get()` (notes-cache.c:71): the note blob's content for `key`, or
    /// `None` when the tree has no note for it.
    pub(crate) fn get(&self, repo: &gix::Repository, key: &ObjectId) -> Option<Vec<u8>> {
        let value = self.notes.map.get(key)?;
        repo.find_object(*value).ok().map(|o| o.data.clone())
    }

    /// `notes_cache_put()` (notes-cache.c:88) followed immediately by
    /// `notes_cache_write()` (notes-cache.c:50), which is how `fill_textconv()` uses
    /// them: "we could save up changes and flush them all at the end, but we would
    /// need an extra call after all diffing is done" (diff.c:7101-7105). Every miss
    /// therefore writes one more root commit.
    ///
    /// git ignores errors here — "we might be in a readonly repository" (diff.c:7098)
    /// — so a failure to write leaves the patch alone.
    pub(crate) fn put(&mut self, repo: &gix::Repository, key: ObjectId, data: &[u8]) {
        let Ok(blob) = repo.write_blob(data) else { return };
        self.notes.map.insert(key, blob.detach());
        let Ok(tree_id) = write_tree(repo, &self.notes) else { return };
        let Ok(commit) = repo.new_commit(&self.validity, tree_id, None::<ObjectId>) else {
            return;
        };
        let Ok(name) = self.notes_ref.as_str().try_into() else { return };
        let _ = repo.edit_reference(RefEdit {
            change: Change::Update {
                log: LogChange {
                    mode: RefLog::AndReference,
                    force_create_reflog: false,
                    message: "update notes cache".into(),
                },
                // `update_ref(…, NULL, 0, …)`: no expected old value.
                expected: PreviousValue::Any,
                new: Target::Object(commit.id().detach()),
            },
            name,
            deref: false,
        });
    }
}

/// `builtin/notes.c:init_notes_check()` (`builtin/notes.c:418-435`) — *every*
/// subcommand that opens a notes tree, reading ones included, dies when the ref
/// it would open is outside `refs/notes/`. The read-only callers pass
/// `flags == 0` and so check `t->ref`, the writing ones pass `NOTES_INIT_WRITABLE`
/// and check `t->update_ref`; both hold the same name here.
fn check_notes_ref(notes_ref: &str, sub: &str) -> Result<Option<ExitCode>> {
    if !notes_ref.starts_with("refs/notes/") {
        eprintln!("fatal: refusing to {sub} notes in {notes_ref} (outside of refs/notes/)");
        return Ok(Some(ExitCode::from(128)));
    }
    Ok(None)
}

// ---------------------------------------------------------------------------
// object + message helpers
// ---------------------------------------------------------------------------

/// Resolve one `<object>` argument. Notes annotate any object, so the spec is
/// not peeled — `git notes add v1.0` annotates the tag object itself.
///
/// `get_oid_basic()` returns a full-length hex object name as-is, *without*
/// checking that the object exists, so `git notes show <missing-40-hex>` reaches
/// the note lookup and reports "no note found" instead of failing to parse its
/// argument. [`crate::objname::resolve`] is that branch, and carries the
/// `warning: refname … is ambiguous.` a same-named ref earns — every notes
/// subcommand takes its `<object>` straight off argv and calls `get_oid()` once
/// for it, so once per operand is exactly right here.
fn resolve(repo: &gix::Repository, spec: &str) -> Result<ObjectId, String> {
    crate::objname::resolve(repo, spec)
        .ok_or_else(|| format!("failed to resolve '{spec}' as a valid ref."))
}

/// One `-m`/`-F`/`-C` value, carrying the per-option stripspace default that
/// git's `struct note_msg` records: set for `-m`/`-F`, clear for `-C`.
struct Msg {
    bytes: Vec<u8>,
    strip: bool,
}

/// Everything the message-taking subcommands parse in common.
struct MsgOpts {
    msgs: Vec<Msg>,
    /// `None` until `--stripspace`/`--no-stripspace` pins it (git's UNSPECIFIED).
    stripspace: Option<bool>,
    separator: Option<String>,
    allow_empty: bool,
    force: bool,
    /// `-e`/`--edit` (and `-c`, which implies it): open the editor even when a
    /// message option was given.
    use_editor: bool,
    object: Option<String>,
}

impl Default for MsgOpts {
    fn default() -> Self {
        MsgOpts {
            msgs: Vec::new(),
            stripspace: None,
            separator: Some("\n".to_string()),
            allow_empty: false,
            force: false,
            use_editor: false,
            object: None,
        }
    }
}

/// Parse the shared `add`/`append` option set, in order — the stripspace
/// default of the last message option is what git ends up using.
fn parse_msg_opts(
    repo: &gix::Repository,
    args: &[String],
    sub: &str,
) -> Result<std::result::Result<MsgOpts, ExitCode>> {
    let table = match sub {
        "append" => &APPEND_TABLE,
        "edit" => &EDIT_TABLE,
        _ => &ADD_TABLE,
    };
    let args = &match normalize_sub_args(args, table) {
        Ok(v) => v,
        Err(code) => return Ok(Err(code)),
    };
    let mut o = MsgOpts::default();
    let mut extra: Vec<String> = Vec::new();
    let mut i = 0;
    let mut literal = false;

    // Pull the separate value of a `-x <value>` style option, advancing `i`.
    fn detached(args: &[String], i: &mut usize) -> Option<String> {
        *i += 1;
        args.get(*i).cloned()
    }

    // `-m`/`-F` push text (stripspace on); `-C` pushes a blob verbatim (off).
    // Message-load failures are git's `fatal:` (128), missing values its usage
    // (129), both surfaced by an early `return Ok(Err(<exit code>))`.
    while i < args.len() {
        let a = args[i].as_str();

        if literal || !a.starts_with('-') || a == "-" {
            extra.push(a.to_string());
            i += 1;
            continue;
        }

        match a {
            "--" => literal = true,
            // Ahead of every `starts_with` arm below so that `--help-all=x`
            // stays an unknown option, as `parse_options_step()`'s exact
            // `strcmp()` leaves it.
            _ if is_help(a, msg_shorts(sub)) => return Ok(Err(msg_sub_help(sub))),
            // `OPT__FORCE` is in `add()`'s table alone.
            "-f" | "--force" if sub == "add" => o.force = true,
            "--no-force" if sub == "add" => o.force = false,
            "--allow-empty" => o.allow_empty = true,
            "--no-allow-empty" => o.allow_empty = false,
            "--stripspace" => o.stripspace = Some(true),
            "--no-stripspace" => o.stripspace = Some(false),
            "--separator" => o.separator = Some("\n".to_string()),
            "--no-separator" => o.separator = None,
            "-m" | "--message" => match detached(args, &mut i) {
                Some(v) => o.msgs.push(Msg { bytes: v.into_bytes(), strip: true }),
                None => return Ok(Err(missing_value(a)?)),
            },
            "-F" | "--file" => match detached(args, &mut i) {
                Some(v) => match read_file(&v) {
                    Ok(b) => o.msgs.push(Msg { bytes: b, strip: true }),
                    Err(m) => return Ok(Err(fatal128(&m))),
                },
                None => return Ok(Err(missing_value(a)?)),
            },
            "-C" | "--reuse-message" => match detached(args, &mut i) {
                Some(v) => match read_note_blob(repo, &v) {
                    Ok(b) => o.msgs.push(Msg { bytes: b, strip: false }),
                    Err(m) => return Ok(Err(fatal128(&m))),
                },
                None => return Ok(Err(missing_value(a)?)),
            },
            "-e" | "--edit" => o.use_editor = true,
            "--no-edit" => o.use_editor = false,
            // `parse_reedit_arg()` is `parse_reuse_arg()` plus `use_editor = 1`.
            "-c" | "--reedit-message" => match detached(args, &mut i) {
                Some(v) => match read_note_blob(repo, &v) {
                    Ok(b) => {
                        o.use_editor = true;
                        o.msgs.push(Msg { bytes: b, strip: false });
                    }
                    Err(m) => return Ok(Err(fatal128(&m))),
                },
                None => return Ok(Err(missing_value(a)?)),
            },
            _ if a.starts_with("--reedit-message=") => {
                match read_note_blob(repo, &a["--reedit-message=".len()..]) {
                    Ok(b) => {
                        o.use_editor = true;
                        o.msgs.push(Msg { bytes: b, strip: false });
                    }
                    Err(m) => return Ok(Err(fatal128(&m))),
                }
            }
            _ if a.starts_with("-c") => match read_note_blob(repo, &a[2..]) {
                Ok(b) => {
                    o.use_editor = true;
                    o.msgs.push(Msg { bytes: b, strip: false });
                }
                Err(m) => return Ok(Err(fatal128(&m))),
            },
            _ if a.starts_with("--separator=") => {
                o.separator = Some(a["--separator=".len()..].to_string())
            }
            _ if a.starts_with("--message=") => o.msgs.push(Msg {
                bytes: a.as_bytes()["--message=".len()..].to_vec(),
                strip: true,
            }),
            _ if a.starts_with("--file=") => match read_file(&a["--file=".len()..]) {
                Ok(b) => o.msgs.push(Msg { bytes: b, strip: true }),
                Err(m) => return Ok(Err(fatal128(&m))),
            },
            _ if a.starts_with("--reuse-message=") => {
                match read_note_blob(repo, &a["--reuse-message=".len()..]) {
                    Ok(b) => o.msgs.push(Msg { bytes: b, strip: false }),
                    Err(m) => return Ok(Err(fatal128(&m))),
                }
            }
            _ if a.starts_with("-m") => o.msgs.push(Msg {
                bytes: a.as_bytes()[2..].to_vec(),
                strip: true,
            }),
            _ if a.starts_with("-F") => match read_file(&a[2..]) {
                Ok(b) => o.msgs.push(Msg { bytes: b, strip: true }),
                Err(m) => return Ok(Err(fatal128(&m))),
            },
            _ if a.starts_with("-C") => match read_note_blob(repo, &a[2..]) {
                Ok(b) => o.msgs.push(Msg { bytes: b, strip: false }),
                Err(m) => return Ok(Err(fatal128(&m))),
            },
            _ => return Ok(Err(msg_sub_usage(sub, &unknown_opt(a))?)),
        }
        i += 1;
    }

    if extra.len() > 1 {
        return Ok(Err(msg_sub_usage(sub, "too many arguments")?));
    }
    o.object = extra.into_iter().next();
    Ok(Ok(o))
}

/// git's `error:`+usage wording when `-x`/`--long` is missing its value.
fn requires_value(flag: &str) -> String {
    match flag.strip_prefix("--") {
        Some(long) => format!("option `{long}' requires a value"),
        None => {
            let sw = flag[1..].chars().next().unwrap_or(' ');
            format!("switch `{sw}' requires a value")
        }
    }
}

/// A missing option argument, reported the way `parse_options()` reports it.
///
/// `parse-options.c:60` raises it through plain `error()` and returns -1, which
/// `get_value()` turns into `PARSE_OPT_ERROR` (`parse-options.c:606`); the
/// `PARSE_OPT_ERROR` arm of `parse_options()` (`parse-options.c:1198-1201`) then
/// calls `exit(129)` directly. Unlike `PARSE_OPT_UNKNOWN`
/// (`parse-options.c:1214-1223`) it never reaches `usage_with_options()`, so no
/// usage block is printed — only the one `error:` line.
fn missing_value(flag: &str) -> Result<ExitCode> {
    eprintln!("error: {}", requires_value(flag));
    Ok(ExitCode::from(129))
}

/// Print a git `fatal:` line and yield its exit code (128).
fn fatal128(msg: &str) -> ExitCode {
    eprintln!("fatal: {msg}");
    ExitCode::from(128)
}

/// Trim the Rust-only ` (os error N)` tail so the text matches git's wording.
fn os_msg(e: &std::io::Error) -> String {
    let s = e.to_string();
    match s.rfind(" (os error ") {
        Some(pos) => s[..pos].to_string(),
        None => s,
    }
}

/// The `Err` carries git's `fatal:` message body (no prefix) for a 128 exit.
fn read_file(path: &str) -> std::result::Result<Vec<u8>, String> {
    if path == "-" {
        let mut buf = Vec::new();
        std::io::stdin()
            .read_to_end(&mut buf)
            .map_err(|e| format!("cannot read '-': {}", os_msg(&e)))?;
        return Ok(buf);
    }
    std::fs::read(path).map_err(|e| format!("could not open or read '{path}': {}", os_msg(&e)))
}

/// `-C <object>`: the note text is the named blob, verbatim.
fn read_note_blob(repo: &gix::Repository, spec: &str) -> std::result::Result<Vec<u8>, String> {
    let id = resolve(repo, spec)?;
    let object = repo
        .find_object(id)
        .map_err(|_| format!("failed to read object '{spec}'."))?;
    if object.kind != gix::object::Kind::Blob {
        return Err(format!("cannot read note data from non-blob object '{spec}'."));
    }
    Ok(object.data.clone())
}

/// `builtin/notes.c:concat_messages()` — join the message list with the
/// separator, re-running stripspace over the accumulated buffer after each
/// message that asks for it (which is why `-C … -m …` ends up stripped).
fn concat_messages(o: &MsgOpts) -> Vec<u8> {
    let mut buf: Vec<u8> = Vec::new();
    for m in &o.msgs {
        if !buf.is_empty() {
            append_separator(&mut buf, o.separator.as_deref());
        }
        buf.extend_from_slice(&m.bytes);
        if o.stripspace.unwrap_or(m.strip) {
            buf = strip_space(&buf);
        }
    }
    buf
}

/// `builtin/notes.c:note_template` — the one-line hint above the commented object.
const NOTE_TEMPLATE: &str = "Write/edit the notes for the following object:";

/// `builtin/notes.c:prepare_note_data()` — the editor round-trip.
///
/// A no-op unless `-e`/`--edit` was given or no `-m`/`-F`/`-c`/`-C` was; in that
/// case `$GIT_DIR/NOTES_EDITMSG` is written with the message so far (or, when
/// `old_note` is set, that note's blob), git's commented template, and a
/// commented `git show --stat --no-notes <object>`, the editor is opened on it,
/// and what comes back replaces the message. `stripspace` is git's tri-state:
/// only an explicit `--no-stripspace` keeps the comment lines.
///
/// `Err` is git's `die(_("please supply the note contents using either -m or -F
/// option"))` — every `launch_editor()` failure lands there, after the editor
/// layer has printed its own `error:` line.
fn prepare_note_data(
    repo: &gix::Repository,
    object: &ObjectId,
    body: Vec<u8>,
    have_msgs: bool,
    use_editor: bool,
    old_note: Option<ObjectId>,
    stripspace: Option<bool>,
) -> Result<std::result::Result<Vec<u8>, ExitCode>> {
    if !use_editor && have_msgs {
        return Ok(Ok(body));
    }

    let comment = super::rebase_todo::comment_prefix(repo);
    let edit_path = repo.git_dir().join("NOTES_EDITMSG");

    let mut file: Vec<u8> = Vec::new();
    if have_msgs {
        file.extend_from_slice(&body);
    } else if let Some(old) = old_note {
        // `copy_obj_to_fd()` — the previous note verbatim, or nothing when the
        // object cannot be read (git ignores that failure here).
        if let Ok(blob) = repo.find_object(old) {
            file.extend_from_slice(&blob.data);
        }
    }
    file.push(b'\n');
    let banner = format!("\n{NOTE_TEMPLATE}\n\n");
    file.extend_from_slice(&super::stripspace::comment_lines(
        banner.as_bytes(),
        comment.as_bytes(),
    ));
    file.extend_from_slice(&super::stripspace::comment_lines(
        &show_stat(object)?,
        comment.as_bytes(),
    ));

    std::fs::write(&edit_path, &file)?;
    let edited = crate::editor::launch_editor_read(Some(repo), &edit_path, &[]).ok();
    // `free_note_data()` unlinks the scratch file whatever happened.
    let _ = std::fs::remove_file(&edit_path);

    let Some(mut buf) = edited else {
        eprintln!("fatal: please supply the note contents using either -m or -F option");
        return Ok(Err(ExitCode::from(128)));
    };
    if stripspace != Some(false) {
        buf = super::stripspace::strip_space(&buf, Some(comment.as_bytes()));
    }
    Ok(Ok(buf))
}

/// `write_commented_object()`'s child: `git show --stat --no-notes <object>`,
/// run as our own binary (git's `show.git_cmd = 1`) with its stderr inherited.
fn show_stat(object: &ObjectId) -> Result<Vec<u8>> {
    let exe = crate::hosted::git_exe()?;
    let out = std::process::Command::new(exe)
        .args(["show", "--stat", "--no-notes", &object.to_string()])
        .stdin(std::process::Stdio::null())
        .output()
        .map_err(|_| anyhow!("unable to start 'show' for object '{object}'"))?;
    if !out.status.success() {
        bail!("failed to finish 'show' for object '{object}'");
    }
    Ok(out.stdout)
}

/// `builtin/notes.c:append_separator()` — the separator always ends a line, so
/// one is added unless it already carries its own.
fn append_separator(buf: &mut Vec<u8>, separator: Option<&str>) {
    let Some(sep) = separator else { return };
    buf.extend_from_slice(sep.as_bytes());
    if !sep.ends_with('\n') {
        buf.push(b'\n');
    }
}

/// `strbuf_stripspace()` with no comment prefix: trailing whitespace goes from
/// every line, runs of blank lines collapse to one, leading and trailing blanks
/// vanish, and every kept line is re-terminated with `\n`.
fn strip_space(input: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(input.len() + 1);
    let mut empties = 0usize;
    let mut rest = input;

    while !rest.is_empty() {
        let len = match rest.iter().position(|&b| b == b'\n') {
            Some(offset) => offset + 1,
            None => rest.len(),
        };
        let (line, tail) = rest.split_at(len);
        rest = tail;

        let mut end = line.len();
        while end > 0 && line[end - 1].is_ascii_whitespace() {
            end -= 1;
        }
        if end == 0 {
            empties += 1;
            continue;
        }
        if empties > 0 && !out.is_empty() {
            out.push(b'\n');
        }
        empties = 0;
        out.extend_from_slice(&line[..end]);
        out.push(b'\n');
    }
    out
}

/// git's parse-options wording for an option it does not recognise: `--foo`
/// becomes ``unknown option `foo'`` and `-x` becomes ``unknown switch `x'``.
fn unknown_opt(a: &str) -> String {
    match a.strip_prefix("--") {
        Some(long) => format!("unknown option `{long}'"),
        None => {
            let sw = a[1..].chars().next().unwrap_or(' ');
            format!("unknown switch `{sw}'")
        }
    }
}

/// Full `usage_with_options()` output: the usage lines, a blank line, then the
/// option help (with its own trailing blank line) when the subcommand has any.
fn sub_usage(msg: &str, lines: &[&str], options: &[&str]) -> Result<ExitCode> {
    eprintln!("error: {msg}");
    eprint!("{}", rendered(lines, options));
    Ok(ExitCode::from(129))
}

/// The same block [`sub_usage`] renders, reached the way `-h` reaches it:
/// `usage_with_options_internal(..., USAGE_TO_STDOUT)` with no `error:` line.
///
/// Each `git notes` subcommand runs its own `parse_options()` over its own
/// table, so once the subcommand word has been read the help question is that
/// subcommand's — `git notes add -h` prints `git_notes_add_usage`, not
/// `git_notes_usage`. It exits 0 and on stdout, with no `error:` line; a
/// refusal exits 129 on stderr.
fn sub_help(lines: &[&str], options: &[&str]) -> ExitCode {
    super::show_usage(&rendered(lines, options))
}

/// Whether this argument is one of the spellings `parse_options()` answers
/// itself, for a subcommand whose argument-less short options are `shorts`.
///
/// No `notes` table has a `PARSE_OPT_HIDDEN` entry, so `-h` and `--help-all`
/// render the same block. `-m`/`-F`/`-c`/`-C` take values and so are absent from
/// every `shorts` below: `git notes add -Ch` reuses the note named `h`, it does
/// not ask for help.
fn is_help(tok: &str, shorts: &str) -> bool {
    super::asks_for_help(tok, shorts)
}

/// One subcommand's `struct option options[]`: the long entries in the C
/// table's order (abbreviation and `--no-` resolution read it), the short
/// characters `parse_short_opt()` knows, and the usage block a refusal prints.
struct SubTable {
    longs: &'static [super::LongOpt],
    shorts: crate::parseopt::Shorts<'static>,
    usage: fn() -> String,
}

/// `parse_options()`'s spelling rules applied to a subcommand's argv, so that
/// the exact-match loop each subcommand runs afterwards sees what
/// `parse_options_step()` would have handed its table:
///
/// * a clump of short options is split as `parse_short_opt()` walks it, a
///   value-taking character swallowing the rest of the word or the next word
///   (parse-options.c:426-461, `get_arg()` at :47-62);
/// * a long name is expanded from its unique abbreviation, `--no-<name>`
///   included, and an ambiguous one refused with the usage block
///   (`parse_long_opt()`, parse-options.c:519-594); a `PARSE_OPT_NOARG` entry
///   given `=<value>` is ``option `x' takes no value`` (:62-70); the next word
///   after a value-taking long option is its value and is copied untouched;
/// * `--end-of-options` ends option parsing as `--` does (:1113-1124); both
///   come out as `--` followed by every remaining word, verbatim.
///
/// A word no entry claims is left as typed, for the subcommand's own
/// ``unknown option`` arm.
fn normalize_sub_args(args: &[String], t: &SubTable) -> std::result::Result<Vec<String>, ExitCode> {
    let mut out = Vec::with_capacity(args.len());
    let mut rest = args.iter();
    while let Some(a) = rest.next() {
        if a == "--" || a == "--end-of-options" {
            out.push("--".to_string());
            out.extend(rest.cloned());
            break;
        }
        if !a.starts_with('-') || a == "-" || a == "--help-all" {
            out.push(a.clone());
            continue;
        }
        if let Some(body) = a.strip_prefix("--") {
            if let Some(code) = super::long_takes_no_value(a, t.longs) {
                return Err(code);
            }
            match super::canonical_long(a, t.longs) {
                super::Long::Ambiguous(first, second) => {
                    return Err(super::ambiguous_option(a, &first, &second, &(t.usage)()))
                }
                super::Long::Name(name) => out.push(name.into_owned()),
            }
            if !body.contains('=') {
                if let super::Resolved::One(opt, false) = super::resolve_long(t.longs, body) {
                    if opt.arg == super::Arg::Required {
                        if let Some(v) = rest.next() {
                            out.push(v.clone());
                        }
                    }
                }
            }
            continue;
        }
        if crate::parseopt::expand_word(&a[1..], t.shorts, &mut out) {
            if let Some(v) = rest.next() {
                out.push(v.clone());
            }
        }
    }
    Ok(out)
}

use super::{Arg, LongOpt};

/// `add()`'s table (builtin/notes.c:488-513).
const ADD_LONGS: &[LongOpt] = &[
    LongOpt { name: "message", neg: false, arg: Arg::Required },
    LongOpt { name: "file", neg: false, arg: Arg::Required },
    LongOpt { name: "reedit-message", neg: false, arg: Arg::Required },
    LongOpt { name: "edit", neg: true, arg: Arg::None },
    LongOpt { name: "reuse-message", neg: false, arg: Arg::Required },
    LongOpt { name: "allow-empty", neg: true, arg: Arg::None },
    LongOpt { name: "force", neg: true, arg: Arg::None },
    LongOpt { name: "separator", neg: true, arg: Arg::Optional },
    LongOpt { name: "stripspace", neg: true, arg: Arg::None },
];
/// `append_edit()`'s table (builtin/notes.c:667-691): no `-f`, `-C` before `-e`.
const APPEND_LONGS: &[LongOpt] = &[
    LongOpt { name: "message", neg: false, arg: Arg::Required },
    LongOpt { name: "file", neg: false, arg: Arg::Required },
    LongOpt { name: "reedit-message", neg: false, arg: Arg::Required },
    LongOpt { name: "reuse-message", neg: false, arg: Arg::Required },
    LongOpt { name: "edit", neg: true, arg: Arg::None },
    LongOpt { name: "allow-empty", neg: true, arg: Arg::None },
    LongOpt { name: "separator", neg: true, arg: Arg::Optional },
    LongOpt { name: "stripspace", neg: true, arg: Arg::None },
];
/// `copy()`'s table (builtin/notes.c:587-594).
const COPY_LONGS: &[LongOpt] = &[
    LongOpt { name: "force", neg: true, arg: Arg::None },
    LongOpt { name: "stdin", neg: true, arg: Arg::None },
    LongOpt { name: "for-rewrite", neg: true, arg: Arg::Required },
];
/// `remove_cmd()`'s table (builtin/notes.c:1039-1046).
const REMOVE_LONGS: &[LongOpt] = &[
    LongOpt { name: "ignore-missing", neg: true, arg: Arg::None },
    LongOpt { name: "stdin", neg: true, arg: Arg::None },
];
/// `prune()`'s table (builtin/notes.c:1083-1087).
const PRUNE_LONGS: &[LongOpt] = &[
    LongOpt { name: "dry-run", neg: true, arg: Arg::None },
    LongOpt { name: "verbose", neg: true, arg: Arg::None },
];
/// `merge()`'s table (builtin/notes.c:897-913); `--commit` and `--abort` are
/// `PARSE_OPT_NONEG`.
const MERGE_LONGS: &[LongOpt] = &[
    LongOpt { name: "verbose", neg: true, arg: Arg::None },
    LongOpt { name: "quiet", neg: true, arg: Arg::None },
    LongOpt { name: "strategy", neg: true, arg: Arg::Required },
    LongOpt { name: "commit", neg: false, arg: Arg::None },
    LongOpt { name: "abort", neg: false, arg: Arg::None },
];
const MSG_SHORTS: crate::parseopt::Shorts<'static> =
    crate::parseopt::Shorts { flags: "e", values: "mFcC", optargs: "", number: false };
const ADD_SHORTS: crate::parseopt::Shorts<'static> =
    crate::parseopt::Shorts { flags: "ef", values: "mFcC", optargs: "", number: false };

const ADD_TABLE: SubTable = SubTable {
    longs: ADD_LONGS,
    shorts: ADD_SHORTS,
    usage: || rendered(ADD_USAGE, ADD_OPTS),
};
const APPEND_TABLE: SubTable = SubTable {
    longs: APPEND_LONGS,
    shorts: MSG_SHORTS,
    usage: || rendered(APPEND_USAGE, APPEND_OPTS),
};
const EDIT_TABLE: SubTable = SubTable {
    longs: APPEND_LONGS,
    shorts: MSG_SHORTS,
    usage: || rendered(EDIT_USAGE, APPEND_OPTS),
};
const COPY_TABLE: SubTable = SubTable {
    longs: COPY_LONGS,
    shorts: crate::parseopt::Shorts::flags("f"),
    usage: || rendered(COPY_USAGE, COPY_OPTS),
};
const REMOVE_TABLE: SubTable = SubTable {
    longs: REMOVE_LONGS,
    shorts: crate::parseopt::Shorts::flags(""),
    usage: || rendered(REMOVE_USAGE, REMOVE_OPTS),
};
const PRUNE_TABLE: SubTable = SubTable {
    longs: PRUNE_LONGS,
    shorts: crate::parseopt::Shorts::flags("nv"),
    usage: || rendered(PRUNE_USAGE, PRUNE_OPTS),
};
const MERGE_TABLE: SubTable = SubTable {
    longs: MERGE_LONGS,
    shorts: crate::parseopt::Shorts { flags: "vq", values: "s", optargs: "", number: false },
    usage: || {
        let mut buf = Vec::new();
        let _ = merge_print_usage(&mut buf);
        String::from_utf8_lossy(&buf).into_owned()
    },
};
/// `list()`, `show()` and `get_ref()` run `parse_options()` over an empty table.
const EMPTY_TABLE: SubTable = SubTable {
    longs: &[],
    shorts: crate::parseopt::Shorts::flags(""),
    usage: String::new,
};

/// `usage_with_options_internal()`'s layout, stream-independent.
fn rendered(lines: &[&str], options: &[&str]) -> String {
    let mut out = String::new();
    for (n, l) in lines.iter().enumerate() {
        out.push_str(if n == 0 { "usage: " } else { "   or: " });
        out.push_str(l);
        out.push('\n');
    }
    out.push('\n');
    if !options.is_empty() {
        for o in options {
            out.push_str(o);
            out.push('\n');
        }
        out.push('\n');
    }
    out
}

const ADD_USAGE: &[&str] = &["git notes add [<options>] [<object>]"];
const APPEND_USAGE: &[&str] = &["git notes append [<options>] [<object>]"];
const EDIT_USAGE: &[&str] = &["git notes edit [<object>]"];
const LIST_USAGE: &[&str] = &["git notes [list [<object>]]"];
const SHOW_USAGE: &[&str] = &["git notes show [<object>]"];
const COPY_USAGE: &[&str] = &[
    "git notes copy [<options>] <from-object> <to-object>",
    "git notes copy --stdin [<from-object> <to-object>]...",
];
const REMOVE_USAGE: &[&str] = &["git notes remove [<object>]"];

const ADD_OPTS: &[&str] = &[
    "    -m, --message <message>",
    "                          note contents as a string",
    "    -F, --file <file>     note contents in a file",
    "    -c, --reedit-message <object>",
    "                          reuse and edit specified note object",
    "    -e, --[no-]edit       edit note message in editor",
    "    -C, --reuse-message <object>",
    "                          reuse specified note object",
    "    --[no-]allow-empty    allow storing empty note",
    "    -f, --[no-]force      replace existing notes",
    "    --[no-]separator[=<paragraph-break>]",
    "                          insert <paragraph-break> between paragraphs",
    "    --[no-]stripspace     remove unnecessary whitespace",
];
// `append` and `edit` share an option list: no `-f`, and `-C` before `-e`.
const APPEND_OPTS: &[&str] = &[
    "    -m, --message <message>",
    "                          note contents as a string",
    "    -F, --file <file>     note contents in a file",
    "    -c, --reedit-message <object>",
    "                          reuse and edit specified note object",
    "    -C, --reuse-message <object>",
    "                          reuse specified note object",
    "    -e, --[no-]edit       edit note message in editor",
    "    --[no-]allow-empty    allow storing empty note",
    "    --[no-]separator[=<paragraph-break>]",
    "                          insert <paragraph-break> between paragraphs",
    "    --[no-]stripspace     remove unnecessary whitespace",
];
const COPY_OPTS: &[&str] = &[
    "    -f, --[no-]force      replace existing notes",
    "    --[no-]stdin          read objects from stdin",
    "    --[no-]for-rewrite <command>",
    "                          load rewriting config for <command> (implies --stdin)",
];
const REMOVE_OPTS: &[&str] = &[
    "    --[no-]ignore-missing attempt to remove non-existent note is not an error",
    "    --[no-]stdin          read object names from the standard input",
];
const PRUNE_USAGE: &[&str] = &["git notes prune [<options>]"];
const PRUNE_OPTS: &[&str] = &[
    "    -n, --[no-]dry-run    do not remove, show only",
    "    -v, --[no-]verbose    report pruned notes",
];
/// `get_ref()`'s table (builtin/notes.c:1111) is `OPT_END()` alone, so the block
/// is the usage line and the blank line `usage_with_options_internal()` always
/// ends on.
const GET_REF_USAGE: &[&str] = &["git notes get-ref"];

/// The `add`/`append`/`edit` usage block, chosen by the subcommand name.
fn msg_sub_usage(sub: &str, msg: &str) -> Result<ExitCode> {
    match sub {
        "append" => sub_usage(msg, APPEND_USAGE, APPEND_OPTS),
        "edit" => sub_usage(msg, EDIT_USAGE, APPEND_OPTS),
        _ => sub_usage(msg, ADD_USAGE, ADD_OPTS),
    }
}

/// The argument-less short options of `add`/`append`/`edit`: `-e` for all three,
/// plus `add`'s own `OPT__FORCE`.
fn msg_shorts(sub: &str) -> &'static str {
    match sub {
        "append" | "edit" => "e",
        _ => "fe",
    }
}

/// [`msg_sub_usage`]'s help twin, for the same three subcommands.
fn msg_sub_help(sub: &str) -> ExitCode {
    match sub {
        "append" => sub_help(APPEND_USAGE, APPEND_OPTS),
        "edit" => sub_help(EDIT_USAGE, APPEND_OPTS),
        _ => sub_help(ADD_USAGE, ADD_OPTS),
    }
}

// ---------------------------------------------------------------------------
// subcommands
// ---------------------------------------------------------------------------

fn list(repo: &gix::Repository, notes_ref: &str, args: &[String]) -> Result<ExitCode> {
    // git runs parse-options first, so an unknown switch is reported before any
    // "too many arguments"; `list` itself has no options of its own.
    let args = &match normalize_sub_args(args, &EMPTY_TABLE) {
        Ok(v) => v,
        Err(code) => return Ok(code),
    };
    let mut positional: Vec<&String> = Vec::new();
    let mut literal = false;
    for a in args {
        if literal {
            positional.push(a);
            continue;
        }
        if a == "--" {
            literal = true;
            continue;
        }
        if is_help(a, "") {
            return Ok(sub_help(LIST_USAGE, &[]));
        }
        if a.starts_with('-') && a != "-" {
            return sub_usage(&unknown_opt(a), LIST_USAGE, &[]);
        }
        positional.push(a);
    }
    if positional.len() > 1 {
        return sub_usage("too many arguments", LIST_USAGE, &[]);
    }
    // `builtin/notes.c:457`: the ref check runs before the operand is resolved.
    if let Some(code) = check_notes_ref(notes_ref, "list")? {
        return Ok(code);
    }
    let (notes, _) = load(repo, notes_ref)?;

    match positional.first().map(|s| s.as_str()) {
        Some(spec) => {
            let object = match resolve(repo, spec) {
                Ok(id) => id,
                Err(e) => {
                    eprintln!("fatal: {e}");
                    return Ok(ExitCode::from(128));
                }
            };
            match notes.map.get(&object) {
                Some(note) => {
                    println!("{note}");
                    Ok(ExitCode::SUCCESS)
                }
                None => {
                    eprintln!("error: no note found for object {object}.");
                    Ok(ExitCode::from(1))
                }
            }
        }
        None => {
            for (object, note) in &notes.map {
                println!("{note} {object}");
            }
            Ok(ExitCode::SUCCESS)
        }
    }
}

fn show(repo: &gix::Repository, notes_ref: &str, args: &[String]) -> Result<ExitCode> {
    let args = &match normalize_sub_args(args, &EMPTY_TABLE) {
        Ok(v) => v,
        Err(code) => return Ok(code),
    };
    let mut positional: Vec<&String> = Vec::new();
    let mut literal = false;
    for a in args {
        if literal {
            positional.push(a);
            continue;
        }
        if a == "--" {
            literal = true;
            continue;
        }
        if is_help(a, "") {
            return Ok(sub_help(SHOW_USAGE, &[]));
        }
        if a.starts_with('-') && a != "-" {
            return sub_usage(&unknown_opt(a), SHOW_USAGE, &[]);
        }
        positional.push(a);
    }
    if positional.len() > 1 {
        return sub_usage("too many arguments", SHOW_USAGE, &[]);
    }
    let spec = positional.first().map(|s| s.as_str()).unwrap_or("HEAD");
    let object = match resolve(repo, spec) {
        Ok(id) => id,
        Err(e) => {
            eprintln!("fatal: {e}");
            return Ok(ExitCode::from(128));
        }
    };

    // `builtin/notes.c:782-785`: `show` resolves its operand first, then opens
    // the notes tree — so a bad operand outranks a bad notes ref here.
    if let Some(code) = check_notes_ref(notes_ref, "show")? {
        return Ok(code);
    }
    let (notes, _) = load(repo, notes_ref)?;
    match notes.map.get(&object) {
        Some(note) => {
            // `git notes show` execs `git show <blob>`, which writes the blob
            // out untouched — after reading the configuration `git show` reads
            // (`git_log_config()`, then `repo_init_revisions()`'s grep pass), so a
            // refused `diff.*` / `log.*` value dies here, and only when a note exists:
            // `notes list` and a miss never start the child.
            crate::log_config::validate_log(repo)
                .and_then(|()| crate::cmd_config::validate_grep_only(repo))
                .map_err(crate::default_config::Rejection::into_error)?;
            use std::io::Write;
            let blob = repo.find_object(*note)?;
            std::io::stdout().write_all(&blob.data)?;
            Ok(ExitCode::SUCCESS)
        }
        None => {
            eprintln!("error: no note found for object {object}.");
            Ok(ExitCode::from(1))
        }
    }
}

fn add(repo: &gix::Repository, notes_ref: &str, args: &[String]) -> Result<ExitCode> {
    let o = match parse_msg_opts(repo, args, "add")? {
        Ok(o) => o,
        Err(code) => return Ok(code),
    };

    let spec = o.object.clone().unwrap_or_else(|| "HEAD".to_string());
    let object = match resolve(repo, &spec) {
        Ok(id) => id,
        Err(e) => {
            eprintln!("fatal: {e}");
            return Ok(ExitCode::from(128));
        }
    };
    if let Some(code) = check_notes_ref(notes_ref, "add")? {
        return Ok(code);
    }

    let _lock = crate::lock::RepoLock::acquire(repo.git_dir());
    let (mut notes, parent) = load(repo, notes_ref)?;

    let note = notes.map.get(&object).copied();
    if note.is_some() {
        if o.force {
            eprintln!("Overwriting existing notes for object {object}");
        } else if !o.msgs.is_empty() {
            eprintln!(
                "error: Cannot add notes. Found existing notes for object {object}. \
                 Use '-f' to overwrite existing notes"
            );
            return Ok(ExitCode::from(1));
        } else {
            // With neither `-f` nor a message git rewrites `argv[0]` to `edit`
            // and re-enters `append_edit()`, which pre-fills the editor with the
            // existing note and logs the change as `git notes edit`.
            drop(_lock);
            return edit(repo, notes_ref, args);
        }
    }

    let body = match prepare_note_data(
        repo,
        &object,
        concat_messages(&o),
        !o.msgs.is_empty(),
        o.use_editor,
        note,
        o.stripspace,
    )? {
        Ok(b) => b,
        Err(code) => return Ok(code),
    };
    if !body.is_empty() || o.allow_empty {
        let blob = repo.write_blob(&body)?.detach();
        notes.map.insert(object, blob);
        commit_notes(
            repo,
            notes_ref,
            &notes,
            parent,
            "Notes added by 'git notes add'",
        )?;
    } else {
        eprintln!("Removing note for object {object}");
        // git only commits when the tree actually changed.
        if notes.map.remove(&object).is_some() {
            commit_notes(
                repo,
                notes_ref,
                &notes,
                parent,
                "Notes removed by 'git notes add'",
            )?;
        }
    }
    Ok(ExitCode::SUCCESS)
}

fn append(repo: &gix::Repository, notes_ref: &str, args: &[String]) -> Result<ExitCode> {
    let o = match parse_msg_opts(repo, args, "append")? {
        Ok(o) => o,
        Err(code) => return Ok(code),
    };
    let spec = o.object.clone().unwrap_or_else(|| "HEAD".to_string());
    let object = match resolve(repo, &spec) {
        Ok(id) => id,
        Err(e) => {
            eprintln!("fatal: {e}");
            return Ok(ExitCode::from(128));
        }
    };
    if let Some(code) = check_notes_ref(notes_ref, "append")? {
        return Ok(code);
    }

    let _lock = crate::lock::RepoLock::acquire(repo.git_dir());
    let (mut notes, parent) = load(repo, notes_ref)?;

    // `append_edit()` passes no `old_note`: appending never pre-fills the editor
    // with the previous note, it prepends it afterwards.
    let mut body = match prepare_note_data(
        repo,
        &object,
        concat_messages(&o),
        !o.msgs.is_empty(),
        o.use_editor,
        None,
        o.stripspace,
    )? {
        Ok(b) => b,
        Err(code) => return Ok(code),
    };
    if let Some(prev) = notes.map.get(&object) {
        // Previous contents first, then a separator when both sides are
        // non-empty; the joined result is not re-stripped.
        let mut head = repo.find_object(*prev)?.data.clone();
        if !body.is_empty() && !head.is_empty() {
            append_separator(&mut head, o.separator.as_deref());
        }
        head.extend_from_slice(&body);
        body = head;
    }

    if !body.is_empty() || o.allow_empty {
        let blob = repo.write_blob(&body)?.detach();
        notes.map.insert(object, blob);
        commit_notes(
            repo,
            notes_ref,
            &notes,
            parent,
            "Notes added by 'git notes append'",
        )?;
    } else {
        eprintln!("Removing note for object {object}");
        if notes.map.remove(&object).is_some() {
            commit_notes(
                repo,
                notes_ref,
                &notes,
                parent,
                "Notes removed by 'git notes append'",
            )?;
        }
    }
    Ok(ExitCode::SUCCESS)
}

/// `git notes edit` — git's `append_edit()` reached via the `edit` subcommand.
///
/// With any of `-m`/`-F`/`-c`/`-C` present git prints a deprecation notice and
/// then behaves exactly like `add -f` (force implied, but without the
/// "Overwriting existing notes" line). Its reflog messages say `git notes edit`.
/// A bare `edit` opens the editor on the existing note, if any.
fn edit(repo: &gix::Repository, notes_ref: &str, args: &[String]) -> Result<ExitCode> {
    let o = match parse_msg_opts(repo, args, "edit")? {
        Ok(o) => o,
        Err(code) => return Ok(code),
    };
    if !o.msgs.is_empty() {
        eprintln!("The -m/-F/-c/-C options have been deprecated for the 'edit' subcommand.");
        eprintln!("Please use 'git notes add -f -m/-F/-c/-C' instead.");
    }

    let spec = o.object.clone().unwrap_or_else(|| "HEAD".to_string());
    let object = match resolve(repo, &spec) {
        Ok(id) => id,
        Err(e) => {
            eprintln!("fatal: {e}");
            return Ok(ExitCode::from(128));
        }
    };
    if let Some(code) = check_notes_ref(notes_ref, "edit")? {
        return Ok(code);
    }

    let _lock = crate::lock::RepoLock::acquire(repo.git_dir());
    let (mut notes, parent) = load(repo, notes_ref)?;

    // `edit && note ? note : NULL` — re-editing pre-fills with the current note.
    let body = match prepare_note_data(
        repo,
        &object,
        concat_messages(&o),
        !o.msgs.is_empty(),
        o.use_editor,
        notes.map.get(&object).copied(),
        o.stripspace,
    )? {
        Ok(b) => b,
        Err(code) => return Ok(code),
    };
    if !body.is_empty() || o.allow_empty {
        let blob = repo.write_blob(&body)?.detach();
        notes.map.insert(object, blob);
        commit_notes(repo, notes_ref, &notes, parent, "Notes added by 'git notes edit'")?;
    } else {
        eprintln!("Removing note for object {object}");
        if notes.map.remove(&object).is_some() {
            commit_notes(repo, notes_ref, &notes, parent, "Notes removed by 'git notes edit'")?;
        }
    }
    Ok(ExitCode::SUCCESS)
}

fn copy(repo: &gix::Repository, notes_ref: &str, args: &[String]) -> Result<ExitCode> {
    let mut force = false;
    let mut stdin = false;
    let mut for_rewrite: Option<String> = None;
    let mut positional: Vec<&str> = Vec::new();
    let args = &match normalize_sub_args(args, &COPY_TABLE) {
        Ok(v) => v,
        Err(code) => return Ok(code),
    };
    let mut literal = false;
    let mut i = 0;
    while i < args.len() {
        let a = args[i].as_str();
        if literal {
            positional.push(a);
            i += 1;
            continue;
        }
        match a {
            "--" => literal = true,
            _ if is_help(a, "f") => return Ok(sub_help(COPY_USAGE, COPY_OPTS)),
            "-f" | "--force" => force = true,
            "--no-force" => force = false,
            "--stdin" => stdin = true,
            "--no-stdin" => stdin = false,
            "--no-for-rewrite" => for_rewrite = None,
            "--for-rewrite" => {
                i += 1;
                match args.get(i) {
                    Some(v) => for_rewrite = Some(v.clone()),
                    None => return missing_value("--for-rewrite"),
                }
            }
            s if s.starts_with("--for-rewrite=") => {
                for_rewrite = Some(s["--for-rewrite=".len()..].to_string())
            }
            s if s.starts_with('-') && s != "-" => {
                return sub_usage(&unknown_opt(s), COPY_USAGE, COPY_OPTS)
            }
            s => positional.push(s),
        }
        i += 1;
    }
    if stdin || for_rewrite.is_some() {
        // `builtin/notes.c:599-606`: `--stdin`/`--for-rewrite` take no
        // positional arguments at all; any leftover is "too many arguments".
        if !positional.is_empty() {
            return sub_usage("too many arguments", COPY_USAGE, COPY_OPTS);
        }
        return copy_stdin(repo, notes_ref, force, for_rewrite.as_deref());
    }
    if positional.is_empty() {
        return sub_usage("too few arguments", COPY_USAGE, COPY_OPTS);
    }
    if positional.len() > 2 {
        return sub_usage("too many arguments", COPY_USAGE, COPY_OPTS);
    }
    let from = match resolve(repo, positional[0]) {
        Ok(id) => id,
        Err(e) => {
            eprintln!("fatal: {e}");
            return Ok(ExitCode::from(128));
        }
    };
    let to_spec = positional.get(1).copied().unwrap_or("HEAD");
    let to = match resolve(repo, to_spec) {
        Ok(id) => id,
        Err(e) => {
            eprintln!("fatal: {e}");
            return Ok(ExitCode::from(128));
        }
    };
    if let Some(code) = check_notes_ref(notes_ref, "copy")? {
        return Ok(code);
    }

    let _lock = crate::lock::RepoLock::acquire(repo.git_dir());
    let (mut notes, parent) = load(repo, notes_ref)?;

    if notes.map.contains_key(&to) {
        if !force {
            eprintln!(
                "error: Cannot copy notes. Found existing notes for object {to}. \
                 Use '-f' to overwrite existing notes"
            );
            return Ok(ExitCode::from(1));
        }
        eprintln!("Overwriting existing notes for object {to}");
    }
    let Some(source) = notes.map.get(&from).copied() else {
        eprintln!("error: missing notes on source object {from}. Cannot copy.");
        return Ok(ExitCode::from(1));
    };

    notes.map.insert(to, source);
    commit_notes(
        repo,
        notes_ref,
        &notes,
        parent,
        "Notes added by 'git notes copy'",
    )?;
    Ok(ExitCode::SUCCESS)
}

/// How a copy-stdin combines a source note onto an existing target note.
#[derive(Clone, Copy, PartialEq)]
enum Combine {
    /// Plain `--stdin`: overwrite, but only if `-f` was given.
    Overwrite,
    /// `--for-rewrite` `concatenate` (default): old, blank line, new.
    Concatenate,
    /// `--for-rewrite` `cat_sort_uniq`.
    CatSortUniq,
    /// `--for-rewrite` `ignore`: keep the existing note untouched.
    Ignore,
}

/// `builtin/notes.c:notes_copy_from_stdin()` and its `--for-rewrite` variant.
///
/// Reads `<from-object> SP <to-object>` lines from stdin. `--for-rewrite=<cmd>`
/// is gated on `notes.rewrite.<cmd>` (default true) and whether the notes ref is
/// selected by `notes.rewriteRef`/`GIT_NOTES_REWRITE_REF`; when it is not, the
/// input is consumed and nothing is written, exactly like git.
fn copy_stdin(
    repo: &gix::Repository,
    notes_ref: &str,
    force: bool,
    for_rewrite: Option<&str>,
) -> Result<ExitCode> {
    // `--for-rewrite` ignores `--ref` entirely: git loads the notes trees the
    // rewrite configuration names and copies into all of them.
    let rewrite = match for_rewrite {
        None => None,
        Some(cmd) => match RewriteCfg::init(repo, cmd)? {
            Some(cfg) => Some(cfg),
            // Rewriting is off, or nothing is configured: `notes_copy_from_stdin()`
            // returns before it ever reads a line.
            None => return Ok(ExitCode::SUCCESS),
        },
    };

    let mut input = Vec::new();
    std::io::stdin().read_to_end(&mut input)?;
    // Split into records the way `strbuf_getline()` does: a trailing newline
    // marks EOF, not an empty record, so it is dropped; every other line — blank
    // ones included — is a record git will try (and fail) to parse.
    let mut records: Vec<&[u8]> = Vec::new();
    if !input.is_empty() {
        records = input.split(|&b| b == b'\n').collect();
        if input.last() == Some(&b'\n') {
            records.pop();
        }
    }
    // Parse every line up front so a malformed line aborts before any write.
    let mut pairs: Vec<(String, String)> = Vec::new();
    for line in records {
        let text = String::from_utf8_lossy(line).into_owned();
        let mut it = text.split_whitespace();
        match (it.next(), it.next()) {
            (Some(a), Some(b)) => pairs.push((a.to_string(), b.to_string())),
            _ => {
                eprintln!("fatal: malformed input line: '{text}'.");
                return Ok(ExitCode::from(128));
            }
        }
    }

    let _lock = crate::lock::RepoLock::acquire(repo.git_dir());
    const MSG: &str = "Notes added by 'git notes copy'";

    let mut resolved: Vec<(ObjectId, ObjectId)> = Vec::with_capacity(pairs.len());
    for (from_spec, to_spec) in &pairs {
        let (Ok(from), Ok(to)) = (resolve(repo, from_spec), resolve(repo, to_spec)) else {
            // Lower-case `failed` here and capitalised in `notes remove`: the
            // two call sites really do word it differently in git.
            eprintln!("fatal: failed to resolve '{from_spec}' as a valid ref.");
            return Ok(ExitCode::from(128));
        };
        resolved.push((from, to));
    }

    if let Some(cfg) = rewrite {
        // `error(_("failed to copy notes from '%s' to '%s'"))` names the two
        // operands as the line gave them (builtin/notes.c:399-403).
        let failed = cfg.copy(repo, &resolved, MSG)?;
        let mut code = ExitCode::SUCCESS;
        for ((from_spec, to_spec), _) in pairs.iter().zip(failed).filter(|(_, f)| *f) {
            eprintln!("error: failed to copy notes from '{from_spec}' to '{to_spec}'");
            code = ExitCode::from(1);
        }
        return Ok(code);
    }

    let (mut notes, parent) = load(repo, notes_ref)?;
    let mut changed = false;
    let mut err = false;

    for ((from, to), (from_spec, to_spec)) in resolved.into_iter().zip(&pairs) {
        // `copy_note(t, from, to, force, combine_notes_overwrite)`
        // (notes.c:1353-1368): an existing target refuses without `-f`, whether
        // or not the source has a note; with `-f` a source without one removes
        // the target's. The error names the operands as the line gave them
        // (builtin/notes.c:399-403).
        let src = notes.map.get(&from).copied();
        let existing = notes.map.contains_key(&to);
        if existing && !force {
            eprintln!("error: failed to copy notes from '{from_spec}' to '{to_spec}'");
            err = true;
            continue;
        }
        if src.is_some() || existing {
            add_note(repo, &mut notes, to, src, Combine::Overwrite)?;
            changed = true;
        }
    }

    if changed {
        commit_notes(repo, notes_ref, &notes, parent, MSG)?;
    }
    if err {
        return Ok(ExitCode::from(1));
    }
    Ok(ExitCode::SUCCESS)
}

/// Port of `notes-utils.c:parse_combine_notes_fn()` — the four mode names, case
/// folded; anything else is a configuration error the caller reports.
fn parse_combine(value: &str) -> Option<Combine> {
    match value.to_ascii_lowercase().as_str() {
        "overwrite" => Some(Combine::Overwrite),
        "ignore" => Some(Combine::Ignore),
        "concatenate" => Some(Combine::Concatenate),
        "cat_sort_uniq" => Some(Combine::CatSortUniq),
        _ => None,
    }
}

/// Port of `notes-utils.c:struct notes_rewrite_cfg` — which notes refs a
/// history-rewriting command carries notes across, and how a copied note is
/// combined onto one the target object already has.
///
/// `GIT_NOTES_REWRITE_REF` and `GIT_NOTES_REWRITE_MODE` do not merely take
/// precedence over `notes.rewriteRef`/`notes.rewriteMode`: setting either makes
/// `notes_rewrite_config()` skip the matching config key outright, which is what
/// `refs_from_env`/`mode_from_env` record.
pub(crate) struct RewriteCfg {
    /// Notes refs to rewrite, globs already expanded, in git's list order.
    pub(crate) refs: Vec<String>,
    combine: Combine,
}

impl RewriteCfg {
    /// Port of `init_copy_notes_for_rewrite(<cmd>)`. `None` — the common case —
    /// when `notes.rewrite.<cmd>` is false or nothing selects a ref, and the
    /// caller then copies nothing at all.
    pub(crate) fn init(repo: &gix::Repository, cmd: &str) -> Result<Option<RewriteCfg>> {
        let mut combine = Combine::Concatenate;
        // A value git cannot parse does not fall back to `concatenate`: it
        // leaves the copy without a combine function of its own, so `add_note()`
        // uses the notes tree's, which `load_notes_trees()` set to
        // `combine_notes_ignore`. An unparsable mode therefore *keeps* whatever
        // note the target already had — measured against git 2.55.0.
        let mode_from_env = match std::env::var("GIT_NOTES_REWRITE_MODE") {
            Ok(v) => {
                match parse_combine(&v) {
                    Some(c) => combine = c,
                    None => {
                        eprintln!("error: Bad GIT_NOTES_REWRITE_MODE value: '{v}'");
                        combine = Combine::Ignore;
                    }
                }
                true
            }
            Err(_) => false,
        };

        let mut refs: Vec<String> = Vec::new();
        let refs_from_env = match std::env::var("GIT_NOTES_REWRITE_REF") {
            Ok(v) => {
                for entry in v.split(':').filter(|s| !s.is_empty()) {
                    add_by_glob(repo, &mut refs, entry)?;
                }
                true
            }
            Err(_) => false,
        };

        // `repo_config(the_repository, notes_rewrite_config, c)`
        // (notes-utils.c:103-132): the callback runs once per configured value,
        // in configuration order, so every value of each key is validated — a
        // bad `notes.rewriteMode` is reported even when a later one replaces
        // it — and a valueless mode or ref is `config_error_nonbool()`, which
        // `configset_iter()` turns into `git_die_config_linenr()` for that
        // occurrence (config.c:1654-1673). The mode's own refusal returns 1,
        // which the walk ignores.
        let enable_key = format!("notes.rewrite.{cmd}");
        let mut enabled = true;
        for v in crate::config::walk_config(repo) {
            if v.key == enable_key {
                // `git_config_bool()`: NULL is true, anything
                // `git_parse_maybe_bool()` refuses dies.
                enabled = match v.value.as_deref() {
                    None => true,
                    Some(raw) => match crate::optint::maybe_bool(raw) {
                        Some(b) => b,
                        None => crate::git_fatal!("bad boolean config value '{raw}' for '{}'", v.key),
                    },
                };
            } else if !mode_from_env && v.key == "notes.rewritemode" {
                let Some(raw) = v.value.as_deref() else {
                    eprintln!("error: missing value for '{}'", v.key);
                    crate::git_fatal!("{}", v.origin.die_linenr(&v.key));
                };
                match parse_combine(raw) {
                    Some(c) => combine = c,
                    None => {
                        eprintln!("error: Bad notes.rewriteMode value: '{raw}'");
                        combine = Combine::Ignore;
                    }
                }
            } else if !refs_from_env && v.key == "notes.rewriteref" {
                let Some(raw) = v.value.as_deref() else {
                    eprintln!("error: missing value for '{}'", v.key);
                    crate::git_fatal!("{}", v.origin.die_linenr(&v.key));
                };
                if raw.starts_with("refs/notes/") {
                    add_by_glob(repo, &mut refs, raw)?;
                } else {
                    eprintln!("warning: Refusing to rewrite notes in {raw} (outside of refs/notes/)");
                }
            }
        }

        if !enabled || refs.is_empty() {
            return Ok(None);
        }
        Ok(Some(RewriteCfg { refs, combine }))
    }

    /// Port of `copy_note_for_rewrite()` over every pair, followed by
    /// `finish_copy_notes_for_rewrite()`: each ref that `add_note()` touched
    /// gets a notes commit carrying `msg`, and an untouched one is left alone.
    ///
    /// Answers whether each pair's `copy_note_for_rewrite()` failed in any tree
    /// (notes-utils.c:178-186), for the caller's own diagnostic.
    pub(crate) fn copy(
        &self,
        repo: &gix::Repository,
        pairs: &[(ObjectId, ObjectId)],
        msg: &str,
    ) -> Result<Vec<bool>> {
        let mut failed = vec![false; pairs.len()];
        for notes_ref in &self.refs {
            let (mut notes, parent) = load(repo, notes_ref)?;
            let mut dirty = false;
            for (n, (from, to)) in pairs.iter().enumerate() {
                // `copy_note()` with `force` (notes.c:1353-1368): whenever it
                // reaches `add_note()` the tree is marked dirty, even when the
                // combine function then leaves the value alone — so `ignore`
                // still writes a commit. With no source note, an existing target
                // is combined with the null oid.
                let src = match notes.map.get(from).copied() {
                    Some(src) => Some(src),
                    None if notes.map.contains_key(to) => None,
                    None => continue,
                };
                dirty = true;
                failed[n] |= add_note(repo, &mut notes, *to, src, self.combine)?;
            }
            if dirty {
                commit_notes(repo, notes_ref, &notes, parent, msg)?;
            }
        }
        Ok(failed)
    }
}

/// `notes.c:add_note()` through `note_tree_insert()` (notes.c:253-286): an empty
/// slot takes the new note unless it is null; a slot already holding the same
/// blob is left alone; otherwise the combine function decides, and a null result
/// removes the note.
///
/// `Ok(true)` is the combine function's non-zero return, which
/// `note_tree_insert()` hands back with the slot left as it was.
fn add_note(
    repo: &gix::Repository,
    notes: &mut Notes,
    to: ObjectId,
    new: Option<ObjectId>,
    combine: Combine,
) -> Result<bool> {
    let Some(cur) = notes.map.get(&to).copied() else {
        if let Some(new) = new {
            notes.map.insert(to, new);
        }
        return Ok(false);
    };
    if Some(cur) == new {
        return Ok(false);
    }
    // A note blob that cannot be read, or is no blob: `odb_read_object()` failing
    // or `type != OBJ_BLOB` in the combine functions.
    let read = |id: Option<ObjectId>| -> Option<Vec<u8>> {
        let obj = repo.find_object(id?).ok()?;
        (obj.kind == gix::object::Kind::Blob).then(|| obj.data.clone())
    };
    let result = match combine {
        Combine::Overwrite => new,
        Combine::Ignore => Some(cur),
        // combine_notes_concatenate() (notes.c:810-856): an empty or unreadable
        // new note keeps the current one, an empty or unreadable current one
        // takes the new one, and only two non-empty blobs are joined.
        Combine::Concatenate => match read(new).filter(|n| !n.is_empty()) {
            None => Some(cur),
            Some(new_msg) => match read(Some(cur)).filter(|c| !c.is_empty()) {
                None => new,
                Some(cur_msg) => Some(repo.write_blob(combine_concatenate(&cur_msg, &new_msg))?.detach()),
            },
        },
        // combine_notes_cat_sort_uniq() (notes.c:912-939): a null side adds no
        // lines, and the sorted result is always written back as a new blob. A
        // side `string_list_add_note_lines()` cannot read, or that is no blob,
        // fails the combine and leaves the note alone (notes.c:885-889, :920-923).
        Combine::CatSortUniq => {
            let lines = |id: Option<ObjectId>| match id {
                None => Some(Vec::new()),
                Some(_) => read(id),
            };
            let (Some(cur_msg), Some(new_msg)) = (lines(Some(cur)), lines(new)) else {
                return Ok(true);
            };
            Some(repo.write_blob(combine_cat_sort_uniq(&cur_msg, &new_msg))?.detach())
        }
    };
    match result {
        Some(id) => notes.map.insert(to, id),
        None => notes.map.remove(&to),
    };
    Ok(false)
}

/// `notes.c:combine_notes_concatenate()` — one trailing newline is trimmed from
/// the current note, then the two blobs are joined by a blank line.
fn combine_concatenate(cur: &[u8], new: &[u8]) -> Vec<u8> {
    let cur = match cur.last() {
        Some(&b'\n') => &cur[..cur.len() - 1],
        _ => cur,
    };
    let mut out = Vec::with_capacity(cur.len() + 2 + new.len());
    out.extend_from_slice(cur);
    out.extend_from_slice(b"\n\n");
    out.extend_from_slice(new);
    out
}

/// `notes.c:combine_notes_cat_sort_uniq()` — concatenate, split into lines, drop
/// empties, sort by byte value (`LC_ALL=C`), and remove duplicates.
fn combine_cat_sort_uniq(cur: &[u8], new: &[u8]) -> Vec<u8> {
    let mut buf = cur.to_vec();
    if buf.last().is_some_and(|&b| b != b'\n') {
        buf.push(b'\n');
    }
    buf.extend_from_slice(new);
    let mut lines: Vec<&[u8]> = buf.split(|&b| b == b'\n').filter(|l| !l.is_empty()).collect();
    lines.sort_unstable();
    lines.dedup();
    let mut out = Vec::with_capacity(buf.len());
    for l in lines {
        out.extend_from_slice(l);
        out.push(b'\n');
    }
    out
}

/// `builtin/notes.c:prune()` — drop every note whose annotated object is no
/// longer in the object database. `notes.c:prune_notes()` reports each pruned
/// object on stdout when verbose, and `-n` implies verbose because git ORs
/// `NOTES_PRUNE_VERBOSE` into the dry-run flags.
fn prune(repo: &gix::Repository, notes_ref: &str, args: &[String]) -> Result<ExitCode> {
    let mut dry_run = false;
    let mut verbose = false;
    let mut literal = false;
    let args = &match normalize_sub_args(args, &PRUNE_TABLE) {
        Ok(v) => v,
        Err(code) => return Ok(code),
    };
    for a in args {
        if literal {
            return sub_usage("too many arguments", PRUNE_USAGE, PRUNE_OPTS);
        }
        match a.as_str() {
            // `--` ends option parsing; prune takes no positional, so anything
            // after it is one argument too many.
            "--" => literal = true,
            s if is_help(s, "nv") => return Ok(sub_help(PRUNE_USAGE, PRUNE_OPTS)),
            "-n" | "--dry-run" => dry_run = true,
            "--no-dry-run" => dry_run = false,
            "-v" | "--verbose" => verbose = true,
            "--no-verbose" => verbose = false,
            s if s.starts_with('-') && s != "-" => {
                return sub_usage(&unknown_opt(s), PRUNE_USAGE, PRUNE_OPTS)
            }
            _ => return sub_usage("too many arguments", PRUNE_USAGE, PRUNE_OPTS),
        }
    }
    if let Some(code) = check_notes_ref(notes_ref, "prune")? {
        return Ok(code);
    }

    let _lock = crate::lock::RepoLock::acquire(repo.git_dir());
    let (mut notes, parent) = load(repo, notes_ref)?;

    let dead: Vec<ObjectId> = notes
        .map
        .keys()
        .filter(|id| !repo.has_object(**id))
        .copied()
        .collect();
    for id in &dead {
        if verbose || dry_run {
            println!("{id}");
        }
        if !dry_run {
            notes.map.remove(id);
        }
    }

    if !dry_run && !dead.is_empty() {
        commit_notes(
            repo,
            notes_ref,
            &notes,
            parent,
            "Notes removed by 'git notes prune'",
        )?;
    }
    Ok(ExitCode::SUCCESS)
}

fn remove(repo: &gix::Repository, notes_ref: &str, args: &[String]) -> Result<ExitCode> {
    let mut ignore_missing = false;
    let mut from_stdin = false;
    let mut specs: Vec<String> = Vec::new();
    let args = &match normalize_sub_args(args, &REMOVE_TABLE) {
        Ok(v) => v,
        Err(code) => return Ok(code),
    };
    let mut literal = false;
    for a in args {
        if literal {
            specs.push(a.clone());
            continue;
        }
        match a.as_str() {
            "--" => literal = true,
            s if is_help(s, "") => return Ok(sub_help(REMOVE_USAGE, REMOVE_OPTS)),
            "--ignore-missing" => ignore_missing = true,
            "--no-ignore-missing" => ignore_missing = false,
            "--stdin" => from_stdin = true,
            "--no-stdin" => from_stdin = false,
            s if s.starts_with('-') && s != "-" => {
                return sub_usage(&unknown_opt(s), REMOVE_USAGE, REMOVE_OPTS)
            }
            s => specs.push(s.to_string()),
        }
    }
    if let Some(code) = check_notes_ref(notes_ref, "remove")? {
        return Ok(code);
    }
    // git processes the given objects, then everything on stdin. The `HEAD`
    // default fires only when neither source names anything.
    if from_stdin {
        let mut input = Vec::new();
        std::io::stdin().read_to_end(&mut input)?;
        if !input.is_empty() {
            let mut lines: Vec<&[u8]> = input.split(|&b| b == b'\n').collect();
            if input.last() == Some(&b'\n') {
                lines.pop();
            }
            for line in lines {
                specs.push(String::from_utf8_lossy(line).into_owned());
            }
        }
    } else if specs.is_empty() {
        specs.push("HEAD".to_string());
    }

    let _lock = crate::lock::RepoLock::acquire(repo.git_dir());
    let (mut notes, parent) = load(repo, notes_ref)?;

    // git reports every object by the name the user typed, accumulates the
    // failures, and commits only when all removals succeeded.
    let mut failed = false;
    let mut changed = false;
    for spec in &specs {
        let object = match resolve(repo, spec) {
            Ok(id) => id,
            Err(_) => {
                eprintln!("error: Failed to resolve '{spec}' as a valid ref.");
                failed = true;
                continue;
            }
        };
        if notes.map.remove(&object).is_some() {
            eprintln!("Removing note for object {spec}");
            changed = true;
        } else {
            eprintln!("Object {spec} has no note");
            if !ignore_missing {
                failed = true;
            }
        }
    }

    if failed {
        return Ok(ExitCode::from(1));
    }
    if changed {
        commit_notes(
            repo,
            notes_ref,
            &notes,
            parent,
            "Notes removed by 'git notes remove'",
        )?;
    }
    Ok(ExitCode::SUCCESS)
}

// ---------------------------------------------------------------------------
// notes merge
// ---------------------------------------------------------------------------

/// `git_notes_merge_usage[]`, as `usage_with_options()` renders it.
fn merge_print_usage(out: &mut impl std::io::Write) -> std::io::Result<()> {
    writeln!(out, "usage: git notes merge [<options>] <notes-ref>")?;
    writeln!(out, "   or: git notes merge --commit [<options>]")?;
    writeln!(out, "   or: git notes merge --abort [<options>]")?;
    writeln!(out)?;
    writeln!(out, "General options")?;
    writeln!(out, "    -v, --[no-]verbose    be more verbose")?;
    writeln!(out, "    -q, --[no-]quiet      be more quiet")?;
    writeln!(out)?;
    writeln!(out, "Merge options")?;
    writeln!(out, "    -s, --[no-]strategy <strategy>")?;
    writeln!(
        out,
        "                          resolve notes conflicts using the given strategy (manual/ours/theirs/union/cat_sort_uniq)"
    )?;
    writeln!(out)?;
    writeln!(out, "Committing unmerged notes")?;
    writeln!(out, "    --commit              finalize notes merge by committing unmerged notes")?;
    writeln!(out)?;
    writeln!(out, "Aborting notes merge resolution")?;
    writeln!(out, "    --abort               abort notes merge")?;
    writeln!(out)?;
    Ok(())
}

/// `parse-options-cb.c:parse_opt_verbosity_cb()` (`parse-options-cb.c:65-85`),
/// which `OPT__VERBOSITY` installs for `-v`/`-q`: the counter saturates through
/// zero rather than cancelling, so `-vq` lands on `-1` and `-qv` on `1`.
fn bump_verbosity(target: &mut i32, verbose: bool) {
    if verbose {
        *target = if *target >= 0 { *target + 1 } else { 1 };
    } else {
        *target = if *target <= 0 { *target - 1 } else { -1 };
    }
}

/// `-h`/`--help-all` under `notes merge`: the merge usage block on stdout,
/// `PARSE_OPT_HELP`.
fn merge_help() -> Result<ExitCode> {
    let mut buf = Vec::new();
    merge_print_usage(&mut buf)?;
    Ok(super::show_usage(&String::from_utf8_lossy(&buf)))
}

/// A merge-specific usage error: `error:` then the merge usage block, exit 129.
fn merge_usage(msg: &str) -> Result<ExitCode> {
    eprintln!("error: {msg}");
    merge_print_usage(&mut std::io::stderr())?;
    Ok(ExitCode::from(129))
}

/// `builtin/notes.c:merge()` — the notes-merge driver.
fn merge(repo: &gix::Repository, notes_ref: &str, args: &[String]) -> Result<ExitCode> {
    // `notes-merge.c` reports each change with `printf()` and `builtin/notes.c` the
    // conflict summary with `fprintf(stderr, …)` (builtin/notes.c:1004), so off a
    // terminal the summary comes out first and the per-note lines at `exit()`.
    crate::cstdio::defer();
    let mut verbosity: i32 = 0;
    let mut strategy: Option<String> = None;
    let mut do_commit = false;
    let mut do_abort = false;
    let mut positional: Vec<String> = Vec::new();
    let args = &match normalize_sub_args(args, &MERGE_TABLE) {
        Ok(v) => v,
        Err(code) => return Ok(code),
    };
    let mut literal = false;
    let mut i = 0;
    while i < args.len() {
        let a = args[i].as_str();
        if literal {
            positional.push(a.to_string());
            i += 1;
            continue;
        }
        match a {
            "--" => literal = true,
            "-v" | "--verbose" => bump_verbosity(&mut verbosity, true),
            "--no-verbose" | "--no-quiet" => verbosity = 0,
            "-q" | "--quiet" => bump_verbosity(&mut verbosity, false),
            "--commit" => do_commit = true,
            "--abort" => do_abort = true,
            "-s" | "--strategy" => {
                i += 1;
                match args.get(i) {
                    Some(v) => strategy = Some(v.clone()),
                    None => return missing_value(a),
                }
            }
            "--no-strategy" => strategy = None,
            s if s.starts_with("--strategy=") => {
                strategy = Some(s["--strategy=".len()..].to_string())
            }
            // `-h` and `--help-all`: parse_options_step() answers both itself
            // and renders `USAGE_FULL`, identical here because the merge
            // option table has no `PARSE_OPT_HIDDEN` entry. The `--help-all`
            // compare is exact, so `--help-a` and `--help-all=x` stay
            // unknown-option reports.
            _ if is_help(a, "vq") => {
                return merge_help();
            }
            s if s.starts_with("--") => {
                return merge_usage(&format!("unknown option `{}'", &s[2..]))
            }
            s if s.starts_with('-') && s != "-" => {
                let sw = s[1..].chars().next().unwrap_or(' ');
                return merge_usage(&format!("unknown switch `{sw}'"));
            }
            s => positional.push(s.to_string()),
        }
        i += 1;
    }

    // `builtin/notes.c:919-924`: `-s` implies a merge, as does naming neither
    // `--commit` nor `--abort`; exactly one of the three modes may be active.
    let merging = strategy.is_some() || !(do_commit || do_abort);
    if usize::from(merging) + usize::from(do_commit) + usize::from(do_abort) != 1 {
        return merge_usage("cannot mix --commit, --abort or -s/--strategy");
    }
    // `builtin/notes.c:926-932`: the operand count is checked before the mode
    // runs, and before `-s` is looked at — `git notes merge -s bogus` is a
    // missing-operand report, not an unknown-strategy one.
    if merging && positional.len() != 1 {
        return merge_usage("must specify a notes ref to merge");
    } else if !merging && !positional.is_empty() {
        return merge_usage("too many arguments");
    }

    if do_abort {
        return merge_abort(repo, verbosity);
    }
    if do_commit {
        return merge_commit(repo, verbosity);
    }
    // `builtin/notes.c:948`: the notes tree is opened — and the ref refused if
    // it is outside `refs/notes/` — before `-s` is parsed at `:950`.
    if let Some(code) = check_notes_ref(notes_ref, "merge")? {
        return Ok(code);
    }
    // Without `-s`, `notes.<name>.mergeStrategy` then the general
    // `notes.mergeStrategy` supply the strategy, falling back to git's `manual`.
    let strat = match strategy.as_deref() {
        Some(name) => match parse_strategy(name) {
            Some(s) => s,
            None => return merge_usage(&format!("unknown -s/--strategy: {name}")),
        },
        None => match config_merge_strategy(repo, notes_ref) {
            Ok(s) => s,
            Err(code) => return Ok(code),
        },
    };
    do_merge(repo, notes_ref, &positional[0], strat, verbosity)
}

#[derive(Clone, Copy, PartialEq)]
enum Strategy {
    Manual,
    Ours,
    Theirs,
    Union,
    CatSortUniq,
}

/// `notes.c:parse_notes_merge_strategy()` — the name accepted by both the
/// `-s/--strategy` option and the `notes.mergeStrategy` config keys.
fn parse_strategy(name: &str) -> Option<Strategy> {
    Some(match name {
        "manual" => Strategy::Manual,
        "ours" => Strategy::Ours,
        "theirs" => Strategy::Theirs,
        "union" => Strategy::Union,
        "cat_sort_uniq" => Strategy::CatSortUniq,
        _ => return None,
    })
}

/// The merge strategy chosen when no `-s/--strategy` is given, from
/// `builtin/notes.c:merge()`: `notes.<name>.mergeStrategy` (where `<name>` is
/// the notes ref with its `refs/notes/` prefix removed) takes precedence over
/// the general `notes.mergeStrategy`, and either overrides git's `manual`
/// default. A present-but-unrecognised value is fatal, matching
/// `notes-utils.c:git_config_get_notes_strategy()`.
fn config_merge_strategy(
    repo: &gix::Repository,
    notes_ref: &str,
) -> std::result::Result<Strategy, ExitCode> {
    // git BUGs if the notes ref is not under refs/notes/, so the per-ref key is
    // only consulted when the prefix is present.
    if let Some(name) = notes_ref.strip_prefix("refs/notes/") {
        if let Some(s) = notes_strategy_config(repo, Some(name)) {
            return Ok(s);
        }
    }
    if let Some(s) = notes_strategy_config(repo, None) {
        return Ok(s);
    }
    Ok(Strategy::Manual)
}

/// The effective `notes[.<subsection>].mergeStrategy` value, parsed. `Ok(None)`
/// when unset; a present-but-invalid value prints git's config error and yields
/// exit 128.
fn notes_strategy_config(repo: &gix::Repository, subsection: Option<&str>) -> Option<Strategy> {
    // `git_config_get_notes_strategy()` (builtin/notes.c:873-886):
    // `repo_config_get_string()`, which dies through `git_die_config()` on a
    // valueless key, then `git_die_config()` again for a name
    // `parse_notes_merge_strategy()` rejects. The subsection keeps its case, as
    // git's configset keys do.
    let key = match subsection {
        Some(name) => format!("notes.{name}.mergeStrategy"),
        None => "notes.mergeStrategy".to_string(),
    };
    let value = crate::config::config_get_string(Some(repo), &key)?;
    match parse_strategy(&value) {
        Some(s) => Some(s),
        None => crate::config::die_config(
            Some(repo),
            &key,
            Some(&format!("unknown notes merge strategy {value}")),
        ),
    }
}

/// Move a notes ref, writing git's `notes: `-prefixed reflog line.
fn move_notes_ref(repo: &gix::Repository, notes_ref: &str, to: ObjectId, reflog: &str) -> Result<()> {
    // `refs_update_ref(…, notes_ref, &result_oid, NULL, 0, UPDATE_REFS_DIE_ON_ERR)`
    // (builtin/notes.c:979-981, and :875-878 once `merge_commit()` has checked the
    // old value itself): no `REF_NO_DEREF`, so a symbolic notes ref moves its
    // referent.
    repo.edit_reference(RefEdit {
        change: Change::Update {
            log: LogChange {
                mode: RefLog::AndReference,
                force_create_reflog: false,
                message: format!("notes: {reflog}").into(),
            },
            expected: PreviousValue::Any,
            new: Target::Object(to),
        },
        name: notes_ref
            .try_into()
            .map_err(|e| anyhow!("invalid notes ref {notes_ref:?}: {e}"))?,
        deref: true,
    })?;
    Ok(())
}

fn do_merge(
    repo: &gix::Repository,
    local_ref: &str,
    remote_spec: &str,
    strat: Strategy,
    verbosity: i32,
) -> Result<ExitCode> {
    // ```c
    // static void expand_loose_notes_ref(struct strbuf *sb)
    // {
    //         struct object_id object;
    //         if (repo_get_oid(the_repository, sb->buf, &object))
    //                 /* fallback to expand_notes_ref */
    //                 expand_notes_ref(sb);
    // }
    // ```
    //
    // (builtin/notes.c.) `merge` is the one subcommand that expands its operand
    // this way: a name that already resolves to an object is taken as written, so
    // `git notes merge HEAD` merges the tree `HEAD` names rather than looking for
    // `refs/notes/HEAD`. Only a name that resolves to nothing gets the prefix.
    let remote_ref = match crate::objname::resolve(repo, remote_spec) {
        Some(_) => remote_spec.to_string(),
        None => expand_notes_ref(remote_spec),
    };
    let reflog = format!("Merged notes from {remote_ref} into {local_ref}");

    let resolve_tip = |name: &str| -> Result<Option<ObjectId>> {
        match repo.try_find_reference(name) {
            Ok(Some(r)) => Ok(Some(r.into_fully_peeled_id()?.detach())),
            Ok(None) => Ok(None),
            Err(gix::reference::find::Error::Find(
                gix::refs::file::find::Error::RefnameValidation(_),
            )) => Ok(None),
            Err(e) => Err(e.into()),
        }
    };
    let local_tip = resolve_tip(local_ref)?;
    let remote_tip = resolve_tip(&remote_ref)?;

    let _lock = crate::lock::RepoLock::acquire(repo.git_dir());

    let (l, r) = match (local_tip, remote_tip) {
        (None, None) => {
            eprintln!(
                "fatal: Cannot merge empty notes ref ({remote_ref}) into empty notes ref ({local_ref})"
            );
            return Ok(ExitCode::from(128));
        }
        // Local has notes, remote is absent: nothing to merge.
        (Some(_), None) => return Ok(ExitCode::SUCCESS),
        // Local is unborn: adopt the remote notes wholesale.
        (None, Some(r)) => {
            move_notes_ref(repo, local_ref, r, &reflog)?;
            return Ok(ExitCode::SUCCESS);
        }
        (Some(l), Some(r)) => (l, r),
    };

    // `notes-merge.c:616-643`. git's verbosity counts from
    // `NOTES_MERGE_VERBOSITY_DEFAULT` (2), so its `>= 4` is `>= 2` here and its
    // `>= 3` is `>= 1`.
    let bases: Vec<ObjectId> =
        repo.merge_bases_many(l, &[r])?.into_iter().map(|id| id.detach()).collect();
    let base = bases.first().copied();
    // `%.7s` of the full hex, which is a plain truncation — not `find_unique_abbrev()`.
    let short = |id: &ObjectId| id.to_string()[..7].to_string();
    let base_oid = base.unwrap_or_else(|| ObjectId::null(repo.object_hash()));
    match bases.len() {
        0 if verbosity >= 2 => println!("No merge base found; doing history-less merge"),
        1 if verbosity >= 2 => println!("One merge base found ({})", short(&bases[0])),
        n if n > 1 && verbosity >= 1 => {
            println!("Multiple merge bases found. Using the first ({})", short(&bases[0]));
        }
        _ => {}
    }
    if verbosity >= 2 {
        println!(
            "Merging remote commit {} into local commit {} with merge-base {}",
            short(&r),
            short(&l),
            short(&base_oid)
        );
    }
    if base == Some(r) {
        if verbosity >= 0 {
            println!("Already up to date.");
        }
        return Ok(ExitCode::SUCCESS);
    }
    if base == Some(l) {
        if verbosity >= 0 {
            println!("Fast-forward");
        }
        move_notes_ref(repo, local_ref, r, &reflog)?;
        return Ok(ExitCode::SUCCESS);
    }

    // Genuine three-way merge.
    let base_notes = match base {
        Some(b) => load_from_commit(repo, b)?,
        None => Notes {
            map: BTreeMap::new(),
            non_notes: Vec::new(),
        },
    };
    let local_notes = load_from_commit(repo, l)?;
    let remote_notes = load_from_commit(repo, r)?;

    let mut keys: Vec<ObjectId> = Vec::new();
    for m in [&base_notes.map, &local_notes.map, &remote_notes.map] {
        for k in m.keys() {
            if !keys.contains(k) {
                keys.push(*k);
            }
        }
    }
    keys.sort();

    let mut merged: BTreeMap<ObjectId, ObjectId> = BTreeMap::new();
    let mut conflicts: Vec<ObjectId> = Vec::new();
    let mut worktree_checked = false;

    for obj in &keys {
        let b = base_notes.map.get(obj).copied();
        let lo = local_notes.map.get(obj).copied();
        let re = remote_notes.map.get(obj).copied();
        let result: Option<ObjectId> = if lo == re {
            lo
        } else if b == lo {
            re
        } else if b == re {
            lo
        } else {
            // Both sides changed the note differently: a real conflict.
            match strat {
                Strategy::Ours => {
                    if verbosity >= 0 {
                        println!("Using local notes for {obj}");
                    }
                    lo
                }
                Strategy::Theirs => {
                    if verbosity >= 0 {
                        println!("Using remote notes for {obj}");
                    }
                    re
                }
                // `add_note(t, &p->obj, &p->remote, combine_notes_…)` into the
                // local tree (notes-merge.c:466-484): a side that deleted the
                // note is a null oid, which the combine functions skip over.
                Strategy::Union | Strategy::CatSortUniq => {
                    let (combine, name) = match strat {
                        Strategy::Union => {
                            if verbosity >= 0 {
                                println!("Concatenating local and remote notes for {obj}");
                            }
                            (Combine::Concatenate, "combine_notes_concatenate")
                        }
                        _ => {
                            if verbosity >= 0 {
                                println!("Concatenating unique lines in local and remote notes for {obj}");
                            }
                            (Combine::CatSortUniq, "combine_notes_cat_sort_uniq")
                        }
                    };
                    let mut t = Notes {
                        map: lo.map(|lo| BTreeMap::from([(*obj, lo)])).unwrap_or_default(),
                        non_notes: Vec::new(),
                    };
                    if add_note(repo, &mut t, *obj, re, combine)? {
                        eprintln!("fatal: failed to concatenate notes ({name})");
                        return Ok(ExitCode::from(128));
                    }
                    t.map.get(obj).copied()
                }
                Strategy::Manual => {
                    if verbosity >= 0 {
                        println!("Auto-merging notes for {obj}");
                    }
                    // `notes-merge.c:406` calls `check_notes_merge_worktree()`
                    // between the "Auto-merging" line and the CONFLICT line, and
                    // only for the first conflict — `o->has_worktree` is set
                    // afterwards (`notes-merge.c:303`).
                    if !worktree_checked {
                        worktree_checked = true;
                        if let Some(code) = check_merge_worktree(repo)? {
                            return Ok(code);
                        }
                    }
                    // `merge_one_change_manual()` (notes-merge.c:408-439): a side
                    // that deleted the note leaves the other side's note in the
                    // tree as it is; otherwise ll_merge() writes the conflict,
                    // `add/add` when the merge base carries no note for the object.
                    let content = match (lo, re) {
                        (None, _) | (_, None) => {
                            let (deleted_in, modified_in) = match lo {
                                None => (local_ref, &remote_ref[..]),
                                Some(_) => (&remote_ref[..], local_ref),
                            };
                            if verbosity >= -1 {
                                println!(
                                    "CONFLICT (delete/modify): Notes for object {obj} deleted in {deleted_in} and modified in {modified_in}. Version from {modified_in} left in tree."
                                );
                            }
                            // `write_note_to_worktree()` (notes-merge.c:337-352).
                            let note = lo.or(re).expect("one side holds the note");
                            match repo.try_find_object(note).ok().flatten() {
                                Some(o) if o.kind == gix::object::Kind::Blob => o.data.clone(),
                                Some(_) => {
                                    eprintln!("fatal: blob expected in note {note} for object {obj}");
                                    return Ok(ExitCode::from(128));
                                }
                                None => {
                                    eprintln!("fatal: cannot read note {note} for object {obj}");
                                    return Ok(ExitCode::from(128));
                                }
                            }
                        }
                        (Some(_), Some(_)) => {
                            if verbosity >= -1 {
                                let reason = if b.is_none() { "add/add" } else { "content" };
                                println!(
                                    "CONFLICT ({reason}): Merge conflict in notes for object {obj}"
                                );
                            }
                            // `read_mmblob()` (xdiff-interface.c:179-195) of the
                            // three sides, the null base reading as empty.
                            let mut sides = Vec::with_capacity(3);
                            for id in [b, lo, re] {
                                let Some(id) = id else {
                                    sides.push(Vec::new());
                                    continue;
                                };
                                match repo.try_find_object(id).ok().flatten() {
                                    Some(o) if o.kind == gix::object::Kind::Blob => sides.push(o.data.clone()),
                                    _ => {
                                        eprintln!("fatal: unable to read blob object {id}");
                                        return Ok(ExitCode::from(128));
                                    }
                                }
                            }
                            let [base, local, remote] = <[Vec<u8>; 3]>::try_from(sides).expect("three sides");
                            match ll_merge_in_worktree(repo, obj, local_ref, &remote_ref, &base, &local, &remote)? {
                                Some(merged) => merged,
                                None => return Ok(ExitCode::from(128)),
                            }
                        }
                    };
                    // `write_buf_to_worktree()` (notes-merge.c:312-335) as each
                    // conflict is met, so a later death leaves the earlier ones.
                    std::fs::write(repo.git_dir().join("NOTES_MERGE_WORKTREE").join(obj.to_string()), &content)?;
                    conflicts.push(*obj);
                    // Conflicted notes stay out of the partial tree.
                    None
                }
            }
        };
        if let Some(id) = result {
            merged.insert(*obj, id);
        }
    }

    // `notes-merge.c:545-549`: `t->dirty` is set by every `add_note()`/
    // `remove_note()` `merge_changes()` applied to the local tree, so it is on
    // exactly when the merge resolved at least one change into it.
    if verbosity >= 2 {
        let dirty = if merged == local_notes.map { "clean" } else { "dirty" };
        println!(
            "Merge result: {} unmerged notes and a {dirty} notes tree",
            conflicts.len()
        );
    }

    let out_notes = Notes {
        map: merged,
        non_notes: local_notes.non_notes.clone(),
    };

    if conflicts.is_empty() {
        // Clean merge: a real two-parent merge commit.
        //
        // `builtin/notes.c:merge()` hands `notes_merge()` the headline with no
        // trailing newline, and `create_notes_commit()` writes the buffer
        // verbatim — so the merge commit's message ends at `…into <ref>`. The
        // `\n` that `commit_notes()` completes ordinary note commits with is
        // *not* added here.
        let tree_id = write_tree(repo, &out_notes)?;
        let commit = notes_commit(repo, reflog.clone(), tree_id, vec![l, r])?;
        move_notes_ref(repo, local_ref, commit, &reflog)?;
        Ok(ExitCode::SUCCESS)
    } else {
        // Manual strategy with conflicts: stage the partial merge on disk.
        let mut msg = format!("{reflog}\n\nConflicts:\n");
        for obj in &conflicts {
            msg.push('\t');
            msg.push_str(&obj.to_string());
            msg.push('\n');
        }
        let tree_id = write_tree(repo, &out_notes)?;
        let partial = notes_commit(repo, msg, tree_id, vec![l, r])?;

        // `refs_update_ref(…, msg.buf, "NOTES_MERGE_PARTIAL", …)` and
        // `refs_update_symref(…, "NOTES_MERGE_REF", notes_ref, NULL)`
        // (builtin/notes.c:988-999): files in the files backend, records in
        // the worktree's stack in a reftable one.
        crate::refstore::state_ref_write(
            repo,
            "NOTES_MERGE_PARTIAL",
            &crate::refstore::StateRef::Object(partial),
            &format!("notes: {reflog}"),
        )?;
        // `find_shared_symref(worktrees, "NOTES_MERGE_REF", notes_ref)`
        // (builtin/notes.c:992-997): a worktree already merging into this ref,
        // this one included, refuses the second merge after its partial commit.
        if let Some(path) = super::worktree::find_shared_symref(repo, "NOTES_MERGE_REF", local_ref) {
            eprintln!(
                "fatal: a notes merge into {local_ref} is already in-progress at {}",
                path.display()
            );
            return Ok(ExitCode::from(128));
        }
        crate::refstore::state_ref_write(
            repo,
            "NOTES_MERGE_REF",
            &crate::refstore::StateRef::Symbolic(local_ref.into()),
            "",
        )?;
        // git names the worktree with `git_path(NOTES_MERGE_WORKTREE)`.
        let wt_display = git_path_shown(repo, "NOTES_MERGE_WORKTREE");
        eprintln!(
            "Automatic notes merge failed. Fix conflicts in {wt_display} and commit the result with 'git notes merge --commit', or abort the merge with 'git notes merge --abort'."
        );
        Ok(ExitCode::from(1))
    }
}

/// `notes-merge.c:check_notes_merge_worktree()` (`notes-merge.c:276-310`).
///
/// A manual merge that is about to stage its first conflict refuses to start
/// when `$GIT_DIR/NOTES_MERGE_WORKTREE` already exists and is not empty: the
/// previous merge has not been concluded. `advice.resolveConflict` picks which
/// of the two wordings git dies with.
fn check_merge_worktree(repo: &gix::Repository) -> Result<Option<ExitCode>> {
    let wt = repo.git_dir().join("NOTES_MERGE_WORKTREE");
    let occupied = match std::fs::read_dir(&wt) {
        Ok(mut entries) => entries.next().is_some(),
        Err(_) => false,
    };
    if !occupied {
        std::fs::create_dir_all(&wt)?;
        return Ok(None);
    }
    let shown = git_path_shown(repo, "NOTES_MERGE_*");
    if crate::advice::Advice::ResolveConflict.enabled_in(repo) {
        eprintln!(
            "fatal: You have not concluded your previous notes merge ({shown} exists).\n\
             Please, use 'git notes merge --commit' or 'git notes merge --abort' to \
             commit/abort the previous merge before you start a new notes merge."
        );
    } else {
        eprintln!("fatal: You have not concluded your notes merge ({shown} exists).");
    }
    Ok(Some(ExitCode::from(128)))
}

/// `git_path(<name>)` as git prints it: the git dir exactly as setup resolved
/// it, with the `./` prefix `gix` can carry dropped and an absolute git dir
/// under the cwd shortened back to a relative path.
fn git_path_shown(repo: &gix::Repository, name: &str) -> String {
    // `repo->gitdir` as setup left it, relative to the top of the work tree it
    // moved to, whichever subdirectory the command was typed in.
    if let Some(s) = crate::setup::after_setup(repo) {
        return s.git_dir.join(name).display().to_string();
    }
    let path = repo.git_dir().join(name);
    let shown = path.display().to_string();
    match shown.strip_prefix("./") {
        Some(rest) => rest.to_string(),
        None => std::env::current_dir()
            .ok()
            .and_then(|cwd| path.strip_prefix(&cwd).ok().map(|p| p.display().to_string()))
            .unwrap_or(shown),
    }
}

/// `ll_merge_in_worktree()` (notes-merge.c:355-384): `ll_merge()` of the three
/// note blobs under the object's hex name, labelled with the two notes refs and
/// no ancestor label, with `o->repo->index` — which `git notes` never reads — as
/// the attribute index. A binary conflict is warned about; an error or a result
/// the driver did not leave behind is `die("Failed to execute internal merge")`,
/// reported here as `None` once printed.
fn ll_merge_in_worktree(
    repo: &gix::Repository,
    obj: &ObjectId,
    local_ref: &str,
    remote_ref: &str,
    base: &[u8],
    local: &[u8],
    remote: &[u8],
) -> Result<Option<Vec<u8>>> {
    let path = obj.to_string();
    let (status, result) = ll_merge(repo, &path, base, local_ref, local, remote_ref, remote)?;
    if status == LlMergeResult::BinaryConflict {
        eprintln!("warning: Cannot merge binary files: {path} ({local_ref} vs. {remote_ref})");
    }
    match (status, result) {
        (LlMergeResult::Error, _) | (_, None) => {
            eprintln!("fatal: Failed to execute internal merge");
            Ok(None)
        }
        (_, Some(merged)) => Ok(Some(merged)),
    }
}

/// `enum ll_merge_result` (merge-ll.h).
#[derive(Clone, Copy, PartialEq, Eq)]
enum LlMergeResult {
    Ok,
    Error,
    Conflict,
    BinaryConflict,
}

/// The low-level driver `find_ll_merge_driver()` picks (merge-ll.c:363-394).
enum LlMergeDriver {
    /// `ll_merge_drv[LL_BINARY_MERGE]`.
    Binary,
    /// `ll_merge_drv[LL_TEXT_MERGE]`.
    Text,
    /// `ll_merge_drv[LL_UNION_MERGE]`.
    Union,
    /// A `merge.<name>.*` driver run by `ll_ext_merge()`, with its last
    /// `merge.<name>.driver` command line.
    External { name: String, cmdline: Option<String> },
}

/// `ll_merge()` (merge-ll.c:409-451) with `LL_MERGE_OPTIONS_INIT`: no
/// renormalization, no virtual ancestor, no extra marker size. Returns the status
/// and the result buffer, `None` when the driver produced none.
fn ll_merge(
    repo: &gix::Repository,
    path: &str,
    ancestor: &[u8],
    our_label: &str,
    ours: &[u8],
    their_label: &str,
    theirs: &[u8],
) -> Result<(LlMergeResult, Option<Vec<u8>>)> {
    // `git_check_attr(istate, path, check)` over `merge` and
    // `conflict-marker-size` (merge-ll.c:427-437).
    let empty = gix::index::State::new(repo.object_hash());
    let mut stack = repo.attributes_only(
        &empty,
        gix::worktree::stack::state::attributes::Source::WorktreeThenIdMapping,
    )?;
    let mode = Some(gix::index::entry::Mode::FILE);
    // The first descent loads the attribute files so the collection knows both
    // names before the outcome is sized against it.
    let _ = stack.at_entry(path, mode)?;
    let mut outcome = gix::attrs::search::Outcome::default();
    outcome.initialize_with_selection(stack.attributes_collection(), ["merge", "conflict-marker-size"]);
    stack.at_entry(path, mode)?.matching_attributes(&mut outcome);
    let mut merge_attr = gix::attrs::State::Unspecified;
    let mut marker_attr = None;
    for m in outcome.iter_selected() {
        match m.assignment.name.as_str() {
            "merge" => merge_attr = m.assignment.state.to_owned(),
            _ => marker_attr = Some(m.assignment.state.to_owned()),
        }
    }
    let marker_size = super::rerere::marker_size_from_attr(marker_attr.as_ref().map(|s| s.as_ref()));
    let driver = find_ll_merge_driver(repo, &merge_attr);

    match driver {
        LlMergeDriver::Binary => Ok(ll_binary_merge(ours)),
        LlMergeDriver::Text => Ok(ll_xdl_merge(ancestor, our_label, ours, their_label, theirs, marker_size, false)),
        LlMergeDriver::Union => Ok(ll_xdl_merge(ancestor, our_label, ours, their_label, theirs, marker_size, true)),
        LlMergeDriver::External { name, cmdline } => {
            let Some(cmdline) = cmdline else {
                crate::git_fatal!("custom merge driver {name} lacks command line.");
            };
            ll_ext_merge(repo, &cmdline, path, ancestor, our_label, ours, their_label, theirs, marker_size)
        }
    }
}

/// `find_ll_merge_driver()` (merge-ll.c:363-394) over `read_merge_config()`
/// (merge-ll.c:282-355): `merge` set is text, `-merge` binary, unset falls back
/// to `merge.default` (text when that is unset too), and a name is looked for
/// among the `merge.<name>.*` drivers before the three built-ins, defaulting to
/// text.
fn find_ll_merge_driver(repo: &gix::Repository, merge_attr: &gix::attrs::State) -> LlMergeDriver {
    let config = repo.config_snapshot();
    let name: BString = match merge_attr {
        gix::attrs::State::Set => return LlMergeDriver::Text,
        gix::attrs::State::Unset => return LlMergeDriver::Binary,
        gix::attrs::State::Unspecified => match config.string("merge.default") {
            Some(name) => name,
            None => return LlMergeDriver::Text,
        },
        gix::attrs::State::Value(v) => v.as_ref().as_bstr().to_owned(),
    };
    // Any `merge.<name>.<key>` defines a user driver, whose `cmdline` is the last
    // `driver` value; the subsection name compares case-sensitively.
    let mut user = false;
    let mut cmdline = None;
    if let Some(sections) = config.plumbing().sections_by_name("merge") {
        for section in sections {
            if section.header().subsection_name() != Some(name.as_bstr()) {
                continue;
            }
            user = true;
            if let Some(v) = section.values("driver").last() {
                cmdline = Some(v.to_str_lossy().into_owned());
            }
        }
    }
    if user {
        return LlMergeDriver::External {
            name: name.to_str_lossy().into_owned(),
            cmdline,
        };
    }
    match name.as_bytes() {
        b"binary" => LlMergeDriver::Binary,
        b"union" => LlMergeDriver::Union,
        // "text" and every unknown name: `/* default to the 3-way */`.
        _ => LlMergeDriver::Text,
    }
}

/// `ll_binary_merge()` (merge-ll.c:57-100) with the default variant: the result
/// is ours, and a binary conflict.
fn ll_binary_merge(ours: &[u8]) -> (LlMergeResult, Option<Vec<u8>>) {
    (LlMergeResult::BinaryConflict, Some(ours.to_vec()))
}

/// `ll_xdl_merge()` (merge-ll.c:102-145), and `ll_union_merge()` (merge-ll.c:147-163)
/// as the same merge under `XDL_MERGE_FAVOR_UNION`. A binary or oversized side
/// goes to the binary driver. `git notes` never reads `merge.conflictStyle`
/// (builtin/notes.c:1150 is `git_default_config`), so `git_xmerge_style` stays -1
/// and the style is xdiff's default.
fn ll_xdl_merge(
    ancestor: &[u8],
    our_label: &str,
    ours: &[u8],
    their_label: &str,
    theirs: &[u8],
    marker_size: usize,
    union: bool,
) -> (LlMergeResult, Option<Vec<u8>>) {
    use gix::merge::blob::builtin_driver::text::{Conflict, ConflictStyle, Labels, Options};
    // `MAX_XDIFF_SIZE` (xdiff-interface.h:14).
    const MAX_XDIFF_SIZE: usize = 1024 * 1024 * 1023;
    if [ancestor, ours, theirs]
        .iter()
        .any(|b| b.len() > MAX_XDIFF_SIZE || super::diffcore_rename::buffer_is_binary(b))
    {
        return ll_binary_merge(ours);
    }
    let conflict = if union {
        Conflict::ResolveWithUnion
    } else {
        Conflict::Keep {
            style: ConflictStyle::Merge,
            // gix carries the marker width in a `NonZeroU8`.
            marker_size: std::num::NonZeroU8::new(marker_size.min(u8::MAX as usize) as u8)
                .expect("marker size is positive"),
        }
    };
    let mut out = Vec::new();
    let mut input = gix::diff::blob::InternedInput::default();
    let resolution = gix::merge::blob::builtin_driver::text(
        &mut out,
        &mut input,
        Labels {
            ancestor: None,
            current: Some(our_label.as_bytes().as_bstr()),
            other: Some(their_label.as_bytes().as_bstr()),
        },
        ours,
        ancestor,
        theirs,
        Options {
            conflict,
            ..Options::default()
        },
    );
    let status = match resolution {
        gix::merge::blob::Resolution::Conflict => LlMergeResult::Conflict,
        _ => LlMergeResult::Ok,
    };
    (status, Some(out))
}

/// `create_temp()` (merge-ll.c:177-186): `xmkstemp()` of `.merge_file_XXXXXX` in
/// the current directory, holding `src`.
fn ll_create_temp(dir: &std::path::Path, src: &[u8]) -> Result<String> {
    use std::os::unix::fs::OpenOptionsExt;
    let mut attempt = 0u32;
    loop {
        let name = format!(".merge_file_{}", ll_temp_suffix(attempt));
        match std::fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(dir.join(&name)) {
            Ok(mut f) => {
                if let Err(e) = f.write_all(src) {
                    crate::git_fatal!("unable to write temp-file: {}", crate::external::strerror(&e));
                }
                return Ok(name);
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => attempt += 1,
            Err(e) => crate::git_fatal!(
                "Unable to create temporary file '{}': {}",
                dir.join(&name).display(),
                crate::external::strerror(&e)
            ),
        }
    }
}

/// The six characters `mkstemp()` substitutes for `XXXXXX`, from the pid, the
/// clock and the attempt number.
fn ll_temp_suffix(attempt: u32) -> String {
    const SET: &[u8] = b"abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789";
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0);
    let mut v = (u64::from(std::process::id()) << 32) ^ u64::from(nanos) ^ (u64::from(attempt) << 20);
    (0..6)
        .map(|_| {
            let c = SET[(v % SET.len() as u64) as usize] as char;
            v /= SET.len() as u64;
            c
        })
        .collect()
}

/// `ll_ext_merge()` (merge-ll.c:191-272): the three sides go to temporary files,
/// the command line is expanded (`%O %A %B %L %P %S %X %Y %%`) and run through
/// the shell, and the result is whatever the driver left in the `%A` file. A
/// zero exit is clean, one up to 128 a conflict, and above that (a signal) an
/// error.
#[allow(clippy::too_many_arguments)]
fn ll_ext_merge(
    repo: &gix::Repository,
    cmdline: &str,
    path: &str,
    ancestor: &[u8],
    our_label: &str,
    ours: &[u8],
    their_label: &str,
    theirs: &[u8],
    marker_size: usize,
) -> Result<(LlMergeResult, Option<Vec<u8>>)> {
    // The temporaries are named relative to where setup left git standing —
    // the top of the work tree — which is also where the driver runs.
    let setup = crate::setup::after_setup(repo);
    let dir = match &setup {
        Some(s) => s.cwd.clone(),
        None => std::env::current_dir()?,
    };
    let temp = [
        ll_create_temp(&dir, ancestor)?,
        ll_create_temp(&dir, ours)?,
        ll_create_temp(&dir, theirs)?,
    ];
    let sq = |s: &str| {
        let mut out = Vec::new();
        super::rev_parse::sq_quote_buf(&mut out, s.as_bytes());
        String::from_utf8_lossy(&out).into_owned()
    };
    let mut cmd = String::new();
    let mut chars = cmdline.chars();
    while let Some(c) = chars.next() {
        if c != '%' {
            cmd.push(c);
            continue;
        }
        let mut rest = chars.clone();
        match rest.next() {
            Some('%') => cmd.push('%'),
            Some('O') => cmd.push_str(&temp[0]),
            Some('A') => cmd.push_str(&temp[1]),
            Some('B') => cmd.push_str(&temp[2]),
            Some('L') => cmd.push_str(&marker_size.to_string()),
            Some('P') => cmd.push_str(&sq(path)),
            // `orig_name` is NULL here.
            Some('S') => cmd.push_str(&sq("")),
            Some('X') => cmd.push_str(&sq(our_label)),
            Some('Y') => cmd.push_str(&sq(their_label)),
            _ => {
                cmd.push('%');
                continue;
            }
        }
        chars = rest;
    }

    // `run_command()` with `use_shell`: `fflush(NULL)` before the fork
    // (run-command.c:743), then `wait_or_whine()`'s status.
    crate::cstdio::before_spawn();
    let _ = std::io::stdout().flush();
    let mut child = crate::external::prepare_shell_cmd_str(&cmd, crate::external::NO_ARGS);
    crate::setup::export_to_child(repo, setup.as_ref(), &mut child);
    let status: i32 = match child.status() {
        Ok(st) => match st.code() {
            Some(code) => code,
            None => {
                let sig = std::os::unix::process::ExitStatusExt::signal(&st).unwrap_or(0);
                if sig != libc::SIGINT && sig != libc::SIGQUIT && sig != libc::SIGPIPE {
                    eprintln!("error: {cmd} died of signal {sig}");
                }
                sig + 128
            }
        },
        Err(e) => {
            eprintln!("error: cannot run {cmd}: {}", crate::external::strerror(&e));
            -1
        }
    };
    let result = std::fs::read(dir.join(&temp[1])).ok();
    // `unlink_or_warn()` of the three temporaries.
    for t in &temp {
        if let Err(e) = std::fs::remove_file(dir.join(t)) {
            if e.kind() != std::io::ErrorKind::NotFound {
                eprintln!("warning: unable to unlink '{t}': {}", crate::external::strerror(&e));
            }
        }
    }
    let status = if status == 0 {
        LlMergeResult::Ok
    } else if status <= 128 {
        LlMergeResult::Conflict
    } else {
        LlMergeResult::Error
    };
    Ok((status, result))
}


/// `git notes merge --commit` — finalize a manual merge staged on disk.
fn merge_commit(repo: &gix::Repository, verbosity: i32) -> Result<ExitCode> {
    let git_dir = repo.git_dir();
    // `repo_get_oid(…, "NOTES_MERGE_PARTIAL", …)` and the symbolic target of
    // `NOTES_MERGE_REF` (builtin/notes.c:832-850).
    let partial = match crate::refstore::state_ref_read(repo, "NOTES_MERGE_PARTIAL")? {
        Some(crate::refstore::StateRef::Object(id)) => id,
        _ => {
            eprintln!("fatal: failed to read ref NOTES_MERGE_PARTIAL");
            return Ok(ExitCode::from(128));
        }
    };
    let local_ref = match crate::refstore::state_ref_read(repo, "NOTES_MERGE_REF")? {
        Some(crate::refstore::StateRef::Symbolic(target)) => target.to_string(),
        _ => {
            eprintln!("fatal: failed to resolve NOTES_MERGE_REF");
            return Ok(ExitCode::from(128));
        }
    };

    let _lock = crate::lock::RepoLock::acquire(git_dir);
    let partial_commit = repo.find_commit(partial)?;
    let parents: Vec<ObjectId> = partial_commit.parent_ids().map(|id| id.detach()).collect();
    let full_msg = partial_commit.message_raw()?.to_string();
    // `notes-merge.c:notes_merge_commit()` reuses the partial commit's message
    // verbatim, `Conflicts:` section and all; only the *reflog* line is the
    // headline on its own.
    let headline = full_msg.split("\n\nConflicts:").next().unwrap_or(&full_msg);
    let reflog = headline.trim_end_matches('\n').to_string();

    let mut notes = load_from_commit(repo, partial)?;
    let wt = git_dir.join("NOTES_MERGE_WORKTREE");
    // `notes-merge.c:703-705`: git's own verbosity counts from
    // `NOTES_MERGE_VERBOSITY_DEFAULT` (2), so its `>= 3` is one `-v` here.
    let wt_shown = git_path_shown(repo, "NOTES_MERGE_WORKTREE");
    if verbosity >= 1 {
        println!("Committing notes in notes merge worktree at {wt_shown}");
    }
    if let Ok(entries) = std::fs::read_dir(&wt) {
        for entry in entries.flatten() {
            let name = entry.file_name();
            let Some(name) = name.to_str() else { continue };
            let Ok(obj) = ObjectId::from_hex(name.as_bytes()) else {
                // `notes-merge.c:721-726`.
                if verbosity >= 1 {
                    println!("Skipping non-SHA1 entry '{wt_shown}/{name}'");
                }
                continue;
            };
            let content = std::fs::read(entry.path())?;
            if content.is_empty() {
                notes.map.remove(&obj);
            } else {
                let blob = repo.write_blob(&content)?.detach();
                // `notes-merge.c:737-739`.
                if verbosity >= 2 {
                    println!("Added resolved note for object {obj}: {blob}");
                }
                notes.map.insert(obj, blob);
            }
        }
    }

    let tree_id = write_tree(repo, &notes)?;
    let commit = notes_commit(repo, full_msg.clone(), tree_id, parents.clone())?;
    // `notes-merge.c:746-748`.
    if verbosity >= 2 {
        println!("Finalized notes merge commit: {commit}");
    }
    let local_tip = repo
        .try_find_reference(local_ref.as_str())?
        .map(|r| r.into_fully_peeled_id())
        .transpose()?
        .map(|id| id.detach());
    // `refs_update_ref(…, o->local_ref, &oid, is_null_oid(&parent_oid) ? NULL :
    // &parent_oid, 0, UPDATE_REFS_DIE_ON_ERR)` (builtin/notes.c:875-878): the ref
    // must still be where the merge started, the partial commit's first parent,
    // and a ref that moved or went away meanwhile dies before the merge state is
    // cleaned up.
    if let Some(&expected) = parents.first() {
        let reason = match local_tip {
            Some(cur) if cur == expected => None,
            Some(cur) => Some(format!("is at {cur} but expected {expected}")),
            None => Some(format!("unable to resolve reference '{local_ref}'")),
        };
        if let Some(reason) = reason {
            eprintln!("fatal: update_ref failed for ref '{local_ref}': cannot lock ref '{local_ref}': {reason}");
            return Ok(ExitCode::from(128));
        }
    }
    move_notes_ref(repo, &local_ref, commit, &reflog)?;

    // Clear the staged merge.
    let _ = crate::refstore::state_ref_delete(repo, "NOTES_MERGE_PARTIAL", "");
    let _ = crate::refstore::state_ref_delete(repo, "NOTES_MERGE_REF", "");
    clear_merge_worktree(&wt, &wt_shown, verbosity);
    Ok(ExitCode::SUCCESS)
}

/// `remove_dir_recursively(&buf, REMOVE_DIR_KEEP_TOPLEVEL)` on
/// `$GIT_DIR/NOTES_MERGE_WORKTREE`, which is how both `--commit` and `--abort`
/// tear the staged merge down: the conflict files go, the directory stays —
/// git keeps it because it may be the user's current working directory.
fn clear_merge_worktree(wt: &std::path::Path, shown: &str, verbosity: i32) {
    // `notes-merge.c:765-766`.
    if verbosity >= 1 {
        println!("Removing notes merge worktree at {shown}/*");
    }
    let Ok(entries) = std::fs::read_dir(wt) else { return };
    for entry in entries.flatten() {
        let path = entry.path();
        let _ = match entry.file_type() {
            Ok(t) if t.is_dir() => std::fs::remove_dir_all(&path),
            _ => std::fs::remove_file(&path),
        };
    }
}

/// `git notes merge --abort` — discard a staged manual merge.
fn merge_abort(repo: &gix::Repository, verbosity: i32) -> Result<ExitCode> {
    let git_dir = repo.git_dir();
    let wt = git_dir.join("NOTES_MERGE_WORKTREE");
    let shown = git_path_shown(repo, "NOTES_MERGE_WORKTREE");
    // `notes-merge.c:764-767` announces the removal before attempting it, so
    // the line is printed even when there is no worktree left to remove.
    if !wt.exists() {
        if verbosity >= 1 {
            println!("Removing notes merge worktree at {shown}/*");
        }
        eprintln!("error: failed to remove 'git notes merge' worktree");
        return Ok(ExitCode::from(1));
    }
    clear_merge_worktree(&wt, &shown, verbosity);
    let _ = crate::refstore::state_ref_delete(repo, "NOTES_MERGE_PARTIAL", "");
    let _ = crate::refstore::state_ref_delete(repo, "NOTES_MERGE_REF", "");
    Ok(ExitCode::SUCCESS)
}

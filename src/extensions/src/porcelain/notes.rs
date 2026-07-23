use anyhow::{anyhow, bail, Result};
use std::collections::BTreeMap;
use std::io::Read;
use std::process::ExitCode;

use gix::bstr::{BStr, BString, ByteSlice};
use gix::hash::ObjectId;
use gix::objs::tree::{EntryKind, EntryMode};
use gix::refs::transaction::{Change, LogChange, PreviousValue, RefEdit, RefLog};
use gix::refs::Target;

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
/// then `notes.mergeStrategy`.
///
/// The only paths kept as a terse bail are the genuinely interactive editor
/// flows — a bare `edit`/`add`/`append` with no message option, and `-e` /
/// `-c`/`--reedit-message` — which require a live terminal that has no
/// equivalent here. The manual merge strategy's conflict blob is written with
/// whole-content conflict markers (git's `ll_merge` output for single-block
/// notes); its stdout, exit code and staged-merge state match stock git.
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
        if a == "-h" {
            print_usage(&mut std::io::stdout())?;
            return Ok(ExitCode::from(129));
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
                        None => return top_usage("option `ref' requires a value"),
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

    let repo = gix::discover(".")?;
    let notes_ref = resolve_notes_ref(&repo, override_ref.as_deref());

    match sub {
        "list" => list(&repo, &notes_ref, sub_args),
        "show" => show(&repo, &notes_ref, sub_args),
        "get-ref" => {
            if !sub_args.is_empty() {
                return usage(&["git notes get-ref"], "too many arguments");
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
fn resolve_notes_ref(repo: &gix::Repository, override_ref: Option<&str>) -> String {
    if let Some(r) = override_ref {
        return expand_notes_ref(r);
    }
    if let Ok(env) = std::env::var("GIT_NOTES_REF") {
        if !env.is_empty() {
            return env;
        }
    }
    let snapshot = repo.config_snapshot();
    if let Some(v) = snapshot.string("core.notesRef") {
        return v.to_str_lossy().into_owned();
    }
    "refs/notes/commits".to_string()
}

/// `notes.c:expand_notes_ref()` — a bare name becomes `refs/notes/<name>`, a
/// `notes/`-prefixed name gains `refs/`, a full name is left alone.
fn expand_notes_ref(name: &str) -> String {
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
struct Notes {
    /// annotated object id → note blob id, ordered as git emits them.
    map: BTreeMap<ObjectId, ObjectId>,
    /// (full path, mode, id) for entries that are not notes, sorted by path.
    non_notes: Vec<(BString, EntryMode, ObjectId)>,
}

/// Read the notes ref and load the tree it points at (empty when unborn).
fn load(repo: &gix::Repository, notes_ref: &str) -> Result<(Notes, Option<ObjectId>)> {
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
    let commit = repo
        .new_commit(format!("{msg}\n"), tree_id, parent)?
        .id()
        .detach();
    repo.edit_reference(RefEdit {
        change: Change::Update {
            log: LogChange {
                mode: RefLog::AndReference,
                force_create_reflog: false,
                message: format!("notes: {msg}").into(),
            },
            expected: match parent {
                Some(p) => PreviousValue::MustExistAndMatch(Target::Object(p)),
                None => PreviousValue::MustNotExist,
            },
            new: Target::Object(commit),
        },
        name: notes_ref
            .try_into()
            .map_err(|e| anyhow!("invalid notes ref {notes_ref:?}: {e}"))?,
        deref: false,
    })?;
    Ok(())
}

/// Writing subcommands refuse to touch anything outside `refs/notes/`.
fn check_writable(notes_ref: &str, sub: &str) -> Result<Option<ExitCode>> {
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
fn resolve(repo: &gix::Repository, spec: &str) -> Result<ObjectId, String> {
    repo.rev_parse_single(spec)
        .map(|id| id.detach())
        .map_err(|_| format!("failed to resolve '{spec}' as a valid ref."))
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
            "-f" | "--force" => o.force = true,
            "--allow-empty" => o.allow_empty = true,
            "--stripspace" => o.stripspace = Some(true),
            "--no-stripspace" => o.stripspace = Some(false),
            "--separator" => o.separator = Some("\n".to_string()),
            "--no-separator" => o.separator = None,
            "-m" | "--message" => match detached(args, &mut i) {
                Some(v) => o.msgs.push(Msg { bytes: v.into_bytes(), strip: true }),
                None => return Ok(Err(msg_sub_usage(sub, &requires_value(a))?)),
            },
            "-F" | "--file" => match detached(args, &mut i) {
                Some(v) => match read_file(&v) {
                    Ok(b) => o.msgs.push(Msg { bytes: b, strip: true }),
                    Err(m) => return Ok(Err(fatal128(&m))),
                },
                None => return Ok(Err(msg_sub_usage(sub, &requires_value(a))?)),
            },
            "-C" | "--reuse-message" => match detached(args, &mut i) {
                Some(v) => match read_note_blob(repo, &v) {
                    Ok(b) => o.msgs.push(Msg { bytes: b, strip: false }),
                    Err(m) => return Ok(Err(fatal128(&m))),
                },
                None => return Ok(Err(msg_sub_usage(sub, &requires_value(a))?)),
            },
            "-e" | "--edit" => bail!("`-e`/`--edit` is not supported (it requires an editor)"),
            "-c" | "--reedit-message" => {
                bail!("`-c`/`--reedit-message` is not supported (it requires an editor)")
            }
            _ if a.starts_with("--separator=") => {
                o.separator = Some(a["--separator=".len()..].to_string())
            }
            _ if a.starts_with("--message=") => o.msgs.push(Msg {
                bytes: a["--message=".len()..].as_bytes().to_vec(),
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
                bytes: a[2..].as_bytes().to_vec(),
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

/// git's `usage_with_options()` path: `error:` then the usage block, exit 129.
fn usage(lines: &[&str], msg: &str) -> Result<ExitCode> {
    eprintln!("error: {msg}");
    for (n, l) in lines.iter().enumerate() {
        if n == 0 {
            eprintln!("usage: {l}");
        } else {
            eprintln!("   or: {l}");
        }
    }
    Ok(ExitCode::from(129))
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
    for (n, l) in lines.iter().enumerate() {
        eprintln!("{} {l}", if n == 0 { "usage:" } else { "   or:" });
    }
    eprintln!();
    if !options.is_empty() {
        for o in options {
            eprintln!("{o}");
        }
        eprintln!();
    }
    Ok(ExitCode::from(129))
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

/// The `add`/`append`/`edit` usage block, chosen by the subcommand name.
fn msg_sub_usage(sub: &str, msg: &str) -> Result<ExitCode> {
    match sub {
        "append" => sub_usage(msg, APPEND_USAGE, APPEND_OPTS),
        "edit" => sub_usage(msg, EDIT_USAGE, APPEND_OPTS),
        _ => sub_usage(msg, ADD_USAGE, ADD_OPTS),
    }
}

// ---------------------------------------------------------------------------
// subcommands
// ---------------------------------------------------------------------------

fn list(repo: &gix::Repository, notes_ref: &str, args: &[String]) -> Result<ExitCode> {
    // git runs parse-options first, so an unknown switch is reported before any
    // "too many arguments"; `list` itself has no options of its own.
    let mut positional: Vec<&String> = Vec::new();
    for a in args {
        if a.starts_with('-') && a != "-" {
            return sub_usage(&unknown_opt(a), LIST_USAGE, &[]);
        }
        positional.push(a);
    }
    if positional.len() > 1 {
        return sub_usage("too many arguments", LIST_USAGE, &[]);
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
    let mut positional: Vec<&String> = Vec::new();
    for a in args {
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

    let (notes, _) = load(repo, notes_ref)?;
    match notes.map.get(&object) {
        Some(note) => {
            // `git notes show` execs `git show <blob>`, which writes the blob
            // out untouched.
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
    if let Some(code) = check_writable(notes_ref, "add")? {
        return Ok(code);
    }

    let _lock = crate::lock::RepoLock::acquire(repo.git_dir());
    let (mut notes, parent) = load(repo, notes_ref)?;

    if notes.map.contains_key(&object) {
        if o.force {
            eprintln!("Overwriting existing notes for object {object}");
        } else if !o.msgs.is_empty() {
            eprintln!(
                "error: Cannot add notes. Found existing notes for object {object}. \
                 Use '-f' to overwrite existing notes"
            );
            return Ok(ExitCode::from(1));
        }
        // A bare `add` on an existing note with neither `-f` nor a message
        // falls through to git's editor path (the dumb-terminal error below).
    }
    if o.msgs.is_empty() {
        // git opens $EDITOR here; with no terminal it dies 128.
        eprintln!("error: Terminal is dumb, but EDITOR unset");
        eprintln!("fatal: please supply the note contents using either -m or -F option");
        return Ok(ExitCode::from(128));
    }

    let body = concat_messages(&o);
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
    if o.msgs.is_empty() {
        // git falls back to $EDITOR; with no terminal it dies 128.
        eprintln!("error: Terminal is dumb, but EDITOR unset");
        eprintln!("fatal: please supply the note contents using either -m or -F option");
        return Ok(ExitCode::from(128));
    }

    let spec = o.object.clone().unwrap_or_else(|| "HEAD".to_string());
    let object = match resolve(repo, &spec) {
        Ok(id) => id,
        Err(e) => {
            eprintln!("fatal: {e}");
            return Ok(ExitCode::from(128));
        }
    };
    if let Some(code) = check_writable(notes_ref, "append")? {
        return Ok(code);
    }

    let _lock = crate::lock::RepoLock::acquire(repo.git_dir());
    let (mut notes, parent) = load(repo, notes_ref)?;

    let mut body = concat_messages(&o);
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
/// A bare `edit` with no message option is the editor path, which cannot run
/// without an interactive terminal.
fn edit(repo: &gix::Repository, notes_ref: &str, args: &[String]) -> Result<ExitCode> {
    let o = match parse_msg_opts(repo, args, "edit")? {
        Ok(o) => o,
        Err(code) => return Ok(code),
    };
    if o.msgs.is_empty() {
        // git opens $EDITOR here; with no terminal it dies 128.
        eprintln!("error: Terminal is dumb, but EDITOR unset");
        eprintln!("fatal: please supply the note contents using either -m or -F option");
        return Ok(ExitCode::from(128));
    }
    eprintln!("The -m/-F/-c/-C options have been deprecated for the 'edit' subcommand.");
    eprintln!("Please use 'git notes add -f -m/-F/-c/-C' instead.");

    let spec = o.object.clone().unwrap_or_else(|| "HEAD".to_string());
    let object = match resolve(repo, &spec) {
        Ok(id) => id,
        Err(e) => {
            eprintln!("fatal: {e}");
            return Ok(ExitCode::from(128));
        }
    };
    if let Some(code) = check_writable(notes_ref, "edit")? {
        return Ok(code);
    }

    let _lock = crate::lock::RepoLock::acquire(repo.git_dir());
    let (mut notes, parent) = load(repo, notes_ref)?;

    let body = concat_messages(&o);
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
    let mut i = 0;
    while i < args.len() {
        let a = args[i].as_str();
        match a {
            "-f" | "--force" => force = true,
            "--stdin" => stdin = true,
            "--for-rewrite" => {
                i += 1;
                match args.get(i) {
                    Some(v) => for_rewrite = Some(v.clone()),
                    None => return sub_usage("option `for-rewrite' requires a value", COPY_USAGE, COPY_OPTS),
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
    if let Some(code) = check_writable(notes_ref, "copy")? {
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
    // Resolve the combine mode and whether we are active at all.
    let (active, combine) = match for_rewrite {
        None => (true, Combine::Overwrite),
        Some(cmd) => {
            let snap = repo.config_snapshot();
            let enabled = snap.boolean(&format!("notes.rewrite.{cmd}")).unwrap_or(true);
            let mut selected = std::env::var("GIT_NOTES_REWRITE_REF")
                .ok()
                .into_iter()
                .flat_map(|v| v.split(':').map(str::to_string).collect::<Vec<_>>())
                .any(|r| ref_selects(&r, notes_ref));
            if let Some(cfg) = snap.string("notes.rewriteRef") {
                if ref_selects(&cfg.to_str_lossy(), notes_ref) {
                    selected = true;
                }
            }
            let mode = std::env::var("GIT_NOTES_REWRITE_MODE")
                .ok()
                .or_else(|| snap.string("notes.rewriteMode").map(|s| s.to_string()))
                .unwrap_or_else(|| "concatenate".to_string());
            let combine = match mode.as_str() {
                "overwrite" => Combine::Overwrite,
                "cat_sort_uniq" => Combine::CatSortUniq,
                "ignore" => Combine::Ignore,
                _ => Combine::Concatenate,
            };
            (enabled && selected, combine)
        }
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

    if !active {
        return Ok(ExitCode::SUCCESS);
    }

    let _lock = crate::lock::RepoLock::acquire(repo.git_dir());
    let (mut notes, parent) = load(repo, notes_ref)?;
    let mut changed = false;
    let mut err = false;

    for (from_spec, to_spec) in pairs {
        let (Ok(from), Ok(to)) = (resolve(repo, &from_spec), resolve(repo, &to_spec)) else {
            eprintln!("fatal: Failed to resolve '{from_spec}' as a valid ref.");
            return Ok(ExitCode::from(128));
        };
        let Some(src) = notes.map.get(&from).copied() else {
            // No source note: nothing to copy, silently skip.
            continue;
        };
        let existing = notes.map.get(&to).copied();
        match combine {
            Combine::Overwrite => {
                if existing.is_some() && for_rewrite.is_none() && !force {
                    eprintln!("error: failed to copy notes from '{from}' to '{to}'");
                    err = true;
                    continue;
                }
                notes.map.insert(to, src);
                changed = true;
            }
            Combine::Ignore => {
                if existing.is_none() {
                    notes.map.insert(to, src);
                    changed = true;
                }
            }
            Combine::Concatenate | Combine::CatSortUniq => {
                let new = repo.find_object(src)?.data.clone();
                let body = match existing {
                    None => new,
                    Some(cur_id) => {
                        let cur = repo.find_object(cur_id)?.data.clone();
                        if combine == Combine::CatSortUniq {
                            combine_cat_sort_uniq(&cur, &new)
                        } else {
                            combine_concatenate(&cur, &new)
                        }
                    }
                };
                let blob = repo.write_blob(&body)?.detach();
                notes.map.insert(to, blob);
                changed = true;
            }
        }
    }

    if changed {
        commit_notes(repo, notes_ref, &notes, parent, "Notes added by 'git notes copy'")?;
    }
    if err {
        return Ok(ExitCode::from(1));
    }
    Ok(ExitCode::SUCCESS)
}

/// `notes.rewriteRef` entries are refs or globs; git matches with fnmatch, but a
/// plain ref name is by far the common case, so support an exact match plus a
/// trailing `*` glob.
fn ref_selects(pattern: &str, notes_ref: &str) -> bool {
    match pattern.strip_suffix('*') {
        Some(prefix) => notes_ref.starts_with(prefix),
        None => pattern == notes_ref,
    }
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
    for a in args {
        if literal {
            return usage(&["git notes prune [<options>]"], "too many arguments");
        }
        match a.as_str() {
            // `--` ends option parsing; prune takes no positional, so anything
            // after it is one argument too many.
            "--" => literal = true,
            "-n" | "--dry-run" => dry_run = true,
            "-v" | "--verbose" => verbose = true,
            s if s.starts_with("--") => {
                return usage(
                    &["git notes prune [<options>]"],
                    &format!("unknown option `{}'", &s[2..]),
                )
            }
            s if s.starts_with('-') && s != "-" => {
                let switch = s[1..].chars().next().unwrap_or(' ');
                return usage(
                    &["git notes prune [<options>]"],
                    &format!("unknown switch `{switch}'"),
                );
            }
            _ => return usage(&["git notes prune [<options>]"], "too many arguments"),
        }
    }
    if let Some(code) = check_writable(notes_ref, "prune")? {
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
    for a in args {
        match a.as_str() {
            "--ignore-missing" => ignore_missing = true,
            "--stdin" => from_stdin = true,
            s if s.starts_with('-') && s != "-" => {
                return sub_usage(&unknown_opt(s), REMOVE_USAGE, REMOVE_OPTS)
            }
            s => specs.push(s.to_string()),
        }
    }
    if let Some(code) = check_writable(notes_ref, "remove")? {
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

/// A merge-specific usage error: `error:` then the merge usage block, exit 129.
fn merge_usage(msg: &str) -> Result<ExitCode> {
    eprintln!("error: {msg}");
    merge_print_usage(&mut std::io::stderr())?;
    Ok(ExitCode::from(129))
}

/// `builtin/notes.c:merge()` — the notes-merge driver.
fn merge(repo: &gix::Repository, notes_ref: &str, args: &[String]) -> Result<ExitCode> {
    let mut verbosity: i32 = 0;
    let mut strategy: Option<String> = None;
    let mut do_commit = false;
    let mut do_abort = false;
    let mut positional: Vec<String> = Vec::new();
    let mut i = 0;
    while i < args.len() {
        let a = args[i].as_str();
        match a {
            "-v" | "--verbose" => verbosity += 1,
            "-q" | "--quiet" => verbosity -= 1,
            "--commit" => do_commit = true,
            "--abort" => do_abort = true,
            "-s" | "--strategy" => {
                i += 1;
                match args.get(i) {
                    Some(v) => strategy = Some(v.clone()),
                    None => return merge_usage(&requires_value(a)),
                }
            }
            s if s.starts_with("--strategy=") => {
                strategy = Some(s["--strategy=".len()..].to_string())
            }
            s if s.starts_with("-s") && s.len() > 2 => strategy = Some(s[2..].to_string()),
            "-h" => {
                merge_print_usage(&mut std::io::stdout())?;
                return Ok(ExitCode::from(129));
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

    // An explicit `-s/--strategy` is validated up front, exactly as git's
    // callback does. The config-driven default is resolved only once a real
    // merge is known to be happening: git consults the merge-strategy config
    // *after* the `--abort`/`--commit` early-outs, so a bad `notes.mergeStrategy`
    // never aborts an abort.
    let cli_strat = match strategy.as_deref() {
        None => None,
        Some(name) => match parse_strategy(name) {
            Some(s) => Some(s),
            None => return merge_usage(&format!("unknown -s/--strategy: {name}")),
        },
    };

    if do_abort {
        return merge_abort(repo);
    }
    if do_commit {
        return merge_commit(repo, verbosity);
    }
    if positional.len() != 1 {
        return merge_usage("must specify a notes ref to merge");
    }
    // Without `-s`, `notes.<name>.mergeStrategy` then the general
    // `notes.mergeStrategy` supply the strategy, falling back to git's `manual`.
    let strat = match cli_strat {
        Some(s) => s,
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
    let config = repo.config_snapshot();
    let file = config.plumbing();
    // git BUGs if the notes ref is not under refs/notes/, so the per-ref key is
    // only consulted when the prefix is present.
    if let Some(name) = notes_ref.strip_prefix("refs/notes/") {
        if let Some(s) = notes_strategy_config(file, Some(name))? {
            return Ok(s);
        }
    }
    if let Some(s) = notes_strategy_config(file, None)? {
        return Ok(s);
    }
    Ok(Strategy::Manual)
}

/// The effective `notes[.<subsection>].mergeStrategy` value, parsed. `Ok(None)`
/// when unset; a present-but-invalid value prints git's config error and yields
/// exit 128.
fn notes_strategy_config(
    file: &gix::config::File,
    subsection: Option<&str>,
) -> std::result::Result<Option<Strategy>, ExitCode> {
    // Walk the merged config in order so the last definition wins, keeping the
    // winning value's source metadata for the error message.
    let mut winner: Option<(BString, gix::config::file::Metadata)> = None;
    for section in file.sections() {
        let header = section.header();
        if !header.name().to_string().eq_ignore_ascii_case("notes") {
            continue;
        }
        // Subsection names are matched case-sensitively, byte for byte, exactly
        // as git compares the `notes.<name>` subsection.
        match (subsection, header.subsection_name()) {
            (Some(want), Some(have)) if have.to_string() == want => {}
            (None, None) => {}
            _ => continue,
        }
        if let Some(v) = section.body().value("mergeStrategy") {
            winner = Some((v, section.meta().clone()));
        }
    }
    let Some((value, meta)) = winner else {
        return Ok(None);
    };
    match parse_strategy(&value.to_str_lossy()) {
        Some(s) => Ok(Some(s)),
        None => {
            let key = match subsection {
                Some(name) => format!("notes.{name}.mergeStrategy"),
                None => "notes.mergeStrategy".to_string(),
            };
            Err(notes_config_fatal(&key, &value.to_str_lossy(), &meta))
        }
    }
}

/// `notes-utils.c:git_config_get_notes_strategy()` reaching `git_die_config()`:
/// the `error:` reason then a `fatal:` naming the config source, exit 128. gix
/// records no per-value line number, so the `at line <n>` tail git appends is
/// omitted — the same limitation the crate's other config-fatal paths carry.
fn notes_config_fatal(key: &str, value: &str, meta: &gix::config::file::Metadata) -> ExitCode {
    eprintln!("error: unknown notes merge strategy {value}");
    let origin = match meta.source {
        gix::config::Source::Cli | gix::config::Source::Env => {
            format!("unable to parse '{key}' from command-line config")
        }
        _ => match &meta.path {
            Some(path) => format!("bad config variable '{key}' in file '{}'", path.display()),
            None => format!("bad config variable '{key}'"),
        },
    };
    eprintln!("fatal: {origin}");
    ExitCode::from(128)
}

/// Move a notes ref, writing git's `notes: `-prefixed reflog line.
fn move_notes_ref(
    repo: &gix::Repository,
    notes_ref: &str,
    from: Option<ObjectId>,
    to: ObjectId,
    reflog: &str,
) -> Result<()> {
    repo.edit_reference(RefEdit {
        change: Change::Update {
            log: LogChange {
                mode: RefLog::AndReference,
                force_create_reflog: false,
                message: format!("notes: {reflog}").into(),
            },
            expected: match from {
                Some(p) => PreviousValue::MustExistAndMatch(Target::Object(p)),
                None => PreviousValue::MustNotExist,
            },
            new: Target::Object(to),
        },
        name: notes_ref
            .try_into()
            .map_err(|e| anyhow!("invalid notes ref {notes_ref:?}: {e}"))?,
        deref: false,
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
    let remote_ref = expand_notes_ref(remote_spec);
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
            move_notes_ref(repo, local_ref, None, r, &reflog)?;
            return Ok(ExitCode::SUCCESS);
        }
        (Some(l), Some(r)) => (l, r),
    };

    if l == r {
        if verbosity >= 0 {
            println!("Already up to date.");
        }
        return Ok(ExitCode::SUCCESS);
    }
    let base = repo.merge_bases_many(l, &[r])?.into_iter().next().map(|id| id.detach());
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
        move_notes_ref(repo, local_ref, Some(l), r, &reflog)?;
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
    let mut conflicts: Vec<(ObjectId, Vec<u8>)> = Vec::new();
    let blob = |id: Option<ObjectId>| -> Result<Vec<u8>> {
        Ok(match id {
            Some(id) => repo.find_object(id)?.data.clone(),
            None => Vec::new(),
        })
    };

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
                Strategy::Union => {
                    if verbosity >= 0 {
                        println!("Concatenating local and remote notes for {obj}");
                    }
                    let body = combine_concatenate(&blob(lo)?, &blob(re)?);
                    Some(repo.write_blob(&body)?.detach())
                }
                Strategy::CatSortUniq => {
                    if verbosity >= 0 {
                        println!("Concatenating unique lines in local and remote notes for {obj}");
                    }
                    let body = combine_cat_sort_uniq(&blob(lo)?, &blob(re)?);
                    Some(repo.write_blob(&body)?.detach())
                }
                Strategy::Manual => {
                    if verbosity >= 0 {
                        println!("Auto-merging notes for {obj}");
                    }
                    if verbosity >= -1 {
                        println!(
                            "CONFLICT (content): Merge conflict in notes for object {obj}"
                        );
                    }
                    let content = conflict_content(local_ref, &remote_ref, &blob(lo)?, &blob(re)?);
                    conflicts.push((*obj, content));
                    // Conflicted notes stay out of the partial tree.
                    None
                }
            }
        };
        if let Some(id) = result {
            merged.insert(*obj, id);
        }
    }

    let out_notes = Notes {
        map: merged,
        non_notes: local_notes.non_notes.clone(),
    };

    if conflicts.is_empty() {
        // Clean merge: a real two-parent merge commit.
        let tree_id = write_tree(repo, &out_notes)?;
        let commit = repo
            .new_commit(format!("{reflog}\n"), tree_id, vec![l, r])?
            .id()
            .detach();
        move_notes_ref(repo, local_ref, Some(l), commit, &reflog)?;
        Ok(ExitCode::SUCCESS)
    } else {
        // Manual strategy with conflicts: stage the partial merge on disk.
        let mut msg = format!("{reflog}\n\nConflicts:\n");
        for (obj, _) in &conflicts {
            msg.push('\t');
            msg.push_str(&obj.to_string());
            msg.push('\n');
        }
        let tree_id = write_tree(repo, &out_notes)?;
        let partial = repo.new_commit(msg, tree_id, vec![l, r])?.id().detach();

        let git_dir = repo.git_dir();
        std::fs::write(git_dir.join("NOTES_MERGE_PARTIAL"), format!("{partial}\n"))?;
        std::fs::write(git_dir.join("NOTES_MERGE_REF"), format!("ref: {local_ref}\n"))?;
        let wt = git_dir.join("NOTES_MERGE_WORKTREE");
        std::fs::create_dir_all(&wt)?;
        for (obj, content) in &conflicts {
            std::fs::write(wt.join(obj.to_string()), content)?;
        }
        // git prints the worktree path relative to the cwd (`.git/…` from the
        // repo root), not the absolute git-dir path.
        let wt_display = std::env::current_dir()
            .ok()
            .and_then(|cwd| wt.strip_prefix(&cwd).ok().map(|p| p.display().to_string()))
            .unwrap_or_else(|| wt.display().to_string());
        eprintln!(
            "Automatic notes merge failed. Fix conflicts in {wt_display} and commit the result with 'git notes merge --commit', or abort the merge with 'git notes merge --abort'."
        );
        Ok(ExitCode::from(1))
    }
}

/// The `<<<<<<< / ======= / >>>>>>>` blob git's `ll_merge` writes for a note
/// whose whole content conflicts (the case for single-block notes).
fn conflict_content(local_ref: &str, remote_ref: &str, l: &[u8], r: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(format!("<<<<<<< {local_ref}\n").as_bytes());
    out.extend_from_slice(l);
    if l.last() != Some(&b'\n') {
        out.push(b'\n');
    }
    out.extend_from_slice(b"=======\n");
    out.extend_from_slice(r);
    if r.last() != Some(&b'\n') {
        out.push(b'\n');
    }
    out.extend_from_slice(format!(">>>>>>> {remote_ref}\n").as_bytes());
    out
}

/// `git notes merge --commit` — finalize a manual merge staged on disk.
fn merge_commit(repo: &gix::Repository, _verbosity: i32) -> Result<ExitCode> {
    let git_dir = repo.git_dir();
    let partial_raw = match std::fs::read_to_string(git_dir.join("NOTES_MERGE_PARTIAL")) {
        Ok(s) => s,
        Err(_) => {
            eprintln!("fatal: failed to read ref NOTES_MERGE_PARTIAL");
            return Ok(ExitCode::from(128));
        }
    };
    let partial = ObjectId::from_hex(partial_raw.trim().as_bytes())
        .map_err(|e| anyhow!("invalid NOTES_MERGE_PARTIAL: {e}"))?;
    let local_ref = std::fs::read_to_string(git_dir.join("NOTES_MERGE_REF"))?
        .trim()
        .strip_prefix("ref:")
        .map(|s| s.trim().to_string())
        .ok_or_else(|| anyhow!("invalid NOTES_MERGE_REF"))?;

    let _lock = crate::lock::RepoLock::acquire(git_dir);
    let partial_commit = repo.find_commit(partial)?;
    let parents: Vec<ObjectId> = partial_commit.parent_ids().map(|id| id.detach()).collect();
    let full_msg = partial_commit.message_raw()?.to_string();
    // The final commit drops the trailing `\n\nConflicts:` section.
    let headline = full_msg.split("\n\nConflicts:").next().unwrap_or(&full_msg);
    let reflog = headline.trim_end_matches('\n').to_string();

    let mut notes = load_from_commit(repo, partial)?;
    let wt = git_dir.join("NOTES_MERGE_WORKTREE");
    if let Ok(entries) = std::fs::read_dir(&wt) {
        for entry in entries.flatten() {
            let name = entry.file_name();
            let Some(name) = name.to_str() else { continue };
            let Ok(obj) = ObjectId::from_hex(name.as_bytes()) else {
                continue;
            };
            let content = std::fs::read(entry.path())?;
            if content.is_empty() {
                notes.map.remove(&obj);
            } else {
                let blob = repo.write_blob(&content)?.detach();
                notes.map.insert(obj, blob);
            }
        }
    }

    let tree_id = write_tree(repo, &notes)?;
    let commit = repo
        .new_commit(format!("{reflog}\n"), tree_id, parents)?
        .id()
        .detach();
    let local_tip = repo
        .try_find_reference(local_ref.as_str())?
        .map(|r| r.into_fully_peeled_id())
        .transpose()?
        .map(|id| id.detach());
    move_notes_ref(repo, &local_ref, local_tip, commit, &reflog)?;

    // Clear the staged merge.
    let _ = std::fs::remove_file(git_dir.join("NOTES_MERGE_PARTIAL"));
    let _ = std::fs::remove_file(git_dir.join("NOTES_MERGE_REF"));
    let _ = std::fs::remove_dir_all(&wt);
    Ok(ExitCode::SUCCESS)
}

/// `git notes merge --abort` — discard a staged manual merge.
fn merge_abort(repo: &gix::Repository) -> Result<ExitCode> {
    let git_dir = repo.git_dir();
    let wt = git_dir.join("NOTES_MERGE_WORKTREE");
    if !wt.exists() {
        eprintln!("error: failed to remove 'git notes merge' worktree");
        return Ok(ExitCode::from(1));
    }
    let _ = std::fs::remove_dir_all(&wt);
    let _ = std::fs::remove_file(git_dir.join("NOTES_MERGE_PARTIAL"));
    let _ = std::fs::remove_file(git_dir.join("NOTES_MERGE_REF"));
    Ok(ExitCode::SUCCESS)
}

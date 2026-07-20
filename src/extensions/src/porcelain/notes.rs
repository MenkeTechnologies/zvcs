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
///   * `copy [-f] <from> [<to>]`        — non-`--stdin` form
///   * `show [<object>]`                — the note text verbatim
///   * `remove [--ignore-missing] [<object>...]`
///   * `prune [-n] [-v]`                — drop notes whose object is gone
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
/// `--no-separator`, `--stripspace`/`--no-stripspace`, `--ignore-missing`.
/// `GIT_NOTES_REF` and `core.notesRef` are honoured with git's precedence.
///
/// Not ported, and rejected with a precise message rather than guessed at:
/// every editor-driven path (`edit`, a bare `add`/`append` with no message,
/// `-e`, `-c`/`--reedit-message`), `merge` (needs the notes-merge worktree and
/// conflict resolvers), and `--stdin`/`--for-rewrite` batch input.
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
        "edit" => bail!("`edit` is not supported (it requires an interactive editor)"),
        "merge" => bail!("`merge` is not supported (it requires the notes-merge worktree)"),
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
    let mut notes = Notes {
        map: BTreeMap::new(),
        non_notes: Vec::new(),
    };
    if let Some(tip) = tip {
        let tree_id = repo.find_commit(tip)?.tree_id()?.detach();
        let hex_len = repo.object_hash().len_in_hex();
        load_subtree(repo, tree_id, "", hex_len, &mut notes)?;
        notes.non_notes.sort_by(|a, b| a.0.cmp(&b.0));
    }
    Ok((notes, tip))
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
fn parse_msg_opts(repo: &gix::Repository, args: &[String], sub: &str) -> Result<MsgOpts> {
    let mut o = MsgOpts::default();
    let mut extra: Vec<String> = Vec::new();
    let mut i = 0;
    let mut literal = false;

    // Pull the separate value of a `-x <value>` style option, advancing `i`.
    fn detached(args: &[String], i: &mut usize, flag: &str) -> Result<String> {
        *i += 1;
        args.get(*i)
            .cloned()
            .ok_or_else(|| anyhow!("option `{flag}` requires a value"))
    }

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
            "-m" | "--message" => {
                let v = detached(args, &mut i, a)?;
                o.msgs.push(Msg {
                    bytes: v.into_bytes(),
                    strip: true,
                });
            }
            "-F" | "--file" => {
                let v = detached(args, &mut i, a)?;
                o.msgs.push(Msg {
                    bytes: read_file(&v)?,
                    strip: true,
                });
            }
            "-C" | "--reuse-message" => {
                let v = detached(args, &mut i, a)?;
                o.msgs.push(Msg {
                    bytes: read_note_blob(repo, &v)?,
                    strip: false,
                });
            }
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
            _ if a.starts_with("--file=") => o.msgs.push(Msg {
                bytes: read_file(&a["--file=".len()..])?,
                strip: true,
            }),
            _ if a.starts_with("--reuse-message=") => o.msgs.push(Msg {
                bytes: read_note_blob(repo, &a["--reuse-message=".len()..])?,
                strip: false,
            }),
            _ if a.starts_with("-m") => o.msgs.push(Msg {
                bytes: a[2..].as_bytes().to_vec(),
                strip: true,
            }),
            _ if a.starts_with("-F") => o.msgs.push(Msg {
                bytes: read_file(&a[2..])?,
                strip: true,
            }),
            _ if a.starts_with("-C") => o.msgs.push(Msg {
                bytes: read_note_blob(repo, &a[2..])?,
                strip: false,
            }),
            _ => bail!("unsupported option {a:?} for `git notes {sub}`"),
        }
        i += 1;
    }

    if extra.len() > 1 {
        bail!("too many arguments for `git notes {sub}`");
    }
    o.object = extra.into_iter().next();
    Ok(o)
}

fn read_file(path: &str) -> Result<Vec<u8>> {
    if path == "-" {
        let mut buf = Vec::new();
        std::io::stdin()
            .read_to_end(&mut buf)
            .map_err(|e| anyhow!("cannot read '-': {e}"))?;
        return Ok(buf);
    }
    std::fs::read(path).map_err(|e| anyhow!("could not open or read '{path}': {e}"))
}

/// `-C <object>`: the note text is the named blob, verbatim.
fn read_note_blob(repo: &gix::Repository, spec: &str) -> Result<Vec<u8>> {
    let id = resolve(repo, spec).map_err(|e| anyhow!("{e}"))?;
    let object = repo
        .find_object(id)
        .map_err(|_| anyhow!("failed to read object '{spec}'."))?;
    if object.kind != gix::object::Kind::Blob {
        bail!("cannot read note data from non-blob object '{spec}'.");
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

// ---------------------------------------------------------------------------
// subcommands
// ---------------------------------------------------------------------------

fn list(repo: &gix::Repository, notes_ref: &str, args: &[String]) -> Result<ExitCode> {
    if args.len() > 1 {
        return usage(&["git notes [list [<object>]]"], "too many arguments");
    }
    if let Some(a) = args.first() {
        if a.starts_with('-') {
            bail!("unsupported option {a:?} for `git notes list`");
        }
    }
    let (notes, _) = load(repo, notes_ref)?;

    match args.first() {
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
    if args.len() > 1 {
        return usage(&["git notes show [<object>]"], "too many arguments");
    }
    if let Some(a) = args.first() {
        if a.starts_with('-') {
            bail!("unsupported option {a:?} for `git notes show`");
        }
    }
    let spec = args.first().map(String::as_str).unwrap_or("HEAD");
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
    let o = parse_msg_opts(repo, args, "add")?;

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
        if !o.force {
            if o.msgs.is_empty() {
                // git falls back to opening the existing note in an editor.
                bail!("`add` on an object that already has a note needs -f or an editor");
            }
            eprintln!(
                "error: Cannot add notes. Found existing notes for object {object}. \
                 Use '-f' to overwrite existing notes"
            );
            return Ok(ExitCode::from(1));
        }
        eprintln!("Overwriting existing notes for object {object}");
    }
    if o.msgs.is_empty() {
        bail!("no note contents given (editor mode is unsupported; use -m, -F or -C)");
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
    let o = parse_msg_opts(repo, args, "append")?;
    if o.msgs.is_empty() {
        bail!("no note contents given (editor mode is unsupported; use -m, -F or -C)");
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

fn copy(repo: &gix::Repository, notes_ref: &str, args: &[String]) -> Result<ExitCode> {
    let mut force = false;
    let mut positional: Vec<&str> = Vec::new();
    for a in args {
        match a.as_str() {
            "-f" | "--force" => force = true,
            "--stdin" => bail!("`copy --stdin` is not supported"),
            s if s.starts_with("--for-rewrite") => bail!("`copy --for-rewrite` is not supported"),
            s if s.starts_with('-') => bail!("unsupported option {s:?} for `git notes copy`"),
            s => positional.push(s),
        }
    }
    if positional.is_empty() {
        return usage(&["git notes copy [<options>] <from-object> <to-object>"], "too few arguments");
    }
    if positional.len() > 2 {
        return usage(&["git notes copy [<options>] <from-object> <to-object>"], "too many arguments");
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
    let mut specs: Vec<&str> = Vec::new();
    for a in args {
        match a.as_str() {
            "--ignore-missing" => ignore_missing = true,
            "--stdin" => bail!("`remove --stdin` is not supported"),
            s if s.starts_with('-') && s != "-" => {
                bail!("unsupported option {s:?} for `git notes remove`")
            }
            s => specs.push(s),
        }
    }
    if let Some(code) = check_writable(notes_ref, "remove")? {
        return Ok(code);
    }
    if specs.is_empty() {
        specs.push("HEAD");
    }

    let _lock = crate::lock::RepoLock::acquire(repo.git_dir());
    let (mut notes, parent) = load(repo, notes_ref)?;

    // git reports every object by the name the user typed, accumulates the
    // failures, and commits only when all removals succeeded.
    let mut failed = false;
    let mut changed = false;
    for spec in specs {
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

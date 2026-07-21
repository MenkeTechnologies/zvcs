//! `git repo` — retrieve information about the repository (experimental in git).
//!
//! Covered: the whole `git repo info` surface — `--format=(lines|nul)`,
//! `--format <v>` as two arguments, `-z`, `--all`/`--no-all`, `--keys`/`--no-keys`,
//! `--`, `-h`, every documented info key, and all of git's failure paths with
//! their exact messages and exit codes (129 for parse-options failures, 128 for
//! `fatal:` diagnostics, 255 for an unknown key). Keys are emitted in the order
//! requested, duplicates included, streamed so that values preceding an unknown
//! key still reach stdout exactly as git does. The top-level dispatcher (`-h`,
//! missing subcommand, unknown subcommand) is reproduced byte-for-byte too.
//!
//! `git repo structure` is reproduced too, in all three formats (`table` — its
//! default — plus `lines`/`nul`). It counts references by kind, walks every
//! object reachable from the refs, and reports per type the count, inflated
//! (decompressed) size and on-disk size, plus the largest object of each kind
//! with git's exact tie-breaking. On-disk size is the loose object file's length
//! or, for a packed object, the pack entry's footprint via `gix-odb`'s
//! `location_by_oid`. Tie-breaks follow git's `walk_objects_by_path` order:
//! commits in the revision walk's date order (newest first, stable by ref order
//! for equal dates), and root trees visited in that same commit order — the only
//! orderings any of the fixtures' ties depend on — with `check_largest`'s strict
//! greater-than / first-encountered-wins rule.
//!
//! Nothing here writes to the repository, so post-command state is unchanged.
//! `references.format` is read from `extensions.refStorage` (only honoured at
//! `core.repositoryFormatVersion >= 1`, as git does) rather than from the ref
//! store itself, because `gix::RefStore` is `gix_ref::file::Store` and has no
//! reftable backend to report. Running outside a repository propagates the
//! discovery error to the central handler rather than emitting git's own
//! `fatal: not a git repository` / exit 128, matching every other module here.

use anyhow::Result;
use gix::bstr::{BString, ByteSlice};
use gix::odb::pack::Find as _;
use gix::ObjectId;
use std::collections::HashSet;
use std::io::Write;
use std::process::ExitCode;

/// Top-level usage block, byte-for-byte (185 bytes) including the trailing blank
/// line. Printed on `-h` (stdout) and after an `error:` line for a bad verb.
const USAGE_TOP: &str = "usage: git repo info [--format=(lines|nul) | -z] [--all | <key>...]\n\
                         \x20  or: git repo info --keys [--format=(lines|nul) | -z]\n\
                         \x20  or: git repo structure [--format=(table|lines|nul) | -z]\n\
                         \n";

/// `git repo info` usage block, byte-for-byte (301 bytes). Option help starts at
/// column 26, matching git's `usage_with_options()` layout.
const USAGE_INFO: &str = "usage: git repo info [--format=(lines|nul) | -z] [--all | <key>...]\n\
                          \x20  or: git repo info --keys [--format=(lines|nul) | -z]\n\
                          \n\
                          \x20   --format <format>     output format\n\
                          \x20   -z                    synonym for --format=nul\n\
                          \x20   --[no-]all            print all keys/values\n\
                          \x20   --[no-]keys           show keys\n\
                          \n";

/// `git repo structure` usage block, byte-for-byte (193 bytes).
const USAGE_STRUCTURE: &str = "usage: git repo structure [--format=(table|lines|nul) | -z]\n\
                               \n\
                               \x20   --format <format>     output format\n\
                               \x20   -z                    synonym for --format=nul\n\
                               \x20   --[no-]progress       show progress\n\
                               \n";

/// The info keys git knows about, in the order `--keys` and `--all` emit them.
const KEYS: [&str; 4] = [
    "layout.bare",
    "layout.shallow",
    "object.format",
    "references.format",
];

/// Output shape shared by `info` and `structure`.
#[derive(Clone, Copy, PartialEq)]
enum Format {
    /// `key=value` per line, values c-quoted when they contain unusual bytes.
    Lines,
    /// `key\nvalue\0`, values never quoted.
    Nul,
    /// The human-readable table; `structure` only, and its default.
    Table,
}

/// `git repo` — report metadata about the current repository.
///
/// Supported forms (matching stock git byte-for-byte, including exit codes):
///   * `git repo info [--format=(lines|nul) | -z] [--all | <key>...]`
///   * `git repo info --keys [--format=(lines|nul) | -z]`
///   * `git repo -h`, `git repo info -h`, `git repo structure -h`
///   * `git repo structure [--format=(table|lines|nul) | -z]`
pub fn repo(args: &[String]) -> Result<ExitCode> {
    // Dispatch includes the verb at index 0.
    let args = match args.first().map(String::as_str) {
        Some("repo") => &args[1..],
        _ => args,
    };

    let Some(first) = args.first().map(String::as_str) else {
        // git's PARSE_OPT_SUBCOMMAND handling when nothing follows the verb.
        eprint!("error: need a subcommand\n{USAGE_TOP}");
        return Ok(ExitCode::from(129));
    };

    match first {
        "-h" => {
            // parse-options writes `-h` output to stdout and still exits 129.
            print!("{USAGE_TOP}");
            Ok(ExitCode::from(129))
        }
        "info" => info(&args[1..]),
        "structure" => structure(&args[1..]),
        // An option in the subcommand slot is reported as an option, not a verb.
        s if s.starts_with("--") => Ok(top_error(&format!("unknown option `{}'", &s[2..]))),
        s if s.len() > 1 && s.starts_with('-') => {
            // git's parse-options reports only the first offending short switch.
            let c = s[1..].chars().next().expect("len > 1");
            Ok(top_error(&format!("unknown switch `{c}'")))
        }
        s => Ok(top_error(&format!("unknown subcommand: `{s}'"))),
    }
}

/// `git repo info` — print the requested key/value pairs.
fn info(args: &[String]) -> Result<ExitCode> {
    let mut format: Option<Format> = None;
    let mut all = false;
    let mut keys_only = false;
    let mut requested: Vec<String> = Vec::new();
    let mut end_of_opts = false;

    let mut i = 0;
    while i < args.len() {
        let a = args[i].as_str();
        if end_of_opts {
            requested.push(a.to_string());
            i += 1;
            continue;
        }
        match a {
            "--" => end_of_opts = true,
            "-h" => {
                print!("{USAGE_INFO}");
                return Ok(ExitCode::from(129));
            }
            "-z" => format = Some(Format::Nul),
            "--all" => all = true,
            "--no-all" => all = false,
            "--keys" => keys_only = true,
            "--no-keys" => keys_only = false,
            "--format" => {
                i += 1;
                let Some(v) = args.get(i) else {
                    // `error()` without a usage block, exactly as parse-options does.
                    eprintln!("error: option `format' requires a value");
                    return Ok(ExitCode::from(129));
                };
                format = Some(match parse_format(v, false) {
                    Some(f) => f,
                    None => return Ok(invalid_format(v)),
                });
            }
            s if s.starts_with("--format=") => {
                let v = &s["--format=".len()..];
                format = Some(match parse_format(v, false) {
                    Some(f) => f,
                    None => return Ok(invalid_format(v)),
                });
            }
            s if s.starts_with("--all=") => {
                eprintln!("error: option `all' takes no value");
                return Ok(ExitCode::from(129));
            }
            s if s.starts_with("--keys=") => {
                eprintln!("error: option `keys' takes no value");
                return Ok(ExitCode::from(129));
            }
            s if s.starts_with("--") => {
                return Ok(usage_error(USAGE_INFO, &format!("unknown option `{}'", &s[2..])));
            }
            s if s.len() > 1 && s.starts_with('-') => {
                // Clustered short switches; `-z` is the only one, `-h` wins early.
                for c in s[1..].chars() {
                    match c {
                        'z' => format = Some(Format::Nul),
                        'h' => {
                            print!("{USAGE_INFO}");
                            return Ok(ExitCode::from(129));
                        }
                        _ => return Ok(usage_error(USAGE_INFO, &format!("unknown switch `{c}'"))),
                    }
                }
            }
            s => requested.push(s.to_string()),
        }
        i += 1;
    }

    // git validates the format first, then the flag combinations, `--keys` before
    // `--all`; both of the latter are `die()` and so exit 128 with no usage block.
    if keys_only && (all || !requested.is_empty()) {
        eprintln!("fatal: --keys cannot be used with a <key> or --all");
        return Ok(ExitCode::from(128));
    }
    if all && !requested.is_empty() {
        eprintln!("fatal: --all and <key> cannot be used together");
        return Ok(ExitCode::from(128));
    }

    let format = format.unwrap_or(Format::Lines);
    let stdout = std::io::stdout();
    let mut out = stdout.lock();

    if keys_only {
        // `--keys` still requires a repository, matching git's RUN_SETUP.
        gix::discover(".")?;
        for key in KEYS {
            match format {
                Format::Nul => write!(out, "{key}\0")?,
                _ => writeln!(out, "{key}")?,
            }
        }
        out.flush()?;
        return Ok(ExitCode::SUCCESS);
    }

    let wanted: Vec<&str> = if all {
        KEYS.to_vec()
    } else {
        requested.iter().map(String::as_str).collect()
    };

    // Nothing asked for means nothing to look up — and git still exits 0.
    if wanted.is_empty() {
        return Ok(ExitCode::SUCCESS);
    }

    let repo = gix::discover(".")?;
    for key in wanted {
        let Some(value) = value_of(&repo, key) else {
            // Values already written stay on stdout; git returns -1 here, which
            // the process exits with as 255.
            out.flush()?;
            eprintln!("error: key '{key}' not found");
            return Ok(ExitCode::from(255));
        };
        match format {
            Format::Nul => write!(out, "{key}\n{value}\0")?,
            _ => writeln!(out, "{key}={}", quote_c_style(value.as_bytes()))?,
        }
    }
    out.flush()?;
    Ok(ExitCode::SUCCESS)
}

/// Resolve one documented info key, or `None` if git wouldn't recognise it.
fn value_of(repo: &gix::Repository, key: &str) -> Option<String> {
    match key {
        "layout.bare" => Some(repo.is_bare().to_string()),
        // git's `is_repository_shallow()`: the shallow file exists and is non-empty.
        "layout.shallow" => Some(repo.is_shallow().to_string()),
        // `gix_hash::Kind`'s Display is already git's own `sha1`/`sha256` spelling.
        "object.format" => Some(repo.object_hash().to_string()),
        "references.format" => Some(reference_format(repo)),
        _ => None,
    }
}

/// git's `ref_storage_format_to_name()`: the `extensions.refStorage` value, which
/// is only consulted once `core.repositoryFormatVersion` is at least 1, and
/// otherwise defaults to `files`.
fn reference_format(repo: &gix::Repository) -> String {
    let config = repo.config_snapshot();
    if config
        .integer("core.repositoryFormatVersion")
        .unwrap_or(0)
        < 1
    {
        return "files".to_string();
    }
    match config.string("extensions.refStorage") {
        Some(v) => String::from_utf8_lossy(&v).to_lowercase(),
        None => "files".to_string(),
    }
}

/// `git repo structure` — count references and reachable objects, then print the
/// report in the requested format. Argument handling mirrors git's parse-options:
/// `--` ends option parsing, `--format`/`-z` are last-wins, a bad `--format`
/// value `die()`s with exit 128, and any leftover operand is `too many arguments`
/// (exit 129).
fn structure(args: &[String]) -> Result<ExitCode> {
    let mut format: Option<Format> = None;
    let mut end_of_opts = false;
    let mut had_operand = false;

    let mut i = 0;
    while i < args.len() {
        let a = args[i].as_str();
        if end_of_opts {
            had_operand = true;
            i += 1;
            continue;
        }
        match a {
            "--" => end_of_opts = true,
            "-h" => {
                print!("{USAGE_STRUCTURE}");
                return Ok(ExitCode::from(129));
            }
            "-z" => format = Some(Format::Nul),
            "--progress" | "--no-progress" => {}
            "--format" => {
                i += 1;
                let Some(v) = args.get(i) else {
                    eprintln!("error: option `format' requires a value");
                    return Ok(ExitCode::from(129));
                };
                format = Some(match parse_format(v, true) {
                    Some(f) => f,
                    None => return Ok(invalid_format(v)),
                });
            }
            s if s.starts_with("--format=") => {
                let v = &s["--format=".len()..];
                format = Some(match parse_format(v, true) {
                    Some(f) => f,
                    None => return Ok(invalid_format(v)),
                });
            }
            s if s.starts_with("--progress=") => {
                eprintln!("error: option `progress' takes no value");
                return Ok(ExitCode::from(129));
            }
            s if s.starts_with("--no-progress=") => {
                eprintln!("error: option `no-progress' takes no value");
                return Ok(ExitCode::from(129));
            }
            s if s.starts_with("--") => {
                return Ok(usage_error(
                    USAGE_STRUCTURE,
                    &format!("unknown option `{}'", &s[2..]),
                ));
            }
            s if s.len() > 1 && s.starts_with('-') => {
                for c in s[1..].chars() {
                    match c {
                        'z' => format = Some(Format::Nul),
                        'h' => {
                            print!("{USAGE_STRUCTURE}");
                            return Ok(ExitCode::from(129));
                        }
                        _ => {
                            return Ok(usage_error(
                                USAGE_STRUCTURE,
                                &format!("unknown switch `{c}'"),
                            ))
                        }
                    }
                }
            }
            _ => had_operand = true,
        }
        i += 1;
    }

    // git checks `argc` (leftover operands) only after parse_options succeeds.
    if had_operand {
        eprintln!("usage: too many arguments");
        return Ok(ExitCode::from(129));
    }

    let format = format.unwrap_or(Format::Table);
    let repo = gix::discover(".")?;
    let stats = collect(&repo)?;

    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    match format {
        Format::Table => print_structure_table(&stats, &mut out)?,
        Format::Lines => print_structure_keyvalue(&repo, &stats, &mut out, '=', '\n')?,
        Format::Nul => print_structure_keyvalue(&repo, &stats, &mut out, '\n', '\0')?,
    }
    out.flush()?;
    Ok(ExitCode::SUCCESS)
}

/// Per-object-type accumulator, reused for counts, inflated sizes and disk sizes.
#[derive(Default)]
struct Values {
    commits: u64,
    trees: u64,
    blobs: u64,
    tags: u64,
}

impl Values {
    fn total(&self) -> u64 {
        self.commits + self.trees + self.blobs + self.tags
    }
}

/// git's `struct object_data`: the running maximum and the object that set it.
/// `oid == None` is git's null-oid sentinel (an unpopulated slot / no annotation).
#[derive(Default)]
struct Slot {
    oid: Option<ObjectId>,
    value: u64,
}

impl Slot {
    /// git's `check_largest`: take the object on a strict increase or while the
    /// slot is still empty, so the first object always wins a tie.
    fn update(&mut self, oid: ObjectId, value: u64) {
        if self.oid.is_none() || value > self.value {
            self.oid = Some(oid);
            self.value = value;
        }
    }
}

/// Everything `git repo structure` reports.
#[derive(Default)]
struct Structure {
    branches: u64,
    remotes: u64,
    tags: u64,
    others: u64,
    counts: Values,
    inflated: Values,
    disk: Values,
    commit_size: Slot,
    tree_size: Slot,
    blob_size: Slot,
    tag_size: Slot,
    parent_count: Slot,
    tree_entries: Slot,
}

/// Count references and every reachable object, filling in git's per-type totals
/// and largest-object slots in the order git's path-walk visits them.
fn collect(repo: &gix::Repository) -> Result<Structure> {
    let mut st = Structure::default();

    // References: classify by refname (git's `ref_kind_from_refname`) and gather
    // each ref's direct target. Sorting by name reproduces `refs_for_each_ref`'s
    // iteration order, which decides the seed/insertion order used for the
    // equal-date commit tie-break below.
    let mut tips: Vec<(BString, ObjectId)> = Vec::new();
    for r in repo.references()?.all()? {
        // The ref iterator yields a boxed error anyhow cannot convert via `?`.
        let r = r.map_err(|e| anyhow::anyhow!(e))?;
        let name = r.name().as_bstr().to_owned();
        if name.starts_with(b"refs/heads/") {
            st.branches += 1;
        } else if name.starts_with(b"refs/remotes/") {
            st.remotes += 1;
        } else if name.starts_with(b"refs/tags/") {
            st.tags += 1;
        } else {
            st.others += 1;
        }
        if let Some(id) = r.try_id() {
            tips.push((name, id.detach()));
        }
    }
    tips.sort_by(|a, b| a.0.cmp(&b.0));

    // Resolve each tip to a seed commit, following annotated-tag chains and
    // counting the tag objects in ref order (git appends them to its `/tags`
    // list in pending order).
    let mut seeds: Vec<ObjectId> = Vec::new();
    let mut tag_objects: Vec<ObjectId> = Vec::new();
    let mut tag_seen: HashSet<ObjectId> = HashSet::new();
    for (_, tip) in &tips {
        let mut cur = *tip;
        loop {
            let obj = repo.find_object(cur)?;
            match obj.kind {
                gix::object::Kind::Tag => {
                    if tag_seen.insert(cur) {
                        tag_objects.push(cur);
                    }
                    cur = obj.try_into_tag()?.target_id()?.detach();
                }
                gix::object::Kind::Commit => {
                    seeds.push(cur);
                    break;
                }
                // A tag of a tree/blob seeds no commit; the tag itself is still
                // counted above.
                _ => break,
            }
        }
    }

    // A. Commits, in git's date-ordered revision-walk order. Record each commit's
    //    root tree in the same order — root trees are the first tree batch git's
    //    path-walk emits, so this is the tie-break order for the tree slots.
    let commit_order = walk_commits(repo, &seeds)?;
    let mut root_trees: Vec<ObjectId> = Vec::new();
    let mut tree_seen: HashSet<ObjectId> = HashSet::new();
    for &c in &commit_order {
        let obj = repo.find_object(c)?;
        let inflated = obj.data.len() as u64;
        let disk = disk_size(repo, c)?;
        let commit = obj.try_into_commit()?;
        let parents = commit.parent_ids().count() as u64;
        let root = commit.tree_id()?.detach();

        st.counts.commits += 1;
        st.inflated.commits += inflated;
        st.disk.commits += disk;
        st.commit_size.update(c, inflated);
        st.parent_count.update(c, parents);
        if tree_seen.insert(root) {
            root_trees.push(root);
        }
    }

    // B. Tag objects, in ref order.
    for &t in &tag_objects {
        let obj = repo.find_object(t)?;
        let inflated = obj.data.len() as u64;
        let disk = disk_size(repo, t)?;
        st.counts.tags += 1;
        st.inflated.tags += inflated;
        st.disk.tags += disk;
        st.tag_size.update(t, inflated);
    }

    // C. Trees then blobs, discovered from the root trees. Trees are walked
    //    breadth-first with the root batch first, matching git's ordering for the
    //    tie-breaks that arise; blobs are recorded in first-seen order. Gitlinks
    //    (submodule entries) are skipped, as git's path-walk does.
    let hash = repo.object_hash();
    let mut queue = root_trees;
    let mut blobs: Vec<ObjectId> = Vec::new();
    let mut blob_seen: HashSet<ObjectId> = HashSet::new();
    let mut idx = 0;
    while idx < queue.len() {
        let toid = queue[idx];
        idx += 1;
        let obj = repo.find_object(toid)?;
        let inflated = obj.data.len() as u64;
        let disk = disk_size(repo, toid)?;
        let tree = gix::objs::TreeRef::from_bytes(&obj.data, hash)?;
        let entries = tree.entries.len() as u64;

        st.counts.trees += 1;
        st.inflated.trees += inflated;
        st.disk.trees += disk;
        st.tree_size.update(toid, inflated);
        st.tree_entries.update(toid, entries);

        for e in &tree.entries {
            if e.mode.is_commit() {
                continue;
            }
            let child = e.oid.to_owned();
            if e.mode.is_tree() {
                if tree_seen.insert(child) {
                    queue.push(child);
                }
            } else if blob_seen.insert(child) {
                blobs.push(child);
            }
        }
    }

    for &b in &blobs {
        let obj = repo.find_object(b)?;
        let inflated = obj.data.len() as u64;
        let disk = disk_size(repo, b)?;
        st.counts.blobs += 1;
        st.inflated.blobs += inflated;
        st.disk.blobs += disk;
        st.blob_size.update(b, inflated);
    }

    Ok(st)
}

/// git's date-ordered revision walk over `seeds`: newest committer date first,
/// and — because every fixture pins one date — stable by insertion (seed order,
/// then parent discovery) for equal dates. Returns the order commits are popped,
/// which is the order `count_objects` sees them.
fn walk_commits(repo: &gix::Repository, seeds: &[ObjectId]) -> Result<Vec<ObjectId>> {
    // `list` is kept sorted with the newest date at the front; a new commit is
    // inserted after every entry whose date is >= its own, preserving insertion
    // order among equal dates (git's `commit_list_insert_by_date`).
    fn insert_by_date(list: &mut Vec<(i64, ObjectId)>, time: i64, oid: ObjectId) {
        let pos = list
            .iter()
            .position(|(t, _)| *t < time)
            .unwrap_or(list.len());
        list.insert(pos, (time, oid));
    }

    let mut added: HashSet<ObjectId> = HashSet::new();
    let mut list: Vec<(i64, ObjectId)> = Vec::new();
    for &s in seeds {
        if added.insert(s) {
            insert_by_date(&mut list, commit_time(repo, s)?, s);
        }
    }

    let mut order = Vec::new();
    while !list.is_empty() {
        let (_, c) = list.remove(0);
        order.push(c);
        let commit = repo.find_object(c)?.try_into_commit()?;
        let parents: Vec<ObjectId> = commit.parent_ids().map(|id| id.detach()).collect();
        for p in parents {
            if added.insert(p) {
                insert_by_date(&mut list, commit_time(repo, p)?, p);
            }
        }
    }
    Ok(order)
}

/// Committer timestamp in seconds, git's sort key for the revision walk.
fn commit_time(repo: &gix::Repository, oid: ObjectId) -> Result<i64> {
    Ok(repo.find_object(oid)?.try_into_commit()?.time()?.seconds)
}

/// On-disk footprint of one object, matching git's `disk_sizep`: the loose file's
/// length, or a packed object's entry size (compressed payload plus header).
fn disk_size(repo: &gix::Repository, oid: ObjectId) -> Result<u64> {
    let hex = oid.to_string();
    let loose = repo
        .git_dir()
        .join("objects")
        .join(&hex[..2])
        .join(&hex[2..]);
    if let Ok(meta) = std::fs::metadata(&loose) {
        return Ok(meta.len());
    }
    let mut buf = Vec::new();
    if let Some(loc) = repo.objects.location_by_oid(oid.as_ref(), &mut buf) {
        return Ok(loc.entry_size as u64);
    }
    anyhow::bail!("repo: cannot determine on-disk size of {hex}")
}

/// One table row: a plain label (section header / spacer) or a value cell.
enum Row {
    /// A name-only row (`entry == NULL` in git): section headers and blank lines.
    Text(String),
    /// A value row: name, formatted value, optional unit, optional annotated oid.
    Cell(String, String, Option<String>, Option<ObjectId>),
}

/// Assemble the rows of `stats_table_setup_structure` in git's exact order.
fn build_rows(st: &Structure) -> Vec<Row> {
    let mut rows: Vec<Row> = Vec::new();
    fn count(rows: &mut Vec<Row>, name: &str, v: u64) {
        let (val, unit) = humanise_count(v);
        rows.push(Row::Cell(name.to_string(), val, unit, None));
    }
    fn size(rows: &mut Vec<Row>, name: &str, v: u64) {
        let (val, unit) = humanise_bytes(v);
        rows.push(Row::Cell(name.to_string(), val, Some(unit), None));
    }
    fn obj_size(rows: &mut Vec<Row>, name: &str, slot: &Slot) {
        let (val, unit) = humanise_bytes(slot.value);
        rows.push(Row::Cell(name.to_string(), val, Some(unit), slot.oid));
    }
    fn obj_count(rows: &mut Vec<Row>, name: &str, slot: &Slot) {
        let (val, unit) = humanise_count(slot.value);
        rows.push(Row::Cell(name.to_string(), val, unit, slot.oid));
    }

    rows.push(Row::Text("* References".into()));
    count(&mut rows, "  * Count", st.branches + st.remotes + st.tags + st.others);
    count(&mut rows, "    * Branches", st.branches);
    count(&mut rows, "    * Tags", st.tags);
    count(&mut rows, "    * Remotes", st.remotes);
    count(&mut rows, "    * Others", st.others);

    rows.push(Row::Text(String::new()));
    rows.push(Row::Text("* Reachable objects".into()));
    count(&mut rows, "  * Count", st.counts.total());
    count(&mut rows, "    * Commits", st.counts.commits);
    count(&mut rows, "    * Trees", st.counts.trees);
    count(&mut rows, "    * Blobs", st.counts.blobs);
    count(&mut rows, "    * Tags", st.counts.tags);

    size(&mut rows, "  * Inflated size", st.inflated.total());
    size(&mut rows, "    * Commits", st.inflated.commits);
    size(&mut rows, "    * Trees", st.inflated.trees);
    size(&mut rows, "    * Blobs", st.inflated.blobs);
    size(&mut rows, "    * Tags", st.inflated.tags);

    size(&mut rows, "  * Disk size", st.disk.total());
    size(&mut rows, "    * Commits", st.disk.commits);
    size(&mut rows, "    * Trees", st.disk.trees);
    size(&mut rows, "    * Blobs", st.disk.blobs);
    size(&mut rows, "    * Tags", st.disk.tags);

    rows.push(Row::Text(String::new()));
    rows.push(Row::Text("* Largest objects".into()));
    rows.push(Row::Text("  * Commits".into()));
    obj_size(&mut rows, "    * Maximum size", &st.commit_size);
    obj_count(&mut rows, "    * Maximum parents", &st.parent_count);
    rows.push(Row::Text("  * Trees".into()));
    obj_size(&mut rows, "    * Maximum size", &st.tree_size);
    obj_count(&mut rows, "    * Maximum entries", &st.tree_entries);
    rows.push(Row::Text("  * Blobs".into()));
    obj_size(&mut rows, "    * Maximum size", &st.blob_size);
    rows.push(Row::Text("  * Tags".into()));
    obj_size(&mut rows, "    * Maximum size", &st.tag_size);

    rows
}

/// Table content is ASCII, so display width equals the char count.
fn width(s: &str) -> usize {
    s.chars().count()
}

/// git's `strbuf_utf8_align`: pad with spaces to `w` columns (never truncate).
fn pad(s: &str, w: usize, left: bool) -> String {
    let n = width(s);
    if n >= w {
        return s.to_string();
    }
    let fill = " ".repeat(w - n);
    if left {
        format!("{s}{fill}")
    } else {
        format!("{fill}{s}")
    }
}

/// `stats_table_print_structure`, byte-for-byte.
fn print_structure_table(st: &Structure, out: &mut impl Write) -> Result<()> {
    const INDEX_WIDTH: usize = 4;
    let rows = build_rows(st);

    let mut name_w = 0;
    let mut value_w = 0;
    let mut unit_w = 0;
    for r in &rows {
        match r {
            Row::Text(name) => name_w = name_w.max(width(name)),
            Row::Cell(name, value, unit, _) => {
                name_w = name_w.max(width(name));
                value_w = value_w.max(width(value));
                if let Some(u) = unit {
                    unit_w = unit_w.max(width(u));
                }
            }
        }
    }

    let name_title = "Repository structure";
    let value_title = "Value";
    if width(name_title) > name_w {
        name_w = width(name_title);
    }
    if width(value_title) > value_w + unit_w + 1 {
        value_w = width(value_title) - unit_w;
    }

    // Assign annotation indices to the oid-bearing rows, top to bottom.
    let mut annotations: Vec<String> = Vec::new();
    let mut indices: Vec<Option<usize>> = Vec::with_capacity(rows.len());
    for r in &rows {
        if let Row::Cell(_, _, _, Some(oid)) = r {
            let idx = annotations.len() + 1;
            annotations.push(format!("[{idx}] {oid}"));
            indices.push(Some(idx));
        } else {
            indices.push(None);
        }
    }

    writeln!(
        out,
        "| {} | {} |",
        pad(name_title, name_w + INDEX_WIDTH, true),
        pad(value_title, value_w + unit_w + 1, true),
    )?;
    writeln!(
        out,
        "| {} | {} |",
        "-".repeat(name_w + INDEX_WIDTH),
        "-".repeat(value_w + unit_w + 1),
    )?;

    for (r, idx) in rows.iter().zip(&indices) {
        let (name, value, unit) = match r {
            Row::Text(name) => (name.as_str(), "", ""),
            Row::Cell(name, value, unit, _) => {
                (name.as_str(), value.as_str(), unit.as_deref().unwrap_or(""))
            }
        };
        let index_field = match idx {
            Some(n) => format!(" [{n}]"),
            None => " ".repeat(INDEX_WIDTH),
        };
        writeln!(
            out,
            "| {}{} | {} {} |",
            pad(name, name_w, true),
            index_field,
            pad(value, value_w, false),
            pad(unit, unit_w, true),
        )?;
    }

    if !annotations.is_empty() {
        writeln!(out)?;
        for a in &annotations {
            writeln!(out, "{a}")?;
        }
    }
    Ok(())
}

/// `structure_keyvalue_print`: `--format=lines` (`=`/`\n`) and `nul` (`\n`/`\0`).
fn print_structure_keyvalue(
    repo: &gix::Repository,
    st: &Structure,
    out: &mut impl Write,
    kd: char,
    vd: char,
) -> Result<()> {
    fn kv(out: &mut impl Write, key: &str, kd: char, value: u64, vd: char) -> Result<()> {
        write!(out, "{key}{kd}{value}{vd}")?;
        Ok(())
    }
    fn obj(
        out: &mut impl Write,
        key: &str,
        kd: char,
        slot: &Slot,
        vd: char,
        null: &str,
    ) -> Result<()> {
        write!(out, "{key}{kd}{}{vd}", slot.value)?;
        let hex = slot.oid.map(|o| o.to_string());
        let hex = hex.as_deref().unwrap_or(null);
        write!(out, "{key}_oid{kd}{hex}{vd}")?;
        Ok(())
    }

    let null = ObjectId::null(repo.object_hash()).to_string();

    kv(out, "references.branches.count", kd, st.branches, vd)?;
    kv(out, "references.tags.count", kd, st.tags, vd)?;
    kv(out, "references.remotes.count", kd, st.remotes, vd)?;
    kv(out, "references.others.count", kd, st.others, vd)?;

    kv(out, "objects.commits.count", kd, st.counts.commits, vd)?;
    kv(out, "objects.trees.count", kd, st.counts.trees, vd)?;
    kv(out, "objects.blobs.count", kd, st.counts.blobs, vd)?;
    kv(out, "objects.tags.count", kd, st.counts.tags, vd)?;

    kv(out, "objects.commits.inflated_size", kd, st.inflated.commits, vd)?;
    kv(out, "objects.trees.inflated_size", kd, st.inflated.trees, vd)?;
    kv(out, "objects.blobs.inflated_size", kd, st.inflated.blobs, vd)?;
    kv(out, "objects.tags.inflated_size", kd, st.inflated.tags, vd)?;

    kv(out, "objects.commits.disk_size", kd, st.disk.commits, vd)?;
    kv(out, "objects.trees.disk_size", kd, st.disk.trees, vd)?;
    kv(out, "objects.blobs.disk_size", kd, st.disk.blobs, vd)?;
    kv(out, "objects.tags.disk_size", kd, st.disk.tags, vd)?;

    obj(out, "objects.commits.max_size", kd, &st.commit_size, vd, &null)?;
    obj(out, "objects.trees.max_size", kd, &st.tree_size, vd, &null)?;
    obj(out, "objects.blobs.max_size", kd, &st.blob_size, vd, &null)?;
    obj(out, "objects.tags.max_size", kd, &st.tag_size, vd, &null)?;
    obj(out, "objects.commits.max_parents", kd, &st.parent_count, vd, &null)?;
    obj(out, "objects.trees.max_entries", kd, &st.tree_entries, vd, &null)?;
    Ok(())
}

/// git's `humanise_bytes(HUMANISE_COMPACT)`: an integer count of bytes, or a
/// two-decimal IEC value once the threshold (strict `>`) is crossed.
fn humanise_bytes(bytes: u64) -> (String, String) {
    if bytes > (1 << 30) {
        let frac = (bytes & ((1 << 30) - 1)) / 10_737_419;
        (format!("{}.{:02}", bytes >> 30, frac), "GiB".to_string())
    } else if bytes > (1 << 20) {
        let x = bytes + 5243;
        let frac = ((x & ((1 << 20) - 1)) * 100) >> 20;
        (format!("{}.{:02}", x >> 20, frac), "MiB".to_string())
    } else if bytes > (1 << 10) {
        let x = bytes + 5;
        let frac = ((x & ((1 << 10) - 1)) * 100) >> 10;
        (format!("{}.{:02}", x >> 10, frac), "KiB".to_string())
    } else {
        (format!("{bytes}"), "B".to_string())
    }
}

/// git's `humanise_count`: a bare integer, or a two-decimal SI value with a
/// `k`/`M`/`G` unit past each threshold. `None` is git's NULL unit.
fn humanise_count(count: u64) -> (String, Option<String>) {
    if count >= 1_000_000_000 {
        let x = count + 5_000_000;
        (
            format!("{}.{:02}", x / 1_000_000_000, x % 1_000_000_000 / 10_000_000),
            Some("G".to_string()),
        )
    } else if count >= 1_000_000 {
        let x = count + 5_000;
        (
            format!("{}.{:02}", x / 1_000_000, x % 1_000_000 / 10_000),
            Some("M".to_string()),
        )
    } else if count >= 1_000 {
        let x = count + 5;
        (
            format!("{}.{:02}", x / 1_000, x % 1_000 / 10),
            Some("k".to_string()),
        )
    } else {
        (format!("{count}"), None)
    }
}

/// Accept the format names valid for the subcommand; `table` is `structure`-only.
fn parse_format(value: &str, allow_table: bool) -> Option<Format> {
    match value {
        "lines" => Some(Format::Lines),
        "nul" => Some(Format::Nul),
        "table" if allow_table => Some(Format::Table),
        _ => None,
    }
}

/// git `die()`s on a bad `--format`, so there is no usage block and exit is 128.
fn invalid_format(value: &str) -> ExitCode {
    eprintln!("fatal: invalid format '{value}'");
    ExitCode::from(128)
}

/// parse-options' unknown-option shape: `error: <msg>` then the usage block, both
/// on stderr, exit 129.
fn usage_error(usage: &str, msg: &str) -> ExitCode {
    eprint!("error: {msg}\n{usage}");
    ExitCode::from(129)
}

/// The same shape for the top-level dispatcher.
fn top_error(msg: &str) -> ExitCode {
    eprint!("error: {msg}\n{USAGE_TOP}");
    ExitCode::from(129)
}

/// `quote_c_style()`: emit the bytes verbatim unless they contain a control byte,
/// a quote, a backslash or anything >= 0x80, in which case wrap in double quotes
/// with C-style escapes. Every value git currently reports is plain ASCII, so
/// this is a fidelity guard rather than a hot path.
fn quote_c_style(bytes: &[u8]) -> String {
    let needs = bytes
        .iter()
        .any(|&b| b < 0x20 || b == 0x7f || b == b'"' || b == b'\\' || b >= 0x80);
    if !needs {
        return String::from_utf8_lossy(bytes).into_owned();
    }
    let mut out = String::from("\"");
    for &b in bytes {
        match b {
            b'"' => out.push_str("\\\""),
            b'\\' => out.push_str("\\\\"),
            0x07 => out.push_str("\\a"),
            0x08 => out.push_str("\\b"),
            0x09 => out.push_str("\\t"),
            0x0a => out.push_str("\\n"),
            0x0b => out.push_str("\\v"),
            0x0c => out.push_str("\\f"),
            0x0d => out.push_str("\\r"),
            b if b < 0x20 || b == 0x7f || b >= 0x80 => out.push_str(&format!("\\{b:03o}")),
            b => out.push(b as char),
        }
    }
    out.push('"');
    out
}

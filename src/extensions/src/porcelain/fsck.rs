use anyhow::{bail, Result};
use std::collections::{HashMap, HashSet};
use std::io::IsTerminal;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use gix::hash::ObjectId;
use gix::objs::Kind;

/// `builtin/fsck.c`'s `ERROR_OBJECT` — a bad `<object>` argument, or an object
/// that would not parse.
const ERROR_OBJECT: u8 = 1;
/// `builtin/fsck.c`'s `ERROR_REACHABLE` — something reachable is missing.
const ERROR_REACHABLE: u8 = 2;

/// `git fsck` — verify connectivity of the object database.
///
/// The control flow follows `builtin/fsck.c::cmd_fsck` so that the interleaving
/// of stdout and stderr matches:
///
/// 1. the reference-database check (`--references`, on by default) runs first
///    and, under `--progress`, emits its progress block;
/// 2. `<object>` arguments are resolved; each one that does not resolve prints
///    `error: invalid parameter: expected sha1, got '<arg>'` and sets
///    `ERROR_OBJECT`. Any argument at all suppresses the default head set and
///    turns reflogs off, exactly as `snapshot_refs()` does;
/// 3. unless `--connectivity-only`, every object in the odb is decoded, which is
///    where `--root` and `--tags` lines and the object-directory progress come
///    from;
/// 4. the head set is marked reachable. If nothing at all became a head,
///    `notice: No default references` goes to stderr and `--unreachable` is
///    cleared, so the report falls back to dangling tips;
/// 5. index entries join the head set when no `<object>` was given, or when
///    `--cache` was passed;
/// 6. the connectivity report is printed in `obj_hash` slot order.
///
/// Ported flags:
///   * `<object>...`                  — resolved with gix's rev-parse, the stand-in
///                                       for `repo_get_oid()`.
///   * `--unreachable`                — list every unreachable object instead of
///                                       just the dangling tips.
///   * `--dangling` / `--no-dangling` — dangling reporting is on by default.
///   * `--reflogs` / `--no-reflogs`   — reflog entries in the default head set.
///   * `--root`                       — `root <oid>` for each parentless commit.
///   * `--tags`                       — `tagged <type> <oid> (<tag>) in <oid>`.
///   * `--cache` / `--no-cache`       — index entries as head nodes.
///   * `--connectivity-only`          — skip the object-content pass; this also
///                                       suppresses `--root` and `--tags` output,
///                                       as in git.
///   * `--progress` / `--no-progress` — progress on stderr, defaulting to
///                                       `isatty(2)`, and forced off by
///                                       `--verbose` as `cmd_fsck` does.
///   * `--verbose` / `-v` / `--no-verbose` — the `Checking ...` trace on stderr;
///                                       see divergence 9.
///   * `--name-objects`               — accepted; see divergence 6.
///   * `--references` / `--no-references` — accepted; see divergence 2.
///   * `--full` / `--no-full`         — accepted; `check_full` only gates
///                                       `verify_pack()`, which this port does not
///                                       do either way.
///   * `--strict` / `--no-strict`     — accepted; see divergence 1.
///   * `--lost-found`                  — writes dangling objects into
///                                       `$GIT_DIR/lost-found/{commit,other}/`,
///                                       forcing `check_full` on and reflogs off
///                                       exactly as `cmd_fsck` does. Blobs get
///                                       their content; every other type gets its
///                                       id. This is the one flag that mutates the
///                                       repository.
///   * `-h` / `--help`                 — prints the usage block to stdout, exit 129.
///
/// Unknown, ambiguous, and abbreviated long options are resolved by a faithful
/// port of `parse-options.c::parse_long_opt` (unambiguous prefixes apply, e.g.
/// `--unre` == `--unreachable`; an ambiguous prefix or unknown option prints
/// git's exact diagnostic and exits 129). Linked worktrees contribute their HEAD,
/// index, and per-worktree reflogs to the head set, matching git's
/// `get_default_heads()` over every worktree.
///
/// ### Known divergences from stock git — read before trusting a clean result
///
/// 1. **No fsck message layer.** git additionally lints object *contents*
///    (`badDate`, `missingEmail`, `zeroPaddedFilemode`, `hasDotgit`,
///    `duplicateEntries`, and the rest of the `fsck.<msg-id>` set) and exits 2
///    when an error-severity message fires. None of that lives in the vendored
///    crates, so a repository whose only defect is a semantic lint violation is
///    reported clean here while stock git exits 2. `--strict` selects a stricter
///    severity table for that same layer, so it is accepted and changes nothing.
///    This port is equivalent in depth to `git fsck --connectivity-only`, not to
///    bare `git fsck`.
/// 2. **No `git refs verify`.** git checks the reference database by default
///    (`--references`) by running `git refs verify`; there is no equivalent in the
///    vendored crates. Both spellings of the flag are accepted because the check
///    is skipped either way — only its `--progress` block differs.
/// 3. **No re-hashing.** git recomputes each object's hash to catch a silent
///    `hash mismatch`; this port trusts the odb's own integrity checking.
/// 4. **Corruption exit code is coarse.** An object the odb cannot read is
///    reported `fatal:` with exit 128, which matches git's loose-object
///    corruption path; git distinguishes an unreadable object (128) from a
///    decodable-but-malformed one (2) and this port reports 128 for both.
/// 5. **Gitlink entries are not walked**, matching `gix-fsck` and git: a
///    submodule commit that happens to live in this odb is not marked reachable
///    by the tree or index entry that names it.
/// 6. **`--name-objects` is only accepted where it cannot show.** git decorates
///    an object id with the path it was reached by. Only `missing` lines can
///    carry such a name — dangling and unreachable objects are by definition not
///    reached from a head, so git prints their bare id. This port therefore
///    accepts `--name-objects` and `bail!`s if a `missing` line would be printed
///    while it is on.
/// 7. **`--cache` does not verify the index itself.** git also turns on
///    `verify_index_checksum` and `verify_ce_order`; `gix-index` does not expose
///    either. The head-node half of the flag — index entries and cache-tree ids
///    become heads — is what is implemented.
/// 8. **No `broken link from`/`to` lines.** When the reachable walk reaches an
///    id whose object is gone, git can print a two-line `broken link from <type>
///    <oid>` / `to <type> <oid>` pair in addition to the `missing` line. This
///    port prints only the `missing` line, so a repository with a severed link
///    gets the right exit code (2) and a shorter report.
/// 9. **The `--verbose` trace is approximate in ordering and in one block.**
///    `--verbose` changes nothing on stdout and nothing about the exit code —
///    every one of its lines is a `Checking ...` line on stderr — so the flag is
///    implemented rather than refused. Three caveats about that trace, none of
///    which reach stdout:
///      * the `Checking <type> <oid>` block comes from `fsck_source()`'s raw
///        `readdir()` walk of `.git/objects/??`; this port emits it in the odb
///        iterator's order instead, and emits the `Checking object directory`
///        header once rather than once per odb source;
///      * `Checking <oid>` under `Checking connectivity` is emitted in the
///        `obj_hash` slot order reconstructed below, with ties broken by id.
///        The report itself still refuses to guess when that order is
///        ambiguous, so only the trace can be off, and only within one
///        collision cluster. Its ids are bare: git runs them through
///        `describe_object()`, which appends a path under `--name-objects`
///        (divergence 6);
///      * git shells out to `git refs verify --verbose`, whose
///        `Checking references consistency` / `Checking <ref>` /
///        `Checking packed-refs file <path>` lines follow its own
///        `Checking ref database`. Per divergence 2 that child is not run, so
///        those lines are absent while `Checking ref database` is printed.
///
/// ### Output ordering
///
/// git emits the connectivity report in the slot order of its internal
/// `obj_hash` table (`object.c`): `u32::from_le_bytes(oid[0..4]) % obj_hash_size`
/// with linear probing, iterated from slot 0. That is reproduced here, including
/// the table's growth schedule.
///
/// Collision resolution depends on the order in which `builtin/fsck.c` happens
/// to create objects, and that order includes the raw `readdir()` sequence of
/// `.git/objects/??`, which is a filesystem property and not reproducible. It
/// does not always matter: under linear probing the *set* of occupied slots is
/// independent of insertion order, so slots partition into clusters (maximal
/// runs of occupied slots) whose boundaries are fixed, and an object never lands
/// before its own home slot. Within a cluster whose home slots are all distinct,
/// every object sits exactly on its home slot; between clusters, home-slot order
/// always holds. So the report order is provable unless two reported objects
/// share a cluster that contains a repeated home slot — and only then does this
/// command `bail!` instead of guessing.
///
/// `root` and `tagged` lines come from the object-directory scan instead.
/// `for_each_loose_file_in_source()` (`object-file.c`) walks the 256 subdirectories
/// in numeric order and only the entries within one of them by raw `readdir()`, so
/// these lines are ordered by the first byte of the id. That much is reproducible;
/// two lines sharing a first byte, or any pack at all (`verify_pack()` re-runs
/// `fsck_obj()` over packed objects afterwards, in pack-index order), makes the
/// command `bail!` instead of guessing.
pub fn fsck(args: &[String]) -> Result<ExitCode> {
    // Tolerate the subcommand being present at index 0 regardless of how the
    // dispatcher slices argv.
    let args: &[String] = match args.first() {
        Some(a) if a == "fsck" => &args[1..],
        _ => args,
    };

    let mut opt = Options::default();
    match opt.parse(args) {
        ParseControl::Proceed => {}
        ParseControl::Exit(code) => return Ok(ExitCode::from(code)),
    }

    // `cmd_fsck`: `--lost-found` forces a full check and turns reflogs off.
    if opt.write_lost_and_found {
        opt.check_full = true;
        opt.include_reflogs = false;
    }

    let repo = gix::discover(".")?;

    // Running *from* a linked worktree would make the main worktree's index a
    // head that this port does not reconstruct (its HEAD is covered by the shared
    // reflog, but not its index), so refuse rather than mis-report. Running from
    // the main worktree with linked worktrees present is fully supported below.
    if repo.git_dir() != repo.common_dir() {
        bail!("running from a linked worktree is not supported");
    }

    // `cmd_fsck`: the `isatty(2)` default is resolved first, then `--verbose`
    // unconditionally clears it — the two traces share stderr and git shows only
    // the verbose one.
    let show_progress = !opt.verbose
        && match opt.progress {
            Some(explicit) => explicit,
            None => std::io::stderr().is_terminal(),
        };
    if show_progress {
        // Each odb source gets its own "Checking object directories" block
        // (handled at the object-directory progress point below), and `--full`
        // adds a "Checking objects" / commit-graph block per pack that this port
        // cannot reproduce (no pack verification in the vendored crates).
        if opt.check_full && !opt.connectivity_only && has_packs(&repo) {
            bail!("--progress --full is not ported for a repository with packs: pack verification emits its own progress block");
        }
    }

    let mut errors: u8 = 0;
    let mut state = State::default();

    // ---- 1. reference-database check ---------------------------------------
    if opt.check_references {
        if show_progress {
            progress_block("Checking ref database", 1);
        }
        if opt.verbose {
            eprintln!("Checking ref database");
        }
    }

    // ---- 2. explicit <object> arguments ------------------------------------
    //
    // `snapshot_refs()`: any argument at all replaces the default head set and
    // turns reflogs off, whether or not the argument resolved.
    let mut heads: Vec<ObjectId> = Vec::new();
    let mut default_refs = 0usize;
    for arg in &opt.objects {
        match repo.rev_parse_single(arg.as_str()) {
            Ok(id) => {
                default_refs += 1;
                let id = id.detach();
                state.note(id);
                heads.push(id);
            }
            Err(_) => {
                eprintln!("error: invalid parameter: expected sha1, got '{arg}'");
                errors |= ERROR_OBJECT;
            }
        }
    }
    let explicit_heads = !opt.objects.is_empty();
    if explicit_heads {
        opt.include_reflogs = false;
    }

    // ---- 3. every object in the odb ----------------------------------------
    //
    // `all` is the odb's contents, so membership must not depend on whether the
    // id was already `note`d: an `<object>` argument resolves in step 2 and
    // enters `known` there, but `fsck_object_dir()` still visits it, still
    // reports its `root`/`tagged` line, and still marks its children `used`.
    let mut all: Vec<ObjectId> = Vec::new();
    let mut in_odb: HashSet<ObjectId> = HashSet::new();
    for id in repo.objects.iter()? {
        let id = id?;
        state.note(id);
        // The odb iterator can yield the same id from more than one source.
        if in_odb.insert(id) {
            all.push(id);
        }
    }

    // Children of every object, for `used` and `missing`. git checks every
    // object in the odb, not just the reachable ones, and marks each child it
    // sees as used. `dangling` is precisely "unreachable and never used", so
    // this pass has to cover unreachable objects too.
    let mut scan_lines: Vec<(ObjectId, String)> = Vec::new();
    // `fsck_source()` announces the directory once per odb source before walking
    // it; `--connectivity-only` skips `fsck_source()` altogether.
    if opt.verbose && !opt.connectivity_only {
        eprintln!("Checking object directory");
    }
    for &id in &all {
        let kind = match repo.find_header(id) {
            Ok(h) => h.kind(),
            Err(e) => return Ok(fatal_corrupt(id, &e)),
        };
        // `fsck_obj()`'s own line, which covers blobs too.
        if opt.verbose && !opt.connectivity_only {
            eprintln!("Checking {kind} {id}");
        }
        if kind == Kind::Blob {
            continue;
        }
        let decoded = match decode(&repo, id) {
            Ok(d) => d,
            Err(e) => return Ok(fatal_corrupt(id, &e)),
        };
        for (child, _) in decoded.children {
            // Absent children are `note`d all the same: `fsck_walk()` creates
            // them, so they occupy an `obj_hash` slot. They are not *reported*
            // here — `check_unreachable_object()` never prints `missing`, so an
            // object that only an unreachable object names stays quiet.
            state.note(child);
            state.used.insert(child);
        }
        // `--root` and `--tags` lines are emitted by `fsck_obj()`, which
        // `--connectivity-only` skips entirely.
        if opt.connectivity_only {
            continue;
        }
        if opt.show_root && decoded.is_root_commit {
            scan_lines.push((id, format!("root {id}")));
        }
        if opt.show_tags {
            if let Some((target_kind, target, name)) = decoded.tag {
                scan_lines.push((id, format!("tagged {target_kind} {target} ({name}) in {id}")));
            }
        }
    }
    // `for_each_loose_file_in_source()` walks the 256 subdirectories in numeric
    // order and only the entries *within* one of them in raw `readdir()` order
    // (`object-file.c`). So these lines are ordered by the first byte of the id,
    // and only a pair sharing a first byte is unresolvable.
    //
    // A pack breaks the argument outright: `verify_pack()` re-runs `fsck_obj()`
    // over packed objects after every loose one, in pack-index order.
    if scan_lines.len() > 1 {
        let mut by_subdir: HashSet<u8> = HashSet::new();
        let collides = scan_lines
            .iter()
            .any(|(id, _)| !by_subdir.insert(id.as_bytes()[0]));
        if collides || has_packs(&repo) {
            bail!(
                "refusing to guess the output order: git emits these {} lines during its \
                 object-directory scan, and two of them share the raw readdir() sequence of one \
                 .git/objects/?? subdirectory",
                scan_lines.len()
            );
        }
        scan_lines.sort_by_key(|(id, _)| id.as_bytes()[0]);
    }
    if show_progress && !opt.connectivity_only {
        // `fsck_object_dir()` runs once per odb source (the main object directory
        // plus every alternate, followed transitively), each emitting its own
        // 256-subdirectory progress block.
        for _ in 0..odb_source_count(&repo) {
            progress_block("Checking object directories", 256);
        }
    }

    // ---- 4. the head set ----------------------------------------------------
    if !explicit_heads {
        default_refs += collect_default_heads(&repo, &mut state, &mut heads)?;
    }
    if opt.include_reflogs {
        let logs_root = repo.common_dir().join("logs");
        errors |= collect_reflog_heads(&repo, &logs_root, &mut state, &mut heads, opt.verbose)?;
    }
    // Every linked worktree contributes its own HEAD, index, and per-worktree
    // reflogs, exactly as git's `get_default_heads()` iterates all worktrees.
    // Order is irrelevant: heads only feed the reachability set and `known`,
    // both of which are membership-based.
    {
        let (wt_count, wt_err) =
            collect_linked_worktree_heads(&repo, &mut state, &mut heads, &opt, explicit_heads)?;
        default_refs += wt_count;
        errors |= wt_err;
    }
    if default_refs == 0 {
        eprintln!("notice: No default references");
        // git clears `show_unreachable` here: with no heads at all, everything
        // is trivially unreachable and the listing would be noise.
        opt.show_unreachable = false;
    }

    // ---- 5. index entries as heads -----------------------------------------
    if !explicit_heads || opt.keep_cache_objects {
        collect_index_heads(&repo, &mut state, &mut heads, opt.verbose);
    }

    // ---- 6. reachability ----------------------------------------------------
    let mut queue: Vec<ObjectId> = Vec::new();
    for id in heads {
        if state.reachable.insert(id) {
            queue.push(id);
        }
    }
    while let Some(id) = queue.pop() {
        let kind = match repo.find_header(id) {
            Ok(h) => h.kind(),
            // Missing heads are already recorded; nothing to descend into.
            Err(_) => continue,
        };
        if kind == Kind::Blob {
            continue;
        }
        let decoded = match decode(&repo, id) {
            Ok(d) => d,
            Err(e) => return Ok(fatal_corrupt(id, &e)),
        };
        for (child, child_kind) in decoded.children {
            if !repo.has_object(child) {
                state.missing.insert(child, child_kind);
                continue;
            }
            if state.reachable.insert(child) {
                queue.push(child);
            }
        }
    }

    // ---- 7. the connectivity report ----------------------------------------
    if opt.name_objects && !state.missing.is_empty() {
        bail!(
            "--name-objects is not ported for a repository with missing objects: git decorates a \
             `missing` line with the path the object was reached by"
        );
    }

    let mut lines: Vec<(ObjectId, String)> = Vec::new();
    if !state.missing.is_empty() {
        errors |= ERROR_REACHABLE;
    }
    for (&id, &kind) in &state.missing {
        lines.push((id, format!("missing {kind} {id}")));
    }
    if opt.show_unreachable || opt.show_dangling || opt.write_lost_and_found {
        for &id in &all {
            if state.reachable.contains(&id) {
                continue;
            }
            // `check_unreachable_object()`: a shown-unreachable object returns
            // before the dangling/lost-found block, so `--unreachable` never
            // writes lost-found.
            if opt.show_unreachable {
                let kind = repo.find_header(id)?.kind();
                lines.push((id, format!("unreachable {kind} {id}")));
            } else if !state.used.contains(&id) {
                // `!USED` — the tip of an unreachable set. `dangling` printing and
                // lost-found writing are independent: `--no-dangling --lost-found`
                // still writes the files.
                let kind = repo.find_header(id)?.kind();
                if opt.show_dangling {
                    lines.push((id, format!("dangling {kind} {id}")));
                }
                if opt.write_lost_and_found {
                    write_lost_found(&repo, id, kind)?;
                }
            }
        }
    }

    let order = SlotOrder::new(&state.known);
    let reported: Vec<ObjectId> = lines.iter().map(|(id, _)| *id).collect();
    if order.is_ambiguous_for(&reported) {
        bail!(
            "refusing to guess the output order: git emits these {} lines in obj_hash slot order, \
             and two of them share a collision cluster whose order depends on git's internal \
             object-creation sequence, which this port does not model",
            lines.len()
        );
    }
    lines.sort_by_key(|(id, _)| order.home_of(id));

    if opt.verbose {
        // `check_connectivity()` announces `get_max_object_index()`, which is the
        // size of `obj_hash` rather than the number of objects in it, then walks
        // every occupied slot in order.
        eprintln!(
            "Checking connectivity ({} objects)",
            obj_hash_size(state.known.len())
        );
        let mut walked: Vec<ObjectId> = state.known.iter().copied().collect();
        walked.sort_by_key(|id| (order.home_of(id), *id));
        for id in walked {
            eprintln!("Checking {id}");
        }
    }

    let mut out = String::new();
    for (_, line) in &scan_lines {
        out.push_str(line);
        out.push('\n');
    }
    for (_, line) in &lines {
        out.push_str(line);
        out.push('\n');
    }
    print!("{out}");

    Ok(ExitCode::from(errors))
}

/// The full usage block `parse-options.c` prints for `-h` and after a usage
/// error, reproduced byte-for-byte from `git fsck -h` (git 2.55.0). Ends in two
/// newlines; print with `print!`/`eprint!` (no extra terminator).
const FSCK_USAGE: &str = "usage: git fsck [--tags] [--root] [--unreachable] [--cache] [--no-reflogs]\n                [--[no-]full] [--strict] [--verbose] [--lost-found]\n                [--[no-]dangling] [--[no-]progress] [--connectivity-only]\n                [--[no-]name-objects] [--[no-]references] [<object>...]\n\n    -v, --[no-]verbose    be verbose\n    --[no-]unreachable    show unreachable objects\n    --[no-]dangling       show dangling objects\n    --[no-]tags           report tags\n    --[no-]root           report root nodes\n    --[no-]cache          make index objects head nodes\n    --[no-]reflogs        make reflogs head nodes (default)\n    --[no-]full           also consider packs and alternate objects\n    --[no-]connectivity-only\n                          check only connectivity\n    --[no-]strict         enable more strict checking\n    --[no-]lost-found     write dangling objects in .git/lost-found\n    --[no-]progress       show progress\n    --[no-]name-objects   show verbose names for reachable objects\n    --[no-]references     check reference database consistency\n\n";

/// The `fsck_opts[]` long names, in table order — the order matters because
/// `register_abbrev()` treats the first prefix hit as the candidate and any
/// later hit as the ambiguity partner. Index maps to a field in `apply()`.
const FSCK_OPTS: [&str; 14] = [
    "verbose",
    "unreachable",
    "dangling",
    "tags",
    "root",
    "cache",
    "reflogs",
    "full",
    "connectivity-only",
    "strict",
    "lost-found",
    "progress",
    "name-objects",
    "references",
];

/// Whether option parsing wants the command to proceed or to stop with a code.
enum ParseControl {
    Proceed,
    Exit(u8),
}

/// The outcome of resolving one `--long` token, mirroring `parse_long_opt`'s
/// return values.
enum LongOutcome {
    /// An option (by `FSCK_OPTS` index) with its negation state.
    Apply { idx: usize, unset: bool },
    /// A prefix matched more than one option; carries git's exact message.
    Ambiguous(String),
    /// A boolean option was given `=value`; carries the canonical name.
    TakesNoValue(&'static str),
    /// No option matched.
    Unknown,
}

/// The flags `builtin/fsck.c` keeps as file-scope statics, with git's defaults.
struct Options {
    show_unreachable: bool,
    show_dangling: bool,
    show_root: bool,
    show_tags: bool,
    include_reflogs: bool,
    connectivity_only: bool,
    check_full: bool,
    check_references: bool,
    keep_cache_objects: bool,
    name_objects: bool,
    write_lost_and_found: bool,
    verbose: bool,
    progress: Option<bool>,
    objects: Vec<String>,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            show_unreachable: false,
            show_dangling: true,
            show_root: false,
            show_tags: false,
            include_reflogs: true,
            connectivity_only: false,
            check_full: true,
            check_references: true,
            keep_cache_objects: false,
            name_objects: false,
            write_lost_and_found: false,
            verbose: false,
            progress: None,
            objects: Vec::new(),
        }
    }
}

impl Options {
    fn parse(&mut self, args: &[String]) -> ParseControl {
        let mut only_positionals = false;
        for a in args {
            if only_positionals {
                self.objects.push(a.clone());
                continue;
            }
            let s = a.as_str();
            if s == "--" {
                only_positionals = true;
                continue;
            }
            // `git.c` intercepts `--help` into a man page and parse-options turns
            // `-h` into the usage block; neither is reproducible past the usage
            // text, so both print it to stdout and exit 129 like `-h` does.
            if s == "-h" || s == "--help" {
                print!("{FSCK_USAGE}");
                return ParseControl::Exit(129);
            }
            if let Some(long) = s.strip_prefix("--") {
                match resolve_long(long) {
                    LongOutcome::Apply { idx, unset } => self.apply(idx, unset),
                    LongOutcome::Ambiguous(msg) => {
                        eprintln!("{msg}");
                        return ParseControl::Exit(129);
                    }
                    LongOutcome::TakesNoValue(name) => {
                        eprintln!("error: option `{name}' takes no value");
                        return ParseControl::Exit(129);
                    }
                    LongOutcome::Unknown => {
                        eprint!("error: unknown option `{long}'\n{FSCK_USAGE}");
                        return ParseControl::Exit(129);
                    }
                }
                continue;
            }
            if s.starts_with('-') && s.len() > 1 {
                // The only short option is `-v`; every other switch is unknown.
                for c in s[1..].chars() {
                    match c {
                        'v' => self.verbose = true,
                        'h' => {
                            print!("{FSCK_USAGE}");
                            return ParseControl::Exit(129);
                        }
                        _ => {
                            eprint!("error: unknown switch `{c}'\n{FSCK_USAGE}");
                            return ParseControl::Exit(129);
                        }
                    }
                }
                continue;
            }
            self.objects.push(a.clone());
        }
        ParseControl::Proceed
    }

    /// Set the field named by `FSCK_OPTS[idx]`; `unset` is the `--no-` form.
    fn apply(&mut self, idx: usize, unset: bool) {
        let on = !unset;
        match idx {
            0 => self.verbose = on,
            1 => self.show_unreachable = on,
            2 => self.show_dangling = on,
            3 => self.show_tags = on,
            4 => self.show_root = on,
            5 => self.keep_cache_objects = on,
            6 => self.include_reflogs = on,
            // `check_full` only gates `verify_pack()` and `check_references` only
            // gates `git refs verify`; this port does neither, but the flags are
            // still tracked so `--progress` and the packs guard behave. `strict`
            // selects a message-layer severity this port does not implement.
            7 => self.check_full = on,
            8 => self.connectivity_only = on,
            9 => {} // strict
            10 => self.write_lost_and_found = on,
            11 => self.progress = Some(on),
            12 => self.name_objects = on,
            13 => self.check_references = on,
            _ => unreachable!(),
        }
    }
}

/// A faithful port of `parse-options.c::parse_long_opt` restricted to the fsck
/// option table (no aliases, no argument-taking options, `PARSE_OPT_NONEG`
/// nowhere). `arg` is the token with its leading `--` already removed.
fn resolve_long(arg: &str) -> LongOutcome {
    // `is_alias()` is always false here (fsck registers no alias groups), so a
    // second abbreviation registration always makes the previous one ambiguous.
    fn register(
        abbrev: &mut Option<(usize, bool)>,
        ambiguous: &mut Option<(usize, bool)>,
        idx: usize,
        unset: bool,
    ) {
        if let Some(prev) = *abbrev {
            *ambiguous = Some(prev);
        }
        *abbrev = Some((idx, unset));
    }

    let arg_end = arg.find('=').unwrap_or(arg.len());
    let mut arg_start = arg;
    let mut unset = false;
    let mut arg_starts_with_no_no = false;
    if let Some(rest) = arg_start.strip_prefix("no-") {
        arg_start = rest;
        if let Some(rest2) = arg_start.strip_prefix("no-") {
            arg_start = rest2;
            arg_starts_with_no_no = true;
        } else {
            unset = true;
        }
    }
    // Length of the name portion of `arg_start` (`arg_end - arg_start` in C).
    let consumed = arg.len() - arg_start.len();
    let abbrev_len = arg_end.saturating_sub(consumed);

    let mut abbrev: Option<(usize, bool)> = None;
    let mut ambiguous: Option<(usize, bool)> = None;

    for (i, &long_name) in FSCK_OPTS.iter().enumerate() {
        // No fsck long name starts with "no-", so a "no-no-" argument matches
        // nothing (`else if arg_starts_with_no_no: continue`).
        if arg_starts_with_no_no {
            continue;
        }
        // Exact / consumed prefix: `skip_prefix(arg_start, long_name, &rest)`.
        if let Some(rest) = arg_start.strip_prefix(long_name) {
            if rest.starts_with('=') {
                return LongOutcome::TakesNoValue(long_name);
            } else if !rest.is_empty() {
                continue;
            } else {
                return LongOutcome::Apply { idx: i, unset };
            }
        }
        // Abbreviated? `!strncmp(long_name, arg_start, abbrev_len)`.
        if abbrev_len <= long_name.len()
            && long_name.as_bytes()[..abbrev_len] == arg_start.as_bytes()[..abbrev_len]
        {
            register(&mut abbrev, &mut ambiguous, i, unset);
        }
        // Negated and abbreviated very much? `starts_with("no-", arg)` — i.e.
        // `arg` is a prefix of "no-".
        if "no-".starts_with(arg) {
            register(&mut abbrev, &mut ambiguous, i, true);
        }
    }

    if let Some((ai, au)) = ambiguous {
        let (bi, bu) = abbrev.expect("ambiguous implies an abbrev too");
        let an = if au { "no-" } else { "" };
        let bn = if bu { "no-" } else { "" };
        return LongOutcome::Ambiguous(format!(
            "error: ambiguous option: {arg} (could be --{an}{a} or --{bn}{b})",
            a = FSCK_OPTS[ai],
            b = FSCK_OPTS[bi],
        ));
    }
    if let Some((bi, bu)) = abbrev {
        if arg_end < arg.len() {
            return LongOutcome::TakesNoValue(FSCK_OPTS[bi]);
        }
        return LongOutcome::Apply { idx: bi, unset: bu };
    }
    LongOutcome::Unknown
}

/// Everything accumulated across the passes.
#[derive(Default)]
struct State {
    /// Every object id git's `obj_hash` would hold: present objects plus every
    /// id merely referenced by one. Drives the output ordering.
    known: HashSet<ObjectId>,
    /// Objects referenced by some other object — the complement of `dangling`.
    used: HashSet<ObjectId>,
    /// Objects reachable from the head set.
    reachable: HashSet<ObjectId>,
    /// Reachable but absent, with the type expected at the reference site.
    /// Only the reachable walk fills this: `check_reachable_object()` is the
    /// only place git prints a `missing` line.
    missing: HashMap<ObjectId, Kind>,
}

impl State {
    /// Record `id` as an object git would have created. Returns whether it is new.
    fn note(&mut self, id: ObjectId) -> bool {
        self.known.insert(id)
    }
}

/// What one decoded object contributes.
struct Decoded {
    /// The objects it refers to, paired with the type expected at each site —
    /// which is what git names in a `missing <type> <oid>` line.
    children: Vec<(ObjectId, Kind)>,
    /// A commit with no parents, which `--root` reports.
    is_root_commit: bool,
    /// `(target kind, target id, tag name)`, which `--tags` reports.
    tag: Option<(Kind, ObjectId, String)>,
}

/// Decode `id`. Gitlink tree entries are skipped: they name commits of a
/// different repository, which is also what git's `fsck_walk_tree()` does.
fn decode(repo: &gix::Repository, id: ObjectId) -> Result<Decoded> {
    use gix::objs::tree::EntryKind;

    let object = repo.find_object(id)?;
    let mut children = Vec::new();
    let mut is_root_commit = false;
    let mut tag = None;
    match object.kind {
        Kind::Commit => {
            let commit = gix::objs::CommitRef::from_bytes(&object.data, repo.object_hash())?;
            children.push((commit.tree(), Kind::Tree));
            let parents: Vec<ObjectId> = commit.parents().collect();
            is_root_commit = parents.is_empty();
            children.extend(parents.into_iter().map(|p| (p, Kind::Commit)));
        }
        Kind::Tree => {
            let tree = gix::objs::TreeRef::from_bytes(&object.data, repo.object_hash())?;
            for entry in &tree.entries {
                let kind = match entry.mode.kind() {
                    EntryKind::Tree => Kind::Tree,
                    EntryKind::Blob | EntryKind::BlobExecutable | EntryKind::Link => Kind::Blob,
                    EntryKind::Commit => continue,
                };
                children.push((entry.oid.to_owned(), kind));
            }
        }
        Kind::Tag => {
            let parsed = gix::objs::TagRef::from_bytes(&object.data, repo.object_hash())?;
            let target = parsed.target();
            children.push((target, parsed.target_kind));
            tag = Some((parsed.target_kind, target, parsed.name.to_string()));
        }
        Kind::Blob => {}
    }
    Ok(Decoded {
        children,
        is_root_commit,
        tag,
    })
}

/// git's default head set minus reflogs and the index: every reference plus
/// `HEAD`. Returns how many heads it contributed, which is git's `default_refs`.
///
/// Ids named by a reference but absent from the odb are still `note`d, because
/// `parse_object()` creates those objects too and they occupy an `obj_hash` slot.
fn collect_default_heads(
    repo: &gix::Repository,
    state: &mut State,
    heads: &mut Vec<ObjectId>,
) -> Result<usize> {
    let mut count = 0usize;

    // References, taking each ref's direct target rather than its fully peeled
    // one, so an annotated tag object counts as reachable in its own right.
    for reference in repo.references()?.all()? {
        // The iterator yields a boxed error, which anyhow cannot convert via `?`.
        let reference = reference.map_err(|e| anyhow::anyhow!(e))?;
        // Bind the direct target first so the borrow of `reference` ends before
        // the peeling fallback consumes it.
        let direct: Option<ObjectId> = reference.target().try_id().map(|id| id.to_owned());
        let id = match direct {
            Some(id) => id,
            None => match reference.into_fully_peeled_id() {
                Ok(id) => id.detach(),
                Err(_) => continue,
            },
        };
        state.note(id);
        heads.push(id);
        count += 1;
    }

    // HEAD is a pseudo-ref and is not part of the `refs/` iteration above.
    if let Ok(head) = repo.head() {
        if let Some(id) = head.id() {
            let id = id.detach();
            state.note(id);
            heads.push(id);
            count += 1;
        }
    }

    Ok(count)
}

/// Reflog entries as heads. A reflog id that is not in the odb is an error for
/// git (`ERROR_REACHABLE`) rather than a head, and — because `fsck_handle_reflog_oid()`
/// calls `lookup_object()`, which does not create — it never enters `obj_hash`.
fn collect_reflog_heads(
    repo: &gix::Repository,
    logs_root: &Path,
    state: &mut State,
    heads: &mut Vec<ObjectId>,
    verbose: bool,
) -> Result<u8> {
    let mut errors = 0u8;
    let mut names: Vec<String> = Vec::new();
    collect_log_names(logs_root, "", &mut names)?;
    let mut buf = Vec::new();
    for name in names {
        // A log file whose path is not a well-formed ref name is skipped rather
        // than fatal, matching git's tolerance of stray files there.
        let Ok(Some(iter)) = repo.refs.reflog_iter(name.as_str(), &mut buf) else {
            continue;
        };
        for line in iter {
            let line = line?;
            // `fsck_handle_reflog_ent()` announces the entry before either end
            // of it is handled, null ids included.
            if verbose {
                eprintln!(
                    "Checking reflog {}->{}",
                    line.previous_oid(),
                    line.new_oid()
                );
            }
            for id in [line.previous_oid(), line.new_oid()] {
                if id.is_null() {
                    continue;
                }
                if repo.has_object(id) {
                    state.note(id);
                    heads.push(id);
                } else {
                    eprintln!("error: {name}: invalid reflog entry {id}");
                    errors |= ERROR_REACHABLE;
                }
            }
        }
    }
    Ok(errors)
}

/// Index entries and cache-tree ids as heads, which is `fsck_index()`. Gitlink
/// entries are skipped, matching git's `S_ISGITLINK` guard.
fn collect_index_heads(
    repo: &gix::Repository,
    state: &mut State,
    heads: &mut Vec<ObjectId>,
    verbose: bool,
) {
    let Ok(index) = repo.index_or_empty() else {
        return;
    };
    for entry in index.entries() {
        if entry.mode.is_submodule() {
            continue;
        }
        state.note(entry.id);
        heads.push(entry.id);
    }
    if let Some(tree) = index.tree() {
        let path = verbose.then(|| index_path_label(repo));
        collect_cache_tree(repo, tree, state, heads, path.as_deref());
    }
}

/// The index path as `fsck_index()` names it — git runs from the top of the
/// worktree, so its `.git/index` is the worktree-relative spelling.
fn index_path_label(repo: &gix::Repository) -> String {
    let path = repo.index_path();
    let rela = repo
        .workdir()
        .and_then(|work| path.strip_prefix(work).ok())
        .unwrap_or(path.as_path());
    rela.display().to_string()
}

/// `fsck_cache_tree()`: an entry with a valid count names a tree that is a head.
/// An invalid count (git's negative `entry_count`, gix's `None`) is skipped, but
/// its children are still walked.
fn collect_cache_tree(
    repo: &gix::Repository,
    tree: &gix::index::extension::Tree,
    state: &mut State,
    heads: &mut Vec<ObjectId>,
    verbose_index_path: Option<&str>,
) {
    // `fsck_cache_tree()` announces itself once per node, subtrees included, and
    // before the entry-count guard below.
    if let Some(path) = verbose_index_path {
        eprintln!("Checking cache tree of {path}");
    }
    if tree.num_entries.is_some() && repo.has_object(tree.id) {
        state.note(tree.id);
        heads.push(tree.id);
    }
    for child in &tree.children {
        collect_cache_tree(repo, child, state, heads, verbose_index_path);
    }
}

/// Append every reflog file below `dir` to `out` as a `/`-joined ref name.
fn collect_log_names(dir: &Path, prefix: &str, out: &mut Vec<String>) -> Result<()> {
    let read = match std::fs::read_dir(dir) {
        Ok(read) => read,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e.into()),
    };
    for entry in read {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().into_owned();
        let full = format!("{prefix}{name}");
        if entry.file_type()?.is_dir() {
            collect_log_names(&entry.path(), &format!("{full}/"), out)?;
        } else {
            out.push(full);
        }
    }
    Ok(())
}

/// Whether the odb has any pack, which changes git's `--progress` output.
fn has_packs(repo: &gix::Repository) -> bool {
    std::fs::read_dir(repo.common_dir().join("objects").join("pack"))
        .map(|d| {
            d.filter_map(Result::ok)
                .any(|e| e.path().extension().is_some_and(|x| x == "pack"))
        })
        .unwrap_or(false)
}

/// The number of odb sources git iterates: the main object directory plus every
/// alternate, followed transitively (`objects/info/alternates`). git prints one
/// "Checking object directories" progress block per source. Sources are deduped
/// by canonical path, matching git's device/inode dedup closely enough for the
/// count, which is all the identical progress blocks need.
fn odb_source_count(repo: &gix::Repository) -> usize {
    let mut seen: HashSet<PathBuf> = HashSet::new();
    let mut stack: Vec<PathBuf> = vec![repo.common_dir().join("objects")];
    let mut count = 0usize;
    while let Some(objdir) = stack.pop() {
        let canon = objdir.canonicalize().unwrap_or_else(|_| objdir.clone());
        if !seen.insert(canon) {
            continue;
        }
        count += 1;
        let Ok(content) = std::fs::read(objdir.join("info").join("alternates")) else {
            continue;
        };
        for line in content.split(|&b| b == b'\n') {
            if line.is_empty() || line[0] == b'#' {
                continue;
            }
            let p = Path::new(std::ffi::OsStr::from_bytes(line));
            let alt = if p.is_absolute() {
                p.to_path_buf()
            } else {
                objdir.join(p)
            };
            stack.push(alt);
        }
    }
    count
}

/// Every linked worktree's HEAD, index, and per-worktree reflogs as heads,
/// matching git's `get_default_heads()` iterating all worktrees. The main
/// worktree's HEAD/index/reflogs are collected by the callers above; this adds
/// only the linked ones. Returns the number of HEADs (git's `default_refs`
/// contribution) and any reflog errors, exactly like the main collectors.
fn collect_linked_worktree_heads(
    repo: &gix::Repository,
    state: &mut State,
    heads: &mut Vec<ObjectId>,
    opt: &Options,
    explicit_heads: bool,
) -> Result<(usize, u8)> {
    let mut count = 0usize;
    let mut errors = 0u8;
    let worktrees = match repo.worktrees() {
        Ok(w) => w,
        // No `worktrees/` directory is the common case: nothing to add.
        Err(_) => return Ok((count, errors)),
    };
    for proxy in worktrees {
        let logs_root = proxy.git_dir().join("logs");
        // The worktree's working tree may be missing (a prunable worktree); its
        // HEAD/index/reflogs still live in the git dir and are read regardless.
        let Ok(wt) = proxy.into_repo_with_possibly_inaccessible_worktree() else {
            continue;
        };
        // An explicit `<object>` argument suppresses the default head set for the
        // whole command, worktrees included.
        if !explicit_heads {
            if let Ok(head) = wt.head() {
                if let Some(id) = head.id() {
                    let id = id.detach();
                    state.note(id);
                    heads.push(id);
                    count += 1;
                }
            }
        }
        if opt.include_reflogs {
            errors |= collect_reflog_heads(&wt, &logs_root, state, heads, opt.verbose)?;
        }
        if !explicit_heads || opt.keep_cache_objects {
            collect_index_heads(&wt, state, heads, opt.verbose);
        }
    }
    Ok((count, errors))
}

/// `--lost-found`: write a dangling object into `$GIT_DIR/lost-found/`. Commits
/// go under `commit/`, everything else under `other/`. A blob's file holds its
/// content; every other type's file holds its id followed by a newline. Mirrors
/// `check_unreachable_object()`'s write branch.
fn write_lost_found(repo: &gix::Repository, id: ObjectId, kind: Kind) -> Result<()> {
    let subdir = if kind == Kind::Commit { "commit" } else { "other" };
    let dir = repo.git_dir().join("lost-found").join(subdir);
    std::fs::create_dir_all(&dir)?;
    let path = dir.join(id.to_string());
    if kind == Kind::Blob {
        let object = repo.find_object(id)?;
        std::fs::write(&path, &object.data)?;
    } else {
        std::fs::write(&path, format!("{id}\n"))?;
    }
    Ok(())
}

/// One completed `struct progress` as git renders it on a non-tty: the final
/// percentage line terminated by a carriage return, then the same line with
/// `, done.` and a newline.
fn progress_block(label: &str, total: u64) {
    eprint!("{label}: 100% ({total}/{total})\r");
    eprintln!("{label}: 100% ({total}/{total}), done.");
}

/// git's `obj_hash` table, reconstructed far enough to order the report.
struct SlotOrder {
    home: HashMap<ObjectId, usize>,
    /// Cluster id per slot; `usize::MAX` for an empty slot. A cluster is a
    /// maximal run of occupied slots, and its extent does not depend on
    /// insertion order.
    cluster: Vec<usize>,
    /// Clusters holding a repeated home slot, and so an insertion-order-dependent
    /// internal order.
    ambiguous: HashSet<usize>,
    /// A cluster that wraps past the end of the table breaks the "home slot
    /// order is table order" argument outright.
    wrapped: bool,
}

impl SlotOrder {
    fn new(known: &HashSet<ObjectId>) -> Self {
        let size = obj_hash_size(known.len());
        let mut ids: Vec<&ObjectId> = known.iter().collect();
        ids.sort();

        let mut home = HashMap::with_capacity(ids.len());
        let mut homes_at = vec![0usize; size];
        for id in &ids {
            let h = slot(id, size);
            home.insert((*id).to_owned(), h);
            homes_at[h] += 1;
        }

        // Under linear probing the set of occupied slots is independent of
        // insertion order, so replaying the inserts in any fixed order finds it.
        let mut occupied = vec![false; size];
        let mut wrapped = false;
        for id in &ids {
            let mut i = home[*id];
            while occupied[i] {
                i += 1;
                if i == size {
                    wrapped = true;
                    i = 0;
                }
            }
            occupied[i] = true;
        }
        if size > 0 && occupied[0] && occupied[size - 1] {
            wrapped = true;
        }

        let mut cluster = vec![usize::MAX; size];
        let mut ambiguous = HashSet::new();
        let mut next = 0usize;
        let mut s = 0usize;
        while s < size {
            if !occupied[s] {
                s += 1;
                continue;
            }
            let id = next;
            next += 1;
            let mut repeated = false;
            while s < size && occupied[s] {
                cluster[s] = id;
                repeated |= homes_at[s] > 1;
                s += 1;
            }
            if repeated {
                ambiguous.insert(id);
            }
        }

        Self {
            home,
            cluster,
            ambiguous,
            wrapped,
        }
    }

    fn home_of(&self, id: &ObjectId) -> usize {
        self.home[id]
    }

    /// Whether the relative order of `reported` could differ from home-slot
    /// order. Two objects can only swap if they share a cluster, and only if
    /// that cluster has a repeated home slot for insertion order to exploit.
    fn is_ambiguous_for(&self, reported: &[ObjectId]) -> bool {
        if reported.len() < 2 {
            return false;
        }
        if self.wrapped {
            return true;
        }
        let mut seen: HashSet<usize> = HashSet::new();
        for id in reported {
            let c = self.cluster[self.home[id]];
            if !self.ambiguous.contains(&c) {
                continue;
            }
            if !seen.insert(c) {
                return true;
            }
        }
        false
    }
}

/// The size `obj_hash` ends at after `n` objects have been created, replaying
/// git's growth rule from `object.c::create_object`: before each insertion, grow
/// when `obj_hash_size - 1 <= nr_objs * 2`, to 32 initially and by doubling after.
fn obj_hash_size(n: usize) -> usize {
    let mut size: i64 = 0;
    for nr in 0..n as i64 {
        if size - 1 <= nr * 2 {
            size = if size < 32 { 32 } else { size * 2 };
        }
    }
    size.max(32) as usize
}

/// An object's home slot: the first four bytes of the id read as a native
/// little-endian `u32`, modulo the table size (`object.c::hashtable_index`).
fn slot(id: &ObjectId, size: usize) -> usize {
    let b = id.as_bytes();
    let head = u32::from_le_bytes([b[0], b[1], b[2], b[3]]);
    head as usize % size
}

/// git aborts with `fatal:` and exit 128 when it cannot read an object.
fn fatal_corrupt(id: ObjectId, err: &dyn std::fmt::Display) -> ExitCode {
    eprintln!("fatal: object {id} is corrupt: {err}");
    ExitCode::from(128)
}

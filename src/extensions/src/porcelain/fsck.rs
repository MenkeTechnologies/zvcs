use anyhow::{bail, Result};
use std::collections::{HashMap, HashSet};
use std::io::IsTerminal;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::atomic::AtomicBool;

use gix::hash::ObjectId;
use gix::objs::Kind;
use gix::odb::pack;

/// `builtin/fsck.c`'s `ERROR_OBJECT` — a bad `<object>` argument, or an object
/// that would not parse.
const ERROR_OBJECT: u8 = 1;
/// `builtin/fsck.c`'s `ERROR_REACHABLE` — something reachable is missing.
const ERROR_REACHABLE: u8 = 2;
/// `builtin/fsck.c`'s `ERROR_PACK` — a pack failed `verify_pack()` under `--full`.
const ERROR_PACK: u8 = 4;

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
/// ```text
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
///   * `--full` / `--no-full`         — on by default; `check_full` gates
///                                       `verify_pack()`, ported here as a gix pack
///                                       integrity check over every pack in the main
///                                       object directory and each alternate (the
///                                       `.idx`/`.pack` checksums and every object's
///                                       SHA-1 and CRC-32), setting `ERROR_PACK` on
///                                       failure. The fsck message layer git also
///                                       re-runs over packed objects is not part of
///                                       it; see divergence 1.
///   * `--strict` / `--no-strict`     — promotes every message-layer warning
///                                       that no `fsck.<msg-id>` configured to
///                                       an error, leaving info-severity ids
///                                       (`badFilemode`, `badTagName`,
///                                       `missingTaggerEntry`) alone.
///   * `--lost-found`                  — writes dangling objects into
///                                       `$GIT_DIR/lost-found/{commit,other}/`,
///                                       forcing `check_full` on and reflogs off
///                                       exactly as `cmd_fsck` does. Blobs get
///                                       their content; every other type gets its
///                                       id. This is the one flag that mutates the
///                                       repository.
///   * `-h` / `--help`                 — prints the usage block to stdout, exit 129.
/// ```
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
/// 1. **The fsck message layer covers 28 of git's 76 message ids.** git lints
///    object *contents* on top of the connectivity walk and exits 1 when an
///    error-severity message fires; that layer is ported below (see [`MSGS`]),
///    including `fsck.<msg-id>` severities, `--strict` promotion and
///    `fsck.skipList`. What is *not* covered is listed id by id in
///    [`UNPORTED_MSG_IDS`]: the reference-database ids (`git refs verify` has no
///    equivalent here), the `.gitmodules`/`.gitattributes` blob ids, and the ids
///    git only reaches by linting a buffer its own object parser already
///    rejected. A repository whose only defect is one of those is reported clean
///    here while stock git reports it.
///    `--full` verifies pack *integrity* (checksums, per-object hash/CRC) via the
///    gix pack verifier; the message layer runs over packed objects too, since
///    the object-directory scan below iterates the whole odb rather than only
///    its loose half.
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
            bail!("--progress --full is not ported for a repository with packs: git's pack verification emits its own \"Checking objects\" progress block this port cannot reproduce (the verification itself is done below)");
        }
    }

    // `git_fsck_config()` runs before any checking, so a bad `fsck.<msg-id>`
    // value or an unreadable `fsck.skipList` dies before a line is printed.
    let msg_config = match MsgConfig::new(&repo, MsgSource::Fsck { strict: opt.strict }) {
        Ok(c) => c,
        Err(fatal) => {
            eprintln!("fatal: {fatal}");
            return Ok(ExitCode::from(128));
        }
    };

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
    // `fsck_object()`'s own findings, which go to stderr rather than stdout.
    let mut msg_lines: Vec<(ObjectId, String)> = Vec::new();
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

        // `fsck_obj()` runs `fsck_object()` next, and returns early when it
        // reported an error — so an object that fails the message layer
        // contributes no `root`/`tagged` line.
        let object = repo.find_object(id)?;
        let mut failed = false;
        for finding in check_object(kind, &object.data) {
            match msg_config.severity(&finding, &id) {
                Severity::Ignore => {}
                Severity::Info | Severity::Warn => msg_lines.push((
                    id,
                    format!("warning in {kind} {id}: {}: {}", finding.msg.id, finding.text),
                )),
                Severity::Error | Severity::Fatal => {
                    msg_lines.push((
                        id,
                        format!("error in {kind} {id}: {}: {}", finding.msg.id, finding.text),
                    ));
                    // A loose object is checked by `fsck_object_dir()`, whose
                    // failures are `ERROR_OBJECT`; a packed one is checked by
                    // `verify_pack()`'s `fsck_obj_buffer` callback, and git
                    // reports the whole pack instead with `ERROR_PACK`.
                    errors |= if is_loose(&repo, id) {
                        ERROR_OBJECT
                    } else {
                        ERROR_PACK
                    };
                    failed = true;
                    break;
                }
            }
        }
        if failed {
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

    // The message layer's lines are emitted from the same scan, so they are
    // ordered — and refused when ambiguous — by the same rule.
    if msg_lines.len() > 1 {
        let mut by_subdir: HashSet<u8> = HashSet::new();
        let distinct: Vec<ObjectId> = {
            let mut seen = HashSet::new();
            msg_lines.iter().map(|(id, _)| *id).filter(|id| seen.insert(*id)).collect()
        };
        let collides = distinct.iter().any(|id| !by_subdir.insert(id.as_bytes()[0]));
        if collides || has_packs(&repo) {
            bail!(
                "refusing to guess the output order: git emits these {} object-content messages \
                 during its object-directory scan, and two of them share the raw readdir() \
                 sequence of one .git/objects/?? subdirectory",
                msg_lines.len()
            );
        }
        msg_lines.sort_by_key(|(id, _)| id.as_bytes()[0]);
    }
    for (_, line) in &msg_lines {
        eprintln!("{line}");
    }
    if show_progress && !opt.connectivity_only {
        // `fsck_object_dir()` runs once per odb source (the main object directory
        // plus every alternate, followed transitively), each emitting its own
        // 256-subdirectory progress block.
        for _ in 0..odb_source_count(&repo) {
            progress_block("Checking object directories", 256);
        }
    }

    // ---- 3b. `--full`: verify every pack ------------------------------------
    //
    // `cmd_fsck`: after `fsck_object_dir()` over each odb, `check_full` runs
    // `verify_pack()` across `get_all_packs()` (the main object directory plus
    // every alternate), OR-ing `ERROR_PACK` into `errors_found` on failure.
    // `--connectivity-only` skips the whole object-content phase, this included.
    if opt.check_full && !opt.connectivity_only {
        errors |= verify_packs(&repo);
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
    /// `--strict`: promotes every *defaulted* message-layer warning to an error.
    strict: bool,
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
            strict: false,
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
            // `check_full` gates `verify_pack()`, ported below as a pack integrity
            // check; `check_references` gates `git refs verify`, which the vendored
            // crates do not expose, so it is tracked only so `--progress` and the
            // packs guard behave.
            7 => self.check_full = on,
            8 => self.connectivity_only = on,
            9 => self.strict = on,
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
            // gix validates the whole header; `parse_commit_buffer()` only reads
            // the `tree` and `parent` lines, and leaves everything else to the
            // message layer. Fall back to those two fields so a commit git
            // parses — one with a broken ident line, say — is linted rather
            // than declared corrupt.
            let (tree, parents) = match gix::objs::CommitRef::from_bytes(&object.data, repo.object_hash()) {
                Ok(commit) => (commit.tree(), commit.parents().collect::<Vec<_>>()),
                Err(e) => commit_links(&object.data).ok_or(e)?,
            };
            children.push((tree, Kind::Tree));
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
            // Same as for commits: `parse_tag_buffer()` reads only `object`,
            // `type` and `tag`, so an extra header or a missing tagger is a
            // message-layer matter, not a parse failure.
            let (target, target_kind, name) =
                match gix::objs::TagRef::from_bytes(&object.data, repo.object_hash()) {
                    Ok(parsed) => (parsed.target(), parsed.target_kind, parsed.name.to_string()),
                    Err(e) => tag_links(&object.data).ok_or(e)?,
                };
            children.push((target, target_kind));
            tag = Some((target_kind, target, name));
        }
        Kind::Blob => {}
    }
    Ok(Decoded {
        children,
        is_root_commit,
        tag,
    })
}

/// `commit.c::parse_commit_buffer` reduced to what `fsck_walk_commit()` needs:
/// the `tree` line and every `parent` line, both of which must be well formed
/// or git itself reports the object as unparseable.
fn commit_links(data: &[u8]) -> Option<(ObjectId, Vec<ObjectId>)> {
    let rest = data.strip_prefix(b"tree ")?;
    let (tree, mut rest) = take_oid_line(rest)?;
    let mut parents = Vec::new();
    while let Some(after) = rest.strip_prefix(b"parent ") {
        let (parent, tail) = take_oid_line(after)?;
        parents.push(parent);
        rest = tail;
    }
    Some((tree, parents))
}

/// `tag.c::parse_tag_buffer` reduced to the three headers `fsck_walk_tag()`
/// needs: the target id, its type and the tag name.
fn tag_links(data: &[u8]) -> Option<(ObjectId, Kind, String)> {
    let rest = data.strip_prefix(b"object ")?;
    let (target, rest) = take_oid_line(rest)?;
    let rest = rest.strip_prefix(b"type ")?;
    let eol = rest.iter().position(|&b| b == b'\n')?;
    let kind = Kind::from_bytes(&rest[..eol]).ok()?;
    let rest = rest[eol + 1..].strip_prefix(b"tag ")?;
    let eol = rest.iter().position(|&b| b == b'\n')?;
    Some((target, kind, String::from_utf8_lossy(&rest[..eol]).into_owned()))
}

/// One hex object id followed by a newline, and the rest of the buffer.
fn take_oid_line(data: &[u8]) -> Option<(ObjectId, &[u8])> {
    let eol = data.iter().position(|&b| b == b'\n')?;
    let id = ObjectId::from_hex(&data[..eol]).ok()?;
    Some((id, &data[eol + 1..]))
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

/// `--full`: git's `verify_pack()` over every pack `get_all_packs()` yields — the
/// main object directory first, then each alternate — returning `ERROR_PACK` if
/// any pack fails.
///
/// git re-inflates each packed object and re-runs the fsck message layer through
/// `fsck_obj_buffer`; that half is the missing message layer (divergence 1). What
/// gix exposes, and what this checks, is `verify_pack`'s integrity core: the
/// `.idx` and `.pack` file checksums plus every object's SHA-1 and CRC-32 as
/// stored in the index. The `Mode::HashCrc32` choice and the `verify_integrity`
/// call mirror `porcelain::index_pack`'s `--verify`, git's own `verify_pack` peer.
fn verify_packs(repo: &gix::Repository) -> u8 {
    let hash = repo.object_hash();
    let mut objdirs: Vec<PathBuf> = vec![repo.objects.store_ref().path().to_path_buf()];
    if let Ok(alts) = repo.objects.store_ref().alternate_db_paths() {
        objdirs.extend(alts);
    }

    let mut errors = 0u8;
    for objdir in objdirs {
        let dir = objdir.join("pack");
        let Ok(rd) = std::fs::read_dir(&dir) else {
            continue;
        };
        // `get_all_packs()` iterates a stable list; the order only sequences the
        // error lines, so a deterministic sort by name is enough.
        let mut names: Vec<String> = rd
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();

        for name in names {
            let Some(base) = name.strip_suffix(".idx") else {
                continue;
            };
            let index_path = dir.join(&name);
            let data_path = dir.join(format!("{base}.pack"));
            // `get_all_packs()` only lists a pack whose `.pack` is present next to
            // its `.idx`; a stray index is not a pack and is not verified, exactly
            // as `porcelain::prune_packed`'s `pack_indices()` skips it.
            match std::fs::metadata(&data_path) {
                Ok(md) if md.is_file() => {}
                _ => continue,
            }
            let opened = pack::index::File::at(&index_path, hash)
                .ok()
                .zip(pack::data::File::at(&data_path, hash).ok());
            let Some((index, data)) = opened else {
                // The `.pack` exists but a file will not open/parse — a pack
                // failure in its own right, as `verify_pack()` treats an
                // unopenable pack.
                eprintln!("error: unable to open pack '{}'", data_path.display());
                errors |= ERROR_PACK;
                continue;
            };
            let options = pack::index::verify::integrity::Options {
                // git checks each object's hash and CRC32 against the index plus
                // the two file checksums; it never re-encodes, so the stricter
                // modes would reject packs git accepts.
                verify_mode: pack::index::verify::Mode::HashCrc32,
                thread_limit: None,
                ..Default::default()
            };
            if let Err(e) = index.verify_integrity(
                Some(pack::index::verify::PackContext {
                    data: &data,
                    options,
                }),
                &mut gix::progress::Discard,
                &AtomicBool::new(false),
            ) {
                // `verify_pack()` "gives error messages itself"; report the real
                // failure rather than inventing git's per-corruption text.
                eprintln!("error: {e}");
                errors |= ERROR_PACK;
            }
        }
    }
    errors
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
/// Whether `id` exists as a loose object in any odb source — which decides
/// whether git found it through `fsck_object_dir()` or through `verify_pack()`,
/// and so which error bit its messages set.
fn is_loose(repo: &gix::Repository, id: ObjectId) -> bool {
    let hex = id.to_hex().to_string();
    odb_sources(repo)
        .into_iter()
        .any(|objdir| objdir.join(&hex[..2]).join(&hex[2..]).is_file())
}

/// Every object directory git would search: the repository's own, plus each
/// alternate reached transitively.
fn odb_sources(repo: &gix::Repository) -> Vec<PathBuf> {
    let mut seen: HashSet<PathBuf> = HashSet::new();
    let mut out: Vec<PathBuf> = Vec::new();
    let mut stack: Vec<PathBuf> = vec![repo.common_dir().join("objects")];
    while let Some(objdir) = stack.pop() {
        let canon = objdir.canonicalize().unwrap_or_else(|_| objdir.clone());
        if !seen.insert(canon) {
            continue;
        }
        out.push(objdir.clone());
        let Ok(content) = std::fs::read(objdir.join("info").join("alternates")) else {
            continue;
        };
        for line in content.split(|&b| b == b'\n') {
            if line.is_empty() || line[0] == b'#' {
                continue;
            }
            let p = Path::new(std::ffi::OsStr::from_bytes(line));
            stack.push(if p.is_absolute() {
                p.to_path_buf()
            } else {
                objdir.join(p)
            });
        }
    }
    out
}

/// How many object directories `fsck_object_dir()` runs over, which is how
/// many progress blocks `--progress` prints.
fn odb_source_count(repo: &gix::Repository) -> usize {
    odb_sources(repo).len()
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

// ===========================================================================
// The fsck message layer — `fsck.c`
// ===========================================================================
//
// On top of the connectivity walk git lints object *contents*: every commit,
// tree and tag it reads goes through `fsck_object()`, which reports one
// `<msg-id>` per defect as
//
// ```text
// error in commit <oid>: missingEmail: invalid author/committer line - missing email
// ```
//
// on stderr. Each id carries a default severity that `fsck.<msg-id>` (for
// `git fsck`) or `receive.fsck.<msg-id>` (for `git receive-pack`) overrides
// with `error`, `warn` or `ignore`, `--strict` promotes every *defaulted*
// warning to an error, and `fsck.skipList` / `receive.fsck.skipList` name a
// file of object ids whose messages are dropped wholesale.
//
// Which ids are here is decided by one rule: a row exists only when this port
// actually performs the check *and* the check can fire on both the `git fsck`
// and the `receive-pack` path. That excludes every id git reports from a place
// this port has no equivalent of — see [`UNPORTED_MSG_IDS`].

/// `fsck.h`'s `enum fsck_msg_type`. `Ignore`, `Warn` and `Error` are the three
/// values a `fsck.<msg-id>` variable can name; `Info` is a default only.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Severity {
    /// The message is not reported at all.
    Ignore,
    /// `fsck_vreport()` prints an `FSCK_INFO` message as a warning, but
    /// `fsck_msg_severity()` only promotes `FSCK_WARN` under `--strict`, so an
    /// info-severity default stays a warning there.
    Info,
    /// Reported as `warning in <type> <oid>: …`; does not affect the exit code.
    Warn,
    /// Reported as `error in <type> <oid>: …`; sets `ERROR_OBJECT`.
    Error,
    /// `FSCK_FATAL`: reported exactly like `Error`, but
    /// `fsck_set_msg_type_from_ids()` refuses to demote it — configuring
    /// anything but `error` dies with `Cannot demote <id> to <value>`.
    Fatal,
}

/// One row of `fsck.h`'s `FOREACH_FSCK_MSG_ID` table.
pub struct Msg {
    /// The `<msg-id>` git prints in front of the message text.
    pub id: &'static str,
    /// The variable `git fsck` reads for this check's severity.
    pub fsck_key: &'static str,
    /// The variable `git receive-pack` reads for it. git deliberately does
    /// *not* let this fall back to `fsck_key` (`git help config`: "the
    /// receive.fsck.<msg-id> … variables will not fall back on the
    /// fsck.<msg-id> configuration").
    pub receive_key: &'static str,
    /// `fsck.h`'s severity for the id when nothing configures it.
    pub default: Severity,
}

/// Build one table row; the two config keys are always `fsck.<id>` and
/// `receive.fsck.<id>`, spelled out so both are greppable literals.
macro_rules! msg {
    ($konst:ident, $id:literal, $fsck:literal, $receive:literal, $sev:ident) => {
        #[doc = concat!("`", $id, "`, whose severity comes from `", $fsck, "` under ")]
        #[doc = concat!("`git fsck` and from `", $receive, "` under `git receive-pack`.")]
        const $konst: Msg = Msg {
            id: $id,
            fsck_key: $fsck,
            receive_key: $receive,
            default: Severity::$sev,
        };
    };
}

// --- commit header checks (`verify_headers`, `fsck_commit`, `fsck_ident`) ---
msg!(MISSING_AUTHOR, "missingAuthor", "fsck.missingAuthor", "receive.fsck.missingAuthor", Error);
msg!(MULTIPLE_AUTHORS, "multipleAuthors", "fsck.multipleAuthors", "receive.fsck.multipleAuthors", Error);
msg!(MISSING_COMMITTER, "missingCommitter", "fsck.missingCommitter", "receive.fsck.missingCommitter", Error);
msg!(MISSING_NAME_BEFORE_EMAIL, "missingNameBeforeEmail", "fsck.missingNameBeforeEmail", "receive.fsck.missingNameBeforeEmail", Error);
msg!(BAD_EMAIL, "badEmail", "fsck.badEmail", "receive.fsck.badEmail", Error);
msg!(MISSING_EMAIL, "missingEmail", "fsck.missingEmail", "receive.fsck.missingEmail", Error);
msg!(MISSING_SPACE_BEFORE_EMAIL, "missingSpaceBeforeEmail", "fsck.missingSpaceBeforeEmail", "receive.fsck.missingSpaceBeforeEmail", Error);
msg!(MISSING_SPACE_BEFORE_DATE, "missingSpaceBeforeDate", "fsck.missingSpaceBeforeDate", "receive.fsck.missingSpaceBeforeDate", Error);
msg!(ZERO_PADDED_DATE, "zeroPaddedDate", "fsck.zeroPaddedDate", "receive.fsck.zeroPaddedDate", Error);
msg!(BAD_DATE_OVERFLOW, "badDateOverflow", "fsck.badDateOverflow", "receive.fsck.badDateOverflow", Error);
msg!(BAD_DATE, "badDate", "fsck.badDate", "receive.fsck.badDate", Error);
msg!(BAD_TIMEZONE, "badTimezone", "fsck.badTimezone", "receive.fsck.badTimezone", Error);
msg!(NUL_IN_COMMIT, "nulInCommit", "fsck.nulInCommit", "receive.fsck.nulInCommit", Warn);
msg!(UNTERMINATED_HEADER, "unterminatedHeader", "fsck.unterminatedHeader", "receive.fsck.unterminatedHeader", Fatal);
msg!(NUL_IN_HEADER, "nulInHeader", "fsck.nulInHeader", "receive.fsck.nulInHeader", Fatal);

// --- tree checks (`fsck_tree`) ---------------------------------------------
msg!(NULL_SHA1, "nullSha1", "fsck.nullSha1", "receive.fsck.nullSha1", Warn);
msg!(FULL_PATHNAME, "fullPathname", "fsck.fullPathname", "receive.fsck.fullPathname", Warn);
msg!(HAS_DOT, "hasDot", "fsck.hasDot", "receive.fsck.hasDot", Warn);
msg!(HAS_DOTDOT, "hasDotdot", "fsck.hasDotdot", "receive.fsck.hasDotdot", Warn);
msg!(HAS_DOTGIT, "hasDotgit", "fsck.hasDotgit", "receive.fsck.hasDotgit", Warn);
msg!(ZERO_PADDED_FILEMODE, "zeroPaddedFilemode", "fsck.zeroPaddedFilemode", "receive.fsck.zeroPaddedFilemode", Warn);
msg!(BAD_FILEMODE, "badFilemode", "fsck.badFilemode", "receive.fsck.badFilemode", Info);
msg!(DUPLICATE_ENTRIES, "duplicateEntries", "fsck.duplicateEntries", "receive.fsck.duplicateEntries", Error);
msg!(TREE_NOT_SORTED, "treeNotSorted", "fsck.treeNotSorted", "receive.fsck.treeNotSorted", Error);
msg!(LARGE_PATHNAME, "largePathname", "fsck.largePathname", "receive.fsck.largePathname", Warn);

// --- tag checks (`fsck_tag`) -----------------------------------------------
msg!(MISSING_TAGGER_ENTRY, "missingTaggerEntry", "fsck.missingTaggerEntry", "receive.fsck.missingTaggerEntry", Info);
msg!(BAD_TAG_NAME, "badTagName", "fsck.badTagName", "receive.fsck.badTagName", Info);
msg!(EXTRA_HEADER_ENTRY, "extraHeaderEntry", "fsck.extraHeaderEntry", "receive.fsck.extraHeaderEntry", Ignore);

/// Every row this port implements, for severity resolution and for telling a
/// misspelled `fsck.<x>` key from a real one.
pub const MSGS: &[Msg] = &[
    MISSING_AUTHOR,
    MULTIPLE_AUTHORS,
    MISSING_COMMITTER,
    MISSING_NAME_BEFORE_EMAIL,
    BAD_EMAIL,
    MISSING_EMAIL,
    MISSING_SPACE_BEFORE_EMAIL,
    MISSING_SPACE_BEFORE_DATE,
    ZERO_PADDED_DATE,
    BAD_DATE_OVERFLOW,
    BAD_DATE,
    BAD_TIMEZONE,
    NUL_IN_COMMIT,
    UNTERMINATED_HEADER,
    NUL_IN_HEADER,
    NULL_SHA1,
    FULL_PATHNAME,
    HAS_DOT,
    HAS_DOTDOT,
    HAS_DOTGIT,
    ZERO_PADDED_FILEMODE,
    BAD_FILEMODE,
    DUPLICATE_ENTRIES,
    TREE_NOT_SORTED,
    LARGE_PATHNAME,
    MISSING_TAGGER_ENTRY,
    BAD_TAG_NAME,
    EXTRA_HEADER_ENTRY,
];

/// The rest of `FOREACH_FSCK_MSG_ID`: ids git knows and this port never
/// reports. They are listed by bare id and not by variable name on purpose —
/// nothing reads a `fsck.<one-of-these>` variable, so naming one would claim
/// support that does not exist. Configuring one is accepted (git would too)
/// and changes nothing, which is the honest outcome for a check that is not
/// performed. Three groups:
///
///   * **the reference-database ids** (`badRefName`, `badRefContent`,
///     `packedRefUnsorted`, `symlinkRef`, `badReftableTableName`, …) belong to
///     `git refs verify`, which the vendored crates do not implement (see
///     divergence 2 above);
///   * **the `.gitmodules`/`.gitattributes`/`.gitignore`/`.mailmap` blob ids**
///     need the blob-content lint git runs after collecting those paths from
///     every tree, which this port does not run;
///   * **the ids reachable only past a parse failure** (`missingTree`,
///     `badTreeSha1`, `badParentSha1`, `missingObject`, `missingType`,
///     `unknownType`, `emptyName`, …). git reaches those by fsck'ing a raw
///     buffer its own object parser already rejected; this port only reaches
///     `fsck_object` for objects gix could parse, so they can never fire.
const UNPORTED_MSG_IDS: &[&str] = &[
    "badGpgsig",
    "badHeaderContinuation",
    "badHeadTarget",
    "badName",
    "badObjectSha1",
    "badPackedRefEntry",
    "badPackedRefHeader",
    "badParentSha1",
    "badRefContent",
    "badReferentName",
    "badRefFiletype",
    "badRefName",
    "badRefOid",
    "badReftableTableName",
    "badTree",
    "badTreeSha1",
    "badType",
    "emptyName",
    "emptyPackedRefsFile",
    "gitattributesBlob",
    "gitattributesLarge",
    "gitattributesLineLength",
    "gitattributesMissing",
    "gitattributesSymlink",
    "gitignoreSymlink",
    "gitmodulesBlob",
    "gitmodulesLarge",
    "gitmodulesMissing",
    "gitmodulesName",
    "gitmodulesParse",
    "gitmodulesPath",
    "gitmodulesSymlink",
    "gitmodulesUpdate",
    "gitmodulesUrl",
    "mailmapSymlink",
    "missingObject",
    "missingTag",
    "missingTagEntry",
    "missingTree",
    "missingType",
    "missingTypeEntry",
    "packedRefEntryNotTerminated",
    "packedRefUnsorted",
    "refMissingNewline",
    "symlinkRef",
    "symrefTargetIsNotARef",
    "trailingRefContent",
    "unknownType",
];

/// One reported defect: the table row that names it plus the rendered text.
pub struct Finding {
    /// The row, which decides the severity and prints the `<msg-id>:` prefix.
    pub msg: &'static Msg,
    /// The message body, already formatted (`nulInHeader` and `badTagName`
    /// interpolate).
    pub text: String,
}

/// Whether the caller is `git fsck` or `git receive-pack`, which picks the
/// variable family and the `--strict` behaviour.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum MsgSource {
    /// `fsck.<msg-id>` / `fsck.skipList`, with `--strict` honoured.
    Fsck { strict: bool },
    /// `receive.fsck.<msg-id>` / `receive.fsck.skipList`. The transfer check
    /// always runs `index-pack`/`unpack-objects` with `--strict`, so a
    /// defaulted warning is an error here even though the same object only
    /// warns under a plain `git fsck`.
    Receive,
}

/// The resolved severity of every message id plus the skipped-object set —
/// `fsck.c`'s `struct fsck_options` fields `msg_type` and `skiplist`.
pub struct MsgConfig {
    /// Severity per `Msg::id`.
    levels: HashMap<&'static str, Severity>,
    /// Object ids the skip list names; every message about them is dropped.
    skip: HashSet<ObjectId>,
    /// A `die()` that `git fsck` reaches while reading its own configuration,
    /// but `receive-pack` only reaches inside the `index-pack`/`unpack-objects`
    /// child it hands `--strict=<types>` to — so the pusher sees it on the
    /// side band and the push fails with that child's abnormal exit rather
    /// than the session dying before the advertisement.
    pub deferred_fatal: Option<String>,
}

impl MsgConfig {
    /// Resolve every severity from the repository's configuration.
    ///
    /// Returns the message git dies with — without its `fatal: ` prefix — for
    /// a bad value (`Unknown fsck message type`) or, under `git fsck` only, a
    /// misspelled id (`Unhandled message id`). The two failures the transfer
    /// side only reaches inside its child land in [`Self::deferred_fatal`].
    pub fn new(repo: &gix::Repository, source: MsgSource) -> Result<Self, String> {
        let config = repo.config_snapshot();
        let strict = matches!(source, MsgSource::Fsck { strict: true } | MsgSource::Receive);
        let mut deferred_fatal: Option<String> = None;
        let mut levels = HashMap::with_capacity(MSGS.len());
        for m in MSGS {
            let key = match source {
                MsgSource::Fsck { .. } => m.fsck_key,
                MsgSource::Receive => m.receive_key,
            };
            let level = match config.string(key) {
                Some(v) => {
                    let value = v.to_string();
                    // `is_valid_msg_type()` runs in receive-pack's own config
                    // callback, so an unknown *value* is fatal on both paths.
                    let level = parse_severity(&value)?;
                    if m.default == Severity::Fatal && level != Severity::Error {
                        let text = format!("Cannot demote {} to {value}", m.id.to_lowercase());
                        match source {
                            MsgSource::Fsck { .. } => return Err(text),
                            MsgSource::Receive => {
                                deferred_fatal.get_or_insert(text);
                            }
                        }
                    }
                    level
                }
                // `fsck_msg_severity()`: an unconfigured warning becomes an
                // error under `--strict`; a configured one, and an
                // info-severity default, are left alone.
                None if strict && m.default == Severity::Warn => Severity::Error,
                None => m.default,
            };
            levels.insert(m.id, level);
        }

        // git validates *every* variable in the family, including the ids this
        // port does not check, and dies on one it does not know at all.
        let (section, subsection) = match source {
            MsgSource::Fsck { .. } => ("fsck", None),
            MsgSource::Receive => ("receive", Some("fsck")),
        };
        for name in value_names(&config, section, subsection) {
            let lower = name.to_lowercase();
            if lower == "skiplist" {
                continue;
            }
            let known = MSGS.iter().any(|m| m.id.eq_ignore_ascii_case(&name))
                || UNPORTED_MSG_IDS.iter().any(|id| id.eq_ignore_ascii_case(&name));
            if known {
                continue;
            }
            // `git help config`: an unknown `fsck.<msg-id>` kills fsck, while
            // the same under `receive.fsck.` is only a warning.
            match source {
                MsgSource::Fsck { .. } => return Err(format!("Unhandled message id: {lower}")),
                MsgSource::Receive => eprintln!("warning: skipping unknown msg id '{lower}'"),
            }
        }

        let skip_key = match source {
            MsgSource::Fsck { .. } => "fsck.skipList",
            MsgSource::Receive => "receive.fsck.skipList",
        };
        let skip = match config.string(skip_key) {
            Some(path) => match read_skip_list(&path.to_string()) {
                Ok(skip) => skip,
                // `init_skiplist()` runs where the checking runs, so the
                // transfer side hits this inside its child.
                Err(text) => match source {
                    MsgSource::Fsck { .. } => return Err(text),
                    MsgSource::Receive => {
                        deferred_fatal.get_or_insert(text);
                        HashSet::new()
                    }
                },
            },
            None => HashSet::new(),
        };
        Ok(Self {
            levels,
            skip,
            deferred_fatal,
        })
    }

    /// `fsck.c::report()` minus the printing: the severity a finding about
    /// `oid` is reported at, or `Ignore` when it is suppressed.
    pub fn severity(&self, finding: &Finding, oid: &ObjectId) -> Severity {
        let level = self.levels.get(finding.msg.id).copied().unwrap_or(finding.msg.default);
        if level != Severity::Ignore && self.skip.contains(oid) {
            return Severity::Ignore;
        }
        level
    }
}

/// Every value name configured in `[<section> "<subsection>"]`, across all
/// files of the snapshot, so a misspelled id can be diagnosed.
fn value_names(
    config: &gix::config::Snapshot<'_>,
    section: &str,
    subsection: Option<&str>,
) -> Vec<String> {
    let mut out = Vec::new();
    let Some(sections) = config.plumbing().sections_by_name(section) else {
        return out;
    };
    for s in sections {
        let sub = s.header().subsection_name().map(|n| n.to_string());
        if sub.as_deref() != subsection {
            continue;
        }
        out.extend(s.body().value_names());
    }
    out
}

/// `fsck.c::parse_msg_type` — the three names, compared case-sensitively.
fn parse_severity(value: &str) -> Result<Severity, String> {
    match value {
        "error" => Ok(Severity::Error),
        "warn" => Ok(Severity::Warn),
        "ignore" => Ok(Severity::Ignore),
        other => Err(format!("Unknown fsck message type: '{other}'")),
    }
}

/// `fsck.c::init_skiplist` plus `oidset_parse_file_carefully`: one object id
/// per line, with `#` comments, blank lines and surrounding whitespace ignored.
fn read_skip_list(path: &str) -> Result<HashSet<ObjectId>, String> {
    let text = std::fs::read_to_string(path)
        .map_err(|_| format!("could not open object name list: {path}"))?;
    let mut out = HashSet::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let id = ObjectId::from_hex(line.as_bytes())
            .map_err(|_| format!("invalid object name: {line}"))?;
        out.insert(id);
    }
    Ok(out)
}

/// `fsck.c::fsck_object` for the three types whose contents this port lints.
/// Findings come back in git's reporting order; the caller decides which of
/// them are severe enough to print.
pub fn check_object(kind: Kind, data: &[u8]) -> Vec<Finding> {
    let mut out = Vec::new();
    match kind {
        Kind::Commit => check_commit(data, &mut out),
        Kind::Tree => check_tree(data, &mut out),
        Kind::Tag => check_tag(data, &mut out),
        Kind::Blob => {}
    }
    out
}

/// Push one finding.
fn report(out: &mut Vec<Finding>, msg: &'static Msg, text: impl Into<String>) {
    out.push(Finding { msg, text: text.into() });
}

/// `fsck.c::verify_headers`: the header block must be terminated, and must not
/// contain a NUL. Returns `true` when it reported, which stops the caller —
/// everything after this point indexes into the buffer.
fn verify_headers(data: &[u8], out: &mut Vec<Finding>) -> bool {
    for (i, &b) in data.iter().enumerate() {
        match b {
            0 => {
                report(out, &NUL_IN_HEADER, format!("unterminated header: NUL at offset {i}"));
                return true;
            }
            b'\n' if data.get(i + 1) == Some(&b'\n') => return false,
            _ => {}
        }
    }
    // No blank line: a body is optional, but the last header line still has to
    // end in a newline.
    if data.last() == Some(&b'\n') {
        return false;
    }
    report(out, &UNTERMINATED_HEADER, "unterminated header");
    true
}

/// `fsck.c::fsck_commit`, entered only for a commit gix already parsed — so the
/// `tree`/`parent` lines are known well formed and the ids git would report
/// there (`missingTree`, `badTreeSha1`, `badParentSha1`) cannot arise. Anything
/// unexpected in those lines stops the check instead of reporting an id this
/// port does not claim.
fn check_commit(data: &[u8], out: &mut Vec<Finding>) {
    if verify_headers(data, out) {
        return;
    }
    let Some(mut p) = strip(data, b"tree ") else { return };
    let Some(rest) = skip_line(p) else { return };
    p = rest;
    while let Some(after) = strip(p, b"parent ") {
        let Some(rest) = skip_line(after) else { return };
        p = rest;
    }

    let mut authors = 0;
    while let Some(after) = strip(p, b"author ") {
        authors += 1;
        match check_ident(after, out) {
            None => return,
            Some(rest) => p = rest,
        }
    }
    if authors < 1 {
        report(out, &MISSING_AUTHOR, "invalid format - expected 'author' line");
        return;
    }
    if authors > 1 {
        report(out, &MULTIPLE_AUTHORS, "invalid format - multiple 'author' lines");
        return;
    }
    let Some(after) = strip(p, b"committer ") else {
        report(out, &MISSING_COMMITTER, "invalid format - expected 'committer' line");
        return;
    };
    if check_ident(after, out).is_none() {
        return;
    }
    if data.contains(&0) {
        report(out, &NUL_IN_COMMIT, "NUL byte in the commit object body");
    }
}

/// `fsck.c::fsck_ident`, checked branch by branch in git's order. `ident`
/// starts just past `author `/`committer `/`tagger `. `None` means a defect was
/// pushed, which makes `fsck_commit`/`fsck_tag` return immediately; `Some` is
/// the rest of the buffer, cursor advanced past the line.
fn check_ident<'a>(ident: &'a [u8], out: &mut Vec<Finding>) -> Option<&'a [u8]> {
    if ident.first() == Some(&b'<') {
        report(out, &MISSING_NAME_BEFORE_EMAIL, "invalid author/committer line - missing space before email");
        return None;
    }
    let mut i = span_to_email(ident, 0);
    match ident.get(i) {
        Some(b'>') => {
            report(out, &BAD_EMAIL, "invalid author/committer line - bad email");
            return None;
        }
        Some(b'<') => {}
        _ => {
            report(out, &MISSING_EMAIL, "invalid author/committer line - missing email");
            return None;
        }
    }
    if i == 0 || ident[i - 1] != b' ' {
        report(out, &MISSING_SPACE_BEFORE_EMAIL, "invalid author/committer line - missing space before email");
        return None;
    }
    i = span_to_email(ident, i + 1);
    if ident.get(i) != Some(&b'>') {
        report(out, &BAD_EMAIL, "invalid author/committer line - bad email");
        return None;
    }
    i += 1;
    if ident.get(i) != Some(&b' ') {
        report(out, &MISSING_SPACE_BEFORE_DATE, "invalid author/committer line - missing space before date");
        return None;
    }
    i += 1;
    if ident.get(i) == Some(&b'0') && ident.get(i + 1) != Some(&b' ') {
        report(out, &ZERO_PADDED_DATE, "invalid author/committer line - zero-padded date");
        return None;
    }
    let digits = ident[i.min(ident.len())..]
        .iter()
        .take_while(|b| b.is_ascii_digit())
        .count();
    // `parse_timestamp` is `strtoumax`, which saturates on overflow;
    // `date_overflows` then rejects anything past `TIME_MAX` (`i64::MAX` here).
    let value = std::str::from_utf8(&ident[i..i + digits])
        .ok()
        .and_then(|s| s.parse::<u64>().ok());
    if digits > 0 && value.is_none_or(|v| v > i64::MAX as u64) {
        report(out, &BAD_DATE_OVERFLOW, "invalid author/committer line - date causes integer overflow");
        return None;
    }
    if digits == 0 || ident.get(i + digits) != Some(&b' ') {
        report(out, &BAD_DATE, "invalid author/committer line - bad date");
        return None;
    }
    i += digits + 1;
    let tz = &ident[i.min(ident.len())..];
    let tz_ok = matches!(tz.first(), Some(b'+' | b'-'))
        && tz.len() > 5
        && tz[1..5].iter().all(u8::is_ascii_digit)
        && tz[5] == b'\n';
    if !tz_ok {
        report(out, &BAD_TIMEZONE, "invalid author/committer line - bad time zone");
        return None;
    }
    Some(skip_line(ident).unwrap_or(&[]))
}

/// `strcspn(p, "<>\n")` from `from`: the index of the first `<`, `>` or newline,
/// or the buffer length.
fn span_to_email(p: &[u8], from: usize) -> usize {
    p.iter()
        .enumerate()
        .skip(from)
        .find(|(_, b)| matches!(b, b'<' | b'>' | b'\n'))
        .map_or(p.len(), |(i, _)| i)
}

/// `skip_prefix`: the remainder after `prefix`, or `None` when it is absent.
fn strip<'a>(data: &'a [u8], prefix: &[u8]) -> Option<&'a [u8]> {
    data.strip_prefix(prefix)
}

/// The remainder after the next newline, or `None` when there is none.
fn skip_line(data: &[u8]) -> Option<&[u8]> {
    data.iter().position(|&b| b == b'\n').map(|i| &data[i + 1..])
}

/// git's tree-entry mode set. `S_IFREG | 0664` is also accepted outside
/// `--strict`, which this port folds in: it does not run the strict mode table.
const GOOD_MODES: [u32; 6] = [0o100644, 0o100664, 0o100755, 0o120000, 0o040000, 0o160000];

/// `fsck.c::fsck_tree`. Every defect is accumulated over the whole tree and
/// reported once, in git's fixed order.
fn check_tree(data: &[u8], out: &mut Vec<Finding>) {
    let mut has_null_oid = false;
    let mut has_full_path = false;
    let mut has_dot = false;
    let mut has_dotdot = false;
    let mut has_dotgit = false;
    let mut has_zero_pad = false;
    let mut has_bad_modes = false;
    let mut has_dup_entries = false;
    let mut not_properly_sorted = false;
    let mut has_large_name = false;

    let mut p = data;
    let mut previous: Option<Vec<u8>> = None;
    while !p.is_empty() {
        let Some(space) = p.iter().position(|&b| b == b' ') else { return };
        let mode_field = &p[..space];
        let Some(nul) = p[space + 1..].iter().position(|&b| b == 0) else { return };
        let name = &p[space + 1..space + 1 + nul];
        let oid_at = space + 1 + nul + 1;
        if oid_at + 20 > p.len() {
            return;
        }
        let oid = &p[oid_at..oid_at + 20];
        p = &p[oid_at + 20..];

        let Some(mode) = std::str::from_utf8(mode_field)
            .ok()
            .and_then(|m| u32::from_str_radix(m, 8).ok())
        else {
            return;
        };
        has_zero_pad |= mode_field.first() == Some(&b'0');
        has_bad_modes |= !GOOD_MODES.contains(&mode);
        has_null_oid |= oid.iter().all(|&b| b == 0);
        has_full_path |= name.contains(&b'/');
        has_dot |= name == b".";
        has_dotdot |= name == b"..";
        has_dotgit |= is_dotgit(name);
        has_large_name |= name.len() > 4096;

        // `verify_ordered`: names compare with a directory's implicit trailing
        // slash, so `a` (tree) sorts after `a.`
        let mut key = name.to_vec();
        if mode & 0o170000 == 0o040000 {
            key.push(b'/');
        }
        if let Some(prev) = &previous {
            match key.cmp(prev) {
                std::cmp::Ordering::Less => not_properly_sorted = true,
                std::cmp::Ordering::Equal => has_dup_entries = true,
                std::cmp::Ordering::Greater => {}
            }
        }
        previous = Some(key);
    }

    if has_null_oid {
        report(out, &NULL_SHA1, "contains entries pointing to null sha1");
    }
    if has_full_path {
        report(out, &FULL_PATHNAME, "contains full pathnames");
    }
    if has_dot {
        report(out, &HAS_DOT, "contains '.'");
    }
    if has_dotdot {
        report(out, &HAS_DOTDOT, "contains '..'");
    }
    if has_dotgit {
        report(out, &HAS_DOTGIT, "contains '.git'");
    }
    if has_zero_pad {
        report(out, &ZERO_PADDED_FILEMODE, "contains zero-padded file modes");
    }
    if has_bad_modes {
        report(out, &BAD_FILEMODE, "contains bad file modes");
    }
    if has_dup_entries {
        report(out, &DUPLICATE_ENTRIES, "contains duplicate file entries");
    }
    if not_properly_sorted {
        report(out, &TREE_NOT_SORTED, "not properly sorted");
    }
    if has_large_name {
        report(out, &LARGE_PATHNAME, "contains excessively large pathname");
    }
}

/// The spellings of `.git` git's `is_hfs_dotgit`/`is_ntfs_dotgit` reject that
/// are expressible in ASCII: any case of `.git`, and the NTFS forms `.git`
/// with trailing dots or spaces plus the `git~1` short name. The Unicode
/// confusables `is_hfs_dotgit` also folds away (zero-width joiners, the
/// Hebrew/Arabic direction marks) are not covered.
fn is_dotgit(name: &[u8]) -> bool {
    let trimmed: &[u8] = {
        let mut end = name.len();
        while end > 0 && (name[end - 1] == b'.' || name[end - 1] == b' ') {
            end -= 1;
        }
        &name[..end]
    };
    trimmed.eq_ignore_ascii_case(b".git") || trimmed.eq_ignore_ascii_case(b"git~1")
}

/// `fsck.c::fsck_tag`, entered only for a tag gix already parsed — so the
/// `object`/`type`/`tag` lines are present and the ids git reports for their
/// absence cannot arise. The `tag` name is still validated, because gix accepts
/// names `check_refname_format()` rejects.
fn check_tag(data: &[u8], out: &mut Vec<Finding>) {
    if verify_headers(data, out) {
        return;
    }
    let Some(p) = strip(data, b"object ") else { return };
    let Some(p) = skip_line(p) else { return };
    let Some(p) = strip(p, b"type ") else { return };
    let Some(p) = skip_line(p) else { return };
    let Some(p) = strip(p, b"tag ") else { return };
    let Some(eol) = p.iter().position(|&b| b == b'\n') else { return };
    let name = &p[..eol];
    if !refname_ok(name) {
        report(out, &BAD_TAG_NAME, format!("invalid 'tag' name: {}", String::from_utf8_lossy(name)));
    }
    let p = &p[eol + 1..];
    let rest = match strip(p, b"tagger ") {
        None => {
            // Early tags have no tagger, so git only warns.
            report(out, &MISSING_TAGGER_ENTRY, "invalid format - expected 'tagger' line");
            p
        }
        Some(after) => match check_ident(after, out) {
            None => return,
            Some(rest) => rest,
        },
    };
    if !rest.is_empty() && !rest.starts_with(b"\n") {
        report(out, &EXTRA_HEADER_ENTRY, "invalid format - extra header(s) after 'tagger'");
    }
}

/// `refs.c::check_refname_format(refs/tags/<name>, 0)` reduced to the rules a
/// tag name can break: no empty component, no leading dot or trailing `.lock`
/// in a component, no `..`, no ASCII control character, none of ` ~^:?*[\`,
/// no trailing slash and no `@{`.
fn refname_ok(name: &[u8]) -> bool {
    if name.is_empty() || name.ends_with(b"/") || name.ends_with(b".") {
        return false;
    }
    for component in name.split(|&b| b == b'/') {
        if component.is_empty() || component.starts_with(b".") || component.ends_with(b".lock") {
            return false;
        }
    }
    for (i, &b) in name.iter().enumerate() {
        let bad = b < 0x20
            || b == 0x7f
            || matches!(b, b' ' | b'~' | b'^' | b':' | b'?' | b'*' | b'[' | b'\\')
            || (b == b'.' && name.get(i + 1) == Some(&b'.'))
            || (b == b'@' && name.get(i + 1) == Some(&b'{'));
        if bad {
            return false;
        }
    }
    true
}

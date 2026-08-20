use anyhow::{bail, Result};
use std::collections::{HashMap, HashSet};
use std::ffi::OsStr;
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
/// `builtin/fsck.c`'s `ERROR_REFS` — the `git refs verify` child failed.
const ERROR_REFS: u8 = 8;

/// `git fsck` — verify connectivity of the object database.
///
/// The control flow follows `builtin/fsck.c::cmd_fsck` so that the interleaving
/// of stdout and stderr matches:
///
/// 1. the reference-database check (`--references`, on by default) runs first
///    and, under `--progress`, emits its progress block;
/// 2. `<object>` arguments are resolved by `repo_get_oid()`, which accepts a
///    full-length hex id without consulting the odb. An argument it cannot turn
///    into an id at all prints `error: invalid parameter: expected sha1, got
///    '<arg>'` and sets `ERROR_OBJECT`; one that yields an id no object backs
///    reaches `snapshot_ref()`, which prints `error: <arg>: invalid sha1 pointer
///    <oid>`, sets `ERROR_REACHABLE` and leaves `default_refs` alone. Any
///    argument at all suppresses the default head set and turns reflogs off,
///    exactly as `snapshot_refs()` does;
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
/// 1. **The fsck message layer covers every message id's *configuration*, but
///    not every message id's *check*.** git lints object *contents* on top of
///    the connectivity walk and exits 1 when an error-severity message fires;
///    that layer is ported below (see [`MSGS`]), including `fsck.<msg-id>`
///    severities, `--strict` promotion, `fsck.skipList`, the
///    `.gitmodules`/`.gitattributes` blob lint and `fsck_finish()`'s sweep over
///    the paths the trees named. [`MSGS`] holds a row for all 76 ids of
///    `FOREACH_FSCK_MSG_ID`, so every `fsck.<msg-id>` parses, validates and
///    round-trips as git's does. The rows built with `msg_config_only!` are the
///    ones whose *check* is not performed — each says why in its own doc, and
///    all but `badReftableTableName` name a finding that is unreachable in stock
///    git too. `--full` verifies pack *integrity* (checksums, per-object
///    hash/CRC) via the gix pack verifier; the message layer runs over packed
///    objects too, since the object-directory scan below iterates the whole odb
///    rather than only its loose half.
/// 2. **The reference-database check runs in-process.** git checks the
///    reference database by default (`--references`) by *running* `git refs
///    verify` as a child; [`fsck_refs`] is that check, called directly instead.
///    Its findings, its `--verbose` trace and its `ERROR_REFS` exit bit are all
///    reproduced. One message id of the family is missing —
///    `badReftableTableName`, which only the reftable backend raises and the
///    vendored `gix-ref` has no reftable backend.
/// 3. **No re-hashing.** git recomputes each object's hash to catch a silent
///    `hash mismatch`; this port trusts the odb's own integrity checking.
/// 3b. **A reference the ref store cannot resolve is reported, not fatal.**
///    `snapshot_ref()` prints `error: <ref>: invalid sha1 pointer <null-oid>`,
///    sets `ERROR_REACHABLE` and carries on with the remaining refs, so
///    `git fsck` still reports the rest of the repository — which is what a
///    `refs/remotes/<remote>/HEAD` left pointing at a renamed default branch
///    produces.
/// 4. **An unreadable object is reported and stepped over**, as `fsck_loose()`
///    does — that is the whole point of the command. [`read_loose_object`] is a
///    port of `object-file.c`'s function of that name, so a loose object that
///    will not inflate, whose header will not parse, whose type is not a type,
///    whose body is truncated or has trailing garbage, or whose contents hash to
///    a different id gets git's own diagnostic (`corrupt loose object '<oid>'`,
///    `garbage at end of loose object '<oid>'`, `unable to unpack header of
///    <path>`, …) followed by `error: <oid>: object corrupt or missing: <path>`
///    or `error: <oid>: hash-path mismatch, found at: <path>`, sets
///    `ERROR_OBJECT`, and the scan continues. Such an object never gains
///    `HAS_OBJ`, so it draws no `unreachable`/`dangling` line and something
///    reachable that names it draws a `missing` one. Two details are not
///    reproduced: git's `error_errno()` line after an empty object file carries
///    a stale errno from an unrelated syscall, and a *corrupt* blob larger than
///    `core.bigFileThreshold` is verified by git with `check_stream_oid()`
///    rather than `unpack_loose_rest()`, which only changes which message
///    precedes the identical `object corrupt or missing` line. The threshold's
///    other consequence — that an intact blob over it reaches `fsck_blob()` with
///    no buffer, which is what reports `gitmodulesLarge` — *is* reproduced; see
///    [`blob_buffer`]. The neighbouring case — an object that reads fine but that
///    `parse_object_buffer()` rejects — is ported too: the `error: <oid>: object
///    could not be parsed: <path>` line, the `error:` diagnostic
///    `parse_commit_buffer()`/`parse_tag_buffer()` prints ahead of it,
///    `ERROR_OBJECT`, and carrying on with the rest of the odb.
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
///        `Checking ref database`. Per divergence 2 those lines are produced
///        here too, but the refs of one directory are traced in
///        `std::fs::read_dir` order rather than git's `readdir()` order — the
///        same system call over the same directory, and so the same order in
///        practice, but not a guarantee.
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
    //
    // `fsck_refs()` starts its progress, prints its own `--verbose` header, runs
    // `git refs verify` as a child, and only then displays the progress — so
    // every line the check itself emits lands between the header and the
    // progress block. [`fsck_refs`] is that child, in-process.
    if opt.check_references {
        if opt.verbose {
            eprintln!("Checking ref database");
        }
        if fsck_refs(&repo, &msg_config, opt.verbose) {
            errors |= ERROR_REFS;
        }
        if show_progress {
            progress_block("Checking ref database", 1);
        }
    }

    // ---- 2. explicit <object> arguments ------------------------------------
    //
    // `snapshot_refs()`: any argument at all replaces the default head set and
    // turns reflogs off, whether or not the argument resolved.
    let mut heads: Vec<ObjectId> = Vec::new();
    let mut default_refs = 0usize;
    // Objects `snapshot_ref()`'s `parse_object()` has already parsed. A second
    // parse of one of them is a no-op, which the object scan below has to know:
    // see `creates_children` there.
    let mut pre_parsed: HashSet<ObjectId> = HashSet::new();
    for arg in &opt.objects {
        // `repo_get_oid()` turns a full-length hex id into an object id without
        // consulting the odb, so an id that names nothing still reaches
        // `snapshot_ref()`; every shorter or symbolic form has to resolve.
        let resolved = repo
            .rev_parse_single(arg.as_str())
            .map(|id| id.detach())
            .ok()
            .or_else(|| ObjectId::from_hex(arg.as_bytes()).ok());
        match resolved {
            // `snapshot_ref()`: `parse_object()` returning NULL is the id's
            // problem, not the argument's — a different message and a different
            // error bit, and `default_refs` is left alone so the head set stays
            // empty.
            Some(id) if !repo.has_object(id) => {
                eprintln!("error: {arg}: invalid sha1 pointer {id}");
                errors |= ERROR_REACHABLE;
            }
            Some(id) => {
                default_refs += 1;
                // `snapshot_ref()` calls `parse_object()`, which creates the
                // object and then parses it — and the parse creates the links it
                // stores: `parse_commit_buffer()` its tree and every parent,
                // `parse_tag_buffer()` the tagged object. A tree's entries are
                // *not* created here; `parse_tree_buffer()` only keeps the
                // buffer, and the entries are decoded later by the walk.
                state.note(id);
                if matches!(
                    repo.find_header(id).map(|h| h.kind()),
                    Ok(Kind::Commit) | Ok(Kind::Tag)
                ) {
                    if let Ok(Ok(parsed)) = decode(&repo, id) {
                        for (child, _) in &parsed.children {
                            state.note(*child);
                        }
                        pre_parsed.insert(id);
                    }
                }
                heads.push(id);
            }
            None => {
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
        // The odb iterator can yield the same id from more than one source.
        if in_odb.insert(id) {
            all.push(id);
        }
    }
    // The gix iterator's order is its own; git's is `fsck_source()`'s, and that
    // is what decides where two colliding ids land in `obj_hash`. Take only
    // membership from the iterator and re-lay `all` in git's order whenever that
    // order is reconstructible — see [`loose_scan_order`].
    let scan_ordered = match loose_scan_order(&repo) {
        Some(order) if order.len() == all.len() && order.iter().all(|id| in_odb.contains(id)) => {
            all = order;
            true
        }
        _ => false,
    };
    // `lookup_<type>()` is called in one long sequence over the whole command,
    // and [`SlotOrder`] can only replay it when every caller is accounted for.
    // That holds when the scan itself is in git's order, when the scan runs at
    // all (`--connectivity-only` replaces it with a listing walk this port does
    // not order), when an explicit `<object>` argument made `snapshot_refs()`
    // return before touching the ref store and turned reflogs off — the
    // ref-and-reflog snapshot is the one phase this port takes at git's
    // *handling* point rather than git's *snapshotting* point — and when no
    // linked worktree can put its index ahead of the main one under `--cache`.
    let creation_modeled = scan_ordered
        && !opt.connectivity_only
        && explicit_heads
        && repo.worktrees().map(|w| w.is_empty()).unwrap_or(true);

    // Children of every object, for `used` and `missing`. git checks every
    // object in the odb, not just the reachable ones, and marks each child it
    // sees as used. `dangling` is precisely "unreachable and never used", so
    // this pass has to cover unreachable objects too.
    let mut scan_lines: Vec<(ObjectId, String)> = Vec::new();
    // `fsck_object()`'s own findings, which go to stderr rather than stdout.
    let mut msg_lines: Vec<(Slot, ObjectId, String)> = Vec::new();
    // `fsck_options`' two oidsets, mapped to the first byte of the earliest
    // tree that named the blob — which is what decides whether the blob's own
    // scan slot sees the set already populated. See [`Slot`].
    let mut gitmodules_found: HashMap<ObjectId, u8> = HashMap::new();
    let mut gitattributes_found: HashMap<ObjectId, u8> = HashMap::new();
    // Objects `parse_object_buffer()` rejected. `fsck_loose()` returns before
    // setting `HAS_OBJ`, so git treats them exactly as absent from here on: no
    // `unreachable`/`dangling` line, no lost-found file, and a `missing` line if
    // something reachable names them.
    let mut unparseable: HashMap<ObjectId, Kind> = HashMap::new();
    // Objects `read_loose_object()` could not read back at all. `fsck_loose()`
    // reports each one and returns 0 — "keep checking other objects" — so a
    // damaged repository still gets a full report. Like [`unparseable`] these
    // never receive `HAS_OBJ`, and unlike it their type is unknown, so a
    // `missing` line for one carries the type expected at the reference site.
    let mut corrupt: HashSet<ObjectId> = HashSet::new();
    // `fsck_source()` announces the directory once per odb source before walking
    // it; `--connectivity-only` skips `fsck_source()` altogether.
    if opt.verbose && !opt.connectivity_only {
        eprintln!("Checking object directory");
    }
    for &id in &all {
        // `mark_object_for_connectivity()` creates the object straight from the
        // odb's file listing, before anything is read.
        if opt.connectivity_only {
            state.note(id);
        }
        // `fsck_loose()` reads the object out of the odb first of all. Every
        // failure of that read is reported and stepped over.
        let (kind, data) = match read_for_fsck(&repo, id) {
            Ok(read) => read,
            // `--connectivity-only` replaces `fsck_source()` with
            // `mark_object_for_connectivity()`, which sets `HAS_OBJ` from the
            // odb's file listing without reading a byte. Nothing is reported and
            // the object still counts as present; the reachability walk below is
            // where a corrupt one is finally noticed, and only if it is a
            // non-blob something reaches.
            Err(_) if opt.connectivity_only => continue,
            Err(lines) => {
                let slot = Slot::Scan(id.as_bytes()[0]);
                for line in lines {
                    msg_lines.push((slot, id, line));
                }
                errors |= ERROR_OBJECT;
                corrupt.insert(id);
                continue;
            }
        };
        // The read succeeded, so `fsck_loose()` reaches `parse_object_buffer()`,
        // whose `lookup_<type>()` creates the object *before* the parse — an
        // object the parse then rejects still occupies a slot, one the read
        // never produced does not.
        state.note(id);
        // `fsck_loose()` parses before it does anything else, and skips
        // `fsck_obj()` — the verbose line included — when the parse fails.
        let decoded = match parse_object_buffer(id, kind, &data, repo.object_hash().len_in_hex()) {
            Ok(d) => d,
            Err(failed) => {
                let slot = Slot::Scan(id.as_bytes()[0]);
                if let Some(line) = failed.diagnostic {
                    msg_lines.push((slot, id, line));
                }
                let path = loose_object_label(&repo, id).ok_or_else(|| {
                    anyhow::anyhow!(
                        "{id} is a packed object git cannot parse: git reports those through \
                         verify_pack()'s `object corrupt or missing` line, which this port does \
                         not reach"
                    )
                })?;
                msg_lines.push((slot, id, format!("error: {id}: object could not be parsed: {path}")));
                errors |= ERROR_OBJECT;
                unparseable.insert(id, kind);
                continue;
            }
        };
        // `fsck_obj()`'s own line, which covers blobs too.
        if opt.verbose && !opt.connectivity_only {
            eprintln!("Checking {kind} {id}");
        }
        // A commit's tree and parents, and a tag's tagged object, are created by
        // `parse_commit_buffer()`/`parse_tag_buffer()` — and both return
        // immediately once `object.parsed` is set, after which
        // `fsck_walk_commit()`/`fsck_walk_tag()` only follow the pointers that
        // first parse stored. So an `<object>` argument, which `snapshot_ref()`
        // parsed before the scan started, has its links looked up here exactly
        // never. A tree is the other way round: `parse_tree_buffer()` creates
        // nothing and `fsck_walk_tree()` decodes the entries and looks every one
        // of them up, on every run.
        let creates_children = kind == Kind::Tree || !pre_parsed.contains(&id);
        for (child, _) in &decoded.children {
            // Absent children are `note`d all the same: `fsck_walk()` creates
            // them, so they occupy an `obj_hash` slot. They are not *reported*
            // here — `check_unreachable_object()` never prints `missing`, so an
            // object that only an unreachable object names stays quiet.
            if creates_children {
                state.note(*child);
            }
            state.used.insert(*child);
        }
        // `--root` and `--tags` lines are emitted by `fsck_obj()`, which
        // `--connectivity-only` skips entirely.
        if opt.connectivity_only {
            continue;
        }

        // `fsck_obj()` runs `fsck_walk()` before `fsck_object()`, and turns a
        // walk that failed outright into `objerror(… "broken links")`.
        if let Some((line, broken)) = decoded.walk_error {
            msg_lines.push((Slot::Scan(id.as_bytes()[0]), id, line));
            if broken {
                msg_lines.push((
                    Slot::Scan(id.as_bytes()[0]),
                    id,
                    format!("error in {kind} {id}: broken links"),
                ));
                errors |= ERROR_OBJECT;
            }
        }

        // `fsck_object()` next; `fsck_obj()` skips the `root`/`tagged` line when
        // it reported an error.
        let checked = check_object(kind, &data, opt.strict, repo.object_hash().len_in_hex());
        for line in checked.raw {
            msg_lines.push((Slot::Scan(id.as_bytes()[0]), id, line));
        }
        for blob in checked.gitmodules {
            let entry = gitmodules_found.entry(blob).or_insert(u8::MAX);
            *entry = (*entry).min(id.as_bytes()[0]);
        }
        for blob in checked.gitattributes {
            let entry = gitattributes_found.entry(blob).or_insert(u8::MAX);
            *entry = (*entry).min(id.as_bytes()[0]);
        }
        let mut failed = false;
        for finding in checked.findings {
            match msg_config.severity(&finding, &id) {
                Severity::Ignore => {}
                Severity::Info | Severity::Warn => msg_lines.push((
                    Slot::Scan(id.as_bytes()[0]),
                    id,
                    format!("warning in {kind} {id}: {}: {}", finding.msg.id, finding.text),
                )),
                Severity::Error | Severity::Fatal => {
                    msg_lines.push((
                        Slot::Scan(id.as_bytes()[0]),
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

    // `fsck_loose()` gives up before `parse_object_buffer()`, so a corrupt
    // object never enters `obj_hash` on its own account — only a reference to
    // it, which `fsck_walk()` turns into a `lookup_*()` and so into a slot.
    for id in &corrupt {
        if !state.used.contains(id) {
            state.forget(id);
        }
    }

    // ---- 3a. the blob-content lint ------------------------------------------
    //
    // `fsck_blob()` for every blob a tree named `.gitmodules`/`.gitattributes`,
    // plus `fsck_finish()`'s report for one that is absent or is not a blob.
    // `--connectivity-only` skips both, since it skips `fsck_source()` and
    // `fsck_finish()` alike.
    if !opt.connectivity_only {
        errors |= lint_special_blobs(
            &repo,
            &msg_config,
            &gitmodules_found,
            &gitattributes_found,
            &mut msg_lines,
        );
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
            anyhow::bail!(
                "refusing to guess the output order: git emits these {} lines during its \
                 object-directory scan, and two of them share the raw readdir() sequence of one \
                 .git/objects/?? subdirectory",
                scan_lines.len()
            );
        }
        scan_lines.sort_by_key(|(id, _)| id.as_bytes()[0]);
    }

    // The message layer's lines come from the same scan plus `fsck_finish()`,
    // so they are ordered — and refused when ambiguous — by the same rule, with
    // the two finish sweeps landing after every scan line. See [`Slot`].
    if msg_lines.len() > 1 {
        let mut by_slot: HashMap<Slot, HashSet<ObjectId>> = HashMap::new();
        for &(slot, id, _) in &msg_lines {
            by_slot.entry(slot).or_default().insert(id);
        }
        // Within one `.git/objects/??` subdirectory the walk is raw `readdir()`
        // order; within one `fsck_finish()` sweep it is git's khash order.
        // Either way, two distinct ids in the same slot are unorderable.
        let collides = by_slot.values().any(|ids| ids.len() > 1);
        if collides || has_packs(&repo) {
            anyhow::bail!(
                "refusing to guess the output order: git emits these {} object-content messages \
                 during its object-directory scan, and two of them share the raw readdir() \
                 sequence of one .git/objects/?? subdirectory",
                msg_lines.len()
            );
        }
        msg_lines.sort_by_key(|&(slot, _, _)| slot);
    }
    for (_, _, line) in &msg_lines {
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

    // ---- 3c. `process_refs()`: the snapshot, handled ------------------------
    //
    // `cmd_fsck` takes the head snapshot *before* the object scan and only acts
    // on it here, and `fsck_handle_ref()` opens with another `parse_object()`.
    // For an id the scan has already created that is a plain `lookup_object()`,
    // which moves it back to its home slot — so the call has to be replayed even
    // though it changes nothing about the head set.
    //
    // Only the explicit-`<object>` snapshot is replayed: the ref/reflog snapshot
    // is taken below, at the point git *handles* it rather than the point git
    // takes it, and `creation_modeled` is off for that case.
    if explicit_heads {
        for id in heads.clone() {
            state.note(id);
        }
    }

    // ---- 4. the head set ----------------------------------------------------
    if !explicit_heads {
        default_refs += collect_default_heads(&repo, &mut state, &mut heads, &mut errors)?;
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
    // Each entry carries the type the reference site expected — git's
    // `lookup_tree()`/`lookup_blob()` in the parent's `fsck_walk()` — which is
    // what decides whether an unreadable object is even opened here. A head has
    // no such site.
    let mut queue: Vec<(ObjectId, Option<Kind>)> = Vec::new();
    for id in heads {
        if state.reachable.insert(id) {
            queue.push((id, None));
        }
    }
    while let Some((id, expected)) = queue.pop() {
        let kind = match repo.find_header(id) {
            Ok(h) => h.kind(),
            // The odb cannot read it. Under a full scan the object is in
            // `corrupt` and was never queued, so this is `--connectivity-only`,
            // where nothing has read it yet. `fsck_walk_blob()` reads nothing,
            // so a blob stays quiet; anything else is `parse_tree()` /
            // `parse_commit()` failing, which is where git gives up. Its own
            // diagnostic ahead of this line names the specific odb failure and
            // is not reproduced.
            Err(_) => match expected {
                Some(Kind::Blob) | None => continue,
                Some(_) => {
                    let path = loose_object_label(&repo, id)
                        .map(|p| format!(" (stored in {p})"))
                        .unwrap_or_default();
                    eprintln!("fatal: loose object {id}{path} is corrupt");
                    return Ok(ExitCode::from(128));
                }
            },
        };
        if kind == Kind::Blob {
            continue;
        }
        // An object `parse_object_buffer()` rejected has no links to follow:
        // `fsck_walk_commit()`/`fsck_walk_tag()` read fields the failed parse
        // never filled in. The scan above already reported it.
        let decoded = match decode(&repo, id) {
            Ok(Ok(d)) => d,
            // Rejected by `parse_object_buffer()`; the scan above reported it.
            Ok(Err(_)) => continue,
            // Readable a moment ago, unreadable now: treat it as git's
            // `parse_object()` failure, as above.
            Err(_) => {
                let path = loose_object_label(&repo, id)
                    .map(|p| format!(" (stored in {p})"))
                    .unwrap_or_default();
                eprintln!("fatal: loose object {id}{path} is corrupt");
                return Ok(ExitCode::from(128));
            }
        };
        for (child, child_kind) in decoded.children {
            // `fsck_walk_tree()` resolves every entry through `lookup_tree()` /
            // `lookup_blob()`, and a lookup moves a displaced object back to its
            // home slot, so the call has to be replayed here as well as in the
            // scan. `fsck_walk_commit()` and `fsck_walk_tag()` follow pointers
            // `parse_object()` already stored and look nothing up, which is why
            // only a tree's children are recorded.
            if kind == Kind::Tree {
                state.note(child);
            }
            // A corrupt object never got `HAS_OBJ`, so `check_reachable_object()`
            // prints `missing` for it with the type the reference site expected.
            if corrupt.contains(&child) || !repo.has_object(child) {
                state.missing.insert(child, child_kind);
                continue;
            }
            if state.reachable.insert(child) {
                queue.push((child, Some(child_kind)));
            }
        }
    }

    // ---- 7. the connectivity report ----------------------------------------
    //
    // `check_reachable_object()` prints `missing` for anything reachable that
    // lacks `HAS_OBJ`, which an object the scan failed to parse does.
    for (&id, &kind) in &unparseable {
        if state.reachable.contains(&id) {
            state.missing.insert(id, kind);
        }
    }
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
            // `check_unreachable_object()` returns immediately without
            // `HAS_OBJ`, so an object the scan could not parse is silent here.
            if state.reachable.contains(&id)
                || unparseable.contains_key(&id)
                || corrupt.contains(&id)
            {
                continue;
            }
            // `check_unreachable_object()`: a shown-unreachable object returns
            // before the dangling/lost-found block, so `--unreachable` never
            // writes lost-found.
            // Under `--connectivity-only` nothing has read this object, so it
            // can still turn out to be unreadable here. git's `printable_type()`
            // answers `unknown` for one and prints the line anyway; this port
            // has no `unknown` object kind, so it stays silent instead.
            let Ok(header) = repo.find_header(id) else { continue };
            if opt.show_unreachable {
                let kind = header.kind();
                lines.push((id, format!("unreachable {kind} {id}")));
            } else if !state.used.contains(&id) {
                // `!USED` — the tip of an unreachable set. `dangling` printing and
                // lost-found writing are independent: `--no-dangling --lost-found`
                // still writes the files.
                let kind = header.kind();
                if opt.show_dangling {
                    lines.push((id, format!("dangling {kind} {id}")));
                }
                if opt.write_lost_and_found {
                    write_lost_found(&repo, id, kind)?;
                }
            }
        }
    }

    let order = SlotOrder::new(
        &state.known,
        creation_modeled.then_some(state.ops.as_slice()),
    );
    let reported: Vec<ObjectId> = lines.iter().map(|(id, _)| *id).collect();
    if order.is_ambiguous_for(&reported) {
        anyhow::bail!(
            "refusing to guess the output order: git emits these {} lines in obj_hash slot order, \
             and two of them share a collision cluster whose order depends on git's internal \
             object-creation sequence, which this port does not model",
            lines.len()
        );
    }
    lines.sort_by_key(|(id, _)| order.slot_of(id));

    if opt.verbose {
        // `check_connectivity()` announces `get_max_object_index()`, which is the
        // size of `obj_hash` rather than the number of objects in it, then walks
        // every occupied slot in order.
        eprintln!(
            "Checking connectivity ({} objects)",
            obj_hash_size(state.known.len())
        );
        let mut walked: Vec<ObjectId> = state.known.iter().copied().collect();
        walked.sort_by_key(|id| (order.slot_of(id), *id));
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
            // `--help-all` is a `strcmp()` of its own in `parse_options_step()`,
            // ahead of `parse_long_opt()`: it never abbreviates and never takes
            // an `=<value>`. It renders `USAGE_FULL`, the same block as `-h`
            // here, since no entry of this table is `PARSE_OPT_HIDDEN`.
            if s == "-h" || s == "--help" || s == "--help-all" {
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
    /// Every `lookup_<type>()` git would call, in call order.
    ///
    /// The creation sequence alone does not decide `obj_hash`: `lookup_object()`
    /// moves a hit back to the slot the probe started at, so a *lookup* of an
    /// already-created object reorders the table too. Both kinds of call go
    /// through the same `lookup_<type>()` front door, so one entry per call is
    /// enough to replay the table exactly — see [`replay_obj_hash`].
    ops: Vec<ObjectId>,
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
    /// Record one `lookup_<type>(id)` — which creates `id` when it is new and
    /// otherwise finds it. Returns whether it is new.
    fn note(&mut self, id: ObjectId) -> bool {
        self.ops.push(id);
        self.known.insert(id)
    }

    /// Undo every [`note`](Self::note) of `id` — for an id git turns out never
    /// to have looked up at all. Keeps `ops` in step with `known`, which the
    /// slot replay depends on.
    fn forget(&mut self, id: &ObjectId) {
        if self.known.remove(id) {
            self.ops.retain(|c| c != id);
        }
    }
}

/// Where in `git fsck`'s stderr a message-layer line lands.
///
/// `cmd_fsck` runs `fsck_source()` over every odb source and only then
/// `fsck_finish()`, so every finish line follows every scan line. Within the
/// scan, `for_each_loose_file_in_source()` walks the 256 subdirectories in
/// numeric order, which is the first byte of the id. Within `fsck_finish()` the
/// `.gitmodules` sweep runs before the `.gitattributes` one.
///
/// A blob a tree named `.gitmodules` is linted twice over in git — once by
/// `fsck_blob()` if the scan reaches the blob after the naming tree, once by
/// `fsck_finish()` otherwise — and `gitmodules_done` keeps it to one. Which of
/// the two happens is decided by comparing the two first bytes, so the slot is
/// derivable; a blob and its naming tree in the *same* subdirectory is the
/// unorderable case the caller refuses.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
enum Slot {
    /// `fsck_source()`, keyed by the `.git/objects/??` subdirectory.
    Scan(u8),
    /// `fsck_finish()`'s `gitmodules_found` sweep.
    FinishGitmodules,
    /// `fsck_finish()`'s `gitattributes_found` sweep.
    FinishGitattributes,
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
    /// The message `fsck_walk_tree()`'s tree-entry decoder printed, and whether
    /// it was the `init_tree_desc_gently()` failure that makes `fsck_walk()`
    /// return `-1` — which is what `fsck_obj()` turns into `broken links`.
    walk_error: Option<(String, bool)>,
}

/// Why `parse_object_buffer()` returned NULL. git prints its own diagnostic
/// first for some failures and stays quiet for the rest, then `fsck_loose()`
/// adds the `object could not be parsed` line either way.
struct ParseFailed {
    /// The `error:` line `parse_commit_buffer()`/`parse_tag_buffer()` printed
    /// before giving up, if it printed one.
    diagnostic: Option<String>,
}

/// `object.c::parse_object_buffer` for the four types, reduced to what
/// `builtin/fsck.c` uses of the result: the links `fsck_walk()` follows and the
/// two fields `--root`/`--tags` print. Blobs and trees always parse — a tree's
/// entries are only decoded later, by the walk — so only a commit or a tag can
/// fail here.
///
/// Gitlink tree entries are skipped: they name commits of a different
/// repository, which is also what git's `fsck_walk_tree()` does.
fn decode(repo: &gix::Repository, id: ObjectId) -> Result<Result<Decoded, ParseFailed>> {
    let object = repo.find_object(id)?;
    Ok(parse_object_buffer(
        id,
        object.kind,
        &object.data,
        repo.object_hash().len_in_hex(),
    ))
}

/// The buffer half of [`decode`], so `receive-pack` can reuse it.
///
/// `hexsz` is git's `the_hash_algo->hexsz` — 40 in a sha1 repository, 64 in a
/// sha256 one. Every header offset below is expressed in terms of it, exactly as
/// `commit.c`/`tag.c` write them, so the same code parses either format.
fn parse_object_buffer(
    id: ObjectId,
    kind: Kind,
    data: &[u8],
    hexsz: usize,
) -> Result<Decoded, ParseFailed> {
    let mut children = Vec::new();
    let mut is_root_commit = false;
    let mut tag = None;
    let mut walk_error = None;
    match kind {
        Kind::Commit => {
            let (tree, parents) = parse_commit_buffer(id, data, hexsz)?;
            children.push((tree, Kind::Tree));
            is_root_commit = parents.is_empty();
            children.extend(parents.into_iter().map(|p| (p, Kind::Commit)));
        }
        Kind::Tree => {
            // `parse_tree_buffer()` only hands the buffer to the tree object;
            // a malformed entry is not noticed until `fsck_walk_tree()` decodes
            // it, which is what the rest of this arm is.
            let (entries, stop) = tree_entries(data, hexsz);
            for entry in &entries {
                // `fsck_walk_tree()` canonicalizes the mode (its `tree_desc`
                // has no `TREE_DESC_RAW_MODES`) and skips gitlinks.
                let kind = match entry.mode & 0o170000 {
                    0o040000 => Kind::Tree,
                    0o120000 | 0o100000 => Kind::Blob,
                    _ => continue,
                };
                children.push((ObjectId::from_bytes_or_panic(entry.oid), kind));
            }
            walk_error = match stop {
                TreeStop::End => None,
                TreeStop::AtInit(msg) => {
                    // `init_tree_desc_gently()` failed, so not one entry was
                    // walked and `fsck_walk()` reports the whole tree broken.
                    children.clear();
                    Some((format!("error: {msg}"), true))
                }
                TreeStop::AtUpdate(msg) => {
                    // `tree_entry_gently()` reports end-of-tree, so the entry
                    // the advance failed on is never handed to the walker.
                    children.pop();
                    Some((format!("error: {msg}"), false))
                }
            };
        }
        Kind::Tag => {
            let (target, target_kind, name) = parse_tag_buffer(id, data, hexsz)?;
            children.push((target, target_kind));
            tag = Some((target_kind, target, name));
        }
        Kind::Blob => {}
    }
    Ok(Decoded {
        children,
        is_root_commit,
        tag,
        walk_error,
    })
}

/// The length of a `tree <hex>` line without its newline
/// (`the_hash_algo->hexsz + 5`), for a repository whose hex digest is `hexsz`
/// characters wide.
fn tree_entry_len(hexsz: usize) -> usize {
    hexsz + 5
}
/// The `parent <hex>` counterpart of [`tree_entry_len`]
/// (`the_hash_algo->hexsz + 7`).
fn parent_entry_len(hexsz: usize) -> usize {
    hexsz + 7
}

/// `commit.c::parse_commit_buffer`, which reads only the `tree` line and the
/// `parent` lines and leaves every other header to the message layer. Its two
/// `lookup_*` failures are not modelled: they need an id already known to this
/// process under a conflicting type.
fn parse_commit_buffer(
    id: ObjectId,
    data: &[u8],
    hexsz: usize,
) -> Result<(ObjectId, Vec<ObjectId>), ParseFailed> {
    let tree_entry_len = tree_entry_len(hexsz);
    let parent_entry_len = parent_entry_len(hexsz);
    let bogus = || ParseFailed { diagnostic: Some(format!("error: bogus commit object {id}")) };
    if data.len() <= tree_entry_len + 1
        || !data.starts_with(b"tree ")
        || data[tree_entry_len] != b'\n'
    {
        return Err(bogus());
    }
    let tree = ObjectId::from_hex(&data[5..tree_entry_len]).map_err(|_| ParseFailed {
        diagnostic: Some(format!("error: bad tree pointer in commit {id}")),
    })?;

    let mut at = tree_entry_len + 1;
    let mut parents = Vec::new();
    while at + parent_entry_len < data.len() && data[at..].starts_with(b"parent ") {
        let bad = || ParseFailed { diagnostic: Some(format!("error: bad parents in commit {id}")) };
        if data.len() <= at + parent_entry_len + 1 || data[at + parent_entry_len] != b'\n' {
            return Err(bad());
        }
        parents.push(ObjectId::from_hex(&data[at + 7..at + parent_entry_len]).map_err(|_| bad())?);
        at += parent_entry_len + 1;
    }
    Ok((tree, parents))
}

/// `tag.c::parse_tag_buffer`, which reads only the `object`, `type` and `tag`
/// headers. Every failure but the unknown type is silent — it returns `-1`
/// without printing.
fn parse_tag_buffer(
    id: ObjectId,
    data: &[u8],
    hexsz: usize,
) -> Result<(ObjectId, Kind, String), ParseFailed> {
    let silent = || ParseFailed { diagnostic: None };
    // `hexsz + 24`: the shortest buffer that could hold all three headers.
    if data.len() < hexsz + 24 || !data.starts_with(b"object ") {
        return Err(silent());
    }
    // `object ` is 7 bytes, so the hex digest ends at `7 + hexsz` and the `type`
    // header starts one newline later.
    let object_end = 7 + hexsz;
    let target = ObjectId::from_hex(&data[7..object_end]).map_err(|_| silent())?;
    if data[object_end] != b'\n' || !data[object_end + 1..].starts_with(b"type ") {
        return Err(silent());
    }
    let after_type = &data[object_end + 6..];
    let nl = after_type.iter().position(|&b| b == b'\n').ok_or_else(silent)?;
    // `char type[20]`, so a type name of 19 characters is the longest that fits.
    if nl >= 20 {
        return Err(silent());
    }
    let target_kind = Kind::from_bytes(&after_type[..nl]).map_err(|_| ParseFailed {
        diagnostic: Some(format!(
            "error: unknown tag type '{}' in {id}",
            String::from_utf8_lossy(&after_type[..nl])
        )),
    })?;

    let rest = &after_type[nl + 1..];
    // `bufptr + 4 < tail`: a `tag ` header needs at least one more byte.
    if rest.len() <= 4 || !rest.starts_with(b"tag ") {
        return Err(silent());
    }
    let name = &rest[4..];
    let nl = name.iter().position(|&b| b == b'\n').ok_or_else(silent)?;
    Ok((target, target_kind, String::from_utf8_lossy(&name[..nl]).into_owned()))
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
    errors: &mut u8,
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
        let name = reference.name().as_bstr().to_string();
        let id = match direct {
            Some(id) => id,
            None => match reference.into_fully_peeled_id() {
                Ok(id) => id.detach(),
                // `snapshot_ref()` is handed the null id for a ref the store
                // cannot resolve — a symbolic ref whose target is gone — and
                // reports it against the *ref*, sets `ERROR_REACHABLE`, and
                // carries on with the rest of the repository.
                Err(_) => {
                    eprintln!("error: {name}: invalid sha1 pointer {}", ObjectId::null(repo.object_hash()));
                    *errors |= ERROR_REACHABLE;
                    continue;
                }
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
///
/// Unsorted and pre-order, which is what git's `dir_iterator_begin(path, 0)`
/// hands `for_each_reflog()`. Shared with [`super::shortlog`], whose `--reflog`
/// walks the same directory through the same iterator.
pub(super) fn collect_log_names(dir: &Path, prefix: &str, out: &mut Vec<String>) -> Result<()> {
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

/// The loose object file backing `id`, in the odb source that holds it. `None`
/// when the object is not loose — git reaches those through `verify_pack()`,
/// which reports differently.
fn loose_object_path(repo: &gix::Repository, id: ObjectId) -> Option<PathBuf> {
    let hex = id.to_hex().to_string();
    odb_sources(repo)
        .into_iter()
        .map(|objdir| objdir.join(&hex[..2]).join(&hex[2..]))
        .find(|p| p.is_file())
}

/// The path `fsck_loose()` names in its diagnostics, as [`loose_object_path`]
/// spells it.
///
/// git chdirs to the top of the worktree during setup, so its object directory
/// is `.git/objects` for an ordinary repository; the workdir prefix is stripped
/// here to reproduce that spelling from an absolute path.
fn loose_object_label(repo: &gix::Repository, id: ObjectId) -> Option<String> {
    let full = loose_object_path(repo, id)?;
    Some(loose_label_of(repo, &full))
}

/// See [`loose_object_label`].
fn loose_label_of(repo: &gix::Repository, full: &Path) -> String {
    let rela = repo
        .workdir()
        .and_then(|work| full.strip_prefix(work).ok())
        .unwrap_or(full);
    rela.display().to_string()
}

/// `object-file.h`'s `MAX_HEADER_LEN` — the `<type> <size>\0` buffer
/// `unpack_loose_header()` inflates into.
const MAX_HEADER_LEN: usize = 32;

/// `object-file.c`'s `enum unpack_loose_header_result`.
enum LooseHeader {
    /// The whole `<type> <size>\0` header landed in the buffer.
    Ok,
    /// zlib refused the stream.
    Bad,
    /// The header is longer than [`MAX_HEADER_LEN`].
    TooLong,
}

/// `git-zlib.c::zerr_to_string` over the error codes `gix-zlib` distinguishes.
fn zerr_to_string(e: &gix::zlib::DecompressError) -> &'static str {
    use gix::zlib::DecompressError as E;
    match e {
        E::InsufficientMemory => "out of memory",
        E::NeedDict => "needs dictionary",
        E::DataError => "data stream error",
        E::StreamError => "stream consistency error",
    }
}

/// `git-zlib.c::git_inflate`'s diagnostic for a status below `Z_OK`.
fn inflate_error_line(z: &gix::zlib::Decompress, e: &gix::zlib::DecompressError) -> String {
    format!(
        "error: inflate: {} ({})",
        zerr_to_string(e),
        z.error_message().unwrap_or("no message")
    )
}

/// `object-file.c::unpack_loose_header`: one `git_inflate()` of the mapped file
/// into a [`MAX_HEADER_LEN`] buffer, which either contains the terminating NUL
/// or does not. Appends `git_inflate()`'s own line to `diag` when zlib errors.
fn unpack_loose_header(
    z: &mut gix::zlib::Decompress,
    map: &[u8],
    hdr: &mut [u8; MAX_HEADER_LEN],
    diag: &mut Vec<String>,
) -> LooseHeader {
    let status = match z.decompress(map, hdr, gix::zlib::FlushDecompress::None) {
        Ok(s) => s,
        Err(e) => {
            diag.push(inflate_error_line(z, &e));
            return LooseHeader::Bad;
        }
    };
    // `if (status != Z_OK && status != Z_STREAM_END) return ULHR_BAD;`
    if matches!(status, gix::zlib::Status::BufError) {
        return LooseHeader::Bad;
    }
    let produced = z.total_out() as usize;
    if hdr[..produced].contains(&0) {
        LooseHeader::Ok
    } else {
        LooseHeader::TooLong
    }
}

/// `object-file.c::parse_loose_header`: `<type> <size>\0` by hand, refusing a
/// non-canonical decimal size. `None` is the function's `-1`; the type is `None`
/// when the format is valid but the type name is not (`OBJ_BAD`).
fn parse_loose_header(hdr: &[u8]) -> Option<(Option<Kind>, usize)> {
    let space = hdr.iter().position(|&c| c == b' ' || c == 0)?;
    if hdr[space] != b' ' {
        return None;
    }
    let kind = Kind::from_bytes(&hdr[..space]).ok();

    let rest = &hdr[space + 1..];
    let mut at = 0usize;
    let mut size = usize::from(*rest.first()? & 0xff);
    size = size.checked_sub(usize::from(b'0'))?;
    if size > 9 {
        return None;
    }
    at += 1;
    if size != 0 {
        while let Some(&c) = rest.get(at) {
            if !c.is_ascii_digit() {
                break;
            }
            at += 1;
            size = size.checked_mul(10)?.checked_add(usize::from(c - b'0'))?;
        }
    }
    // "The length must be followed by a zero byte".
    if *rest.get(at)? != 0 {
        return None;
    }
    Some((kind, size))
}

/// `object-file.c::unpack_loose_rest`: the body bytes already sitting in the
/// header buffer, plus however much more zlib yields, checked for a clean
/// `Z_STREAM_END` and for no trailing input.
fn unpack_loose_rest(
    z: &mut gix::zlib::Decompress,
    map: &[u8],
    hdr: &[u8; MAX_HEADER_LEN],
    size: usize,
    id: ObjectId,
    diag: &mut Vec<String>,
) -> Option<Vec<u8>> {
    let header_len = hdr.iter().position(|&c| c == 0)? + 1;
    let mut buf = vec![0u8; size];
    let mut bytes = (z.total_out() as usize - header_len).min(size);
    buf[..bytes].copy_from_slice(&hdr[header_len..header_len + bytes]);

    let mut status = gix::zlib::Status::Ok;
    while matches!(status, gix::zlib::Status::Ok) {
        let (before_in, before_out) = (z.total_in() as usize, z.total_out() as usize);
        match z.decompress(
            &map[before_in..],
            &mut buf[bytes..],
            gix::zlib::FlushDecompress::Finish,
        ) {
            Ok(s) => status = s,
            Err(e) => {
                diag.push(inflate_error_line(z, &e));
                break;
            }
        }
        bytes += z.total_out() as usize - before_out;
        // zlib cannot make progress on an exhausted stream; C's `avail_in`/
        // `avail_out` bookkeeping ends the loop through `Z_BUF_ERROR` instead.
        if z.total_in() as usize == before_in && z.total_out() as usize == before_out {
            break;
        }
    }

    if !matches!(status, gix::zlib::Status::StreamEnd) {
        diag.push(format!("error: corrupt loose object '{id}'"));
        return None;
    }
    if (z.total_in() as usize) < map.len() {
        diag.push(format!("error: garbage at end of loose object '{id}'"));
        return None;
    }
    Some(buf)
}

/// `object-file.c::read_loose_object` as `fsck_loose()` uses it, followed by
/// `fsck_loose()`'s own two messages. `Ok` is the object's type and contents;
/// `Err` is every `error:` line git would have printed, in order, so the caller
/// can place them in the object-directory scan's output slot instead of
/// printing them where they were produced.
///
/// The one branch not ported is the streaming one: git checks a blob larger
/// than `core.bigFileThreshold` with `check_stream_oid()` rather than holding
/// it in memory, which only changes which of `corrupt loose object` /
/// `garbage at end of loose object` / `hash mismatch for <path>` it prints
/// ahead of the identical `object corrupt or missing` line below.
fn read_loose_object(path: &Path, label: &str, id: ObjectId) -> Result<(Kind, Vec<u8>), Vec<String>> {
    let mut diag: Vec<String> = Vec::new();
    // `fsck_loose()` reports `object corrupt or missing` for every failure that
    // did not produce contents whose hash disagrees with the file's name.
    let corrupt_or_missing = || format!("error: {id}: object corrupt or missing: {label}");

    let map = match std::fs::read(path) {
        Ok(m) => m,
        Err(e) => {
            diag.push(format!("error: unable to mmap {label}: {}", strerror(&e)));
            diag.push(corrupt_or_missing());
            return Err(diag);
        }
    };
    // `map_fd()`: "mmap() is forbidden on empty files", so an empty object file
    // is reported and mapped to NULL, which `read_loose_object()` then reports
    // again through `error_errno()`. git's second line carries whatever errno a
    // previous syscall happened to leave behind — nothing set it here — so only
    // the message is reproduced, not that stale suffix.
    if map.is_empty() {
        diag.push(format!("error: object file {label} is empty"));
        diag.push(format!("error: unable to mmap {label}"));
        diag.push(corrupt_or_missing());
        return Err(diag);
    }

    let mut z = gix::zlib::Decompress::new();
    let mut hdr = [0u8; MAX_HEADER_LEN];
    match unpack_loose_header(&mut z, &map, &mut hdr, &mut diag) {
        LooseHeader::Ok => {}
        LooseHeader::Bad | LooseHeader::TooLong => {
            diag.push(format!("error: unable to unpack header of {label}"));
            diag.push(corrupt_or_missing());
            return Err(diag);
        }
    }

    let Some((kind, size)) = parse_loose_header(&hdr) else {
        diag.push(format!("error: unable to parse header of {label}"));
        diag.push(corrupt_or_missing());
        return Err(diag);
    };
    let Some(kind) = kind else {
        let name = String::from_utf8_lossy(&hdr[..hdr.iter().position(|&c| c == 0).unwrap_or(0)])
            .into_owned();
        diag.push(format!("error: unable to parse type from header '{name}' of {label}"));
        diag.push(corrupt_or_missing());
        return Err(diag);
    };

    let Some(contents) = unpack_loose_rest(&mut z, &map, &hdr, size, id, &mut diag) else {
        diag.push(format!("error: unable to unpack contents of {label}"));
        diag.push(corrupt_or_missing());
        return Err(diag);
    };

    // `read_loose_object()` leaves the hash comparison to its caller, which is
    // the one failure that gets `fsck_loose()`'s other message.
    match gix::objs::compute_hash(id.kind(), kind, &contents) {
        Ok(real) if real == id => Ok((kind, contents)),
        Ok(real) => Err(vec![format!("error: {real}: hash-path mismatch, found at: {label}")]),
        Err(_) => Err(vec![corrupt_or_missing()]),
    }
}

/// `strerror(errno)`, which is what git's `error_errno()` appends.
fn strerror(e: &std::io::Error) -> String {
    match e.raw_os_error() {
        Some(code) => unsafe { std::ffi::CStr::from_ptr(libc::strerror(code)) }
            .to_string_lossy()
            .into_owned(),
        None => e.to_string(),
    }
}

/// The object as the object-directory scan reads it: `fsck_loose()` for a loose
/// object, `verify_pack()`'s `fsck_obj_buffer()` for a packed one. `Err` is the
/// `error:` lines git prints before setting `ERROR_OBJECT` and moving on to the
/// next object — neither path aborts the walk, which is the whole point of
/// `git fsck` on a damaged repository.
fn read_for_fsck(repo: &gix::Repository, id: ObjectId) -> Result<(Kind, Vec<u8>), Vec<String>> {
    if let Some(path) = loose_object_path(repo, id) {
        let label = loose_label_of(repo, &path);
        return read_loose_object(&path, &label, id);
    }
    // A packed object git reads through `verify_packfile()`, whose per-object
    // failures surface as `fsck_obj_buffer()`'s pathless line.
    match repo.find_object(id) {
        Ok(o) => Ok((o.kind, o.data.clone())),
        Err(_) => Err(vec![format!("error: {id}: object corrupt or missing")]),
    }
}

/// `fsck.c::fsck_blob` for every blob some tree named `.gitmodules` or
/// `.gitattributes`, plus `fsck_finish()`'s two `fsck_blobs()` sweeps for the
/// ones that are absent or are not blobs at all. Appends every reported line to
/// `msg_lines` in the slot git would print it from, and returns the error bits.
fn lint_special_blobs(
    repo: &gix::Repository,
    msg_config: &MsgConfig,
    gitmodules_found: &HashMap<ObjectId, u8>,
    gitattributes_found: &HashMap<ObjectId, u8>,
    msg_lines: &mut Vec<(Slot, ObjectId, String)>,
) -> u8 {
    let mut errors = 0u8;
    // `read_loose_object()` is the only reader on this path that streams past
    // `core.bigFileThreshold`; a packed object reaches `fsck_obj_buffer()` with a
    // real buffer no matter how big it is. Confirmed against git 2.55.0: the same
    // 501-byte `.gitmodules` at `core.bigFileThreshold=100` reports
    // `gitmodulesLarge` while loose and reports nothing once `git repack -ad` has
    // packed it.
    let threshold = big_file_threshold(repo);
    let mut candidates: Vec<ObjectId> =
        gitmodules_found.keys().chain(gitattributes_found.keys()).copied().collect();
    candidates.sort();
    candidates.dedup();

    for id in candidates {
        let as_modules = gitmodules_found.contains_key(&id);
        let as_attrs = gitattributes_found.contains_key(&id);
        // The sweep that would reach this id first, for the ids `fsck_blobs()`
        // reports itself.
        let finish_slot = if as_modules { Slot::FinishGitmodules } else { Slot::FinishGitattributes };

        let (kind, data) = match repo.find_object(id) {
            Ok(object) => (object.kind, object.data.clone()),
            Err(_) => {
                // `fsck_blobs()`: unreadable, and reported once per sweep.
                for (present, slot, msg, label) in [
                    (as_modules, Slot::FinishGitmodules, &GITMODULES_MISSING, ".gitmodules"),
                    (as_attrs, Slot::FinishGitattributes, &GITATTRIBUTES_MISSING, ".gitattributes"),
                ] {
                    if !present {
                        continue;
                    }
                    let finding = Finding {
                        msg,
                        text: format!("unable to read {label} blob"),
                    };
                    errors |= emit_blob_finding(msg_config, &finding, id, Kind::Blob, slot, msg_lines);
                }
                continue;
            }
        };
        if kind != Kind::Blob {
            for (present, slot, msg, label) in [
                (as_modules, Slot::FinishGitmodules, &GITMODULES_BLOB, ".gitmodules"),
                (as_attrs, Slot::FinishGitattributes, &GITATTRIBUTES_BLOB, ".gitattributes"),
            ] {
                if !present {
                    continue;
                }
                let finding = Finding {
                    msg,
                    text: format!("non-blob found at {label}"),
                };
                errors |= emit_blob_finding(msg_config, &finding, id, kind, slot, msg_lines);
            }
            continue;
        }

        // Which slot `fsck_blob()` itself runs in: the blob's own scan slot when
        // the naming tree came first, `fsck_finish()`'s otherwise.
        let earliest_tree = [
            as_modules.then(|| gitmodules_found[&id]),
            as_attrs.then(|| gitattributes_found[&id]),
        ]
        .into_iter()
        .flatten()
        .min()
        .unwrap_or(u8::MAX);
        let slot = match earliest_tree.cmp(&id.as_bytes()[0]) {
            std::cmp::Ordering::Less => Slot::Scan(id.as_bytes()[0]),
            _ => finish_slot,
        };
        // Only the *scan* slot can see a streamed blob, and only for a loose one.
        // `fsck_loose()` is where `read_loose_object()` hands `fsck_object()` a
        // null buffer for a blob over `core.bigFileThreshold`
        // (`object-file.c:1645`); `fsck_finish()`'s sweep reaches the same blob
        // through `fsck_blobs()`, which always calls `odb_read_object()`
        // (`fsck.c:1337`) and so always has the whole thing. A packed object
        // never streams here either — `verify_packfile()` decodes it in full.
        //
        // So `gitmodulesLarge` is scan-order dependent in git itself, and
        // observably so. Confirmed against git 2.55.0 at
        // `core.bigFileThreshold=100` with a 501-byte `.gitmodules`: with the
        // naming tree in fanout `94` and the blob in `b6` the id is reported,
        // and with the blob in `b5` and the tree in `d2` — the blob scanned
        // first, so linted by `fsck_finish()` — it is not.
        let streamed = matches!(slot, Slot::Scan(_)) && is_loose(repo, id);
        let buffer = if streamed { blob_buffer(&data, threshold) } else { Some(&data[..]) };
        for finding in check_blob(buffer, as_modules, as_attrs) {
            errors |= emit_blob_finding(msg_config, &finding, id, Kind::Blob, slot, msg_lines);
        }
    }
    errors
}

/// Render one blob finding at its resolved severity, the way `fsck_obj()`'s
/// error callback does. `fsck_finish()`'s failures are always `ERROR_OBJECT`,
/// and so are `fsck_blob()`'s here: a `.gitmodules` blob git found through a
/// pack is still reported by `fsck_finish()`, which is outside `verify_pack()`.
fn emit_blob_finding(
    msg_config: &MsgConfig,
    finding: &Finding,
    id: ObjectId,
    kind: Kind,
    slot: Slot,
    msg_lines: &mut Vec<(Slot, ObjectId, String)>,
) -> u8 {
    match msg_config.severity(finding, &id) {
        Severity::Ignore => 0,
        Severity::Info | Severity::Warn => {
            msg_lines.push((
                slot,
                id,
                format!("warning in {kind} {id}: {}: {}", finding.msg.id, finding.text),
            ));
            0
        }
        Severity::Error | Severity::Fatal => {
            msg_lines.push((
                slot,
                id,
                format!("error in {kind} {id}: {}: {}", finding.msg.id, finding.text),
            ));
            ERROR_OBJECT
        }
    }
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
///
/// Two reconstructions live here. The exact one replays git's own
/// `lookup_<type>()` call sequence — creations, growth, rehashing and
/// `lookup_object()`'s move-to-front alike — and so knows the slot every object
/// actually landed in. It is only available when that sequence is reproducible
/// (see `creation_modeled` in [`fsck`]); without it the weaker home-slot
/// argument applies, which orders every object whose cluster holds no repeated
/// home slot and refuses the rest.
struct SlotOrder {
    /// The slot each object landed in, from the replay above. `None` when the
    /// call sequence was not reproducible.
    placed: Option<HashMap<ObjectId, usize>>,
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
    /// `ops` is git's `lookup_<type>()` call sequence when this port can
    /// reproduce it, and `None` when it cannot.
    fn new(known: &HashSet<ObjectId>, ops: Option<&[ObjectId]>) -> Self {
        let size = obj_hash_size(known.len());
        // A log that does not account for every object would place the rest
        // wrongly rather than not at all, so take the replay only when the table
        // it produces is exactly `known`.
        let placed = ops
            .map(replay_obj_hash)
            .filter(|table| table.len() == known.len() && known.iter().all(|id| table.contains_key(id)));
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
            placed,
            home,
            cluster,
            ambiguous,
            wrapped,
        }
    }

    /// Where `id` sits in `obj_hash`, which is the order `check_connectivity()`
    /// walks the table in and so the order the report is printed in.
    fn slot_of(&self, id: &ObjectId) -> usize {
        match &self.placed {
            Some(placed) => placed[id],
            None => self.home[id],
        }
    }

    /// Whether the relative order of `reported` could differ from home-slot
    /// order. Two objects can only swap if they share a cluster, and only if
    /// that cluster has a repeated home slot for insertion order to exploit.
    fn is_ambiguous_for(&self, reported: &[ObjectId]) -> bool {
        // The replay knows every slot outright, so nothing is left to guess.
        if self.placed.is_some() {
            return false;
        }
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

/// `object.c`'s `obj_hash` over `ops`, yielding the slot each object ends up in.
///
/// Every `lookup_<type>()` goes through `lookup_object()` first and only calls
/// `create_object()` when that misses, so one pass over the call log reproduces
/// both halves:
///
/// * `lookup_object()` probes from the home slot to the first empty one and, on
///   a hit away from home, **swaps the hit back to the home slot**. That is not
///   a cache detail — it is a visible reordering of the table
///   `check_connectivity()` later walks, and it is why the creation sequence on
///   its own is not enough.
/// * `create_object()` grows first when `obj_hash_size - 1 <= nr_objs * 2` —
///   `grow_object_hash()` re-inserts the old table in *slot* order, not creation
///   order — then places the object with `insert_obj_hash()`'s linear probe.
fn replay_obj_hash(ops: &[ObjectId]) -> HashMap<ObjectId, usize> {
    let mut table: Vec<Option<ObjectId>> = Vec::new();
    let mut nr: i64 = 0;
    for &id in ops {
        if let Some(found) = probe_find(&table, &id) {
            let home = slot(&id, table.len());
            if found != home {
                table.swap(found, home);
            }
            continue;
        }
        if table.len() as i64 - 1 <= nr * 2 {
            let grown = if table.len() < 32 { 32 } else { table.len() * 2 };
            let old = std::mem::replace(&mut table, vec![None; grown]);
            for existing in old.into_iter().flatten() {
                probe_insert(&mut table, existing);
            }
        }
        probe_insert(&mut table, id);
        nr += 1;
    }
    table
        .into_iter()
        .enumerate()
        .filter_map(|(i, cell)| cell.map(|id| (id, i)))
        .collect()
}

/// `object.c::lookup_object`'s probe, without the move-to-front the caller
/// applies: the home slot, then on to the first empty one, wrapping at the end.
/// `None` when the table is empty or the probe reaches a free slot.
fn probe_find(table: &[Option<ObjectId>], id: &ObjectId) -> Option<usize> {
    let size = table.len();
    if size == 0 {
        return None;
    }
    let mut i = slot(id, size);
    while let Some(here) = table[i] {
        if here == *id {
            return Some(i);
        }
        i += 1;
        if i == size {
            i = 0;
        }
    }
    None
}

/// `object.c::insert_obj_hash` — the home slot, then the next free one, wrapping
/// at the end of the table.
fn probe_insert(table: &mut [Option<ObjectId>], id: ObjectId) {
    let size = table.len();
    let mut j = slot(&id, size);
    while table[j].is_some() {
        j += 1;
        if j >= size {
            j = 0;
        }
    }
    table[j] = Some(id);
}

/// The order `fsck_source()` visits the odb in: every source (the main object
/// directory then each alternate), each walked by
/// `for_each_loose_file_in_source()` — the 256 `.git/objects/??` subdirectories
/// in numeric order, and the entries within one of them in raw `readdir()`
/// order, which `std::fs::read_dir` is the same system call for.
///
/// `None` when the odb holds a pack: `check_full` re-runs `fsck_obj()` over
/// packed objects through `verify_pack()` after the loose walk, in an order this
/// port does not model.
fn loose_scan_order(repo: &gix::Repository) -> Option<Vec<ObjectId>> {
    if has_packs(repo) {
        return None;
    }
    let hexsz = repo.object_hash().len_in_hex();
    let mut seen: HashSet<ObjectId> = HashSet::new();
    let mut out: Vec<ObjectId> = Vec::new();
    for objdir in odb_sources(repo) {
        for sub in 0u16..=0xff {
            let dir = objdir.join(format!("{sub:02x}"));
            let Ok(entries) = std::fs::read_dir(&dir) else {
                continue;
            };
            for entry in entries.flatten() {
                let name = entry.file_name();
                let name = name.as_bytes();
                if name.len() != hexsz - 2 {
                    continue;
                }
                let mut hex = format!("{sub:02x}").into_bytes();
                hex.extend_from_slice(name);
                let Ok(id) = ObjectId::from_hex(&hex) else {
                    continue;
                };
                if seen.insert(id) {
                    out.push(id);
                }
            }
        }
    }
    Some(out)
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
// The table below carries a row for every id in `FOREACH_FSCK_MSG_ID`, because
// git's three config callbacks accept every id in every family and diagnose a bad
// value for any of them — an id missing from the table would silently accept a
// severity git rejects, and would reject a severity git accepts. What separates
// the rows is which macro built them: `msg!`, `msg_mktag!` and `msg_refs!` mark a
// check this port performs (and say where it can fire), while `msg_config_only!`
// marks one it does not, with the reason spelled out in the row's own doc.

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
///
/// Every row carries all three variable spellings because git reads all three,
/// unconditionally and for every id in the table. `git_fsck_config()`
/// (`fsck.c:1461`) matches `fsck.<anything>` and hands it to
/// `fsck_set_msg_type()`; `receive-pack`'s `receive_pack_config()`
/// (`builtin/receive-pack.c:186`) and `fetch_pack_fsck_config()`
/// (`fetch-pack.c:1971`) match `receive.fsck.<anything>` / `fetch.fsck.<anything>`
/// and hand it to `is_valid_msg_type()`. None of the three consults the check's
/// reachability, so all three diagnose a bad value — which is why every row here
/// spells out its own three keys instead of leaving a family unread.
///
/// The three families are independent: `git help config` is explicit that "the
/// `receive.fsck.<msg-id>` and `fetch.fsck.<msg-id>` variables will not fall back
/// on the `fsck.<msg-id>` configuration if they aren't set", so [`MsgConfig`]
/// reads exactly one of the three per [`MsgSource`] and never merges them.
pub struct Msg {
    /// The `<msg-id>` git prints in front of the message text.
    pub id: &'static str,
    /// The variable `git fsck` reads for this check's severity — `fsck.c:1461`.
    pub fsck_key: &'static str,
    /// The variable `git receive-pack` reads for it —
    /// `builtin/receive-pack.c:186`, which forwards it to the
    /// `index-pack`/`unpack-objects` child as `--strict=<id>=<severity>`.
    pub receive_key: &'static str,
    /// The variable `git fetch`/`git fetch-pack` reads for it —
    /// `fetch-pack.c:1971`, which forwards it to `index-pack` the same way.
    pub fetch_key: &'static str,
    /// `fsck.h`'s severity for the id when nothing configures it.
    pub default: Severity,
}

/// Build one table row for a check this port performs on the object paths — the
/// `git fsck` object walk, `index-pack`/`unpack-objects`, the `receive-pack`
/// transfer check and the `fetch` transfer check alike. All three config keys are
/// spelled out rather than `concat!`ed so each one is a greppable literal.
macro_rules! msg {
    ($konst:ident, $id:literal, $fsck:literal, $receive:literal, $fetch:literal, $sev:ident) => {
        #[doc = concat!("`", $id, "`, whose severity comes from `", $fsck, "` under ")]
        #[doc = concat!("`git fsck`, `", $receive, "` under `git receive-pack` and `")]
        #[doc = concat!($fetch, "` under `git fetch`. The check is performed.")]
        pub const $konst: Msg = Msg {
            id: $id,
            fsck_key: $fsck,
            receive_key: $receive,
            fetch_key: $fetch,
            default: Severity::$sev,
        };
    };
}

/// Build one row for a check this port performs, but that only `git mktag`
/// reaches.
///
/// `index-pack`, `unpack-objects` and the `git fsck` object walk all run
/// `parse_object_buffer()` before `fsck_object()` and give up when it fails
/// (`builtin/index-pack.c:953`'s `die("invalid %s")`), so a tag broken enough to
/// trip one of these ids never survives to be reported by them. `git mktag` is
/// the one entry point that fscks a raw tag buffer first. The
/// `receive.fsck.<id>` / `fetch.fsck.<id>` spellings are still read, because git
/// reads and validates them there whether or not the check can fire; they simply
/// never select a severity that anything on those paths consults.
macro_rules! msg_mktag {
    ($konst:ident, $id:literal, $fsck:literal, $receive:literal, $fetch:literal, $sev:ident) => {
        #[doc = concat!("`", $id, "`, whose severity comes from `", $fsck, "`. Only ")]
        #[doc = "`git mktag` reports it: it is the one entry point that fscks a raw tag"]
        #[doc = concat!("buffer without `parse_tag_buffer()` rejecting it first. `", $receive)]
        #[doc = concat!("` and `", $fetch, "` are read and validated, as git does, but the")]
        #[doc = "check cannot fire on a transfer."]
        pub const $konst: Msg = Msg {
            id: $id,
            fsck_key: $fsck,
            receive_key: $receive,
            fetch_key: $fetch,
            default: Severity::$sev,
        };
    };
}

/// Build one row for a check this port performs, but that only the
/// reference-database walk reaches — `git refs verify`, which `git fsck` runs for
/// `--references`.
///
/// A transfer never reaches these: `receive-pack` and `fetch-pack` fsck the
/// objects that came over the wire, not the local repository's ref files. The
/// `receive.fsck.<id>` / `fetch.fsck.<id>` spellings are still read, because git
/// reads and validates them there regardless.
macro_rules! msg_refs {
    ($konst:ident, $id:literal, $fsck:literal, $receive:literal, $fetch:literal, $sev:ident) => {
        #[doc = concat!("`", $id, "`, whose severity comes from `", $fsck, "`. Only ")]
        #[doc = "the reference-database check reports it — `git refs verify`, which"]
        #[doc = concat!("`git fsck` runs for `--references`. `", $receive, "` and `", $fetch)]
        #[doc = "` are read and validated, as git does, but the check cannot fire on a"]
        #[doc = "transfer."]
        pub const $konst: Msg = Msg {
            id: $id,
            fsck_key: $fsck,
            receive_key: $receive,
            fetch_key: $fetch,
            default: Severity::$sev,
        };
    };
}

/// Build one row whose check this port does **not** perform.
///
/// The row exists so that all three variables parse, validate and round-trip
/// exactly as git's do — a severity git rejects is the same `fatal:` here, a
/// misspelled id next to it is the same `Unhandled message id`, and a severity
/// git accepts is accepted and stored. What the row does *not* do is report
/// anything: no code path constructs a [`Finding`] naming it. `$why` records the
/// reason, which is the only honest claim available for these ids.
///
/// Leaving the id out of the table entirely would be worse in both directions:
/// `fsck.<id> = bogus` would be silently accepted where git dies, and
/// `fsck.<id> = warn` would be diagnosed as an unknown id where git accepts it.
macro_rules! msg_config_only {
    ($konst:ident, $id:literal, $fsck:literal, $receive:literal, $fetch:literal, $sev:ident,
     $why:literal) => {
        #[doc = concat!("`", $id, "`. **The check is not performed by this port.** ", $why)]
        #[doc = ""]
        #[doc = concat!("`", $fsck, "`, `", $receive, "` and `", $fetch, "` are still read,")]
        #[doc = "validated and stored, so a bad severity or a neighbouring misspelled id"]
        #[doc = "fails exactly as git fails — but no finding ever names this row."]
        pub const $konst: Msg = Msg {
            id: $id,
            fsck_key: $fsck,
            receive_key: $receive,
            fetch_key: $fetch,
            default: Severity::$sev,
        };
    };
}

// --- commit header checks (`verify_headers`, `fsck_commit`, `fsck_ident`) ---
msg!(BAD_PARENT_SHA1, "badParentSha1", "fsck.badParentSha1", "receive.fsck.badParentSha1", "fetch.fsck.badParentSha1", Error);
msg!(MISSING_AUTHOR, "missingAuthor", "fsck.missingAuthor", "receive.fsck.missingAuthor", "fetch.fsck.missingAuthor", Error);
msg!(MULTIPLE_AUTHORS, "multipleAuthors", "fsck.multipleAuthors", "receive.fsck.multipleAuthors", "fetch.fsck.multipleAuthors", Error);
msg!(MISSING_COMMITTER, "missingCommitter", "fsck.missingCommitter", "receive.fsck.missingCommitter", "fetch.fsck.missingCommitter", Error);
msg!(MISSING_NAME_BEFORE_EMAIL, "missingNameBeforeEmail", "fsck.missingNameBeforeEmail", "receive.fsck.missingNameBeforeEmail", "fetch.fsck.missingNameBeforeEmail", Error);
msg!(BAD_NAME, "badName", "fsck.badName", "receive.fsck.badName", "fetch.fsck.badName", Error);
msg!(BAD_EMAIL, "badEmail", "fsck.badEmail", "receive.fsck.badEmail", "fetch.fsck.badEmail", Error);
msg!(MISSING_EMAIL, "missingEmail", "fsck.missingEmail", "receive.fsck.missingEmail", "fetch.fsck.missingEmail", Error);
msg!(MISSING_SPACE_BEFORE_EMAIL, "missingSpaceBeforeEmail", "fsck.missingSpaceBeforeEmail", "receive.fsck.missingSpaceBeforeEmail", "fetch.fsck.missingSpaceBeforeEmail", Error);
msg!(MISSING_SPACE_BEFORE_DATE, "missingSpaceBeforeDate", "fsck.missingSpaceBeforeDate", "receive.fsck.missingSpaceBeforeDate", "fetch.fsck.missingSpaceBeforeDate", Error);
msg!(ZERO_PADDED_DATE, "zeroPaddedDate", "fsck.zeroPaddedDate", "receive.fsck.zeroPaddedDate", "fetch.fsck.zeroPaddedDate", Error);
msg!(BAD_DATE_OVERFLOW, "badDateOverflow", "fsck.badDateOverflow", "receive.fsck.badDateOverflow", "fetch.fsck.badDateOverflow", Error);
msg!(BAD_DATE, "badDate", "fsck.badDate", "receive.fsck.badDate", "fetch.fsck.badDate", Error);
msg!(BAD_TIMEZONE, "badTimezone", "fsck.badTimezone", "receive.fsck.badTimezone", "fetch.fsck.badTimezone", Error);
msg!(NUL_IN_COMMIT, "nulInCommit", "fsck.nulInCommit", "receive.fsck.nulInCommit", "fetch.fsck.nulInCommit", Warn);
msg!(UNTERMINATED_HEADER, "unterminatedHeader", "fsck.unterminatedHeader", "receive.fsck.unterminatedHeader", "fetch.fsck.unterminatedHeader", Fatal);
msg!(NUL_IN_HEADER, "nulInHeader", "fsck.nulInHeader", "receive.fsck.nulInHeader", "fetch.fsck.nulInHeader", Fatal);
// `fsck_commit()`'s two `tree` header ids (`fsck.c:970` and `fsck.c:972`). Both
// sit behind a `parse_commit_buffer()` that has already rejected the same
// buffer, on every path that could reach them, so neither is reportable — see
// the `msg_config_only!` rows below for the evidence.
msg_config_only!(MISSING_TREE, "missingTree", "fsck.missingTree", "receive.fsck.missingTree", "fetch.fsck.missingTree", Error,
    "`fsck_commit()` reports it at `fsck.c:970` for a commit whose first header \
     is not `tree `, but every entry point parses the commit first and gives up: \
     `builtin/fsck.c:754` calls `parse_object_buffer()` and prints `object could \
     not be parsed`, and `builtin/index-pack.c:950` does the same and dies \
     `invalid commit`. Confirmed against git 2.55.0: a commit object with no \
     `tree` line yields `error: bogus commit object <oid>` from `git fsck`, from \
     `git index-pack --strict` and from `git unpack-objects --strict` — never \
     `missingTree`. No command hands a raw *commit* buffer to `fsck_buffer()` \
     the way `git mktag` does for tags.");
msg_config_only!(BAD_TREE_SHA1, "badTreeSha1", "fsck.badTreeSha1", "receive.fsck.badTreeSha1", "fetch.fsck.badTreeSha1", Error,
    "`fsck_commit()` reports it at `fsck.c:972` for a `tree` line whose hex is \
     not a well-formed object id. Unreachable for the same reason as \
     `missingTree`: `parse_commit_buffer()` rejects the buffer first everywhere.");

// --- tree checks (`fsck_tree`) ---------------------------------------------
msg!(NULL_SHA1, "nullSha1", "fsck.nullSha1", "receive.fsck.nullSha1", "fetch.fsck.nullSha1", Warn);
msg!(FULL_PATHNAME, "fullPathname", "fsck.fullPathname", "receive.fsck.fullPathname", "fetch.fsck.fullPathname", Warn);
msg!(HAS_DOT, "hasDot", "fsck.hasDot", "receive.fsck.hasDot", "fetch.fsck.hasDot", Warn);
msg!(HAS_DOTDOT, "hasDotdot", "fsck.hasDotdot", "receive.fsck.hasDotdot", "fetch.fsck.hasDotdot", Warn);
msg!(HAS_DOTGIT, "hasDotgit", "fsck.hasDotgit", "receive.fsck.hasDotgit", "fetch.fsck.hasDotgit", Warn);
msg!(ZERO_PADDED_FILEMODE, "zeroPaddedFilemode", "fsck.zeroPaddedFilemode", "receive.fsck.zeroPaddedFilemode", "fetch.fsck.zeroPaddedFilemode", Warn);
msg!(BAD_FILEMODE, "badFilemode", "fsck.badFilemode", "receive.fsck.badFilemode", "fetch.fsck.badFilemode", Info);
msg!(DUPLICATE_ENTRIES, "duplicateEntries", "fsck.duplicateEntries", "receive.fsck.duplicateEntries", "fetch.fsck.duplicateEntries", Error);
msg!(TREE_NOT_SORTED, "treeNotSorted", "fsck.treeNotSorted", "receive.fsck.treeNotSorted", "fetch.fsck.treeNotSorted", Error);
msg!(LARGE_PATHNAME, "largePathname", "fsck.largePathname", "receive.fsck.largePathname", "fetch.fsck.largePathname", Warn);
msg_config_only!(EMPTY_NAME, "emptyName", "fsck.emptyName", "receive.fsck.emptyName", "fetch.fsck.emptyName", Warn,
    "`fsck_tree()` accumulates `has_empty_name |= !*name` at `fsck.c:657` and \
     reports it at `fsck.c:773`, but the flag is computed from an entry \
     `init_tree_desc_gently()`/`decode_tree_entry()` has already accepted, and \
     that rejects an empty filename outright. A tree holding a zero-length entry \
     name therefore fails to parse and is reported as `badTree` instead. \
     Confirmed against git 2.55.0: a hand-built tree whose single entry is \
     `100644 \\0<oid>` yields `error: empty filename in tree entry` followed by \
     `error in tree <oid>: badTree: cannot be parsed as a tree`, with no \
     `emptyName` line. The id is dead in git itself, not merely unported.");

// --- tag checks (`fsck_tag`) -----------------------------------------------
//
// The first seven are the header walk `parse_tag_buffer()` shadows everywhere
// but in `git mktag`; see [`Msg::receive_key`] and [`super::mktag`].
msg_mktag!(MISSING_OBJECT, "missingObject", "fsck.missingObject", "receive.fsck.missingObject", "fetch.fsck.missingObject", Error);
msg_mktag!(BAD_OBJECT_SHA1, "badObjectSha1", "fsck.badObjectSha1", "receive.fsck.badObjectSha1", "fetch.fsck.badObjectSha1", Error);
msg_mktag!(MISSING_TYPE_ENTRY, "missingTypeEntry", "fsck.missingTypeEntry", "receive.fsck.missingTypeEntry", "fetch.fsck.missingTypeEntry", Error);
msg_mktag!(MISSING_TYPE, "missingType", "fsck.missingType", "receive.fsck.missingType", "fetch.fsck.missingType", Error);
msg_mktag!(BAD_TYPE, "badType", "fsck.badType", "receive.fsck.badType", "fetch.fsck.badType", Error);
msg_mktag!(MISSING_TAG_ENTRY, "missingTagEntry", "fsck.missingTagEntry", "receive.fsck.missingTagEntry", "fetch.fsck.missingTagEntry", Error);
msg_mktag!(MISSING_TAG, "missingTag", "fsck.missingTag", "receive.fsck.missingTag", "fetch.fsck.missingTag", Error);
msg!(MISSING_TAGGER_ENTRY, "missingTaggerEntry", "fsck.missingTaggerEntry", "receive.fsck.missingTaggerEntry", "fetch.fsck.missingTaggerEntry", Info);
msg!(BAD_TAG_NAME, "badTagName", "fsck.badTagName", "receive.fsck.badTagName", "fetch.fsck.badTagName", Info);
msg!(EXTRA_HEADER_ENTRY, "extraHeaderEntry", "fsck.extraHeaderEntry", "receive.fsck.extraHeaderEntry", "fetch.fsck.extraHeaderEntry", Ignore);
msg!(BAD_TREE, "badTree", "fsck.badTree", "receive.fsck.badTree", "fetch.fsck.badTree", Error);
// `fsck_tag()`'s `gpgsig`/`gpgsig-sha256` continuation walk (`fsck.c:1097`).
msg_config_only!(BAD_GPGSIG, "badGpgsig", "fsck.badGpgsig", "receive.fsck.badGpgsig", "fetch.fsck.badGpgsig", Error,
    "`fsck_tag()` reports it at `fsck.c:1100` when a `gpgsig ` header runs to the \
     end of the buffer with no newline. Reaching that line needs \
     `verify_headers()` (`fsck.c:829`) to have returned 0 first, and it only does \
     so when the buffer contains a `\\n\\n` or ends in `\\n` — either of which \
     gives the `gpgsig` line the newline whose absence is the whole finding. A \
     buffer with neither is reported as `unterminatedHeader`, which is \
     `FSCK_FATAL` and so cannot be demoted out of the way \
     (`fsck.c:176`: `Cannot demote unterminatedHeader to <x>`). Confirmed against \
     git 2.55.0 by construction, including via `git mktag`, which is the only \
     caller that fscks a raw tag buffer.");
msg_config_only!(BAD_HEADER_CONTINUATION, "badHeaderContinuation", "fsck.badHeaderContinuation", "receive.fsck.badHeaderContinuation", "fetch.fsck.badHeaderContinuation", Error,
    "`fsck_tag()` reports it at `fsck.c:1108` for a ` `-indented `gpgsig` \
     continuation line that runs to the end of the buffer with no newline. \
     Unreachable for exactly the reason `badGpgsig` is: `verify_headers()` has \
     already turned a buffer with no terminating newline into a `FSCK_FATAL` \
     `unterminatedHeader`.");
// `fsck_buffer()`'s fallthrough for an object that is none of the four types
// (`fsck.c:1276`).
msg_config_only!(UNKNOWN_TYPE, "unknownType", "fsck.unknownType", "receive.fsck.unknownType", "fetch.fsck.unknownType", Error,
    "`fsck_buffer()` reports it at `fsck.c:1276` — `unknown type '%d' (internal \
     fsck error)` — for a type that is not blob, tree, commit or tag. No object \
     database can hand one over: `type_from_string_gently()` refuses to write a \
     fifth type in the first place. Confirmed against git 2.55.0: \
     `git hash-object -t frobnicate --literally` fails with \
     `fatal: invalid object type \"frobnicate\"` before an object exists at all. \
     The message text calls itself an internal error, which is what it is.");

// --- the four special paths, reported against the *tree* that names them
// (`fsck_tree`'s per-entry block) ------------------------------------------
msg!(GITMODULES_SYMLINK, "gitmodulesSymlink", "fsck.gitmodulesSymlink", "receive.fsck.gitmodulesSymlink", "fetch.fsck.gitmodulesSymlink", Error);
msg!(GITATTRIBUTES_SYMLINK, "gitattributesSymlink", "fsck.gitattributesSymlink", "receive.fsck.gitattributesSymlink", "fetch.fsck.gitattributesSymlink", Info);
msg!(GITIGNORE_SYMLINK, "gitignoreSymlink", "fsck.gitignoreSymlink", "receive.fsck.gitignoreSymlink", "fetch.fsck.gitignoreSymlink", Info);
msg!(MAILMAP_SYMLINK, "mailmapSymlink", "fsck.mailmapSymlink", "receive.fsck.mailmapSymlink", "fetch.fsck.mailmapSymlink", Info);

// --- blob-content checks, reported against the *blob* (`fsck_blob`) --------
msg!(GITMODULES_PARSE, "gitmodulesParse", "fsck.gitmodulesParse", "receive.fsck.gitmodulesParse", "fetch.fsck.gitmodulesParse", Info);
msg!(GITMODULES_NAME, "gitmodulesName", "fsck.gitmodulesName", "receive.fsck.gitmodulesName", "fetch.fsck.gitmodulesName", Error);
msg!(GITMODULES_URL, "gitmodulesUrl", "fsck.gitmodulesUrl", "receive.fsck.gitmodulesUrl", "fetch.fsck.gitmodulesUrl", Error);
msg!(GITMODULES_PATH, "gitmodulesPath", "fsck.gitmodulesPath", "receive.fsck.gitmodulesPath", "fetch.fsck.gitmodulesPath", Error);
msg!(GITMODULES_UPDATE, "gitmodulesUpdate", "fsck.gitmodulesUpdate", "receive.fsck.gitmodulesUpdate", "fetch.fsck.gitmodulesUpdate", Error);
msg!(GITATTRIBUTES_LARGE, "gitattributesLarge", "fsck.gitattributesLarge", "receive.fsck.gitattributesLarge", "fetch.fsck.gitattributesLarge", Error);
// `fsck_blob()`'s `!buf` half (`fsck.c:1198`): the caller found the blob too big
// to hold in memory and passed a null buffer, so there is nothing to parse. Which
// blobs those are is `core.bigFileThreshold`'s decision — see [`streamed_blob`].
msg!(GITMODULES_LARGE, "gitmodulesLarge", "fsck.gitmodulesLarge", "receive.fsck.gitmodulesLarge", "fetch.fsck.gitmodulesLarge", Error);
msg!(GITATTRIBUTES_LINE_LENGTH, "gitattributesLineLength", "fsck.gitattributesLineLength", "receive.fsck.gitattributesLineLength", "fetch.fsck.gitattributesLineLength", Error);

// --- `fsck_finish()`'s two sweeps over the collected paths (`fsck_blobs`) --
msg!(GITMODULES_MISSING, "gitmodulesMissing", "fsck.gitmodulesMissing", "receive.fsck.gitmodulesMissing", "fetch.fsck.gitmodulesMissing", Error);
msg!(GITMODULES_BLOB, "gitmodulesBlob", "fsck.gitmodulesBlob", "receive.fsck.gitmodulesBlob", "fetch.fsck.gitmodulesBlob", Error);
msg!(GITATTRIBUTES_MISSING, "gitattributesMissing", "fsck.gitattributesMissing", "receive.fsck.gitattributesMissing", "fetch.fsck.gitattributesMissing", Error);
msg!(GITATTRIBUTES_BLOB, "gitattributesBlob", "fsck.gitattributesBlob", "receive.fsck.gitattributesBlob", "fetch.fsck.gitattributesBlob", Error);

// --- the reference database (`refs_fsck`, `files_fsck`, `packed_fsck`) ------
//
// Reported against a *path* rather than an object id: a refname
// (`refs/heads/x`, `worktrees/wt/HEAD`), or one of `packed-refs`,
// `packed-refs.header`, `packed-refs line <n>`. See [`fsck_refs`].
msg_refs!(BAD_REF_NAME, "badRefName", "fsck.badRefName", "receive.fsck.badRefName", "fetch.fsck.badRefName", Error);
msg_refs!(BAD_REF_FILETYPE, "badRefFiletype", "fsck.badRefFiletype", "receive.fsck.badRefFiletype", "fetch.fsck.badRefFiletype", Error);
msg_refs!(BAD_REF_CONTENT, "badRefContent", "fsck.badRefContent", "receive.fsck.badRefContent", "fetch.fsck.badRefContent", Error);
msg_refs!(BAD_REF_OID, "badRefOid", "fsck.badRefOid", "receive.fsck.badRefOid", "fetch.fsck.badRefOid", Error);
msg_refs!(BAD_HEAD_TARGET, "badHeadTarget", "fsck.badHeadTarget", "receive.fsck.badHeadTarget", "fetch.fsck.badHeadTarget", Error);
msg_refs!(BAD_REFERENT_NAME, "badReferentName", "fsck.badReferentName", "receive.fsck.badReferentName", "fetch.fsck.badReferentName", Error);
msg_refs!(REF_MISSING_NEWLINE, "refMissingNewline", "fsck.refMissingNewline", "receive.fsck.refMissingNewline", "fetch.fsck.refMissingNewline", Info);
msg_refs!(TRAILING_REF_CONTENT, "trailingRefContent", "fsck.trailingRefContent", "receive.fsck.trailingRefContent", "fetch.fsck.trailingRefContent", Info);
msg_refs!(SYMLINK_REF, "symlinkRef", "fsck.symlinkRef", "receive.fsck.symlinkRef", "fetch.fsck.symlinkRef", Info);
msg_refs!(SYMREF_TARGET_IS_NOT_A_REF, "symrefTargetIsNotARef", "fsck.symrefTargetIsNotARef", "receive.fsck.symrefTargetIsNotARef", "fetch.fsck.symrefTargetIsNotARef", Info);
msg_refs!(BAD_PACKED_REF_HEADER, "badPackedRefHeader", "fsck.badPackedRefHeader", "receive.fsck.badPackedRefHeader", "fetch.fsck.badPackedRefHeader", Error);
msg_refs!(BAD_PACKED_REF_ENTRY, "badPackedRefEntry", "fsck.badPackedRefEntry", "receive.fsck.badPackedRefEntry", "fetch.fsck.badPackedRefEntry", Error);
msg_refs!(PACKED_REF_ENTRY_NOT_TERMINATED, "packedRefEntryNotTerminated", "fsck.packedRefEntryNotTerminated", "receive.fsck.packedRefEntryNotTerminated", "fetch.fsck.packedRefEntryNotTerminated", Error);
msg_refs!(PACKED_REF_UNSORTED, "packedRefUnsorted", "fsck.packedRefUnsorted", "receive.fsck.packedRefUnsorted", "fetch.fsck.packedRefUnsorted", Error);
msg_refs!(EMPTY_PACKED_REFS_FILE, "emptyPackedRefsFile", "fsck.emptyPackedRefsFile", "receive.fsck.emptyPackedRefsFile", "fetch.fsck.emptyPackedRefsFile", Info);
msg_config_only!(BAD_REFTABLE_TABLE_NAME, "badReftableTableName", "fsck.badReftableTableName", "receive.fsck.badReftableTableName", "fetch.fsck.badReftableTableName", Warn,
    "`refs/reftable-backend.c::reftable_fsck_error_handler` raises it for a table \
     file inside `reftable/` whose name does not match the \
     `0x%012<PRIx64>-0x%012<PRIx64>-%08x.ref` form the backend writes. The \
     vendored `gix-ref` has no reftable backend at all — only the `files` backend \
     with its `packed-refs` — so a reftable repository cannot even be opened here, \
     let alone have its table names walked. Porting the id would mean porting the \
     backend.");

/// Every row this port implements, for severity resolution and for telling a
/// misspelled `fsck.<x>` key from a real one.
pub const MSGS: &[Msg] = &[
    BAD_PARENT_SHA1,
    MISSING_AUTHOR,
    MULTIPLE_AUTHORS,
    MISSING_COMMITTER,
    MISSING_NAME_BEFORE_EMAIL,
    BAD_NAME,
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
    MISSING_TREE,
    BAD_TREE_SHA1,
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
    EMPTY_NAME,
    MISSING_OBJECT,
    BAD_OBJECT_SHA1,
    MISSING_TYPE_ENTRY,
    MISSING_TYPE,
    BAD_TYPE,
    MISSING_TAG_ENTRY,
    MISSING_TAG,
    MISSING_TAGGER_ENTRY,
    BAD_TAG_NAME,
    EXTRA_HEADER_ENTRY,
    BAD_TREE,
    BAD_GPGSIG,
    BAD_HEADER_CONTINUATION,
    UNKNOWN_TYPE,
    GITMODULES_SYMLINK,
    GITATTRIBUTES_SYMLINK,
    GITIGNORE_SYMLINK,
    MAILMAP_SYMLINK,
    GITMODULES_PARSE,
    GITMODULES_NAME,
    GITMODULES_URL,
    GITMODULES_PATH,
    GITMODULES_UPDATE,
    GITATTRIBUTES_LARGE,
    GITATTRIBUTES_LINE_LENGTH,
    GITMODULES_LARGE,
    GITMODULES_MISSING,
    GITMODULES_BLOB,
    GITATTRIBUTES_MISSING,
    GITATTRIBUTES_BLOB,
    BAD_REF_NAME,
    BAD_REF_FILETYPE,
    BAD_REF_CONTENT,
    BAD_REF_OID,
    BAD_HEAD_TARGET,
    BAD_REFERENT_NAME,
    REF_MISSING_NEWLINE,
    TRAILING_REF_CONTENT,
    SYMLINK_REF,
    SYMREF_TARGET_IS_NOT_A_REF,
    BAD_PACKED_REF_HEADER,
    BAD_PACKED_REF_ENTRY,
    PACKED_REF_ENTRY_NOT_TERMINATED,
    PACKED_REF_UNSORTED,
    EMPTY_PACKED_REFS_FILE,
    BAD_REFTABLE_TABLE_NAME,
];


/// One reported defect: the table row that names it plus the rendered text.
pub struct Finding {
    /// The row, which decides the severity and prints the `<msg-id>:` prefix.
    pub msg: &'static Msg,
    /// The message body, already formatted (`nulInHeader` and `badTagName`
    /// interpolate).
    pub text: String,
}

/// Which of the three variable families the caller reads, which also picks the
/// `--strict` behaviour and where each configuration failure surfaces.
///
/// The three never fall back on one another. `git help config`, under
/// `fsck.<msg-id>`: "the `receive.fsck.<msg-id>` and `fetch.fsck.<msg-id>`
/// variables will not fall back on the `fsck.<msg-id>` configuration if they
/// aren't set. To uniformly configure the same fsck settings in different
/// circumstances, all three of them must be set to the same values."
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum MsgSource {
    /// `fsck.<msg-id>` / `fsck.skipList`, read by `git_fsck_config()`
    /// (`fsck.c:1440`) with `--strict` honoured.
    Fsck { strict: bool },
    /// `receive.fsck.<msg-id>` / `receive.fsck.skipList`, read by
    /// `receive_pack_config()` (`builtin/receive-pack.c:174`). The transfer
    /// check always runs `index-pack`/`unpack-objects` with `--strict`, so a
    /// defaulted warning is an error here even though the same object only
    /// warns under a plain `git fsck`.
    Receive,
    /// `fetch.fsck.<msg-id>` / `fetch.fsck.skipList`, read by
    /// `fetch_pack_fsck_config()` (`fetch-pack.c:1954`). Identical to
    /// [`MsgSource::Receive`] in every respect but the variable names and the
    /// capital `S` in its unknown-id warning — `fetch-pack.c:1978` spells it
    /// `Skipping unknown msg id '%s'`, `builtin/receive-pack.c:193` spells it
    /// `skipping unknown msg id '%s'`. `index-pack` gets the list as
    /// `--strict<list>` (`fetch-pack.c:1061`) and its options carry
    /// `.strict = 1` (`fsck.c:1403`, the `FSCK_OPTIONS_MISSING_GITMODULES` row),
    /// hence the same warning promotion.
    Fetch,
}

/// The resolved severity of every message id plus the skipped-object set —
/// `fsck.c`'s `struct fsck_options` fields `msg_type` and `skiplist`.
pub struct MsgConfig {
    /// Severity per `Msg::id`.
    levels: HashMap<&'static str, Severity>,
    /// Object ids the skip list names; every message about them is dropped.
    skip: HashSet<ObjectId>,
    /// A `die()` that `git fsck` reaches while reading its own configuration,
    /// but `receive-pack` and `fetch-pack` only reach inside the
    /// `index-pack`/`unpack-objects` child they hand `--strict=<types>` to — so
    /// the failure arrives with that child's abnormal exit rather than the
    /// session dying before the advertisement (`receive-pack`) or before the
    /// negotiation (`fetch`).
    ///
    /// Which failures those are follows from *where* git runs each check.
    /// `is_valid_msg_type()` — all `receive.fsck.`/`fetch.fsck.` variables go
    /// through it — calls `parse_msg_id()` then `parse_msg_type()`, so a bad
    /// *value* dies in the parent. The demote rule (`fsck.c:176`) and
    /// `oidset_parse_file()` (`fsck.c:207`) live in `fsck_set_msg_type()` /
    /// `fsck_set_msg_types()`, which the parent never calls: it only appends the
    /// token to the `--strict` list. Both therefore die in the child. Confirmed
    /// against git 2.55.0: with `fetch.fsckObjects` unset,
    /// `fetch.fsck.nulInHeader=warn` and `fetch.fsck.skipList=/nope` fetch
    /// cleanly, and with it set they die `Cannot demote nulinheader to warn` /
    /// `could not open object name list: /nope`, each followed by
    /// `fatal: index-pack failed`.
    pub deferred_fatal: Option<String>,
}

impl MsgConfig {
    /// Resolve every severity from the repository's configuration.
    ///
    /// Returns the message git dies with — without its `fatal: ` prefix — for
    /// a bad value (`Unknown fsck message type`) or, under `git fsck` only, a
    /// misspelled id (`Unhandled message id`). The two failures the transfer
    /// sides only reach inside their child land in [`Self::deferred_fatal`].
    pub fn new(repo: &gix::Repository, source: MsgSource) -> Result<Self, String> {
        let config = repo.config_snapshot();
        // `fsck_options_init()` gives both transfer paths a `strict` option set
        // (`fsck.c:1403`, the `FSCK_OPTIONS_MISSING_GITMODULES` row every
        // `index-pack`/`unpack-objects` uses), so every *defaulted* warning is an
        // error there.
        let strict = matches!(
            source,
            MsgSource::Fsck { strict: true } | MsgSource::Receive | MsgSource::Fetch
        );
        let mut deferred_fatal: Option<String> = None;
        let mut levels = HashMap::with_capacity(MSGS.len());
        for m in MSGS {
            // Every row has all three spellings: git's three config callbacks
            // each match their whole family without consulting the id.
            let key = match source {
                MsgSource::Fsck { .. } => m.fsck_key,
                MsgSource::Receive => m.receive_key,
                MsgSource::Fetch => m.fetch_key,
            };
            let level = match config.string(key) {
                Some(v) => {
                    let value = v.to_string();
                    // `is_valid_msg_type()` runs in the transfer paths' own
                    // config callbacks, so an unknown *value* is fatal on all
                    // three.
                    let level = parse_severity(&value)?;
                    if m.default == Severity::Fatal && level != Severity::Error {
                        let text = format!("Cannot demote {} to {value}", m.id.to_lowercase());
                        match source {
                            MsgSource::Fsck { .. } => return Err(text),
                            MsgSource::Receive | MsgSource::Fetch => {
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

        // git validates *every* variable in the family, including the ids whose
        // check this port does not perform, and diagnoses one it does not know
        // at all.
        let (section, subsection) = match source {
            MsgSource::Fsck { .. } => ("fsck", None),
            MsgSource::Receive => ("receive", Some("fsck")),
            MsgSource::Fetch => ("fetch", Some("fsck")),
        };
        for name in value_names(&config, section, subsection) {
            let lower = name.to_lowercase();
            if lower == "skiplist" {
                continue;
            }
            let known = MSGS.iter().any(|m| m.id.eq_ignore_ascii_case(&name));
            if known {
                continue;
            }
            // `git help config`: an unknown `fsck.<msg-id>` kills fsck, while
            // the same under `receive.fsck.`/`fetch.fsck.` is only a warning.
            // The two warnings differ in one byte — see [`MsgSource::Fetch`].
            match source {
                MsgSource::Fsck { .. } => return Err(format!("Unhandled message id: {lower}")),
                MsgSource::Receive => eprintln!("warning: skipping unknown msg id '{lower}'"),
                MsgSource::Fetch => eprintln!("warning: Skipping unknown msg id '{lower}'"),
            }
        }

        let skip_key = match source {
            MsgSource::Fsck { .. } => "fsck.skipList",
            MsgSource::Receive => "receive.fsck.skipList",
            MsgSource::Fetch => "fetch.fsck.skipList",
        };
        let skip = match config.string(skip_key) {
            Some(path) => match read_skip_list(&path.to_string()) {
                Ok(skip) => skip,
                // `oidset_parse_file()` runs where the checking runs, so the
                // transfer sides hit this inside their child.
                Err(text) => match source {
                    MsgSource::Fsck { .. } => return Err(text),
                    MsgSource::Receive | MsgSource::Fetch => {
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
        let level = self.severity_of(finding.msg);
        if level != Severity::Ignore && self.skip.contains(oid) {
            return Severity::Ignore;
        }
        level
    }

    /// `fsck.c::fsck_msg_type()` on its own, for the reference-database checks.
    /// They report through `fsck_report_ref()`, which — unlike `report()` —
    /// never consults the skip list: that list holds object ids, and a ref
    /// finding names a path.
    pub fn severity_of(&self, msg: &Msg) -> Severity {
        self.levels.get(msg.id).copied().unwrap_or(msg.default)
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

/// `oidset.c::oidset_parse_file_carefully` (`oidset.c:73`), which
/// `fsck_set_msg_types()` reaches for `skiplist=<path>`: one object id per line.
///
/// Its own comment: "Allow trailing comments, leading whitespace (including
/// before commits), and empty or whitespace only lines." The order matters and is
/// reproduced here — the line is first truncated at its *first* `#`, wherever it
/// sits, and only then trimmed. A `<oid>   # note` line therefore names the oid,
/// which trimming first and testing `starts_with('#')` would have rejected.
///
/// Both failures are `die()`s, and the caller decides whether they surface here
/// or inside a transfer's child; see [`MsgConfig::deferred_fatal`].
fn read_skip_list(path: &str) -> Result<HashSet<ObjectId>, String> {
    let text = std::fs::read_to_string(path)
        .map_err(|_| format!("could not open object name list: {path}"))?;
    let mut out = HashSet::new();
    for line in text.lines() {
        let line = match line.find('#') {
            Some(hash) => &line[..hash],
            None => line,
        };
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        // `parse_oid_hex_algop(...) || *p != '\0'`: the whole remainder of the
        // line has to be one full-length id.
        let id = ObjectId::from_hex(line.as_bytes())
            .map_err(|_| format!("invalid object name: {line}"))?;
        out.insert(id);
    }
    Ok(out)
}

// ===========================================================================
// The reference-database check — `git refs verify`
// ===========================================================================
//
// `builtin/fsck.c::fsck_refs()` checks nothing itself: it runs `git refs verify`
// as a child, forwarding `--verbose` and `--strict`, and ORs `ERROR_REFS` into
// the exit code when that child fails. `cmd_refs_verify()` in turn calls
// `refs_fsck()` once per worktree, which dispatches to the storage backend's
// `fsck` — `files_fsck()` in `refs/files-backend.c`, which walks
// `$GIT_DIR/refs`, then the root refs directly under `$GIT_DIR`, then hands
// `packed-refs` to `packed_fsck()` in `refs/packed-backend.c`.
//
// All three are ported here, and both entry points reach them: [`fsck`] for
// `--references`, and `git refs verify` itself. Unlike the object checks the
// findings are reported against a *path* rather than an object id, through
// `fsck_report_ref()`:
//
// ```text
// error: refs/heads/x: badRefName: invalid refname format
// warning: packed-refs: emptyPackedRefsFile: file is empty
// ```
//
// `fsck_report_ref()` deliberately does not consult `fsck.skipList` — that list
// holds object ids and these findings name a path — but it does go through the
// same `fsck_msg_type()`, so `fsck.<msg-id>` behaves exactly as it does for the
// object checks. `--strict` is accepted and changes nothing here: it only
// promotes a *defaulted* `Warn`, and every ported reference id defaults to
// `Error` or `Info`. The one id of this family that is not ported is
// `badReftableTableName`; see [`BAD_REFTABLE_TABLE_NAME`].
//
// ### Known divergences
//
//  1. **Report order within one directory follows `readdir()`.** git's
//     `dir_iterator_begin(path, 0)` is unsorted, so it reports the refs of a
//     directory in whatever order the filesystem returns them;
//     `std::fs::read_dir` is the same `readdir()` over the same directory, so
//     the two agree in practice, but neither order is guaranteed. Directories
//     are descended into as they are met, which is git's pre-order traversal.
//  2. **An I/O failure part way through a directory is skipped rather than
//     reported.** git prints `failed to iterate over '<dir>'` and fails; here
//     only the failure to open the top of `refs/` is reported, as
//     `cannot open directory <dir>`.

/// One worktree as `cmd_refs_verify()` sees it.
/// `get_worktrees_without_reading_head()` yields the main worktree first and
/// then each linked one.
struct RefWorktree {
    /// `wt->id`, `None` for the main worktree. Linked worktrees prefix every
    /// refname they report with `worktrees/<id>/`.
    id: Option<Vec<u8>>,
    /// `refs->base.gitdir`: `$GIT_DIR` for the main worktree,
    /// `$GIT_COMMON_DIR/worktrees/<id>` for a linked one. Both `refs/` and the
    /// root refs are looked for here.
    gitdir: PathBuf,
}

impl RefWorktree {
    /// `is_main_worktree()`.
    fn is_main(&self) -> bool {
        self.id.is_none()
    }

    /// The `worktrees/<id>/` prefix `files_fsck_refs_dir()` and
    /// `files_fsck_root_ref()` put in front of every refname of a linked
    /// worktree.
    fn refname_prefix(&self) -> Vec<u8> {
        match &self.id {
            None => Vec::new(),
            Some(id) => {
                let mut prefix = b"worktrees/".to_vec();
                prefix.extend_from_slice(id);
                prefix.push(b'/');
                prefix
            }
        }
    }
}

/// `refs.c::refs_fsck()` over every worktree — the whole of `git refs verify`.
///
/// Returns `true` when a finding was reported at error severity, which is what
/// `fsck_refs()` turns into `ERROR_REFS` and what makes `git refs verify` fail.
pub fn fsck_refs(repo: &gix::Repository, cfg: &MsgConfig, verbose: bool) -> bool {
    let mut check = RefCheck {
        cfg,
        verbose,
        common_dir: repo.common_dir().to_path_buf(),
        workdir: repo.workdir().map(Path::to_path_buf),
        hexsz: repo.object_hash().len_in_hex(),
        failed: false,
    };

    for wt in ref_worktrees(repo) {
        // `refs_fsck()` announces itself once per worktree, before dispatching
        // to the backend.
        if verbose {
            eprintln!("Checking references consistency");
        }
        check.files_fsck(&wt);
    }
    check.failed
}

/// `get_worktrees_without_reading_head()`: the main worktree, then each linked
/// one. A linked worktree whose working tree is gone still has its refs
/// checked, which is why the proxy is never resolved into a repository here.
fn ref_worktrees(repo: &gix::Repository) -> Vec<RefWorktree> {
    let mut out = vec![RefWorktree {
        id: None,
        gitdir: repo.common_dir().to_path_buf(),
    }];
    if let Ok(worktrees) = repo.worktrees() {
        for proxy in worktrees {
            out.push(RefWorktree {
                id: Some(proxy.id().to_vec()),
                gitdir: proxy.git_dir().to_path_buf(),
            });
        }
    }
    out
}

/// `struct fsck_options` as the reference checks use it, plus the paths and the
/// hash width they read back off the ref store.
struct RefCheck<'a> {
    /// Severities from `fsck.<msg-id>`, resolved once by [`MsgConfig`].
    cfg: &'a MsgConfig,
    /// `o->verbose`: the `Checking <ref>` trace on stderr.
    verbose: bool,
    /// `refs->gitcommondir`, which is where a root ref's *file* lives even when
    /// the refname is prefixed with `worktrees/<id>/`, and where `packed-refs`
    /// always lives.
    common_dir: PathBuf,
    /// The working directory, used only to render the `packed-refs` path in the
    /// `--verbose` trace the way git does — relative to where the command runs.
    workdir: Option<PathBuf>,
    /// `the_hash_algo->hexsz`.
    hexsz: usize,
    /// Set once any finding is reported at error severity.
    failed: bool,
}

impl RefCheck<'_> {
    /// `fsck.c::fsck_report_ref()` plus `fsck_refs_error_function()`: resolve
    /// the severity, drop the finding when it is `ignore`, and print
    /// `error: <path>: <msg-id>: <text>` or the `warning:` form.
    ///
    /// Returns git's non-zero result, i.e. whether this was reported as an
    /// error — several callers stop checking a ref once one fires. Everything
    /// is written as bytes because a refname need not be valid UTF-8 and git
    /// passes it through untouched.
    fn report(&mut self, path: &[u8], msg: &'static Msg, text: &[u8]) -> bool {
        // `fsck_vreport()`: FATAL is reported as ERROR and INFO as WARN.
        let level = match self.cfg.severity_of(msg) {
            Severity::Ignore => return false,
            Severity::Info => Severity::Warn,
            Severity::Fatal => Severity::Error,
            other => other,
        };
        let warn = level == Severity::Warn;

        let mut line = Vec::with_capacity(path.len() + text.len() + 32);
        line.extend_from_slice(if warn {
            b"warning: ".as_slice()
        } else {
            b"error: ".as_slice()
        });
        line.extend_from_slice(path);
        line.extend_from_slice(b": ");
        line.extend_from_slice(msg.id.as_bytes());
        line.extend_from_slice(b": ");
        line.extend_from_slice(text);
        line.push(b'\n');
        let _ = std::io::Write::write_all(&mut std::io::stderr(), &line);

        if warn {
            return false;
        }
        self.failed = true;
        true
    }

    /// `refs/files-backend.c::files_fsck()`.
    fn files_fsck(&mut self, wt: &RefWorktree) {
        self.files_fsck_refs_dir(wt);
        self.for_each_root_ref(wt);
        // `packed-refs` is shared, so only the main worktree checks it.
        if wt.is_main() {
            self.packed_fsck();
        }
    }

    /// `files_fsck_refs_dir()`: every file below `$GIT_DIR/refs`, named
    /// `refs/<relative path>` and prefixed with `worktrees/<id>/` for a linked
    /// worktree. A missing directory is an error for the main worktree and
    /// silently fine for a linked one, which need not have any per-worktree ref.
    fn files_fsck_refs_dir(&mut self, wt: &RefWorktree) {
        let root = wt.gitdir.join("refs");
        let read = match std::fs::read_dir(&root) {
            Ok(read) => read,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound && !wt.is_main() => return,
            Err(e) => {
                eprintln!("error: cannot open directory {}: {e}", root.display());
                self.failed = true;
                return;
            }
        };
        let mut prefix = wt.refname_prefix();
        prefix.extend_from_slice(b"refs");
        self.walk_refs_dir(read, &prefix);
    }

    /// One level of git's `dir_iterator`, which is unsorted and pre-order: an
    /// entry is yielded as `readdir()` returns it, and a directory is descended
    /// into immediately rather than after its siblings. `dir_iterator` `lstat`s,
    /// so a symlink stays a symlink.
    fn walk_refs_dir(&mut self, read: std::fs::ReadDir, refname: &[u8]) {
        for entry in read {
            let Ok(entry) = entry else { continue };
            let path = entry.path();
            let Ok(meta) = path.symlink_metadata() else {
                continue;
            };
            let name = entry.file_name();
            let name = name.as_bytes();

            let mut child = refname.to_vec();
            child.push(b'/');
            child.extend_from_slice(name);

            if meta.is_dir() {
                // `if (S_ISDIR(iter->st.st_mode)) continue;` — the directory is
                // not itself a ref, but its contents are visited next.
                if let Ok(read) = std::fs::read_dir(&path) {
                    self.walk_refs_dir(read, &child);
                }
                continue;
            }

            // Lock files are skipped, but a name that *starts* with a dot is not
            // a lock file — it is an invalid refname that must still be reported.
            if name.first() != Some(&b'.') && name.ends_with(b".lock") {
                continue;
            }

            self.files_fsck_ref(&child, &path, &meta);
        }
    }

    /// `for_each_root_ref()` feeding `files_fsck_root_ref()`: the files directly
    /// under `$GIT_DIR` whose names `is_root_ref()` accepts. Their *file* is
    /// looked up under `gitcommondir` by the already-prefixed refname, which is
    /// how a linked worktree's `HEAD` resolves to
    /// `$GIT_COMMON_DIR/worktrees/<id>/HEAD`. Both the `get_dtype()` filter and
    /// the `stat()` that follows it resolve symlinks, so a symlinked root ref is
    /// read through rather than reported as a `symlinkRef`.
    fn for_each_root_ref(&mut self, wt: &RefWorktree) {
        let Ok(read) = std::fs::read_dir(&wt.gitdir) else {
            return;
        };
        let prefix = wt.refname_prefix();
        for entry in read {
            let Ok(entry) = entry else { continue };
            let name = entry.file_name();
            let name = name.as_bytes();
            if name.first() == Some(&b'.') || name.ends_with(b".lock") {
                continue;
            }
            if !entry.path().is_file() || !super::for_each_ref::is_root_ref(name) {
                continue;
            }

            let mut refname = prefix.clone();
            refname.extend_from_slice(name);
            let path = self.common_dir.join(OsStr::from_bytes(&refname));
            let meta = match path.metadata() {
                Ok(meta) => meta,
                // Raced away between the readdir and the stat.
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
                Err(e) => {
                    eprintln!("error: failed to read ref: '{}': {e}", path.display());
                    self.failed = true;
                    continue;
                }
            };
            self.files_fsck_ref(&refname, &path, &meta);
        }
    }

    /// `files_fsck_ref()`: the two per-ref checks, gated on the file being a
    /// regular file or a symlink.
    fn files_fsck_ref(&mut self, refname: &[u8], path: &Path, meta: &std::fs::Metadata) {
        if self.verbose {
            let mut line = b"Checking ".to_vec();
            line.extend_from_slice(refname);
            line.push(b'\n');
            let _ = std::io::Write::write_all(&mut std::io::stderr(), &line);
        }

        let ty = meta.file_type();
        if !ty.is_file() && !ty.is_symlink() {
            self.report(refname, &BAD_REF_FILETYPE, b"unexpected file type");
            return;
        }

        // git runs both functions and ORs their results; neither short-circuits
        // the other, so a badly named ref still has its contents checked.
        self.files_fsck_refs_name(refname);
        self.files_fsck_refs_content(refname, path, ty.is_symlink());
    }

    /// `files_fsck_refs_name()`. Root refs are one-component names that
    /// `check_refname_format()` would reject, so they are waived outright.
    fn files_fsck_refs_name(&mut self, refname: &[u8]) {
        if super::for_each_ref::is_root_ref(refname) {
            return;
        }
        if !super::check_ref_format::check_refname_format(refname, 0) {
            self.report(refname, &BAD_REF_NAME, b"invalid refname format");
        }
    }

    /// `files_fsck_refs_content()`: parse the ref file the way
    /// `parse_loose_ref_contents()` does and report what it rejects.
    fn files_fsck_refs_content(&mut self, refname: &[u8], path: &Path, is_symlink: bool) {
        if is_symlink {
            self.report(
                refname,
                &SYMLINK_REF,
                b"use deprecated symbolic link for symref",
            );
            // git resolves the link and, when it lands inside `$GIT_DIR`,
            // reports the referent as the path relative to it — so a symlink to
            // `.git/refs/heads/main` reads as the refname `refs/heads/main`.
            // Anything outside is reported as the absolute path it resolved to,
            // which `check_refname_format()` then rejects.
            // The two sides are resolved differently on purpose, exactly as git
            // does it: the link target through `strbuf_add_real_path()`, which
            // follows symlinks, and the git directory through
            // `strbuf_add_absolute_path()` + `strbuf_normalize_path()`, which do
            // not. So a git directory reached through a symlinked path does not
            // strip, and the referent is reported absolute.
            let real = real_path(path, MAX_SYMLINKS);
            let base = absolute_path(&self.common_dir);
            let referent = real.strip_prefix(&base).unwrap_or(real.as_path());
            let referent = referent.as_os_str().as_bytes().to_vec();
            self.files_fsck_symref_target(refname, &referent, true);
            return;
        }

        let content = match std::fs::read(path) {
            Ok(content) => content,
            // Removed by a concurrent process between the walk and the read.
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return,
            Err(e) => {
                eprintln!("error: cannot read ref file '{}': {e}", path.display());
                self.failed = true;
                return;
            }
        };

        match parse_loose_ref_contents(&content, self.hexsz) {
            LooseRef::Symref { referent } => {
                self.files_fsck_symref_target(refname, referent, false);
            }
            LooseRef::Oid { id, trailing } => {
                // C reads a NUL-terminated buffer, so "nothing left" and "an
                // empty rest" are the same thing.
                if trailing.is_empty() {
                    self.report(refname, &REF_MISSING_NEWLINE, b"misses LF at the end");
                    return;
                }
                if trailing != b"\n".as_slice() {
                    let mut text = b"has trailing garbage: '".to_vec();
                    text.extend_from_slice(trailing);
                    text.push(b'\'');
                    self.report(refname, &TRAILING_REF_CONTENT, &text);
                    return;
                }
                // `refs.c::refs_fsck_ref()`.
                if id.is_null() {
                    let text = format!("points to invalid object ID '{id}'");
                    self.report(refname, &BAD_REF_OID, text.as_bytes());
                }
            }
            // git prints the file's contents, right-trimmed, as the whole
            // message.
            LooseRef::Broken => {
                let text = rtrim(&content).to_vec();
                self.report(refname, &BAD_REF_CONTENT, &text);
            }
        }
    }

    /// `files_fsck_symref_target()` plus `refs.c::refs_fsck_symref()`.
    ///
    /// `referent` is the raw remainder of a `ref: ...` line, newline included;
    /// a symlink's referent has no line terminator to check, which is what
    /// `symbolic_link` selects.
    fn files_fsck_symref_target(&mut self, refname: &[u8], referent: &[u8], symbolic_link: bool) {
        let orig_len = referent.len();
        let orig_last_byte = referent.last().copied().unwrap_or(0);
        let target = if symbolic_link {
            referent
        } else {
            rtrim(referent)
        };

        if !symbolic_link {
            // Exactly one trailing LF is the well-formed case: anything else is
            // either a missing terminator or trailing junk, and both can fire.
            if target.len() == orig_len || (target.len() < orig_len && orig_last_byte != b'\n') {
                self.report(refname, &REF_MISSING_NEWLINE, b"misses LF at the end");
            }
            if target.len() != orig_len && target.len() + 1 != orig_len {
                self.report(
                    refname,
                    &TRAILING_REF_CONTENT,
                    b"has trailing whitespaces or newlines",
                );
            }
        }

        // `parse_worktree_ref()`: a linked worktree's `worktrees/<id>/HEAD` is
        // still HEAD as far as the target check is concerned.
        if strip_worktree_prefix(refname) == b"HEAD".as_slice() && !target.starts_with(b"refs/heads/") {
            let mut text = b"HEAD points to non-branch '".to_vec();
            text.extend_from_slice(target);
            text.push(b'\'');
            if self.report(refname, &BAD_HEAD_TARGET, &text) {
                return;
            }
        }

        if super::for_each_ref::is_root_ref(target) {
            return;
        }

        if !super::check_ref_format::check_refname_format(target, 0) {
            let mut text = b"points to invalid refname '".to_vec();
            text.extend_from_slice(target);
            text.push(b'\'');
            if self.report(refname, &BAD_REFERENT_NAME, &text) {
                return;
            }
        }

        if !target.starts_with(b"refs/") && !target.starts_with(b"worktrees/") {
            let mut text = b"points to non-ref target '".to_vec();
            text.extend_from_slice(target);
            text.push(b'\'');
            self.report(refname, &SYMREF_TARGET_IS_NOT_A_REF, &text);
        }
    }

    /// `refs/packed-backend.c::packed_fsck()`.
    fn packed_fsck(&mut self) {
        let path = self.common_dir.join("packed-refs");
        if self.verbose {
            let label = self
                .workdir
                .as_deref()
                .and_then(|work| path.strip_prefix(work).ok())
                .unwrap_or(path.as_path());
            eprintln!("Checking packed-refs file {}", label.display());
        }

        // `open_nofollow()`: a symlinked `packed-refs` is a finding, not
        // something to follow.
        let meta = match path.symlink_metadata() {
            Ok(meta) => meta,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return,
            Err(e) => {
                eprintln!("error: unable to open '{}': {e}", path.display());
                self.failed = true;
                return;
            }
        };
        if meta.file_type().is_symlink() {
            self.report(
                b"packed-refs",
                &BAD_REF_FILETYPE,
                b"not a regular file but a symlink",
            );
            return;
        }
        if !meta.is_file() {
            self.report(b"packed-refs", &BAD_REF_FILETYPE, b"not a regular file");
            return;
        }

        let buf = match std::fs::read(&path) {
            Ok(buf) => buf,
            Err(e) => {
                eprintln!("error: unable to open '{}': {e}", path.display());
                self.failed = true;
                return;
            }
        };
        // `allocate_snapshot_buffer()` fails on a zero-length file.
        if buf.is_empty() {
            self.report(b"packed-refs", &EMPTY_PACKED_REFS_FILE, b"file is empty");
            return;
        }

        let mut sorted = false;
        // The sortedness pass indexes past each line's hash, so git only runs it
        // once the content pass has confirmed every line parses.
        if !self.packed_fsck_ref_content(&buf, &mut sorted) && sorted {
            self.packed_fsck_ref_sorted(&buf);
        }
    }

    /// `packed_fsck_ref_content()`: the header line, then alternating main and
    /// optional `^peeled` lines.
    fn packed_fsck_ref_content(&mut self, buf: &[u8], sorted: &mut bool) -> bool {
        let mut failed = false;
        let mut at = 0usize;
        let mut line_number = 1u64;

        let mut eol = self.packed_fsck_ref_next_line(buf, at, line_number, &mut failed);
        // Only a leading '#' is a header; anything else is read as an entry,
        // which is why a headerless `packed-refs` reports `badPackedRefEntry`
        // rather than `badPackedRefHeader`.
        if buf[at] == b'#' {
            failed |= self.packed_fsck_ref_header(&buf[at..eol], sorted);
            at = eol + 1;
            line_number += 1;
        }

        while at < buf.len() {
            eol = self.packed_fsck_ref_next_line(buf, at, line_number, &mut failed);
            failed |= self.packed_fsck_ref_main_line(&buf[at..eol], line_number);
            at = eol + 1;
            line_number += 1;
            if at < buf.len() && buf[at] == b'^' {
                eol = self.packed_fsck_ref_next_line(buf, at, line_number, &mut failed);
                failed |= self.packed_fsck_ref_peeled_line(&buf[at..eol], line_number);
                at = eol + 1;
                line_number += 1;
            }
        }
        failed
    }

    /// `packed_fsck_ref_next_line()`: the offset of the line's `\n`, or the end
    /// of the buffer once the missing terminator has been reported.
    fn packed_fsck_ref_next_line(
        &mut self,
        buf: &[u8],
        at: usize,
        line_number: u64,
        failed: &mut bool,
    ) -> usize {
        match buf[at..].iter().position(|&b| b == b'\n') {
            Some(off) => at + off,
            None => {
                let mut text = b"'".to_vec();
                text.extend_from_slice(&buf[at..]);
                text.extend_from_slice(b"' is not terminated with a newline");
                *failed |= self.report(
                    packed_entry(line_number).as_bytes(),
                    &PACKED_REF_ENTRY_NOT_TERMINATED,
                    &text,
                );
                buf.len()
            }
        }
    }

    /// `packed_fsck_ref_header()`, which also decides whether the sortedness
    /// pass runs at all.
    fn packed_fsck_ref_header(&mut self, line: &[u8], sorted: &mut bool) -> bool {
        const PREFIX: &[u8] = b"# pack-refs with: ";
        let Some(traits) = line.strip_prefix(PREFIX) else {
            let mut text = b"'".to_vec();
            text.extend_from_slice(line);
            text.extend_from_slice(b"' does not start with '# pack-refs with: '");
            return self.report(b"packed-refs.header", &BAD_PACKED_REF_HEADER, &text);
        };
        *sorted = traits.split(|&b| b == b' ').any(|t| t == b"sorted".as_slice());
        false
    }

    /// `packed_fsck_ref_main_line()`: `<oid> SP <refname>`.
    fn packed_fsck_ref_main_line(&mut self, line: &[u8], line_number: u64) -> bool {
        let entry = packed_entry(line_number);
        let path = entry.as_bytes();

        let Some(id) = parse_oid_prefix(line, self.hexsz) else {
            let mut text = b"'".to_vec();
            text.extend_from_slice(line);
            text.extend_from_slice(b"' has invalid oid");
            return self.report(path, &BAD_PACKED_REF_ENTRY, &text);
        };

        let rest = &line[self.hexsz..];
        if !rest.first().is_some_and(|b| is_space(*b)) {
            let mut text = format!("has no space after oid '{id}' but with '").into_bytes();
            text.extend_from_slice(rest);
            text.push(b'\'');
            return self.report(path, &BAD_PACKED_REF_ENTRY, &text);
        }

        // Both of the remaining checks run; git overwrites its result rather
        // than returning after the first.
        let refname = &rest[1..];
        let mut failed = false;
        if refname.contains(&0) {
            let mut text = b"refname '".to_vec();
            text.extend_from_slice(refname);
            text.extend_from_slice(b"' contains NULL binaries");
            failed |= self.report(path, &BAD_PACKED_REF_ENTRY, &text);
        }
        if !super::check_ref_format::check_refname_format(refname, 0) {
            let mut text = b"has bad refname '".to_vec();
            text.extend_from_slice(refname);
            text.push(b'\'');
            failed |= self.report(path, &BAD_REF_NAME, &text);
        }
        failed
    }

    /// `packed_fsck_ref_peeled_line()`: `^<oid>`, with nothing after it. Every
    /// offset git reports is relative to what follows the `^`.
    fn packed_fsck_ref_peeled_line(&mut self, line: &[u8], line_number: u64) -> bool {
        let entry = packed_entry(line_number);
        let path = entry.as_bytes();
        let body = &line[1..];

        if parse_oid_prefix(body, self.hexsz).is_none() {
            let mut text = b"'".to_vec();
            text.extend_from_slice(body);
            text.extend_from_slice(b"' has invalid peeled oid");
            return self.report(path, &BAD_PACKED_REF_ENTRY, &text);
        }

        let rest = &body[self.hexsz..];
        if !rest.is_empty() {
            let mut text = b"has trailing garbage after peeled oid '".to_vec();
            text.extend_from_slice(rest);
            text.push(b'\'');
            return self.report(path, &BAD_PACKED_REF_ENTRY, &text);
        }
        false
    }

    /// `packed_fsck_ref_sorted()`: with the `sorted` trait advertised, every
    /// refname must be strictly greater than the one before it. git reports the
    /// first inversion and stops.
    ///
    /// `cmp_packed_refname()` compares raw bytes and treats the terminating
    /// newline as lower than every other byte, which is exactly how Rust orders
    /// the newline-free slices taken here.
    fn packed_fsck_ref_sorted(&mut self, buf: &[u8]) {
        let mut line_number = 1u64;
        let mut at = 0usize;
        let mut former: Option<&[u8]> = None;

        if buf[at] == b'#' {
            at = line_end(buf, at) + 1;
            line_number += 1;
        }

        while at < buf.len() {
            let eol = line_end(buf, at);
            // A refname is only well defined once the hash and its separator
            // are there; the content pass has normally proven that, but an
            // `fsck.badPackedRefEntry=ignore` can let a short line through.
            let Some(start) = at.checked_add(self.hexsz + 1).filter(|s| *s <= eol) else {
                return;
            };
            if buf[at] != b'^' {
                let current = &buf[start..eol];
                if let Some(previous) = former {
                    if previous >= current {
                        let mut text = b"refname '".to_vec();
                        text.extend_from_slice(current);
                        text.extend_from_slice(b"' is less than previous refname '");
                        text.extend_from_slice(previous);
                        text.push(b'\'');
                        self.report(
                            packed_entry(line_number).as_bytes(),
                            &PACKED_REF_UNSORTED,
                            &text,
                        );
                        return;
                    }
                }
                former = Some(current);
            }
            at = eol + 1;
            line_number += 1;
        }
    }
}

/// `MAXSYMLINKS`, the hop budget `strbuf_realpath()` gives a chain of symlinks.
const MAX_SYMLINKS: u32 = 32;

/// `strbuf_add_absolute_path()` followed by `strbuf_normalize_path()`: absolute
/// and lexically cleaned, with symlinks left alone.
///
/// The one surprising part is git's own: a relative path is joined onto `$PWD`
/// rather than `getcwd()` whenever the two name the same directory, so a
/// repository reached through a symlinked path keeps the symlinked spelling.
/// That is what decides whether a symlinked ref's referent is reported as a
/// refname or as an absolute path.
fn absolute_path(path: &Path) -> PathBuf {
    let mut out = if path.is_absolute() {
        path.to_path_buf()
    } else {
        let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        let base = match std::env::var_os("PWD") {
            Some(pwd) if Path::new(&pwd) != cwd && same_dir(Path::new(&pwd), &cwd) => {
                PathBuf::from(pwd)
            }
            _ => cwd,
        };
        base.join(path)
    };

    // `strbuf_normalize_path()`: purely lexical, so `..` is dropped along with
    // the component before it without consulting the filesystem.
    let mut parts: Vec<std::ffi::OsString> = Vec::new();
    for c in out.components() {
        match c {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                parts.pop();
            }
            other => parts.push(other.as_os_str().to_os_string()),
        }
    }
    out = parts.iter().collect();
    out
}

/// Whether two paths name the same directory, by device and inode — git's
/// `stat()` comparison in `strbuf_add_absolute_path()`.
fn same_dir(a: &Path, b: &Path) -> bool {
    use std::os::unix::fs::MetadataExt;
    match (std::fs::metadata(a), std::fs::metadata(b)) {
        (Ok(a), Ok(b)) => a.dev() == b.dev() && a.ino() == b.ino(),
        _ => false,
    }
}

/// `strbuf_realpath()`: the absolute path with every symlink resolved, keeping
/// whatever tail does not exist yet rather than failing on it.
///
/// That last part is the whole reason `std::fs::canonicalize` is not enough: a
/// ref symlinked to a branch that has not been created resolves to the refname
/// it *would* have, and git reports no finding for it. Returning an empty
/// referent there would instead be an invalid refname.
fn real_path(path: &Path, hops: u32) -> PathBuf {
    if let Ok(real) = std::fs::canonicalize(path) {
        return real;
    }
    // A link whose target is missing: follow it by hand and resolve the target.
    if hops > 0 {
        if let Ok(target) = std::fs::read_link(path) {
            let joined = match path.parent() {
                Some(parent) if target.is_relative() => parent.join(target),
                _ => target,
            };
            return real_path(&joined, hops - 1);
        }
    }
    // Not a link either, so the tail simply does not exist; resolve the parent
    // and keep the name.
    match (path.parent(), path.file_name()) {
        (Some(parent), Some(name)) if !parent.as_os_str().is_empty() => {
            real_path(parent, hops).join(name)
        }
        _ => path.to_path_buf(),
    }
}

/// The `packed-refs line <n>` path a `packed-refs` finding is reported against.
fn packed_entry(line_number: u64) -> String {
    format!("packed-refs line {line_number}")
}

/// The offset of the `\n` ending the line at `at`, or the end of the buffer.
fn line_end(buf: &[u8], at: usize) -> usize {
    buf[at..]
        .iter()
        .position(|&b| b == b'\n')
        .map_or(buf.len(), |off| at + off)
}

/// C's `isspace()` over the C locale, which counts one byte `u8::is_ascii_whitespace`
/// leaves out: the vertical tab.
fn is_space(b: u8) -> bool {
    b == b' ' || (0x09..=0x0d).contains(&b)
}

/// `strbuf_rtrim()`.
fn rtrim(buf: &[u8]) -> &[u8] {
    let end = buf.iter().rposition(|b| !is_space(*b)).map_or(0, |i| i + 1);
    &buf[..end]
}

/// A full object id at the start of `buf`, as `parse_oid_hex_algop()` reads one:
/// exactly `hexsz` hex digits, with whatever follows left to the caller.
fn parse_oid_prefix(buf: &[u8], hexsz: usize) -> Option<ObjectId> {
    ObjectId::from_hex(buf.get(..hexsz)?).ok()
}

/// What `parse_loose_ref_contents()` made of a loose ref file.
enum LooseRef<'a> {
    /// A `ref: <target>` line; `referent` is everything after the whitespace
    /// that follows the prefix, newline included.
    Symref { referent: &'a [u8] },
    /// A hex object id; `trailing` is the rest of the buffer.
    Oid { id: ObjectId, trailing: &'a [u8] },
    /// Neither — git's `REF_ISBROKEN`.
    Broken,
}

/// `refs/files-backend.c::parse_loose_ref_contents()`.
fn parse_loose_ref_contents(buf: &[u8], hexsz: usize) -> LooseRef<'_> {
    if let Some(rest) = buf.strip_prefix(b"ref:".as_slice()) {
        let start = rest.iter().position(|b| !is_space(*b)).unwrap_or(rest.len());
        return LooseRef::Symref {
            referent: &rest[start..],
        };
    }
    let Some(id) = parse_oid_prefix(buf, hexsz) else {
        return LooseRef::Broken;
    };
    // `FETCH_HEAD` carries more data after the hash, so whitespace — or the end
    // of the buffer — is the only acceptable separator.
    let trailing = &buf[hexsz..];
    match trailing.first() {
        Some(b) if !is_space(*b) => LooseRef::Broken,
        _ => LooseRef::Oid { id, trailing },
    }
}

/// `refs.c::parse_worktree_ref()` reduced to what the symref check needs: the
/// bare refname, with a `worktrees/<id>/` or `main-worktree/` prefix removed.
fn strip_worktree_prefix(refname: &[u8]) -> &[u8] {
    if let Some(rest) = refname.strip_prefix(b"main-worktree/".as_slice()) {
        return rest;
    }
    let Some(rest) = refname.strip_prefix(b"worktrees/".as_slice()) else {
        return refname;
    };
    match rest.iter().position(|&b| b == b'/') {
        Some(slash) => &rest[slash + 1..],
        None => refname,
    }
}

/// What one `fsck_object()` call produced: the findings about the object
/// itself, plus the two `fsck_options` oidsets a tree contributes to.
#[derive(Default)]
pub struct Checked {
    /// Findings in git's reporting order; the caller decides which of them are
    /// severe enough to print.
    pub findings: Vec<Finding>,
    /// Plain `error:` lines the tree-entry decoder printed itself. They carry
    /// no msg-id, so no `fsck.<msg-id>` severity and no skip list applies —
    /// `init_tree_desc_gently()` and `update_tree_entry_gently()` call
    /// `error()` directly.
    pub raw: Vec<String>,
    /// Ids a tree entry named `.gitmodules` — `options->gitmodules_found`.
    /// Their contents are linted once the whole odb has been walked.
    pub gitmodules: Vec<ObjectId>,
    /// The same for `.gitattributes` — `options->gitattributes_found`.
    pub gitattributes: Vec<ObjectId>,
}

/// `fsck.c::fsck_object` for the three types whose contents this port lints.
/// A blob is linted separately by [`check_blob`], because whether it is linted
/// at all depends on the trees walked before it.
///
/// `hexsz` is git's `the_hash_algo->hexsz` for the repository the object came
/// from — 40 in a sha1 repository, 64 in a sha256 one. Tree entries carry raw
/// ids of `hexsz / 2` bytes and a commit's `parent` lines carry `hexsz` hex
/// characters, so the checks below cannot be written against one width.
pub fn check_object(kind: Kind, data: &[u8], strict: bool, hexsz: usize) -> Checked {
    let mut out = Checked::default();
    match kind {
        Kind::Commit => check_commit(data, &mut out.findings, hexsz),
        Kind::Tree => check_tree(data, &mut out, strict, hexsz),
        // `fsck_tag` walks its headers line by line and never measures an id, so
        // it needs no hash width.
        Kind::Tag => check_tag(data, &mut out.findings),
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

/// `fsck.c::fsck_commit`, entered only for a commit `parse_commit_buffer()`
/// already accepted — so the `tree` line is known well formed and the two ids
/// git would report for it (`missingTree`, `badTreeSha1`) cannot arise; an
/// unexpected `tree` line stops the check rather than claiming an id this port
/// does not report.
///
/// A malformed `parent` line *can* survive that parse, because
/// `parse_commit_buffer()` only enters its own parent loop while
/// `bufptr + hexsz + 7 < tail` — a `parent` line in the last `hexsz + 8` bytes
/// of the buffer is never looked at there and reaches `fsck_commit` intact. So
/// `badParentSha1` is reported here.
fn check_commit(data: &[u8], out: &mut Vec<Finding>, hexsz: usize) {
    if verify_headers(data, out) {
        return;
    }
    let Some(mut p) = strip(data, b"tree ") else { return };
    let Some(rest) = skip_line(p) else { return };
    p = rest;
    while let Some(after) = strip(p, b"parent ") {
        // `parse_oid_hex_algop(buffer, &parent_oid, &p) || *p != '\n'`.
        let well_formed = after.len() > hexsz
            && after[..hexsz].iter().all(u8::is_ascii_hexdigit)
            && after[hexsz] == b'\n';
        if !well_formed {
            report(out, &BAD_PARENT_SHA1, "invalid 'parent' line format - bad sha1");
            return;
        }
        p = &after[hexsz + 1..];
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
        // git's name loop reports a `>` before any `<` as `badName`; only the
        // second loop, past the `<`, calls the same shape `badEmail`.
        Some(b'>') => {
            report(out, &BAD_NAME, "invalid author/committer line - bad name");
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

/// The `switch (mode)` in `fsck_tree`'s loop. `S_IFREG | 0664` is a seventh
/// mode git also accepts, but only when `--strict` is off.
const GOOD_MODES: [u32; 5] = [0o100644, 0o100755, 0o120000, 0o040000, 0o160000];

/// `the_hash_algo->rawsz` for a repository whose hex digest is `hexsz`
/// characters wide: 20 for sha1, 32 for sha256. git derives one from the other
/// the same way (`hash.h`: `hexsz == 2 * rawsz`).
fn rawsz(hexsz: usize) -> usize {
    hexsz / 2
}

/// One decoded tree entry — `struct name_entry` plus the raw mode field, which
/// `zeroPaddedFilemode` needs.
struct TreeEntry<'a> {
    /// The mode as `parse_mode()` returns it, truncated to git's `uint16_t`.
    mode: u32,
    /// The mode exactly as it was spelled in the buffer.
    raw_mode: &'a [u8],
    /// The entry name, up to but not including its NUL.
    name: &'a [u8],
    /// The entry's raw object id.
    oid: &'a [u8],
}

/// `tree-walk.c::decode_tree_entry`, returning the message
/// `init_tree_desc_gently()`/`update_tree_entry_gently()` would print on
/// failure. The two `strlen`-based reads git makes past `size` are bounded here
/// instead, and report `too-short tree object`.
fn decode_tree_entry(buf: &[u8], hexsz: usize) -> Result<TreeEntry<'_>, &'static str> {
    let raw = rawsz(hexsz);
    if buf.len() < raw + 3 || buf[buf.len() - (raw + 1)] != 0 {
        return Err("too-short tree object");
    }
    // `parse_mode()`: octal digits up to the separating space.
    if buf[0] == b' ' {
        return Err("malformed mode in tree entry");
    }
    let mut mode: u32 = 0;
    let mut at = 0usize;
    loop {
        let Some(&c) = buf.get(at) else { return Err("malformed mode in tree entry") };
        at += 1;
        if c == b' ' {
            break;
        }
        if !(b'0'..=b'7').contains(&c) {
            return Err("malformed mode in tree entry");
        }
        // git accumulates into an `unsigned int` and stores a `uint16_t`.
        mode = ((mode << 3) + (c - b'0') as u32) & 0xffff;
    }
    let raw_mode = &buf[..at - 1];
    let path = &buf[at..];
    if path.first() == Some(&0) {
        return Err("empty filename in tree entry");
    }
    let Some(nul) = path.iter().position(|&b| b == 0) else {
        return Err("too-short tree object");
    };
    if path.len() < nul + 1 + raw {
        return Err("too-short tree object");
    }
    Ok(TreeEntry {
        mode,
        raw_mode,
        name: &path[..nul],
        oid: &path[nul + 1..nul + 1 + raw],
    })
}

/// How far past an entry the next one starts — `update_tree_entry_internal`'s
/// `end - buf`.
fn tree_entry_span(entry: &TreeEntry<'_>, hexsz: usize) -> usize {
    entry.raw_mode.len() + 1 + entry.name.len() + 1 + rawsz(hexsz)
}

/// Where a tree walk stopped.
enum TreeStop {
    /// The whole buffer decoded.
    End,
    /// `init_tree_desc_gently()` rejected the very first entry, which is what
    /// makes `fsck_walk_tree()` return `-1` and the caller print `broken links`.
    AtInit(&'static str),
    /// `update_tree_entry_gently()` rejected a later entry. The walk stops
    /// quietly — `tree_entry_gently()` just reports end-of-tree.
    AtUpdate(&'static str),
}

/// `init_tree_desc_gently()` followed by `update_tree_entry_gently()` until the
/// buffer runs out or an entry fails to decode.
fn tree_entries(data: &[u8], hexsz: usize) -> (Vec<TreeEntry<'_>>, TreeStop) {
    let mut out = Vec::new();
    if data.is_empty() {
        return (out, TreeStop::End);
    }
    let mut buf = data;
    let entry = match decode_tree_entry(buf, hexsz) {
        Ok(entry) => entry,
        Err(msg) => return (out, TreeStop::AtInit(msg)),
    };
    let mut span = tree_entry_span(&entry, hexsz);
    out.push(entry);
    loop {
        if buf.len() < span {
            // `update_tree_entry_internal()`'s `die("too-short tree file")`,
            // which `decode_tree_entry`'s own guard makes unreachable here.
            return (out, TreeStop::AtUpdate("too-short tree file"));
        }
        buf = &buf[span..];
        if buf.is_empty() {
            return (out, TreeStop::End);
        }
        let entry = match decode_tree_entry(buf, hexsz) {
            Ok(entry) => entry,
            Err(msg) => return (out, TreeStop::AtUpdate(msg)),
        };
        span = tree_entry_span(&entry, hexsz);
        out.push(entry);
    }
}

/// `fsck.c::fsck_tree`. Every defect is accumulated over the whole tree and
/// reported once, in git's fixed order.
fn check_tree(data: &[u8], checked: &mut Checked, strict: bool, hexsz: usize) {
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

    let (entries, stop) = tree_entries(data, hexsz);
    if let TreeStop::AtInit(msg) = stop {
        // `init_tree_desc_gently()` printed its own diagnostic and `fsck_tree`
        // gave up before accumulating anything.
        checked.raw.push(format!("error: {msg}"));
        report(&mut checked.findings, &BAD_TREE, "cannot be parsed as a tree");
        return;
    }
    let out = &mut checked.findings;
    // `update_tree_entry_gently()` prints, `fsck_tree` reports `badTree` and
    // breaks — before the accumulated findings below, and after skipping the
    // last entry's mode and ordering checks.
    let checked_entries = match stop {
        TreeStop::AtUpdate(msg) => {
            checked.raw.push(format!("error: {msg}"));
            report(out, &BAD_TREE, "cannot be parsed as a tree");
            entries.len() - 1
        }
        _ => entries.len(),
    };

    let mut previous: Option<Vec<u8>> = None;
    for (index, entry) in entries.iter().enumerate() {
        let TreeEntry { mode, raw_mode, name, oid } = *entry;
        has_zero_pad |= raw_mode.first() == Some(&b'0');
        has_null_oid |= oid.iter().all(|&b| b == 0);
        has_full_path |= name.contains(&b'/');
        has_dot |= name == b".";
        has_dotdot |= name == b"..";
        has_dotgit |= is_dotgit(name);
        has_large_name |= name.len() > 4096;

        // The four special paths. Unlike everything above these are reported
        // per entry, in entry order and ahead of the accumulated findings —
        // `fsck_tree()` calls `report()` inside the loop for them.
        let entry_id = ObjectId::from_bytes_or_panic(oid);
        let is_link = mode & 0o170000 == 0o120000;
        if is_special(name, Special::Gitmodules) {
            if is_link {
                report(out, &GITMODULES_SYMLINK, ".gitmodules is a symbolic link");
            } else {
                checked.gitmodules.push(entry_id);
            }
        }
        if is_special(name, Special::Gitattributes) {
            if is_link {
                report(out, &GITATTRIBUTES_SYMLINK, ".gitattributes is a symlink");
            } else {
                checked.gitattributes.push(entry_id);
            }
        }
        if is_link {
            if is_special(name, Special::Gitignore) {
                report(out, &GITIGNORE_SYMLINK, ".gitignore is a symlink");
            }
            if is_special(name, Special::Mailmap) {
                report(out, &MAILMAP_SYMLINK, ".mailmap is a symlink");
            }
        }
        // `fsck_tree()` re-tests every `\`-separated tail as an NTFS short
        // name, so `x\.gitmodules` counts as `.gitmodules` too.
        for (i, _) in name.iter().enumerate().filter(|(_, &b)| b == b'\\') {
            let tail = &name[i + 1..];
            has_dotgit |= is_dotgit(tail);
            if is_ntfs_dot(tail, b"gitmodules", b"gi7eba") {
                if is_link {
                    report(out, &GITMODULES_SYMLINK, ".gitmodules is a symbolic link");
                } else {
                    checked.gitmodules.push(entry_id);
                }
            }
        }

        // The mode and ordering checks sit *after* `update_tree_entry_gently()`
        // in git's loop, so the entry the advance failed on skips both.
        if index >= checked_entries {
            continue;
        }
        // `S_IFREG | 0664` falls through to `has_bad_modes` only under `--strict`.
        has_bad_modes |= !GOOD_MODES.contains(&mode) && (strict || mode != 0o100664);

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

/// The four paths whose contents `fsck_tree()` singles out, each with the
/// needle `is_hfs_dot_generic()` takes and the 6-character NTFS short-name
/// prefix `is_ntfs_dot_generic()` takes (`path.c`).
#[derive(Clone, Copy)]
enum Special {
    /// `.gitmodules`
    Gitmodules,
    /// `.gitattributes`
    Gitattributes,
    /// `.gitignore`
    Gitignore,
    /// `.mailmap`
    Mailmap,
}

/// `is_hfs_dot<x>(name) || is_ntfs_dot<x>(name)`, the pair `fsck_tree()` tests
/// each entry name against.
fn is_special(name: &[u8], which: Special) -> bool {
    let (needle, short): (&[u8], &[u8]) = match which {
        Special::Gitmodules => (b"gitmodules", b"gi7eba"),
        Special::Gitattributes => (b"gitattributes", b"gi7d29"),
        Special::Gitignore => (b"gitignore", b"gi250a"),
        Special::Mailmap => (b"mailmap", b"maba30"),
    };
    is_hfs_dot(name, needle) || is_ntfs_dot(name, needle, short)
}

/// `utf8.c::is_hfs_dot_generic` over ASCII: a leading `.`, then the needle
/// compared case-insensitively, then end-of-name or a directory separator.
/// git additionally folds away the Unicode codepoints `next_hfs_char()` ignores
/// (zero-width joiners, the direction marks); those spellings are not covered,
/// exactly as for [`is_dotgit`].
fn is_hfs_dot(name: &[u8], needle: &[u8]) -> bool {
    let Some(rest) = name.strip_prefix(b".") else { return false };
    if rest.len() < needle.len() || !rest[..needle.len()].eq_ignore_ascii_case(needle) {
        return false;
    }
    // `is_dir_sep()`, which off Windows is `/` alone.
    matches!(rest.get(needle.len()), None | Some(b'/'))
}

/// `path.c::is_ntfs_dot_generic`, ported whole — it is pure ASCII. Three
/// spellings match: `.<needle>`, the regular short name `<needle[..6]>~1`
/// through `~4`, and the 8.3 fall-back short name built from
/// `<shortname_prefix>`. Each may be followed by any run of spaces and periods,
/// and ends at NUL or `:`.
fn is_ntfs_dot(name: &[u8], needle: &[u8], shortname_prefix: &[u8]) -> bool {
    /// The `only_spaces_and_periods:` label: everything left must be `' '` or
    /// `'.'` until the name ends or a `:` appears.
    fn only_spaces_and_periods(name: &[u8], mut i: usize) -> bool {
        loop {
            match name.get(i) {
                None | Some(b':') => return true,
                Some(b' ') | Some(b'.') => i += 1,
                Some(_) => return false,
            }
        }
    }

    if let Some(rest) = name.strip_prefix(b".") {
        if rest.len() >= needle.len() && rest[..needle.len()].eq_ignore_ascii_case(needle) {
            return only_spaces_and_periods(name, needle.len() + 1);
        }
    }
    if name.len() >= 8
        && name[..6].eq_ignore_ascii_case(&needle[..6])
        && name[6] == b'~'
        && (b'1'..=b'4').contains(&name[7])
    {
        return only_spaces_and_periods(name, 8);
    }

    let mut saw_tilde = false;
    let mut i = 0usize;
    while i < 8 {
        let Some(&c) = name.get(i) else { return false };
        if saw_tilde {
            if !c.is_ascii_digit() {
                return false;
            }
        } else if c == b'~' {
            i += 1;
            match name.get(i) {
                Some(&d) if (b'1'..=b'9').contains(&d) => {}
                _ => return false,
            }
            saw_tilde = true;
        } else if i >= 6 || c & 0x80 != 0 {
            return false;
        } else if c.to_ascii_lowercase() != shortname_prefix[i] {
            return false;
        }
        i += 1;
    }
    only_spaces_and_periods(name, i)
}

/// `attr.h::ATTR_MAX_LINE_LENGTH`.
const ATTR_MAX_LINE_LENGTH: usize = 2048;
/// `attr.h::ATTR_MAX_FILE_SIZE`.
const ATTR_MAX_FILE_SIZE: usize = 100 * 1024 * 1024;

/// `fsck.c::fsck_blob`: lint a blob some tree named `.gitmodules` and/or
/// `.gitattributes`. `as_modules`/`as_attrs` are the two `oidset_contains()`
/// tests; a blob no tree singled out has nothing checked at all.
///
/// `data` is `None` for git's `!buf` case — the caller found the blob too big to
/// hold in memory and streamed it instead, which is [`streamed_blob`]'s decision.
/// git's comment at `fsck.c:1199` ("Let's just consider that an error") is the
/// whole rationale: with no buffer there is nothing to parse, so the blob is
/// reported rather than skipped. Note the `return` at `fsck.c:1204`: a blob named
/// as *both* `.gitmodules` and `.gitattributes` reports only `gitmodulesLarge`,
/// because the `.gitmodules` half leaves the function before the
/// `.gitattributes` half runs.
pub fn check_blob(data: Option<&[u8]>, as_modules: bool, as_attrs: bool) -> Vec<Finding> {
    let mut out = Vec::new();
    if as_modules {
        let Some(data) = data else {
            report(&mut out, &GITMODULES_LARGE, ".gitmodules too large to parse");
            return out;
        };
        check_gitmodules_blob(data, &mut out);
    }
    if as_attrs {
        // `!buf || size > ATTR_MAX_FILE_SIZE` — the streamed blob and the merely
        // huge one report the same id.
        let Some(data) = data else {
            report(&mut out, &GITATTRIBUTES_LARGE, ".gitattributes too large to parse");
            return out;
        };
        if data.len() > ATTR_MAX_FILE_SIZE {
            report(&mut out, &GITATTRIBUTES_LARGE, ".gitattributes too large to parse");
            return out;
        }
        // `strchrnul()` over a NUL-terminated buffer: git stops at the first
        // NUL, so a line's length is measured within that prefix.
        let text = match data.iter().position(|&b| b == 0) {
            Some(nul) => &data[..nul],
            None => data,
        };
        for line in text.split(|&b| b == b'\n') {
            if line.len() >= ATTR_MAX_LINE_LENGTH {
                report(
                    &mut out,
                    &GITATTRIBUTES_LINE_LENGTH,
                    ".gitattributes has too long lines to parse",
                );
                break;
            }
        }
    }
    out
}

/// `core.bigFileThreshold` — git's `repo_settings_get_big_file_threshold()`
/// (`repo-settings.c:167`), whose default is `512 * 1024 * 1024`.
///
/// It is the only reason `fsck_blob()` ever sees a null buffer, and hence the
/// only way `gitmodulesLarge` can fire. A configured `0` makes every non-empty
/// blob "big": `repo_cfg_ulong()` stores the parsed value verbatim and the test
/// is a plain `size > threshold`.
///
/// One divergence: git's `git_config_ulong()` dies `bad numeric config value` on
/// a value it cannot parse, while a value this port cannot parse falls back to
/// the default. That belongs to `core.bigFileThreshold`'s own port, not to fsck.
pub fn big_file_threshold(repo: &gix::Repository) -> u64 {
    repo.config_snapshot()
        .string("core.bigFileThreshold")
        .and_then(|v| crate::config::parse_config_ulong(&v.to_string()).ok())
        .unwrap_or(512 * 1024 * 1024)
}

/// The buffer `fsck_blob()` is handed for a blob whose contents are `data`:
/// `None` once it is over `threshold`, which is what git passes after
/// `read_loose_object()` (`object-file.c:1645`) or `index-pack`'s `unpack_entry`
/// (`builtin/index-pack.c:488`) streamed it into a fixed-size scratch buffer
/// instead of allocating for it.
///
/// Both tests are `type == OBJ_BLOB && size > threshold`, strictly greater.
/// Confirmed against git 2.55.0 on a 501-byte `.gitmodules`:
/// `core.bigFileThreshold=501` reports nothing, `=500` reports
/// `gitmodulesLarge`.
pub fn blob_buffer(data: &[u8], threshold: u64) -> Option<&[u8]> {
    (data.len() as u64 <= threshold).then_some(data)
}

/// `fsck.c::fsck_gitmodules_fn` run over every `submodule.<name>.<key>` the
/// blob sets, in file order. The submodule *name* is re-checked for every key
/// in its section, which is why a two-key section reports `gitmodulesName`
/// twice.
fn check_gitmodules_blob(data: &[u8], out: &mut Vec<Finding>) {
    let file = match gix::config::File::from_bytes_no_includes(
        data,
        gix::config::file::Metadata::api(),
        gix::config::file::init::Options::default(),
    ) {
        Ok(file) => file,
        // `git_config_from_mem()` failed; `CONFIG_ERROR_SILENT` swallows the
        // parser's own diagnostic and fsck reports the id instead.
        Err(_) => {
            report(out, &GITMODULES_PARSE, "could not parse gitmodules blob");
            return;
        }
    };
    for section in file.sections() {
        if !section.header().name().eq_ignore_ascii_case(b"submodule") {
            continue;
        }
        // `parse_config_key()` requires a subsection; `[submodule]` with no
        // name is not a submodule entry.
        let Some(name) = section.header().subsection_name() else { continue };
        let name = name.to_string();
        // `value_names()` yields one entry per *occurrence*, and so does git's
        // callback, so a repeated key is counted off against `values()`.
        let mut seen: HashMap<String, usize> = HashMap::new();
        for key in section.body().value_names() {
            if !submodule_name_ok(name.as_bytes()) {
                report(out, &GITMODULES_NAME, format!("disallowed submodule name: {name}"));
            }
            let nth = seen.entry(key.to_lowercase()).or_insert(0);
            let value = section.body().values(&key).get(*nth).cloned();
            *nth += 1;
            // A valueless key reaches git's callback with `value == NULL`,
            // which every check below tests for.
            let Some(value) = value else { continue };
            let value = value.to_string();
            if key.eq_ignore_ascii_case("url") && !submodule_url_ok(&value) {
                report(out, &GITMODULES_URL, format!("disallowed submodule url: {value}"));
            }
            if key.eq_ignore_ascii_case("path") && value.starts_with('-') {
                report(out, &GITMODULES_PATH, format!("disallowed submodule path: {value}"));
            }
            // `parse_submodule_update_type()` returns `SM_UPDATE_COMMAND` for
            // anything starting with `!` that is not one of the four names.
            let is_command = !matches!(value.as_str(), "none" | "checkout" | "rebase" | "merge")
                && value.starts_with('!');
            if key.eq_ignore_ascii_case("update") && is_command {
                report(
                    out,
                    &GITMODULES_UPDATE,
                    format!("disallowed submodule update setting: {value}"),
                );
            }
        }
    }
}

/// `submodule-config.c::check_submodule_name`: no empty name, and no `..` as a
/// whole path component under either separator.
fn submodule_name_ok(name: &[u8]) -> bool {
    if name.is_empty() {
        return false;
    }
    let is_sep = |b: u8| b == b'/' || b == b'\\';
    // git enters the check "inside a component" and re-enters it after every
    // separator, so both the first component and every later one are tested.
    let mut starts = vec![0usize];
    starts.extend(name.iter().enumerate().filter(|(_, &b)| is_sep(b)).map(|(i, _)| i + 1));
    for start in starts {
        let rest = &name[start.min(name.len())..];
        if rest.starts_with(b"..") && matches!(rest.get(2), None | Some(&b'/') | Some(&b'\\')) {
            return false;
        }
    }
    true
}

/// `submodule-config.c::check_submodule_url`.
fn submodule_url_ok(url: &str) -> bool {
    // `looks_like_command_line_option()`
    if url.starts_with('-') {
        return false;
    }
    let relative = url.starts_with("./") || url.starts_with("../");
    if relative || url.starts_with("git://") {
        // Appending this to an http URL and url-decoding it must not smuggle a
        // newline in.
        if url_decode(url.as_bytes()).contains(&b'\n') {
            return false;
        }
        // CVE-2020-11008: a URL that escapes its own root with `../` can
        // overwrite the host field.
        let (dotdots, next) = count_leading_dotdots(url);
        if dotdots > 0 && (next.starts_with(':') || next.starts_with('/')) {
            return false;
        }
        return true;
    }
    match url_to_curl_url(url) {
        Some(curl_url) => match url_normalize(curl_url) {
            Some(normalized) => !url_decode(normalized.as_bytes()).contains(&b'\n'),
            None => false,
        },
        None => true,
    }
}

/// `submodule-config.c::count_leading_dotdots`: how many `../` components the
/// URL opens with, plus what follows all the leading `./` and `../`.
fn count_leading_dotdots(url: &str) -> (usize, &str) {
    let mut count = 0usize;
    let mut rest = url;
    loop {
        if let Some(tail) = rest.strip_prefix("../") {
            count += 1;
            rest = tail;
        } else if let Some(tail) = rest.strip_prefix("./") {
            rest = tail;
        } else {
            return (count, rest);
        }
    }
}

/// `submodule-config.c::url_to_curl_url`: the URL git-remote-curl would be
/// handed, when this is a transport it implements.
fn url_to_curl_url(url: &str) -> Option<&str> {
    for prefix in ["http::", "https::", "ftp::", "ftps::"] {
        if let Some(rest) = url.strip_prefix(prefix) {
            return Some(rest);
        }
    }
    for prefix in ["http://", "https://", "ftp://", "ftps://"] {
        if url.starts_with(prefix) {
            return Some(url);
        }
    }
    None
}

/// `url.c::url_decode`, which is `url_decode_mem(url, strlen(url))`: everything
/// up to the first `:` is the scheme and is copied verbatim, the rest is
/// percent-decoded. A `%` that is not followed by two hex digits, or that spells
/// `%00`, is left as a literal `%` — `url_decode_internal()` only substitutes
/// when `hex2chr()` returns a value greater than zero.
fn url_decode(url: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(url.len());
    let mut i = match url.iter().position(|&b| b == b':') {
        Some(colon) if colon > 0 => {
            out.extend_from_slice(&url[..colon]);
            colon
        }
        _ => 0,
    };
    while i < url.len() {
        let c = url[i];
        if c == 0 {
            break;
        }
        if c == b'%' && url.len() - i >= 3 {
            if let Some(val) = hex2chr(&url[i + 1..i + 3]).filter(|&v| v > 0) {
                out.push(val);
                i += 3;
                continue;
            }
        }
        out.push(c);
        i += 1;
    }
    out
}

/// `hex-ll.c::hex2chr`: two hex digits as a byte, or `None` when either is not
/// a hex digit.
fn hex2chr(pair: &[u8]) -> Option<u8> {
    let hi = (pair[0] as char).to_digit(16)?;
    let lo = (pair[1] as char).to_digit(16)?;
    Some((hi * 16 + lo) as u8)
}

const URL_ALPHADIGIT: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789";
/// `urlmatch.c`'s `URL_SCHEME_CHARS`.
const URL_SCHEME_EXTRA: &[u8] = b"+.-";
/// `urlmatch.c`'s `URL_HOST_CHARS` extras — `[:]` is there for IPv6 literals.
const URL_HOST_EXTRA: &[u8] = b".-_[:]";
/// `urlmatch.c`'s `URL_UNSAFE_CHARS`; the `0x00-0x1F`/`0x7F-0xFF` half is a
/// range test rather than a set.
const URL_UNSAFE_CHARS: &[u8] = b" <>\"%{}|\\^`";
/// `urlmatch.c`'s `URL_RESERVED` — `URL_GEN_RESERVED URL_SUB_RESERVED`.
const URL_RESERVED: &[u8] = b":/?#[]@!$&'()*+,;=";

/// `strspn`: how many leading bytes of `s` are in `set`.
fn strspn(s: &[u8], set: &[u8]) -> usize {
    s.iter().take_while(|b| set.contains(b)).count()
}

/// `strcspn`: how many leading bytes of `s` are *not* in `set`.
fn strcspn(s: &[u8], set: &[u8]) -> usize {
    s.iter().take_while(|b| !set.contains(b)).count()
}

/// `urlmatch.c::append_normalized_escapes` with `esc_extra` empty and `esc_ok`
/// always `URL_RESERVED`, which is how `url_normalize_1()` calls it. Returns
/// `false` for a `%` not followed by two hex digits.
fn append_normalized_escapes(buf: &mut Vec<u8>, from: &[u8]) -> bool {
    let mut i = 0usize;
    while i < from.len() {
        let mut ch = from[i];
        i += 1;
        let mut was_esc = false;
        if ch == b'%' {
            if from.len() - i < 2 {
                return false;
            }
            let Some(val) = hex2chr(&from[i..i + 2]) else { return false };
            ch = val;
            i += 2;
            was_esc = true;
        }
        if ch <= 0x1F
            || ch >= 0x7F
            || URL_UNSAFE_CHARS.contains(&ch)
            || (was_esc && URL_RESERVED.contains(&ch))
        {
            buf.extend_from_slice(format!("%{ch:02X}").as_bytes());
        } else {
            buf.push(ch);
        }
    }
    true
}

/// `urlmatch.c::url_normalize` (that is, `url_normalize_1` with `allow_globs`
/// off). Returns the normalized URL, or `None` for a URL it rejects — which is
/// all `check_submodule_url()` needs, since it only asks whether the result is
/// `NULL` and whether decoding it yields a newline.
fn url_normalize(url: &str) -> Option<String> {
    let url = url.as_bytes();
    let mut url_len = url.len();
    let mut norm: Vec<u8> = Vec::with_capacity(url_len);

    // Scheme plus the `://` suffix, lowercased; no %-escapes allowed, and the
    // first character must be alphabetic.
    let spanned = strspn(url, &[URL_ALPHADIGIT, URL_SCHEME_EXTRA].concat());
    if spanned == 0
        || !url[0].is_ascii_alphabetic()
        || spanned + 3 > url_len
        || url[spanned] != b':'
        || url[spanned + 1] != b'/'
        || url[spanned + 2] != b'/'
    {
        return None;
    }
    let mut pos = 0usize;
    url_len -= spanned + 3;
    for _ in 0..spanned + 3 {
        norm.push(url[pos].to_ascii_lowercase());
        pos += 1;
    }

    // Any `user:password@`, with its %-escapes normalized.
    let slash = pos + strcspn(&url[pos..], b"/?#");
    let at = url[pos..].iter().position(|&b| b == b'@').map(|i| pos + i);
    if let Some(at) = at.filter(|&a| a < slash) {
        if at > pos && !append_normalized_escapes(&mut norm, &url[pos..at]) {
            return None;
        }
        norm.push(b'@');
        url_len -= (at + 1) - pos;
        pos = at + 1;
    }

    // The host, excluding any port; no %-escapes allowed. Only a `file:` URL
    // may leave it out.
    let mut has_host = false;
    if url_len == 0 || matches!(url.get(pos), Some(b':' | b'/' | b'?' | b'#')) {
        if !norm.starts_with(b"file:") {
            return None;
        }
    } else {
        has_host = true;
    }
    let mut colon = slash - 1;
    while colon > pos && url[colon] != b':' && url[colon] != b']' {
        colon -= 1;
    }
    if url[colon] != b':' {
        colon = slash;
    } else if !has_host && colon < slash && colon + 1 != slash {
        // `file:` URLs may not have a port number.
        return None;
    }
    if strspn(&url[pos..], &[URL_ALPHADIGIT, URL_HOST_EXTRA].concat()) < colon - pos {
        return None;
    }
    while pos < colon {
        norm.push(url[pos].to_ascii_lowercase());
        pos += 1;
    }

    // The port, with leading zeros dropped and the scheme's default removed.
    if colon < slash {
        pos += 1;
        pos += strspn(&url[pos..], b"0");
        if pos == slash && url[pos - 1] == b'0' {
            pos -= 1;
        }
        let digits = &url[pos..slash];
        if pos == slash {
            // `:` with no number, same as the default.
        } else if digits == b"80" && norm.starts_with(b"http:") {
        } else if digits == b"443" && norm.starts_with(b"https:") {
        } else {
            if strspn(digits, b"0123456789") < digits.len() {
                return None;
            }
            // A 16-bit port, and 0 means "next available" so it is not one.
            let pnum: u64 = if digits.len() <= 5 {
                std::str::from_utf8(digits).ok()?.parse().unwrap_or(0)
            } else {
                0
            };
            if pnum == 0 || pnum > 65535 {
                return None;
            }
            norm.push(b':');
            norm.extend_from_slice(digits);
        }
        pos = slash;
    }

    // The path, with `.`/`..` segments resolved and a leading `/` added if it
    // is missing, being careful not to unescape any delimiter.
    let path_off = norm.len();
    norm.push(b'/');
    if url.get(pos) == Some(&b'/') {
        pos += 1;
    }
    loop {
        let seg_start_off = norm.len();
        let next_slash = pos + strcspn(&url[pos..], b"/?#");
        let mut skip_add_slash = false;
        if !append_normalized_escapes(&mut norm, &url[pos..next_slash]) {
            return None;
        }
        match &norm[seg_start_off..] {
            b"." => {
                // Drop the `.`, but never the path's leading `/`.
                if seg_start_off == path_off + 1 {
                    norm.truncate(norm.len() - 1);
                    skip_add_slash = true;
                } else {
                    norm.truncate(norm.len() - 2);
                }
            }
            b".." => {
                // Drop the `..` and the segment before it, but never the
                // leading `/` — with nothing before it the URL is invalid.
                let mut prev = norm.len() - 3;
                if prev == path_off {
                    return None;
                }
                loop {
                    prev -= 1;
                    if norm[prev] == b'/' {
                        break;
                    }
                }
                if prev == path_off {
                    norm.truncate(prev + 1);
                    skip_add_slash = true;
                } else {
                    norm.truncate(prev);
                }
            }
            _ => {}
        }
        pos = next_slash;
        if url.get(pos) != Some(&b'/') {
            break;
        }
        pos += 1;
        if !skip_add_slash {
            norm.push(b'/');
        }
    }

    // Whatever query or fragment is left, %-escapes normalized.
    if pos < url.len() && !append_normalized_escapes(&mut norm, &url[pos..]) {
        return None;
    }
    // Everything appended above is ASCII: an escape or a byte that passed the
    // `>= 0x7F` test.
    String::from_utf8(norm).ok()
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

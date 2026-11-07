//! What git's fetch side writes to stderr while a pack comes in.
//!
//! Two sources, as in git:
//!
//!   * the server's own messages, which `fetch-pack`'s sideband demultiplexer
//!     copies out as `remote: ` lines (`sideband_demux()`, fetch-pack.c:895, into
//!     `demultiplex_sideband()`, sideband.c:301) — [`RemoteOutput`];
//!   * the meters of the process `get_pack()` spawns to store the pack
//!     (fetch-pack.c:958-1100): `index-pack`'s `Receiving objects` and
//!     `Resolving deltas`, or `unpack-objects`' `Unpacking objects` —
//!     [`Meters`], which the vendored fetch reports into as a progress tree.
//!
//! Whether the server sends progress at all is `args->no_progress`, decided by
//! the command ([`transport_progress`] for `fetch` and `clone`) and put on the
//! wire by the vendored protocol.

use std::io::IsTerminal;
use std::sync::atomic::AtomicUsize;
use std::sync::{Arc, Mutex, MutexGuard};

use prodash::messages::MessageLevel;
use prodash::progress::{Id, Step, StepShared};

use crate::progress::Meter;

/// `transport->progress` as `transport_set_verbosity()` decides it
/// (transport.c:1301-1312): `--progress`/`--no-progress` when given, otherwise a
/// terminal on stderr without `-q`.
pub(crate) fn transport_progress(quiet: bool, force_progress: Option<bool>) -> bool {
    force_progress.unwrap_or_else(|| !quiet && std::io::stderr().is_terminal())
}

/// Hand remote-curl the `verbosity` option for a command whose `OPT__VERBOSITY()` count is `verbosity`:
/// `transport_set_verbosity()` caps `transport->verbose` at 3 and makes any quiet count `-1`
/// (transport.c:1338-1344), and `standard_options()` sends one more than that (transport-helper.c:371-379).
/// Only an http transport has a remote-curl; any other transport's options pass through untouched.
pub(crate) fn with_helper_verbosity(mut options: Box<dyn std::any::Any>, verbosity: i32) -> Box<dyn std::any::Any> {
    use gix::protocol::transport::client::blocking_io::http;
    if let Some(opts) = options.downcast_mut::<http::Options>() {
        opts.verbosity = verbosity.clamp(-1, 3) + 1;
    }
    options
}

/// `check_objects()` (builtin/index-pack.c:278-294): the `Checking objects` meter
/// a `--strict` `index-pack -v` draws after the pack is resolved.
///
/// ```c
/// max = get_max_object_index(the_repository);
/// if (verbose)
///         progress = start_delayed_progress(the_repository,
///                                           _("Checking objects"), max);
/// for (i = 0; i < max; i++) {
///         foreign_nr += check_object(get_indexed_object(the_repository, i));
///         display_progress(progress, i + 1);
/// }
/// stop_progress(&progress);
/// ```
///
/// `max` is not an object count but the size of the child's object hash table
/// (`get_max_object_index()`, object.c:18-21), so the meter counts its slots. The
/// check itself — every linked object present — is the vendored fetch's
/// connectivity check; only the meter is drawn here.
///
/// `grafts` is the shallow file the child reads: the commits
/// `is_repository_shallow()` registers the first time a commit is parsed.
pub(crate) fn check_objects(
    repo: &gix::Repository,
    bundle: &gix::odb::pack::bundle::write::Outcome,
    grafts: &[gix::ObjectId],
    verbose: bool,
) -> Result<(), crate::progress::DelayError> {
    if !verbose {
        return Ok(());
    }
    let max = obj_hash_size(parsed_objects(repo, bundle, grafts));
    let mut meter = Meter::delayed("Checking objects", max, true)?;
    for i in 0..max {
        meter.set(i + 1);
    }
    meter.stop("done");
    Ok(())
}

/// The commits `is_repository_shallow()` reads from the repository's shallow
/// file (shallow.c:63-95), for [`check_objects`].
pub(crate) fn shallow_grafts(repo: &gix::Repository) -> Vec<gix::ObjectId> {
    repo.shallow_commits()
        .ok()
        .flatten()
        .map(|commits| commits.iter().copied().collect())
        .unwrap_or_default()
}

/// `obj_hash_size` once `n` objects have gone through `create_object()`
/// (object.c:147-163): the table starts empty, becomes 32 slots on the first
/// insert, and doubles whenever `obj_hash_size - 1 <= nr_objs * 2`.
fn obj_hash_size(n: usize) -> usize {
    let mut size = 0usize;
    for nr in 0..n {
        if size == 0 || size - 1 <= nr * 2 {
            size = if size < 32 { 32 } else { size * 2 };
        }
    }
    size
}

/// How many distinct objects a `--strict` `index-pack` has created by the time
/// `check_objects()` runs: every object `sha1_object()` parsed
/// (builtin/index-pack.c:928-966) plus everything that parse and `fsck_walk()`
/// looked up.
///
///   * a blob is `lookup_blob()`ed;
///   * a commit's `parse_commit_buffer()` looks up its tree, then — after
///     `lookup_commit_graft()` has registered every shallow commit
///     (commit.c:554, shallow.c:63-95) — its parents, which a shallow commit does
///     not have (commit.c:566-569);
///   * a tag's `parse_tag_buffer()` looks up its target (tag.c:168-175);
///   * a tree's `fsck_walk_tree()` looks up each entry but a gitlink
///     (fsck.c:368-398).
///
/// The bases a thin pack was completed with are not among them:
/// `fix_unresolved_deltas()` appends them without `sha1_object()`.
fn parsed_objects(
    repo: &gix::Repository,
    bundle: &gix::odb::pack::bundle::write::Outcome,
    grafts: &[gix::ObjectId],
) -> usize {
    use gix::objs::{CommitRefIter, TagRef, TreeRefIter};

    let Some(index_path) = &bundle.index_path else {
        return 0;
    };
    let hash = repo.object_hash();
    let Ok(index) = gix::odb::pack::index::File::at(index_path, hash) else {
        return 0;
    };
    let bases: std::collections::HashSet<_> = bundle.thin_pack_bases.iter().collect();
    let mut created = std::collections::HashSet::new();
    let mut grafts_registered = false;
    for entry in index.iter().filter(|e| !bases.contains(&e.oid)) {
        created.insert(entry.oid);
        let Ok(object) = repo.find_object(entry.oid) else { continue };
        match object.kind {
            gix::object::Kind::Blob => {}
            gix::object::Kind::Commit => {
                let mut commit = CommitRefIter::from_bytes(&object.data, hash);
                let Ok(tree) = commit.tree_id() else { continue };
                created.insert(tree);
                if !grafts_registered {
                    grafts_registered = true;
                    created.extend(grafts.iter().copied());
                }
                if !grafts.contains(&entry.oid) {
                    created.extend(commit.parent_ids());
                }
            }
            gix::object::Kind::Tag => {
                if let Ok(tag) = TagRef::from_bytes(&object.data, hash) {
                    created.insert(tag.target());
                }
            }
            gix::object::Kind::Tree => {
                for tree_entry in TreeRefIter::from_bytes(&object.data, hash) {
                    let Ok(tree_entry) = tree_entry else { break };
                    if tree_entry.mode.is_commit() {
                        continue;
                    }
                    if !(tree_entry.mode.is_tree() || tree_entry.mode.is_blob_or_symlink()) {
                        break;
                    }
                    created.insert(tree_entry.oid.to_owned());
                }
            }
        }
    }
    created.len()
}

/// `unpack_limit` after `fetch_pack_setup()` (fetch-pack.c:2015-2026): 100, unless
/// `fetch.unpackLimit` or else `transfer.unpackLimit` is set to a non-negative
/// value.
pub(crate) fn unpack_limit(repo: &gix::Repository) -> u64 {
    let snapshot = repo.config_snapshot();
    let non_negative = |key: &str| snapshot.integer(key).and_then(|v| u64::try_from(v).ok());
    non_negative("fetch.unpackLimit")
        .or_else(|| non_negative("transfer.unpackLimit"))
        .unwrap_or(100)
}

/// The inputs `get_pack()` decides its child and that child's verbosity from.
#[derive(Clone, Copy)]
pub(crate) struct Plan {
    /// `!args->no_progress`.
    pub progress: bool,
    /// `args->quiet`.
    pub quiet: bool,
    /// `args->keep_pack`: `--keep`, and always for `clone` (builtin/clone.c:1362).
    pub keep_pack: bool,
    /// [`unpack_limit`].
    pub unpack_limit: u64,
    /// `args->from_promisor || fsck_objects`, the conditions besides the pack's
    /// size that make `get_pack()` choose `index-pack` (fetch-pack.c:1007).
    pub index_pack_required: bool,
}

impl Plan {
    /// Whether `get_pack()` reads the pack header itself to count the objects,
    /// then hands it on as `--pack_header` (fetch-pack.c:989-998, 1048-1051).
    fn passes_header(&self) -> bool {
        !self.keep_pack && self.unpack_limit != 0
    }

    /// Whether a pack of `nr_objects` goes to `unpack-objects` rather than
    /// `index-pack` (fetch-pack.c:994-997, 1007, 1040-1041). `do_keep` starts as `args->keep_pack` and only the
    /// header read changes it, so with no header read (`fetch.unpackLimit=0`) every pack not kept is unpacked.
    pub(crate) fn unpacks(&self, nr_objects: usize) -> bool {
        !self.do_keep(nr_objects) && !self.index_pack_required
    }

    /// `do_keep` after the header read (fetch-pack.c:967, 991-998).
    pub(crate) fn do_keep(&self, nr_objects: usize) -> bool {
        self.keep_pack || (self.passes_header() && (nr_objects as u64) >= self.unpack_limit)
    }

    /// `index-pack -v` (fetch-pack.c:1013-1014).
    pub(crate) fn index_pack_verbose(&self) -> bool {
        !self.quiet && self.progress
    }

    /// `unpack-objects` without `-q` (fetch-pack.c:1043-1044), which on its own
    /// is `quiet = !isatty(2)` (builtin/unpack-objects.c:626).
    fn unpack_objects_verbose(&self) -> bool {
        self.index_pack_verbose() && std::io::stderr().is_terminal()
    }
}

/// `get_pack()` for a pack whose sideband stream ended before it was taken whole (fetch-pack.c:960-1100), after
/// the demultiplexer said so (see [`gix::remote::fetch::PackStream`]). It prints what git prints on its way to
/// the `die()` the caller exits 128 with, and leaves the repository as the child left it.
///
/// `smart_http` is a transport that runs through remote-curl, whose protocol v0/v1 `fetch-pack` child is not told
/// to keep the pack: `TRANS_OPT_KEEP` is among the helper's `unsupported_options` (transport-helper.c:275-280)
/// and `fetch_git()` passes no `--keep` (remote-curl.c:1203-1234). A v2 fetch runs `fetch_pack()` in this process
/// with the option as it was set (transport.c:490).
///
/// ```c
/// if (!args->keep_pack && unpack_limit && !index_pack_args) {
///         if (read_pack_header(demux.out, &header))
///                 die(_("protocol error: bad pack header"));
///         pass_header = 1;
///         if (ntohl(header.hdr_entries) < unpack_limit)
///                 do_keep = 0;
///         else
///                 do_keep = 1;
/// }
/// ...
/// if (do_keep && (pack_lockfiles || fsck_objects)) {
///         ...
///         if (!is_well_formed)
///                 die(_("fetch-pack: invalid index-pack output"));
/// ...
/// ret = finish_command(&cmd);
/// if (!ret || (args->check_self_contained_and_connected && ret == 1))
///         ...
/// else
///         die(_("%s failed"), cmd_name);
/// if (use_sideband && finish_async(&demux))
///         die(_("error in sideband demultiplexer"));
/// ```
///
/// `pack_lockfiles` is never NULL here: the fetch's own `transport->pack_lockfiles` or remote-curl's
/// `--lock-pack`. A child that read everything leaves its objects: `unpack-objects` writes each as it goes, and
/// `index-pack`'s pack stays with its `.keep`, which a fetch that dies in this process removes on exit
/// (`unlock_pack_atexit()`, builtin/fetch.c:2639) and a `fetch-pack` child cannot report.
pub(crate) fn pack_stream_died(
    repo: &gix::Repository,
    plan: Plan,
    smart_http: bool,
    stream: &gix::remote::fetch::PackStream,
) {
    use gix::protocol::transport::Protocol;
    let in_process = !smart_http || stream.protocol == Protocol::V2;
    let keep_pack = plan.keep_pack && in_process;
    let pass_header = !keep_pack && plan.unpack_limit != 0;
    // `read_pack_header()` (packfile.c): twelve bytes, the signature and a version `pack_version_ok()` accepts.
    let header = &stream.header;
    let nr_objects = (header.len() == 12 && header.starts_with(b"PACK") && matches!(header[7], 2 | 3) && header[4..7] == [0, 0, 0])
        .then(|| u64::from(u32::from_be_bytes(header[8..12].try_into().expect("four bytes"))));
    if pass_header && nr_objects.is_none() {
        eprintln!("fatal: protocol error: bad pack header");
        return;
    }
    let nr_objects = nr_objects.unwrap_or(0);
    let do_keep = keep_pack || (pass_header && nr_objects >= plan.unpack_limit);
    let unpacks = !do_keep && !plan.index_pack_required;
    if unpacks {
        // The pack the vendored writer made goes first: `unpack-objects` writes each object through
        // `odb_write_object()`, which skips one the repository already has (odb.c:993-999), and these are
        // only in that pack. A fresh handle answers "already has" without it.
        if let Some(bundle) = &stream.bundle {
            let rev = bundle.data_path.as_ref().map(|data| data.with_extension("rev"));
            for path in [bundle.data_path.clone(), bundle.index_path.clone(), bundle.keep_path.clone(), rev]
                .into_iter()
                .flatten()
            {
                let _ = std::fs::remove_file(path);
            }
        }
        let fresh = gix::open(repo.git_dir()).ok();
        let retained = stream.retained.as_deref().unwrap_or_default();
        match super::unpack_objects::unpack_received(fresh.as_ref().unwrap_or(repo), retained) {
            Ok(()) => {}
            Err(message) => {
                if let Some(message) = message {
                    eprintln!("fatal: {message}");
                }
                eprintln!("fatal: unpack-objects failed");
                return;
            }
        }
    } else if stream.consumer_failed {
        // `index-pack --stdin` opened `objects/pack/tmp_pack_XXXXXX` before it read the header
        // (`open_pack_file()`, builtin/index-pack.c:359-367, `odb_mkstemp()` mode 0444, odb.c:72-91) and its `die()`
        // leaves it there. What it had flushed into it is not kept: the vendored writer's own temporary holds a
        // rewritten pack, so the file is left empty, as the `index-pack` port leaves it.
        let dir = repo.objects.store_ref().path().join("pack");
        let _ = crate::tmp_objdir::git_mkstemp_mode(dir.join("tmp_pack_XXXXXX").as_os_str(), 0o444);
        // `index-pack`'s `fill()` (builtin/index-pack.c:270-289).
        eprintln!("fatal: early EOF");
        if do_keep {
            eprintln!("fatal: fetch-pack: invalid index-pack output");
        } else {
            eprintln!("fatal: index-pack failed");
        }
        return;
    } else if let Some(bundle) = &stream.bundle {
        super::fetch::finish_kept_pack(repo, bundle);
        // `index-pack` writes a `.keep` only when told `--keep`, for a pack `get_pack()` keeps
        // (fetch-pack.c:1019-1020); an `index-pack` chosen by `fsck_objects` or a promisor fetch alone writes none.
        if in_process || !do_keep {
            if let Some(keep) = &bundle.keep_path {
                let _ = std::fs::remove_file(keep);
            }
        }
    }
    if stream.disconnected {
        eprintln!("fatal: error in sideband demultiplexer");
    } else if stream.response_end_consumed {
        // `do_check_stateless_delimiter()` reads past the response-end packet the demultiplexer took
        // (fetch-pack.c:1847, pkt-line.c:364-372).
        eprintln!("fatal: the remote end hung up unexpectedly");
    }
}

/// How much of the pack `index-pack`'s first `xread()` takes when it reads the
/// header itself, before `start_progress()`. The read asks for its whole
/// `DEFAULT_IO_BUFFER_SIZE` buffer (builtin/index-pack.c:148, 128 KiB), but the
/// demultiplexer hands the pack over through a pipe, which holds 64 KiB: stock
/// 2.55.0 cloning a 62 KiB pack shows no throughput on `Receiving objects`, and a
/// 64 KiB one does, every time. Only a read after the meter started creates its
/// throughput (`display_throughput()`, progress.c:205-211).
const FIRST_READ_BEFORE_METER: u64 = 64 * 1024;

/// The progress tree the vendored fetch reports into, drawing git's meters from
/// the few nodes that correspond to them and ignoring the rest.
#[derive(Clone)]
pub(crate) struct Meters {
    role: Role,
    state: Arc<Mutex<State>>,
}

#[derive(Clone, Copy, PartialEq)]
enum Role {
    Other,
    /// Bytes read from the connection into the pack writer.
    PackBytes,
    /// Objects taken from the pack stream.
    Objects,
    /// Deltas resolved.
    Deltas,
    /// A thin pack's base objects taken from the object database.
    LocalBaseObjects,
}

struct State {
    plan: Plan,
    bytes: u64,
    /// Bytes read before the objects meter's throughput may start counting.
    first_read: u64,
    unpacking: bool,
    objects: Option<Meter>,
    deltas: Option<Meter>,
    local_base_objects: usize,
    /// `progress_title`, for an `index-pack -v` run on its own; the fetch
    /// path's child never gets one.
    title: Option<&'static str>,
    /// [`FIRST_READ_BEFORE_METER`] unless the caller knows better.
    first_read_override: Option<u64>,
    /// `index-pack --show-resolving-progress` without `-v`: only `Resolving deltas` is drawn.
    resolving_only: bool,
    /// Where the meters write instead of stderr, from whichever thread draws them.
    relay: Option<Relay>,
}

/// A sink for meter output that any thread may write to.
pub(crate) type Relay = Arc<dyn Fn(&[u8]) + Send + Sync>;

impl Meters {
    pub(crate) fn new(plan: Plan) -> Self {
        Meters {
            role: Role::Other,
            state: Arc::new(Mutex::new(State {
                plan,
                bytes: 0,
                first_read: 0,
                unpacking: false,
                objects: None,
                deltas: None,
                local_base_objects: 0,
                title: None,
                first_read_override: None,
                resolving_only: false,
                relay: None,
            })),
        }
    }

    /// The meters of an `index-pack -v` run directly rather than by `get_pack()`:
    /// `title` is `progress_title ? progress_title : from_stdin ? "Receiving
    /// objects" : "Indexing objects"` (builtin/index-pack.c:1258-1263), and
    /// `first_read` how many bytes its first `fill()` took before
    /// `start_progress()` — every byte for a pack that is not read from stdin,
    /// which reports no throughput at all.
    pub(crate) fn index_pack(title: &'static str, first_read: u64) -> Self {
        let meters = Meters::new(Plan {
            progress: true,
            quiet: false,
            keep_pack: true,
            unpack_limit: 0,
            index_pack_required: true,
        });
        {
            let mut state = meters.state();
            state.title = Some(title);
            state.first_read_override = Some(first_read);
        }
        meters
    }

    /// The meters of the `index-pack` child `receive-pack` runs for a kept pack: no `-v`, so no
    /// `Receiving objects`, and `--show-resolving-progress` when it is not quiet and has the
    /// sideband muxer for a stderr (object-file.c:1401-1402), so `Resolving deltas` is drawn
    /// (builtin/index-pack.c:1340-1343) and reaches the pusher through `relay`, the
    /// `copy_to_sideband()` muxer (builtin/receive-pack.c:481, :2312-2324).
    pub(crate) fn resolving_only(relay: Relay) -> Self {
        let meters = Meters::index_pack("Receiving objects", u64::MAX);
        {
            let mut state = meters.state();
            state.resolving_only = true;
            state.relay = Some(relay);
        }
        meters
    }

    fn state(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn child(&self, role: Role) -> Self {
        Meters {
            role,
            state: Arc::clone(&self.state),
        }
    }
}

fn role_of(id: Id) -> Role {
    use gix::odb::pack::{bundle, index};
    let is = |other: gix::progress::Id| id == other;
    if is(bundle::write::ProgressId::ReadPackBytes.into()) {
        Role::PackBytes
    } else if is(index::write::ProgressId::IndexObjects.into()) {
        Role::Objects
    } else if is(index::write::ProgressId::ResolveDeltas.into()) {
        Role::Deltas
    } else if is(index::write::ProgressId::LocalBaseObjects.into()) {
        Role::LocalBaseObjects
    } else {
        Role::Other
    }
}

impl prodash::NestedProgress for Meters {
    type SubProgress = Meters;

    fn add_child(&mut self, _name: impl Into<String>) -> Self::SubProgress {
        self.child(Role::Other)
    }

    fn add_child_with_id(&mut self, _name: impl Into<String>, id: Id) -> Self::SubProgress {
        self.child(role_of(id))
    }
}

impl prodash::Count for Meters {
    fn set(&self, step: Step) {
        if self.role == Role::Objects {
            if let Some(meter) = self.state().objects.as_mut() {
                // `display_progress(progress, i + 1)` per object
                // (builtin/index-pack.c:1287, builtin/unpack-objects.c:604).
                meter.set(step);
            }
        }
    }

    fn step(&self) -> Step {
        0
    }

    fn inc_by(&self, step: Step) {
        match self.role {
            Role::PackBytes => {
                let mut state = self.state();
                state.bytes += step as u64;
                let (bytes, first_read) = (state.bytes, state.first_read);
                // `fill()` reports every read (builtin/index-pack.c:333-334), and
                // `use()` every consumed chunk (builtin/unpack-objects.c:103).
                if let (Some(meter), true) = (state.objects.as_mut(), bytes > first_read) {
                    meter.throughput(bytes);
                }
            }
            Role::Deltas => {
                let mut state = self.state();
                let relay = state.relay.clone();
                if let Some(meter) = state.deltas.as_mut() {
                    drawn(relay, || meter.advance(step));
                }
            }
            _ => {}
        }
    }

    fn counter(&self) -> StepShared {
        Arc::new(AtomicUsize::new(0))
    }
}

impl prodash::Progress for Meters {
    fn init(&mut self, max: Option<Step>, _unit: Option<prodash::Unit>) {
        let mut state = self.state();
        let plan = state.plan;
        match self.role {
            Role::Objects => {
                let nr_objects = max.unwrap_or(0);
                state.unpacking = plan.unpacks(nr_objects);
                let mut meter = if state.unpacking {
                    Meter::counted("Unpacking objects", nr_objects, plan.unpack_objects_verbose())
                } else {
                    // `from_stdin ? _("Receiving objects") : ...` (builtin/index-pack.c:1258-1263).
                    Meter::counted(
                        state.title.unwrap_or("Receiving objects"),
                        nr_objects,
                        plan.index_pack_verbose() && !state.resolving_only,
                    )
                };
                // With the header passed on, every read the child makes follows
                // `start_progress()`; `unpack-objects` likewise reports each
                // object's bytes after it. Only an `index-pack` that read the
                // header itself had a first read the meter never saw.
                if plan.passes_header() {
                    state.first_read = 0;
                    meter.throughput(state.bytes);
                } else {
                    state.first_read = state.first_read_override.unwrap_or(FIRST_READ_BEFORE_METER);
                    if state.bytes > state.first_read {
                        meter.throughput(state.bytes);
                    }
                }
                state.objects = Some(meter);
            }
            // `resolve_deltas()` starts no meter for a pack without deltas
            // (builtin/index-pack.c:1333-1343), and `unpack-objects` has none.
            Role::Deltas => {
                let nr_deltas = max.unwrap_or(0);
                if nr_deltas != 0 && !state.unpacking {
                    let mut meter = Meter::counted("Resolving deltas", nr_deltas, plan.index_pack_verbose());
                    // `threaded_second_pass()` displays the resolved count before
                    // it takes its first object (builtin/index-pack.c:1113-1115).
                    drawn(state.relay.clone(), || meter.set(0));
                    state.deltas = Some(meter);
                }
            }
            Role::LocalBaseObjects => state.local_base_objects = max.unwrap_or(0),
            Role::Other | Role::PackBytes => {}
        }
    }

    fn set_name(&mut self, _name: String) {}

    fn name(&self) -> Option<String> {
        None
    }

    fn id(&self) -> Id {
        prodash::progress::UNKNOWN
    }

    fn message(&self, _level: MessageLevel, _message: String) {}
}

impl Drop for Meters {
    fn drop(&mut self) {
        match self.role {
            // `stop_progress()` after the last object (builtin/index-pack.c:1290,
            // builtin/unpack-objects.c:607).
            Role::Objects => {
                if let Some(meter) = self.state().objects.take() {
                    meter.stop("done");
                }
            }
            // `conclude_pack()` (builtin/index-pack.c:1370-1397): a thin pack
            // completed from the object database says how.
            Role::Deltas => {
                let mut state = self.state();
                if let Some(meter) = state.deltas.take() {
                    let msg = match state.local_base_objects {
                        0 => "done".to_string(),
                        1 => "completed with 1 local object".to_string(),
                        n => format!("completed with {n} local objects"),
                    };
                    drawn(state.relay.clone(), || meter.stop(&msg));
                }
            }
            _ => {}
        }
    }
}

/// Run `draw` with what it writes going to `relay` rather than stderr when there is one. The
/// relay is installed for the call itself, because the vendored indexer resolves deltas on
/// worker threads and [`crate::progress::with_relay`] is per thread.
fn drawn(relay: Option<Relay>, draw: impl FnOnce()) {
    match relay {
        Some(relay) => crate::progress::with_relay(move |bytes: &[u8]| relay(bytes), draw),
        None => draw(),
    }
}

/// The stderr half of `fetch-pack`'s sideband demultiplexer: the `remote: `
/// lines, shared between the vendored transport, which feeds it every band 2
/// and band 3 packet, and the command, which flushes a line the stream left
/// unterminated once the pack is in.
#[derive(Clone)]
pub(crate) struct RemoteOutput(Arc<Mutex<super::push_proto::Sideband>>);

impl RemoteOutput {
    pub(crate) fn new(repo: &gix::Repository) -> Self {
        RemoteOutput(Arc::new(Mutex::new(super::push_proto::Sideband::new(repo))))
    }

    /// [`RemoteOutput::new`] without a repository to read `color.remote` from.
    pub(crate) fn plain() -> Self {
        RemoteOutput(Arc::new(Mutex::new(super::push_proto::Sideband::plain())))
    }

    fn lock(&self) -> MutexGuard<'_, super::push_proto::Sideband> {
        self.0.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// The receiver the vendored protocol hands the packets to.
    pub(crate) fn handler(&self) -> gix::protocol::fetch::Sideband {
        let out = self.clone();
        gix::protocol::fetch::Sideband(Arc::new(Mutex::new(move |is_error: bool, data: &[u8]| {
            let mut sideband = out.lock();
            if is_error {
                sideband.remote_error(data);
            } else {
                sideband.progress(data);
            }
        })))
    }

    /// The `cleanup:` of `demultiplex_sideband()` at the stream's flush
    /// (sideband.c:424-432).
    pub(crate) fn finish(&self) {
        self.lock().finish();
    }
}

#[cfg(test)]
mod tests {
    use super::obj_hash_size;

    /// The `Checking objects` totals stock 2.55.0 draws for clones whose
    /// `index-pack` created 16, 17, 32 and 33 objects: the table doubles one
    /// insert after it is half full, not when it is full.
    #[test]
    fn the_object_hash_grows_as_create_object_grows_it() {
        assert_eq!(obj_hash_size(0), 0, "nothing created, nothing allocated");
        assert_eq!(obj_hash_size(1), 32);
        assert_eq!(obj_hash_size(16), 32);
        assert_eq!(obj_hash_size(17), 64);
        assert_eq!(obj_hash_size(32), 64);
        assert_eq!(obj_hash_size(33), 128);
    }
}

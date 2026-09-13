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
    /// `index-pack` (fetch-pack.c:994-997, 1007, 1040-1041).
    fn unpacks(&self, nr_objects: usize) -> bool {
        self.passes_header() && (nr_objects as u64) < self.unpack_limit && !self.index_pack_required
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
}

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
            })),
        }
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
                if let Some(meter) = self.state().deltas.as_mut() {
                    meter.advance(step);
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
                    Meter::counted("Receiving objects", nr_objects, plan.index_pack_verbose())
                };
                // With the header passed on, every read the child makes follows
                // `start_progress()`; `unpack-objects` likewise reports each
                // object's bytes after it. Only an `index-pack` that read the
                // header itself had a first read the meter never saw.
                if plan.passes_header() {
                    state.first_read = 0;
                    meter.throughput(state.bytes);
                } else {
                    state.first_read = FIRST_READ_BEFORE_METER;
                    if state.bytes > FIRST_READ_BEFORE_METER {
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
                    meter.set(0);
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
                    meter.stop(&msg);
                }
            }
            _ => {}
        }
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

//! `git fast-import` — read a command/data stream on stdin and write the objects
//! and refs it describes into the current repository.
//!
//! Covered: the whole stream language as documented in `git-fast-import(1)` —
//! `blob`, `commit` (with `mark`, `original-oid`, `author`, `committer`,
//! `encoding`, `data`, `from`, `merge`, and the `M`/`D`/`C`/`R`/`N`/`deleteall`
//! file-change commands, inline or by dataref), `tag`, `reset`, `alias`,
//! `checkpoint`, `progress`, `done`, `get-mark`, `cat-blob`, `ls`, `feature` and
//! `option`, plus `#` comment lines, both `data` forms (byte count and `<<`
//! delimiter), C-style quoted paths, and the optional trailing LF after every
//! command. Commit and tag objects are assembled byte-for-byte the way
//! `fast-import.c` assembles them — identity strings are copied verbatim from the
//! stream, the author line defaults to the committer line, mode words are
//! canonicalized (`644` → `100644`), and trees are sorted with git's
//! `base_name_compare` — so the object ids match stock git's exactly.
//!
//! Command line: git 2.56's `parse_options()` over `fast_import_options[]` —
//! `--force`, `--quiet`, `--stats`, `--done`, `--date-format`,
//! `--export-marks`, `--import-marks`, `--import-marks-if-exists`,
//! `--[no-]relative-marks`, `--rewrite-submodules-from/-to`,
//! `--signed-commits`, `--signed-tags`, the hidden `--allow-unsafe-features`,
//! `--export-pack-edges` and `--cat-blob-fd`, and the pack-tuning knobs
//! `--depth`, `--active-branches`, `--big-file-threshold` and
//! `--max-pack-size`, which are validated the way git validates them and then
//! ignored because they only steer packing. Names abbreviate, a value may be
//! the next argument, and `--` is accepted. As in git, the command line is
//! read only at the first stream command that is not `feature`/`option` (or
//! at EOF), so stream features are applied first and the command line
//! overrides them; only the exact `--allow-unsafe-features` spelling, found
//! by git's early scan, reaches the stream. Refs are updated at EOF (and at
//! `checkpoint`) with the reflog message `fast-import`; without `--force` a
//! branch that would lose commits is left alone with git's
//! `warning: not updating …` and the run exits 1.
//!
//! git has three distinct failure contracts here and this module reproduces
//! all three. A command line `parse_options()` refuses — an unknown or
//! ambiguous option with the usage block, a missing or unwanted value with one
//! `error:` line, a positional with the block alone, an option value
//! `usagef()` rejects — exits 129, leaving no ref, object or marks file changed
//! but leaving behind the empty `objects/pack/tmp_pack_XXXXXX` git opened
//! before it ever looked at argv, because those exits run none of the cleanup
//! `die_nicely` runs (see [`TempPack`]); help asked for mid-line takes the same
//! route out at 0, on stdout. Anything else fatal prints `fatal: <reason>` and
//! exits 128 — and, as git's `die_nicely` does, unlinks that temporary and then
//! still writes the `--export-marks` file on the way out, unless an
//! import-marks file was named and none was successfully read, in which case
//! `dump_marks` declines to overwrite an export from a half-loaded table.
//! Options take effect strictly left to right, a positional not stopping the
//! sweep, so a failure leaves every option to its left already applied — which
//! is what decides whether the marks file exists after a fatal. A lone `-h` or
//! `--help-all` is not a failure at all: `show_usage_with_options_if_asked()`
//! prints the block on *stdout*, exits 0, and runs before the repository is
//! even opened.
//!
//! Signatures follow git's `--signed-commits=`/`--signed-tags=` modes:
//! `verbatim` and `warn-verbatim` keep them (a commit's `gpgsig`/`gpgsig-sha256`
//! header is folded exactly as git folds it, one space per continuation line),
//! `strip` and `warn-strip` drop them, and `abort` refuses. The three
//! `-if-invalid` modes are accepted on the command line, as git accepts them,
//! and refused at the first signature they would have to verify: verification
//! needs a gpg driver the vendored crates do not provide.
//!
//! Ref names are taken as literally as git takes them: `new_branch()` validates
//! with `check_refname_format(name, REFNAME_ALLOW_ONELEVEL)`, so `commit main`
//! is accepted and lands in `$GIT_DIR/main` rather than under `refs/heads/`,
//! while a *deletion* of that same name is refused by `refname_is_safe()`
//! without failing the run. `refname_ok_onelevel` and `refname_is_safe` below
//! are the two ports that decide this; gix's `FullName` cannot, because it
//! implements the flagless check that rejects every lowercase one-level name.
//!
//! Not covered, each rejected with a precise message rather than guessed at:
//!   * `--rewrite-submodules-from/-to=<name>:<file>` — the marks file is read
//!     where git reads it, so a missing or corrupt one fails identically, but a
//!     stream that actually carries a gitlink to rewrite is refused.
//!
//! The in-memory trees are git's (`struct tree_entry` and `struct
//! tree_content`, see "git's in-memory trees" below): two versions per entry,
//! subtrees read only when a path walks into them, only dirty trees stored, a
//! tree deltified against its old version when that sits in the pack being
//! built, the notes tree re-fanned-out as its note count crosses a power of
//! 256, and branches loaded and evicted through the `--active-branches` LRU.
//! That shape is what the statistics block and the crash report describe.
//!
//! Storage follows git's own rule rather than a fixed choice. `end_packfile()`
//! keeps the pack it wrote and explodes it with `git unpack-objects` only when
//! `object_count <= fastimport.unpackLimit` (100 by default, falling back to
//! `transfer.unpackLimit`), so an import at or below the threshold ends as loose
//! objects and one above it ends as `pack-<hash>.{pack,idx}` — and
//! `--export-pack-edges=<file>` records one boundary line per pack *kept*, which
//! is why a small import leaves the file it created empty. [`PackLog`] is that
//! bookkeeping: which objects `store_object()` counted, and what
//! [`PackLog::flush`] then does with them. Objects are written loose as the
//! stream produces them either way, because the stream can read any of them back
//! (`cat-blob`, `ls`, `from`) before a pack would exist; a run that keeps its
//! pack unlinks the loose files it created once the pack is in place.
//!
//! The pack is git's own bytes. `store_object()` deflates each object, or its
//! `OBJ_OFS_DELTA` against the last object of its kind, at
//! `pack.compression`/`core.compression` with the transcription of zlib, and
//! `stream_blob()` deflates a blob above `--big-file-threshold` in 64 KiB pieces;
//! when the entry would push the pack past `--max-pack-size`/`pack.packSizeLimit`
//! the pack is closed and a new one started, the delta dropped
//! (`fast-import.c:1043-1065`, `:1136-1140`), so a run may leave several packs,
//! each named by the hash of its bytes, each kept or exploded on its own count,
//! each with its own `--export-pack-edges` line. The delta *decisions* are
//! git's — the same base under the same `--depth`/`pack.depth`, with the port of
//! `diff-delta.c` — which is also what the statistics block counts.
//!
//! Without `--quiet` the run ends in git's statistics block on stderr, and a
//! fatal error writes git's crash report to `$GIT_DIR/fast_import_crash_<pid>`
//! and names it after the `fatal:` line. The block's last four `pack_report`
//! counters count the mmap windows `packfile.c`'s `use_pack()` opens whenever
//! anything in the process reads an object out of a pack — fast-import's own
//! read-backs, the ref update's `parse_object()`, a `from` an existing branch.
//! This port reads through gitoxide's object store, which has no such windows,
//! and reports zeros, which is what git reports only for a run that never reads
//! a packed object. The `warning:` lines `parse_one_option()` prints are
//! ordinary stderr a caller reads, and they are reproduced.

use anyhow::{anyhow, bail, Context, Result};
use std::collections::{HashMap, HashSet};
use std::io::{BufRead, Read, Write as _};
use std::process::ExitCode;

use gix::hash::ObjectId;
use gix::objs::{Kind, Write as _};
use gix::refs::transaction::{Change, LogChange, PreviousValue, RefEdit, RefLog};
use gix::refs::{FullName, Target};

/// `usage_with_options(fast_import_usage, fast_import_options)` rendered in
/// `USAGE_NORMAL`: the `PARSE_OPT_HIDDEN` "Advanced" group is left out.
/// Measured byte for byte from stock git 2.56.0's `git fast-import -h`.
const USAGE: &str = "\
usage: git fast-import [<options>]

Common
    --date-format <fmt>   format of the commit/tag dates
    --stats               display some basic statistics (objects, packfiles and memory)
    --quiet               disable the output shown by --stats
    --force               force updating modified existing branches
    --done                require a terminating 'done' command
    --max-pack-size <n>   maximum size of each output pack file
    --big-file-threshold <n>
                          maximum size of a blob that will be deltified
    --depth <n>           maximum delta depth
    --active-branches <n> maximum number of branches to maintain active

Marks
    --import-marks <file> import marks from <file>
    --import-marks-if-exists <file>
                          import marks from <file> if it exists
    --export-marks <file> dump marks to <file>
    --[no-]relative-marks are --(import|export)-marks= paths relative to '.git/info/fast-import'?

Submodule rewrite
    --rewrite-submodules-from <name:filename>
                          rewrite object IDs for submodule <name> from <filename>
    --rewrite-submodules-to <name:filename>
                          rewrite object IDs for submodule <name> to <filename>

Signing
    --signed-commits <mode>
                          how to handle signed commits
    --signed-tags <mode>  how to handle signed tags

";

/// The same table in `USAGE_FULL` (`--help-all`), hidden entries included.
const USAGE_ALL: &str = concat!(
    "\
usage: git fast-import [<options>]

Common
    --date-format <fmt>   format of the commit/tag dates
    --stats               display some basic statistics (objects, packfiles and memory)
    --quiet               disable the output shown by --stats
    --force               force updating modified existing branches
    --done                require a terminating 'done' command
    --max-pack-size <n>   maximum size of each output pack file
    --big-file-threshold <n>
                          maximum size of a blob that will be deltified
    --depth <n>           maximum delta depth
    --active-branches <n> maximum number of branches to maintain active

Marks
    --import-marks <file> import marks from <file>
    --import-marks-if-exists <file>
                          import marks from <file> if it exists
    --export-marks <file> dump marks to <file>
    --[no-]relative-marks are --(import|export)-marks= paths relative to '.git/info/fast-import'?

Submodule rewrite
    --rewrite-submodules-from <name:filename>
                          rewrite object IDs for submodule <name> from <filename>
    --rewrite-submodules-to <name:filename>
                          rewrite object IDs for submodule <name> to <filename>

Signing
    --signed-commits <mode>
                          how to handle signed commits
    --signed-tags <mode>  how to handle signed tags

",
    "\
Advanced
    --allow-unsafe-features
                          allow unsafe mark commands from the stream
    --export-pack-edges <file>
                          dump edge commits to <file>
    --cat-blob-fd <fd>    write some responses to <fd> instead of stdout

",
);

/// `fast_import_options[]` (builtin/fast-import.c:4130-4196) as far as
/// `parse_long_opt()` reads it, in table order — the order decides which two
/// names an `ambiguous option:` sentence quotes. Only `relative-marks` lacks
/// `PARSE_OPT_NONEG`; the `OPT_BOOL`s and `--quiet` are `PARSE_OPT_NOARG`.
const OPTIONS: &[super::LongOpt] = {
    use super::Arg::{None as Flag, Required as Value};
    const fn opt(name: &'static str, neg: bool, arg: super::Arg) -> super::LongOpt {
        super::LongOpt { name, neg, arg }
    }
    &[
        opt("date-format", false, Value),
        opt("stats", false, Flag),
        opt("quiet", false, Flag),
        opt("force", false, Flag),
        opt("done", false, Flag),
        opt("max-pack-size", false, Value),
        opt("big-file-threshold", false, Value),
        opt("depth", false, Value),
        opt("active-branches", false, Value),
        opt("import-marks", false, Value),
        opt("import-marks-if-exists", false, Value),
        opt("export-marks", false, Value),
        opt("relative-marks", true, Flag),
        opt("rewrite-submodules-from", false, Value),
        opt("rewrite-submodules-to", false, Value),
        opt("signed-commits", false, Value),
        opt("signed-tags", false, Value),
        opt("allow-unsafe-features", false, Flag),
        opt("export-pack-edges", false, Value),
        opt("cat-blob-fd", false, Value),
    ]
};

/// An exit git takes without `die()`, so without the cleanup `die_nicely`
/// runs: `usage_with_options()`, `usagef()`, a `parse_options()` refusal, or
/// the help `parse_options()` prints when asked for it mid-line.
///
/// Carries the complete text, trailing LF included, the stream it goes to and
/// the status, because `parse_options()` has four shapes: the usage block on
/// stdout at 0 (help), an `error:` line alone at 129 (`PARSE_OPT_ERROR`), an
/// `error:` line and the block on stderr at 129 (unknown or ambiguous), and
/// the block alone on stderr at 129 (a positional `usage_with_options()`
/// refuses).
#[derive(Debug)]
struct UsageError {
    text: String,
    to_stdout: bool,
    code: u8,
}

impl UsageError {
    /// Text for stderr at 129, the shape of every refusal.
    fn refuse(text: String) -> anyhow::Error {
        UsageError { text, to_stdout: false, code: 129 }.into()
    }
}

impl std::fmt::Display for UsageError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.text.trim_end())
    }
}

impl std::error::Error for UsageError {}

/// A `parse_options()` refusal whose text a shared helper has already written
/// to stderr: only the status is left to carry out.
fn printed(code: u8) -> anyhow::Error {
    UsageError { text: String::new(), to_stdout: false, code }.into()
}

/// `PARSE_OPT_HELP`: the block on stdout, and since 2.56 `exit(0)`
/// (parse-options.c:1207-1208).
fn help(usage: &str) -> anyhow::Error {
    UsageError { text: usage.to_string(), to_stdout: true, code: 0 }.into()
}

/// The `objects/pack/tmp_pack_XXXXXX` git's `start_packfile()` opens, tracked
/// for its lifetime on disk rather than for anything written into it.
///
/// `cmd_fast_import` opens it before it reads a byte of the stream and long
/// before argv is looked at — `start_packfile()` sits at
/// `builtin/fast-import.c:3978`, `parse_argv()` is only reached at 4020 (or, if
/// the stream opens with a data command, from `read_next_command` at 1869) — so
/// the temporary is already in the object store by the time any option can be
/// rejected. Which exit route the run then takes is what decides whether it
/// survives:
///
///   * `usage()` calls `exit(129)` itself, running neither `die_nicely` nor
///     `end_packfile`, so the temporary stays. It is the only thing a rejected
///     `fast-import` command line leaves changed on disk.
///   * `die()` runs `die_nicely`, which calls `end_packfile()` ahead of
///     `dump_marks()` (`fast-import.c:441`). A run that stored nothing takes
///     `end_packfile`'s `discard_pack` branch — close, then
///     `unlink_or_warn(pack_data->pack_name)` (935-936).
///   * A clean run reaches `end_packfile()` at 4025, and the temporary is gone
///     either way it goes: `discard_pack` unlinks it for a run that stored
///     nothing or whose pack `loosen_small_pack()` exploded, and `keep_pack`
///     renames it onto `pack-<hash>.pack` for a run above
///     `fastimport.unpackLimit`. This port writes the kept pack under its own
///     temporary in the same directory and renames that, so the one this struct
///     tracks is unlinked on every route rather than renamed on one of them —
///     the same absence, reached differently. See [`PackLog::flush`].
///
/// It is left empty on the surviving route because the twelve bytes
/// `write_pack_header` produces sit in the `hashfd` buffer until
/// `finalize_hashfile`, which only a run that stored objects reaches. Verified
/// against git 2.55.0: a rejected command line leaves a zero-byte
/// `tmp_pack_XXXXXX` mode 0444.
///
/// `cycle_packfile()` — at a `checkpoint` (3652) or past `--max-pack-size`
/// (1050, 1146) — is not modelled for the temporary, because it unlinks it and
/// immediately opens another: one temporary is alive either
/// way, and only its random tail — which nothing reads — would differ.
struct TempPack {
    path: std::path::PathBuf,
}

impl TempPack {
    /// git's `start_packfile()`: `odb_mkstemp(…, "pack/tmp_pack_XXXXXX")`, which
    /// creates the leading directory and then the file itself, mode 0444.
    fn start(repo: &gix::Repository) -> Result<TempPack> {
        let dir = repo.objects.store_ref().path().join("pack");
        std::fs::create_dir_all(&dir)?;
        // `odb_mkstemp()` is `git_mkstemp_mode(…, 0444)` (odb.c:72-91), an
        // `O_CREAT | O_EXCL` open retried on `EEXIST` (wrapper.c:543-567), so
        // this name is one no other temporary in the directory can share.
        let (path, _fd) =
            crate::tmp_objdir::git_mkstemp_mode(dir.join("tmp_pack_XXXXXX").as_os_str(), 0o444)?;
        Ok(TempPack { path })
    }

    /// git's `end_packfile()` down its `discard_pack` branch: drop the
    /// temporary. Takes `self` by value, which gives the same once-only
    /// guarantee as `end_packfile`'s `running`/`!pack_data` guard.
    fn end(self) {
        // `unlink_or_warn`: git does not treat a failure here as fatal.
        let _ = std::fs::remove_file(&self.path);
    }
}

/// git's packfile bookkeeping for one run: what `store_object()` has counted
/// into the pack now being built, and which loose files this port created on the
/// way there.
///
/// `store_object()` (`builtin/fast-import.c:955-1094`) hashes the object, looks
/// it up in a run-wide table, and stores it only when that table holds no offset
/// for it *and* no packfile already in the object store has it:
///
/// ```c
/// e = insert_object(&oid);
/// if (mark) insert_mark(&marks, mark, e);
/// if (e->idx.offset) { duplicate_count_by_type[type]++; return 1; }
///
/// for (source = the_repository->objects->sources; source; source = source->next) {
///         struct odb_source_files *files = odb_source_files_downcast(source);
///         if (!packfile_list_find_oid(packfile_store_get_packs(files->packed), &oid))
///                 continue;
///         e->type = type;
///         e->pack_id = MAX_PACK_ID;
///         e->idx.offset = 1; /* just not zero! */
///         duplicate_count_by_type[type]++;
///         return 1;
/// }
/// ```
///
/// A *loose* copy is deliberately not consulted, so an object the repository
/// already holds loose is stored into the pack a second time and counted.
/// `object_count` is therefore not the number of objects the import added to the
/// store, which is exactly the distinction `fastimport.unpackLimit` is compared
/// against.
///
/// The pack bytes are built in memory exactly as `store_object()` and
/// `stream_blob()` write them. This port also writes every object loose as it
/// goes, because the stream can read any of them back before the pack is in
/// place — `cat-blob`, `ls`, `from`, and the tree a later commit starts from
/// all go through the object database, where git reads the pack it is writing.
/// [`PackLog::flush`] is then `end_packfile()`: it decides which of the two
/// shapes git would have left behind and makes the store match it.
struct PackLog {
    /// git's `object_table` (`insert_object()`, builtin/fast-import.c:523-536):
    /// one entry per object id the run has hashed, looked up, or loaded from a
    /// marks file, whether or not it was stored.
    table: HashMap<ObjectId, ObjectEntry>,
    /// `pack_id`: the number of the pack now being built. It moves on only when
    /// `end_packfile()` keeps a pack.
    pack_id: u32,
    /// `object_count_by_type`, `duplicate_count_by_type`, `delta_count_by_type`
    /// and `delta_count_attempts_by_type`, indexed by [`type_slot`].
    stored_by_type: [u64; 4],
    duplicates_by_type: [u64; 4],
    deltas_by_type: [u64; 4],
    delta_attempts_by_type: [u64; 4],
    /// `last_blob`: the blob a following blob is deltified against.
    last_blob: LastObject,
    /// `max_depth`: `pack.depth`, `--depth`, or git's 50.
    max_depth: u64,
    /// `repo_settings_get_big_file_threshold()`: `core.bigFileThreshold`,
    /// `--big-file-threshold`, or git's 512 MiB. A `data <n>` above it is
    /// streamed by `stream_blob()`, which never deltifies.
    big_file_threshold: u64,
    /// The objects of the pack now being built, in the order they were stored.
    /// `object_count` is its length.
    ids: Vec<ObjectId>,
    /// Objects this run wrote as loose files that were not there before it ran.
    /// git put them in a packfile instead, so a run that keeps its pack unlinks
    /// them again.
    created: Vec<ObjectId>,
    /// The pack indices the object store held when the run opened, standing in
    /// for `packfile_list_find_oid` above. Loaded once: a pack this run writes
    /// can only hold objects already in [`PackLog::table`], which short-circuits
    /// ahead of this check exactly as git's table does.
    packed: Vec<gix::odb::pack::index::File>,
    /// The primary object directory, which is where the loose files are.
    objdir: std::path::PathBuf,
    /// `fastimport.unpackLimit`, else `transfer.unpackLimit`, else git's 100.
    /// Signed and uncorrected, because `object_count <= unpack_limit` with a
    /// negative limit keeps every pack.
    unpack_limit: i64,
    /// `pack.indexVersion`, which `git_pack_config()` reads into
    /// `pack_idx_opts.version` for `write_idx_file()`.
    index_version: u64,
    /// `--export-pack-edges=<file>`, already open in append mode. One line is
    /// written per pack *kept*, which is why a run whose objects all fit under
    /// `unpack_limit` leaves the file empty.
    edges: Option<std::path::PathBuf>,
    /// `max_packsize`: `pack.packSizeLimit`, then `--max-pack-size`, 0 for none.
    max_packsize: u64,
    /// `pack_compression_level`: `pack.compression`, else `core.compression`,
    /// else zlib's default.
    compression: i32,
    /// What `hashwrite(pack_file, …)` has written past the 12-byte header of
    /// the pack now being built; `pack_size` is its length plus the header.
    body: Vec<u8>,
    /// `create_index()`'s table for that pack: each stored entry's offset and
    /// `crc32_end()`, in store order.
    entries: Vec<super::pack_objects::PackedEntry>,
    /// Packs `end_packfile()` closed inside [`PackLog::store`], by `pack_id`,
    /// with the `.pack` kept or `None` for one `loosen_small_pack()` exploded,
    /// for the importer to name in `--export-pack-edges` or invalidate in its
    /// branch and tag tables before anything moves them.
    closed: Vec<(u32, Option<std::path::PathBuf>)>,
}

impl PackLog {
    /// Take the object store as `start_packfile()` finds it.
    fn new(repo: &gix::Repository) -> PackLog {
        use gix::odb::store::structure::Record;
        let hash = repo.object_hash();
        let mut packed = Vec::new();
        if let Ok(records) = repo.objects.store_ref().structure() {
            for record in records {
                let Record::Index { path, .. } = record else { continue };
                if let Ok(idx) = gix::odb::pack::index::File::at(&path, hash) {
                    packed.push(idx);
                }
            }
        }
        PackLog {
            table: HashMap::new(),
            pack_id: 0,
            stored_by_type: [0; 4],
            duplicates_by_type: [0; 4],
            deltas_by_type: [0; 4],
            delta_attempts_by_type: [0; 4],
            last_blob: LastObject::default(),
            max_depth: 50,
            big_file_threshold: 512 * 1024 * 1024,
            ids: Vec::new(),
            created: Vec::new(),
            packed,
            objdir: repo.objects.store_ref().path().to_path_buf(),
            unpack_limit: 100,
            index_version: 2,
            edges: None,
            max_packsize: 0,
            compression: -1,
            body: Vec::new(),
            entries: Vec::new(),
            closed: Vec::new(),
        }
    }

    /// `git_pack_config()` (`builtin/fast-import.c:3955-3980`): the pack settings
    /// the run is configured with before argv is parsed.
    ///
    /// ```c
    /// if (!repo_config_get_int(the_repository, "pack.indexversion", &indexversion_value)) {
    ///         pack_idx_opts.version = indexversion_value;
    ///         …
    /// }
    /// if (!repo_config_get_int(the_repository, "fastimport.unpacklimit", &limit))
    ///         unpack_limit = limit;
    /// else if (!repo_config_get_int(the_repository, "transfer.unpacklimit", &limit))
    ///         unpack_limit = limit;
    /// ```
    ///
    /// `pack.depth` and `pack.packSizeLimit` are read there too, for the delta
    /// chain and the pack-cycling threshold. All are read in git's order, because a value
    /// `git_config_ulong()` cannot parse is fatal, and the function ends in
    /// `repo_config(the_repository, git_default_config, NULL)`, which refuses every
    /// `core.*`, `push.*` and similar value the default callback refuses.
    fn configure(&mut self, repo: &gix::Repository) -> Result<()> {
        let die = crate::fatal::die;
        // `if (max_depth > MAX_DEPTH) max_depth = MAX_DEPTH;` — clamped here,
        // where `--depth` is refused instead.
        if let Some(depth) = crate::config::config_ulong(repo, "pack.depth").map_err(die)? {
            self.max_depth = depth.min(MAX_DEPTH);
        }
        let index_version = crate::config::config_int(repo, "pack.indexversion").map_err(die)?;
        if let Some(version) = index_version.map(|v| v as i32 as u32).filter(|v| *v > 2) {
            // `git_die_config()`: the `error:` text, then the origin-naming `fatal:`.
            crate::config::die_config(
                Some(repo),
                "pack.indexversion",
                Some(&format!("bad pack.indexVersion={version}")),
            );
        }
        // `if (!repo_config_get_ulong(…, "pack.packsizelimit", &packsizelimit_value))
        //         max_packsize = packsizelimit_value;` (builtin/fast-import.c:3972-3973),
        // taken as it is: the 1 MiB floor is `--max-pack-size`'s alone.
        if let Some(limit) = crate::config::config_ulong(repo, "pack.packsizelimit").map_err(die)? {
            self.max_packsize = limit;
        }
        let unpack_limit = match crate::config::config_int(repo, "fastimport.unpacklimit").map_err(die)? {
            Some(limit) => Some(limit),
            None => crate::config::config_int(repo, "transfer.unpacklimit").map_err(die)?,
        };
        crate::default_config::validate(repo).map_err(crate::default_config::Rejection::into_error)?;
        // `git_default_core_config()` reads `core.bigfilethreshold` with
        // `git_config_ulong()` into the repository settings the blob parser asks.
        if let Some(threshold) = crate::config::config_ulong(repo, "core.bigfilethreshold").map_err(die)? {
            self.big_file_threshold = threshold;
        }
        // `pack_compression_level`: `pack.compression` wins over `core.compression`
        // whichever comes first (`pack_compression_seen`, environment.c), and an
        // out-of-range value has already died in the default callback above.
        let level = match crate::config::config_int(repo, "pack.compression").map_err(die)? {
            Some(level) => Some(level),
            None => crate::config::config_int(repo, "core.compression").map_err(die)?,
        };
        self.compression = level.map_or(-1, |level| level as i32);

        self.unpack_limit = unpack_limit.unwrap_or(100);
        // 1 and 2 are the two versions `write_idx_file()` can write and [`index_file`] emits.
        self.index_version = match index_version {
            Some(1) => 1,
            _ => 2,
        };
        Ok(())
    }

    /// The loose path `<objdir>/ab/cdef…` an object would be written to.
    fn loose_path(&self, id: &ObjectId) -> std::path::PathBuf {
        let hex = id.to_string();
        self.objdir.join(&hex[..2]).join(&hex[2..])
    }

    /// `store_object()` (builtin/fast-import.c:968-1110): hash the object,
    /// decide whether git would have counted it into the pack, try the delta
    /// git would have tried, and write it.
    ///
    /// ```c
    /// e = insert_object(&oid);
    /// if (mark) insert_mark(&marks, mark, e);
    /// if (e->idx.offset) { duplicate_count_by_type[type]++; return 1; }
    /// … packfile_list_find_oid() … { …; duplicate_count_by_type[type]++; return 1; }
    /// if (last && last->data.len && last->data.buf && last->depth < max_depth
    ///         && dat->len > the_hash_algo->rawsz) {
    ///         delta_count_attempts_by_type[type]++;
    ///         delta = diff_delta(last->data.buf, last->data.len,
    ///                 dat->buf, dat->len,
    ///                 &deltalen_st, dat->len - the_hash_algo->rawsz);
    /// } else
    ///         delta = NULL;
    /// ```
    ///
    /// The mark is the caller's to insert, through [`Importer::insert_mark`].
    /// The write happens either way — a duplicate is already in the store, so
    /// `write_buf` is a no-op for it — and only the bookkeeping differs. The
    /// delta is computed with the port of `diff-delta.c`, deflated with the
    /// object's entry header into [`PackLog::body`], unless the entry would cross
    /// `max_packsize`: then the pack is closed through [`PackLog::end_pack`] and the
    /// object written whole into the next one.
    ///
    /// The second half of the return is git's `!store_object()`: whether this
    /// object was *newly stored into the current pack*, which is what
    /// `parse_new_commit` and `parse_new_tag` set `b->pack_id` / `t->pack_id`
    /// from (3101, 3272-3274) and therefore what `--export-pack-edges` names.
    fn store(
        &mut self,
        repo: &gix::Repository,
        kind: Kind,
        data: &[u8],
        mut last: Last<'_>,
    ) -> Result<(ObjectId, bool)> {
        let id = gix::objs::compute_hash(repo.object_hash(), kind, data)?;
        let Some(fresh) = self.write_loose(repo, kind, data, id)? else {
            return Ok((id, false));
        };

        // ```c
        // if (last && last->data.len && last->data.buf && last->depth < max_depth
        //         && dat->len > the_hash_algo->rawsz) {
        //         delta_count_attempts_by_type[type]++;
        //         delta = diff_delta(last->data.buf, last->data.len,
        //                 dat->buf, dat->len,
        //                 &deltalen_st, dat->len - the_hash_algo->rawsz);
        // ```
        let slot = type_slot(kind);
        let rawsz = repo.object_hash().len_in_bytes();
        let base = match &last {
            Last::None => None,
            Last::Blob => Some(&self.last_blob),
            Last::Tree(lo) => Some(&**lo),
        };
        let mut delta = None;
        if let Some(base) = base {
            if !base.data.is_empty() && u64::from(base.depth) < self.max_depth && data.len() > rawsz {
                self.delta_attempts_by_type[slot] += 1;
                delta = gix::odb::pack::data::output::delta::encode::Index::new(base.data.as_slice())
                    .and_then(|index| index.create(data, (data.len() - rawsz) as u64))
                    .map(|delta| (delta, base.offset, base.depth));
            }
        }
        let wrap = gix::zlib::deflate::Wrap::Zlib;
        let mut out = gix::zlib::deflate::compress(delta.as_ref().map_or(data, |(d, _, _)| d.as_slice()), self.compression, wrap);

        // `/* Determine if we should auto-checkpoint. */` (builtin/fast-import.c:1043-1065)
        if self.max_packsize != 0 && self.pack_size() + 3 * rawsz as u64 + out.len() as u64 > self.max_packsize {
            self.end_pack(repo)?;
            // `/* We cannot carry a delta into the new pack. */`
            if delta.take().is_some() {
                out = gix::zlib::deflate::compress(data, self.compression, wrap);
            }
        }

        let offset = self.pack_size();
        let start = self.body.len();
        let depth = match &delta {
            // `hdrlen = encode_in_pack_object_header(hdr, sizeof(hdr), OBJ_OFS_DELTA, deltalen);`
            // and the base's distance back, as `OBJ_OFS_DELTA` spells it (1074-1088).
            Some((bytes, base_offset, base_depth)) => {
                self.deltas_by_type[slot] += 1;
                gix::odb::pack::data::entry::Header::OfsDelta { base_distance: offset - base_offset }
                    .write_to(bytes.len() as u64, &mut self.body)?;
                base_depth + 1
            }
            None => {
                pack_entry_header(kind).write_to(data.len() as u64, &mut self.body)?;
                0
            }
        };
        self.body.extend_from_slice(&out);
        self.record(kind, id, offset, start, depth, fresh);
        // `last->data = *dat` (or the swap), `last->offset = e->idx.offset;
        // last->depth = e->depth;` (1101-1108).
        let last = match &mut last {
            Last::None => None,
            Last::Blob => Some(&mut self.last_blob),
            Last::Tree(lo) => Some(&mut **lo),
        };
        if let Some(last) = last {
            last.data = data.to_vec();
            last.offset = offset;
            last.depth = depth;
        }
        Ok((id, true))
    }

    /// `stream_blob()` (builtin/fast-import.c:1117-1236): a blob above the
    /// big-file threshold, deflated in 64 KiB pieces without a delta, the pack
    /// cycled ahead of it on its *inflated* length, and the bytes truncated away
    /// again when the blob turns out to be a duplicate.
    fn store_streamed(&mut self, repo: &gix::Repository, data: &[u8]) -> Result<ObjectId> {
        use gix::zlib::deflate::{Deflate, Wrap, Z_BUF_ERROR, Z_FINISH, Z_NO_FLUSH, Z_OK, Z_STREAM_END};
        const IN_SZ: usize = 64 * 1024;
        const OUT_SZ: usize = 64 * 1024;

        let rawsz = repo.object_hash().len_in_bytes() as u64;
        let len = data.len() as u64;
        if self.max_packsize != 0 && self.pack_size() + 3 * rawsz + len > self.max_packsize {
            self.end_pack(repo)?;
        }
        let id = gix::objs::compute_hash(repo.object_hash(), Kind::Blob, data)?;
        // `truncate_pack(&checkpoint)` for a duplicate puts the pack back where it
        // was, so it is as if nothing had been written.
        let Some(fresh) = self.write_loose(repo, Kind::Blob, data, id)? else {
            return Ok(id);
        };

        let offset = self.pack_size();
        let start = self.body.len();
        let mut z = Deflate::new(self.compression, Wrap::Zlib);
        let mut out_buf = vec![0u8; OUT_SZ];
        let mut hdr = Vec::new();
        pack_entry_header(Kind::Blob).write_to(len, &mut hdr)?;
        out_buf[..hdr.len()].copy_from_slice(&hdr);
        // `s.next_out = out_buf + hdrlen; s.avail_out = out_sz - hdrlen;`
        let mut out_base = hdr.len();
        z.set_output(OUT_SZ - out_base);
        // `in_buf`: the piece `fread()` last filled, which `next_in` walks.
        let (mut chunk, mut left) = (0..0, data.len());
        let mut status = Z_OK;
        while status != Z_STREAM_END {
            if left > 0 && z.avail_in() == 0 {
                let cnt = IN_SZ.min(left);
                chunk = chunk.end..chunk.end + cnt;
                z.set_input(cnt);
                left -= cnt;
            }
            let flush = if left > 0 { Z_NO_FLUSH } else { Z_FINISH };
            status = z.step(&data[chunk.clone()], &mut out_buf[out_base..], flush);
            if z.avail_out() == 0 || status == Z_STREAM_END {
                let n = out_base + z.out_pos();
                self.body.extend_from_slice(&out_buf[..n]);
                out_base = 0;
                z.set_output(OUT_SZ);
            }
            if !matches!(status, Z_OK | Z_BUF_ERROR | Z_STREAM_END) {
                crate::git_fatal!("unexpected deflate failure: {status}");
            }
        }
        self.record(Kind::Blob, id, offset, start, 0, fresh);
        Ok(id)
    }

    /// The head of `store_object()` and `stream_blob()`: write the object loose
    /// for this port to read back, then git's two duplicate checks. `None` is a
    /// duplicate, counted; otherwise whether the loose file is this run's own.
    ///
    /// ```c
    /// e = insert_object(&oid);
    /// if (e->idx.offset) { duplicate_count_by_type[type]++; return 1; }
    /// … packfile_list_find_oid() … { …; duplicate_count_by_type[type]++; return 1; }
    /// ```
    fn write_loose(&mut self, repo: &gix::Repository, kind: Kind, data: &[u8], id: ObjectId) -> Result<Option<bool>> {
        // Probed before the write, because that is the only moment the answer is
        // still "was it there before this run put it there".
        let loose = self.loose_path(&id);
        let existed = loose.exists();
        let written = repo.write_buf(kind, data).map_err(to_anyhow)?;
        debug_assert_eq!(written, id);
        let fresh = !existed && loose.exists();

        let slot = type_slot(kind);
        let duplicate = self.table.contains_key(&id) || {
            let packed = self.packed.iter().any(|idx| idx.lookup(id).is_some());
            if packed {
                self.table.insert(id, ObjectEntry { kind, pack_id: MAX_PACK_ID, depth: 0, offset: 1 });
            }
            packed
        };
        if duplicate {
            if fresh {
                self.created.push(id);
            }
            self.duplicates_by_type[slot] += 1;
            return Ok(None);
        }
        Ok(Some(fresh))
    }

    /// The tail of `store_object()` and `stream_blob()` once the entry's bytes
    /// are in the pack from `start` on: `e->pack_id = pack_id; e->idx.offset =
    /// …; object_count++; object_count_by_type[type]++; e->idx.crc32 =
    /// crc32_end(pack_file);`.
    fn record(&mut self, kind: Kind, id: ObjectId, offset: u64, start: usize, depth: u32, fresh: bool) {
        let crc32 = gix::features::hash::crc32(&self.body[start..]);
        self.entries.push(super::pack_objects::PackedEntry { id, offset, crc32, kind, name_hash: 0 });
        self.table.insert(id, ObjectEntry { kind, pack_id: self.pack_id, depth, offset });
        self.stored_by_type[type_slot(kind)] += 1;
        self.ids.push(id);
        // Recorded only now: a cycle ahead of the write closes the old pack, and
        // this object's loose file belongs to the new one.
        if fresh {
            self.created.push(id);
        }
    }

    /// `pack_size`: the 12-byte header `write_pack_header()` wrote, and every
    /// entry since.
    fn pack_size(&self) -> u64 {
        PACK_HEADER_LEN + self.body.len() as u64
    }

    /// `end_packfile()` down to what the object table and `last_blob` see:
    /// [`PackLog::flush`], then `invalidate_pack_id()` for a pack
    /// `loosen_small_pack()` exploded or `pack_id++` for one kept, and `/* We
    /// can't carry a delta across packfiles. */` (builtin/fast-import.c:888-963).
    /// The closed pack is queued on [`PackLog::closed`] for the importer's
    /// branch and tag tables. `start_packfile()` is the empty body it leaves.
    fn end_pack(&mut self, repo: &gix::Repository) -> Result<()> {
        let pack_id = self.pack_id;
        let object_count = self.ids.len();
        let kept = self.flush(repo)?;
        self.last_blob = LastObject::default();
        if object_count == 0 {
            return Ok(());
        }
        match kept {
            Some(_) => self.pack_id += 1,
            None => {
                for e in self.table.values_mut().filter(|e| e.pack_id == pack_id) {
                    e.pack_id = MAX_PACK_ID;
                }
            }
        }
        self.closed.push((pack_id, kept));
        Ok(())
    }

    /// `end_packfile()` (`builtin/fast-import.c:888-963`) for the pack the log
    /// has accumulated.
    ///
    /// ```c
    /// if (object_count) {
    ///         …
    ///         finalize_hashfile(pack_file, cur_pack_oid.hash, FSYNC_COMPONENT_PACK, 0);
    ///         fixup_pack_header_footer(the_hash_algo, pack_data->pack_fd,
    ///                                  pack_data->hash, pack_data->pack_name,
    ///                                  object_count, cur_pack_oid.hash,
    ///                                  pack_size);
    ///         if (object_count <= unpack_limit) {
    ///                 if (!loosen_small_pack(pack_data)) {
    ///                         invalidate_pack_id(pack_id);
    ///                         goto discard_pack;
    ///                 }
    ///         }
    ///         close(pack_data->pack_fd);
    ///         idx_name = keep_pack(create_index());
    ///         …
    ///         pack_id++;
    /// } else {
    /// discard_pack:
    ///         close(pack_data->pack_fd);
    ///         unlink_or_warn(pack_data->pack_name);
    /// }
    /// ```
    ///
    /// Three outcomes, and this port reaches all three:
    ///
    ///   * **Nothing stored.** The temporary is unlinked and the object store is
    ///     untouched. There is nothing to undo here either.
    ///   * **At or below `unpack_limit`.** `loosen_small_pack()` pipes the pack
    ///     through `git unpack-objects` and then discards it, which leaves
    ///     precisely the loose objects this port has already written.
    ///   * **Above it.** The pack is kept and git wrote no loose copy of
    ///     anything in it, so the loose files *this run* created are unlinked
    ///     once the pack is in place. Loose objects that were already there stay
    ///     — git never touched them, and the packed copy sits beside them.
    ///
    /// The pack is the bytes `store_object()` and `stream_blob()` wrote, with
    /// `fixup_pack_header_footer()`'s object count in the header and the hash
    /// of the whole file after it, so it is named `pack-<hash>` as git names it;
    /// the index is `write_idx_file()` over `create_index()`'s entries.
    ///
    /// `keep_pack()` also writes a `pack-<hash>.keep` beside the pack, and
    /// `cmd_fast_import` removes every one of them with `unkeep_all_packs()`
    /// before it returns (`builtin/fast-import.c:4028`). The file exists only for
    /// the width of the run, so it is not reproduced.
    /// Returns the `.pack` just installed, for the caller's `--export-pack-edges`
    /// line, or `None` when the pack was discarded.
    fn flush(&mut self, repo: &gix::Repository) -> Result<Option<std::path::PathBuf>> {
        let ids = std::mem::take(&mut self.ids);
        let created = std::mem::take(&mut self.created);
        let mut body = std::mem::take(&mut self.body);
        let mut entries = std::mem::take(&mut self.entries);
        if ids.is_empty() || ids.len() as i64 <= self.unpack_limit {
            return Ok(None);
        }

        let kind = repo.object_hash();
        let mut bytes = Vec::with_capacity(PACK_HEADER_LEN as usize + body.len() + kind.len_in_bytes());
        bytes.extend_from_slice(b"PACK");
        bytes.extend_from_slice(&2u32.to_be_bytes());
        bytes.extend_from_slice(&(ids.len() as u32).to_be_bytes());
        bytes.append(&mut body);
        let mut hasher = gix::hash::hasher(kind);
        hasher.update(&bytes);
        let pack_hash = hasher.try_finalize()?;
        bytes.extend_from_slice(pack_hash.as_slice());

        let dir = self.objdir.join("pack");
        std::fs::create_dir_all(&dir)?;
        let base = dir.join(format!("pack-{pack_hash}"));
        // `write_idx_file()` sorts `create_index()`'s table by object id.
        entries.sort_unstable_by_key(|entry| entry.id);
        let index = super::pack_objects::index_file(kind, self.index_version, &pack_hash, &entries)?;
        let pack = base.with_extension("pack");
        install_pack_file(&dir, "tmp_pack_XXXXXX", &pack, &bytes)?;
        install_pack_file(&dir, "tmp_idx_XXXXXX", &base.with_extension("idx"), &index)?;

        for id in &created {
            let loose = self.loose_path(id);
            let _ = std::fs::remove_file(&loose);
            // The fan-out directory this run created along with it. `remove_dir`
            // declines while anything else lives there, which is the guard.
            if let Some(parent) = loose.parent() {
                let _ = std::fs::remove_dir(parent);
            }
        }

        // The pack a later `store()` must not store into again. git gets this
        // from `packfile_store_load_pack()` registering the kept pack, though its
        // object table short-circuits ahead of the lookup either way.
        if let Ok(idx) =
            gix::odb::pack::index::File::at(base.with_extension("idx"), repo.object_hash())
        {
            self.packed.push(idx);
        }
        Ok(Some(pack))
    }

    /// The `--export-pack-edges` line for a pack just kept.
    ///
    /// ```c
    /// if (pack_edges) {
    ///         fprintf(pack_edges, "%s:", new_p->pack_name);
    ///         for (i = 0; i < branch_table_sz; i++)
    ///                 for (b = branch_table[i]; b; b = b->table_next_branch)
    ///                         if (b->pack_id == pack_id)
    ///                                 fprintf(pack_edges, " %s", oid_to_hex(&b->oid));
    ///         for (t = first_tag; t; t = t->next_tag)
    ///                 if (t->pack_id == pack_id)
    ///                         fprintf(pack_edges, " %s", oid_to_hex(&t->oid));
    ///         fputc('\n', pack_edges);
    ///         fflush(pack_edges);
    /// }
    /// ```
    ///
    /// (`builtin/fast-import.c:913-928`.) A branch or tag carries the current
    /// `pack_id` only when its own commit or tag object was *newly stored*
    /// (3101, 3272-3274), so a tip the pack did not write is not named. The
    /// branches come out of git's hash table in bucket order, which is a function
    /// of the ref names rather than of the stream; here they are in the order the
    /// stream named them.
    fn write_edges(&self, pack: &std::path::Path, tips: &[ObjectId]) -> Result<()> {
        let Some(path) = &self.edges else { return Ok(()) };
        // `odb_pack_name()` builds the name onto the object directory as
        // `setup.c` recorded it, which is `.git/objects` for a discovery that
        // started in the worktree root. gitoxide records the same directory with
        // a leading `./`, and that one component is the whole difference between
        // the two lines.
        let shown = pack.strip_prefix("./").unwrap_or(pack);
        let mut line = shown.display().to_string();
        line.push(':');
        for id in tips {
            line.push(' ');
            line.push_str(&id.to_string());
        }
        line.push('\n');
        let mut file = std::fs::OpenOptions::new().append(true).create(true).open(path)?;
        file.write_all(line.as_bytes())?;
        Ok(())
    }
}

/// git's `finalize_object_file()`: put one pack artifact in place read-only, by
/// rename, so a name that already exists is replaced rather than written into.
///
/// The mode is why the rename matters — a pack whose object set has not changed
/// hashes to the name it already carries, and writing straight at that path
/// would land on the 0444 file the last run left.
///
/// `pattern` is the `odb_mkstemp()` template git writes the artifact under:
/// `tmp_pack_XXXXXX` for the pack (`start_packfile()`, fast-import.c:790-791)
/// and `tmp_idx_XXXXXX` for its index (`write_idx_file()`, pack-write.c:86-87).
/// The bytes go through the descriptor `git_mkstemp_mode()` opened, since the
/// file is 0444 from the moment it exists. A name picked without `O_EXCL` could
/// be the run's own `TempPack`, already 0444, and opening that for writing is
/// `EACCES`.
fn install_pack_file(
    dir: &std::path::Path,
    pattern: &str,
    to: &std::path::Path,
    bytes: &[u8],
) -> Result<()> {
    let (tmp, mut fd) = crate::tmp_objdir::git_mkstemp_mode(dir.join(pattern).as_os_str(), 0o444)?;
    fd.write_all(bytes).inspect_err(|_| {
        let _ = std::fs::remove_file(&tmp);
    })?;
    drop(fd);
    std::fs::rename(&tmp, to).inspect_err(|_| {
        let _ = std::fs::remove_file(&tmp);
    })?;
    Ok(())
}

/// Which `--date-format` the stream uses.
#[derive(Clone, Copy, PartialEq)]
enum DateFormat {
    /// `<seconds> SP <±hhmm>`, with git's sanity checks on the offset.
    Raw,
    /// The same syntax with the offset sanity check relaxed.
    RawPermissive,
    /// Any date `parse_date()` reads, stored as `<seconds> <±hhmm>`.
    Rfc2822,
    /// The literal `now`, replaced by `datestamp()`.
    Now,
}

/// `--signed-commits=`/`--signed-tags=`: what to do with a signature in the stream.
#[derive(Clone, Copy, PartialEq)]
enum SignedMode {
    /// Import the signature silently. git's default.
    Verbatim,
    /// Import it, after a warning.
    WarnVerbatim,
    /// Drop the signature silently.
    Strip,
    /// Drop it, after a warning.
    WarnStrip,
    /// Die on the first signature.
    Abort,
    /// `strip-if-invalid`, `abort-if-invalid` and `sign-if-invalid[=<keyid>]`.
    /// All three decide by *verifying* the signature, which needs a gpg driver
    /// the vendored crates do not provide, so they are accepted here — git
    /// accepts them — and refused at the first signature rather than guessed at.
    NeedsVerification,
}

/// Map a `--signed-commits=`/`--signed-tags=` value onto a mode, rejecting an
/// unknown one through `usage()` as git does.
fn signed_mode(flag: &str, value: &str) -> Result<SignedMode> {
    Ok(match value {
        "verbatim" => SignedMode::Verbatim,
        "warn-verbatim" => SignedMode::WarnVerbatim,
        "strip" => SignedMode::Strip,
        "warn-strip" => SignedMode::WarnStrip,
        "abort" => SignedMode::Abort,
        "strip-if-invalid" | "abort-if-invalid" | "sign-if-invalid" => {
            SignedMode::NeedsVerification
        }
        _ if starts(value, "sign-if-invalid=") => SignedMode::NeedsVerification,
        _ => {
            return Err(UsageError::refuse(format!("usage: unknown {flag} mode '{value}'\n")));
        }
    })
}

/// Everything the command line and the stream's `feature`/`option` commands can set.
struct Opts {
    force: bool,
    date_format: DateFormat,
    require_done: bool,
    /// `export_marks_file`, already resolved by `make_fast_import_path()` when
    /// the option or feature named it, so `--relative-marks` applies to the
    /// names that follow it and not to the ones before.
    export_marks: Option<std::path::PathBuf>,
    /// The same name as git spells it: `prefix_filename()` against the
    /// directory the command started in, or `git_path()` under
    /// `--relative-marks`. The crash report quotes it.
    export_marks_shown: String,
    relative_marks: bool,
    /// `state->allow_unsafe_features`. Set by the early argv scan before the
    /// stream is read, which is the only setting a `feature` line ever sees.
    allow_unsafe: bool,
    cat_blob_fd: Option<i32>,
    signed_commits: SignedMode,
    signed_tags: SignedMode,
    /// `import_marks_file`, with `import_marks_file_from_stream` and
    /// `import_marks_file_ignore_missing`: the one marks file to read, named
    /// but not read until `parse_argv()` finishes.
    import_marks: Option<ImportMarks>,
    /// `import_marks_file_done`. `dump_marks()` refuses to write while a named
    /// import file is unread, so a fatal before the read leaves the export file
    /// untouched.
    import_marks_done: bool,
}

/// The pending `--import-marks`/`feature import-marks` file.
struct ImportMarks {
    path: std::path::PathBuf,
    from_stream: bool,
    ignore_missing: bool,
}

impl Opts {
    /// git's defaults before argv is read.
    fn new() -> Self {
        Opts {
            force: false,
            date_format: DateFormat::Raw,
            require_done: false,
            export_marks: None,
            export_marks_shown: String::new(),
            relative_marks: false,
            allow_unsafe: false,
            cat_blob_fd: None,
            signed_commits: SignedMode::Verbatim,
            signed_tags: SignedMode::Verbatim,
            import_marks: None,
            import_marks_done: false,
        }
    }
}

/// `struct branch`: a ref being built in memory, exactly one per name the
/// stream has named.
struct Branch {
    /// Full ref name, e.g. `refs/heads/main`.
    name: String,
    /// `oid`: the tip this branch will be written at, or `None` for the null id.
    head: Option<ObjectId>,
    /// `branch_tree`: the tree the next commit on this branch starts from.
    tree: TreeEntry,
    /// `last_commit`: `object_count_by_type[OBJ_COMMIT]` when its last commit
    /// was stored — the clock the active-branch LRU evicts by.
    last_commit: u64,
    /// `num_notes`: notes counted in `tree`, which pick the notes fanout.
    notes: u64,
    /// `active`: on the `active_branches` list.
    active: bool,
    /// `delete`: set by `from <null oid>`; the ref is to be deleted rather than
    /// updated.
    delete: bool,
    /// `pack_id`: the pack the commit it points at was newly stored into, or
    /// [`MAX_PACK_ID`]. Only a tip of the pack being closed is named on an
    /// `--export-pack-edges` line, and a later duplicate commit does not clear
    /// it — git only ever assigns it (3135).
    pack_id: u32,
}

/// `struct tag`: one `tag` command, kept for the whole run.
struct Tag {
    name: String,
    /// `oid`: null until the tag object is stored, which is what the crash
    /// report shows for a tag whose command died part way.
    oid: ObjectId,
    /// `pack_id`, as for [`Branch::pack_id`].
    pack_id: u32,
}

/// `struct mark_set`'s shape (builtin/fast-import.c:575-596): the levels of
/// 1024 slots `insert_mark()` allocates, which the statistics block counts as
/// `marks:` and as pool memory.
struct MarkSet {
    /// `shift` of the top level.
    shift: u32,
    /// `(shift, first mark)` of every level below the top allocated so far.
    levels: HashSet<(u32, u64)>,
    /// The slots set so far.
    marked: HashSet<u64>,
}

impl MarkSet {
    fn new() -> MarkSet {
        MarkSet {
            shift: 0,
            levels: HashSet::new(),
            marked: HashSet::new(),
        }
    }

    /// `insert_mark()`'s walk: grow the top until `idnum` fits, then allocate
    /// the missing levels down to its slot. Returns whether the slot was new,
    /// which is what `marks_set_count` counts.
    fn insert(&mut self, pools: &mut Pools, idnum: u64) -> bool {
        while idnum.checked_shr(self.shift).unwrap_or(0) >= 1024 {
            pools.alloc(SIZEOF_MARK_SET);
            // The old top becomes `sets[0]` of the new one.
            self.levels.insert((self.shift, 0));
            self.shift += 10;
        }
        let (mut shift, mut base, mut rest) = (self.shift, 0u64, idnum);
        while shift > 0 {
            let i = rest >> shift;
            rest -= i << shift;
            base += i << shift;
            shift -= 10;
            if self.levels.insert((shift, base)) {
                pools.alloc(SIZEOF_MARK_SET);
            }
        }
        self.marked.insert(idnum)
    }
}

/// `git fast-import` — import a stream of objects and ref updates from stdin.
///
/// The exit contracts are git's. A command line `parse_options()` refuses
/// exits 129 (help it was asked for mid-line exits 0 on stdout); any other
/// failure prints `fatal: <reason>` and exits 128; a run that imported cleanly
/// but declined a non-fast-forward ref update exits 1.
pub fn fast_import(args: &[String]) -> Result<ExitCode> {
    // Dispatch passes the subcommand itself at index 0.
    let args = match args.first() {
        Some(a) if a == "fast-import" => &args[1..],
        _ => args,
    };
    // `show_usage_with_options_if_asked(argc, argv, fast_import_usage,
    // fast_import_options)` (builtin/fast-import.c:4199): the help flag alone,
    // and only alone, prints the block on *stdout* and exits 0. It runs ahead
    // of the repository setup git.c would otherwise do, so it answers outside a
    // repository too, and ahead of `start_packfile()`, so it leaves no
    // temporary behind.
    if let Some(code) = super::show_usage_if_asked_full(args, USAGE, USAGE_ALL) {
        return Ok(code);
    }
    match run(args) {
        Ok(code) => Ok(code),
        Err(e) => match e.downcast_ref::<UsageError>() {
            Some(u) => {
                match u.to_stdout {
                    true => crate::cstdio::write_bytes(u.text.as_bytes()),
                    false => eprint!("{}", u.text),
                }
                Ok(ExitCode::from(u.code))
            }
            None => {
                // A refusal from the default config callback has said all it has to say.
                if e.is::<crate::fatal::Silent>() {
                    return Err(e);
                }
                eprintln!("fatal: {e}");
                Ok(ExitCode::from(128))
            }
        },
    }
}

/// Open the repository, parse the command line, then drive the stream.
fn run(args: &[String]) -> Result<ExitCode> {
    // git runs `setup_git_directory()` before it looks at argv, so even a usage
    // error outside a repository comes out as "not a git repository".
    let mut repo = crate::setup::discover()?;
    // `update_branch()` and `dump_tags()` write their reflogs with
    // `git_committer_info(0)`, the non-strict identity: with no `user.*` it is
    // synthesized from the account and host rather than refused.
    crate::ensure_reflog_identity(&mut repo);
    // Serialize object and ref writes through the repo coordinator, as the other
    // writing porcelain does, so concurrent zvcs writers queue instead of racing.
    let _lock = crate::lock::RepoLock::acquire(repo.git_dir());

    // `reset_pack_idx_option(&pack_idx_opts); git_pack_config();` open
    // `cmd_fast_import` (`builtin/fast-import.c:3942-3943`), before argv and
    // before the stream, so `fastimport.unpackLimit` is settled by the time the
    // first object is stored.
    let mut pack = PackLog::new(&repo);
    pack.configure(&repo)?;

    // `marks = mem_pool_calloc(&fi_mem_pool, 1, sizeof(struct mark_set));` and
    // `rc_free = mem_pool_alloc(&fi_mem_pool, cmd_save * sizeof(*rc_free));`
    // (builtin/fast-import.c:4211, 4239), the pool's first two allocations.
    let mut pools = Pools::new();
    pools.alloc(SIZEOF_MARK_SET);
    pools.alloc(CMD_SAVE as u64 * SIZEOF_RECENT_COMMAND);
    pools.ignore_case = repo.config_snapshot().boolean("core.ignorecase").unwrap_or(false);

    let mut imp = Importer {
        pack,
        repo,
        opts: Opts::new(),
        marks: HashMap::new(),
        mark_set: MarkSet::new(),
        marks_set_count: 0,
        branches: Vec::new(),
        by_name: HashMap::new(),
        active_branches: Vec::new(),
        max_active_branches: 5,
        branch_load_count: 0,
        tags: Vec::new(),
        pools,
        show_stats: true,
        failed: false,
        seen_data_command: false,
        submodule_rewrites: Vec::new(),
        args: args.to_vec(),
    };

    // The early scan (builtin/fast-import.c:4220-4240): `feature` lines are
    // read before `parse_argv()` ever runs, so the one option that steers how
    // they are read is looked for ahead of it — only in its exact spelling, and
    // only up to the first argument that is not an option, as git looks.
    for arg in args {
        if !arg.starts_with('-') || arg == "--" {
            break;
        }
        if arg == "--allow-unsafe-features" {
            imp.opts.allow_unsafe = true;
        }
    }

    // `cmd_fast_import` opens the packfile before it reads the stream and before
    // argv is parsed, so it exists no matter how the run ends. See [`TempPack`].
    let pack = TempPack::start(&imp.repo)?;

    match imp.import() {
        Ok(code) => {
            pack.end();
            Ok(code)
        }
        // git's `usage()` exits without running `die_nicely`, so a usage error
        // leaves both the marks file and the temporary packfile alone; every
        // other failure runs `end_packfile()` and then dumps the marks.
        Err(e) if e.downcast_ref::<UsageError>().is_some() => {
            // Dropping a `TempPack` without `end()` is what leaves it on disk.
            drop(pack);
            Err(e)
        }
        Err(e) => {
            pack.end();
            // `die_nicely()` (builtin/fast-import.c:444-463): the `fatal:` line,
            // then `write_crash_report(message); end_packfile();
            // unkeep_all_packs(); dump_marks();`, in that order — so the report
            // shows the branches and marks as the failing command left them, and
            // a run that stored more than `fastimport.unpackLimit` objects and
            // *then* died still leaves them packed rather than loose. A failure
            // to write the pack here is dropped: git is already on its way out
            // with the caller's error, and `unlink_or_warn`/`die_errno` inside
            // `end_packfile` cannot replace it. A refusal from the default config
            // callback has printed its own lines and carries no message to report.
            let silent = e.is::<crate::fatal::Silent>();
            if !silent {
                eprintln!("fatal: {e}");
                imp.write_crash_report(&e.to_string());
            }
            let _ = imp.end_packfile();
            imp.dump_marks_on_fatal();
            match silent {
                true => Err(e),
                false => Ok(ExitCode::from(128)),
            }
        }
    }
}

/// `str::starts_with` under a shorter name, so the option table stays readable.
fn starts(s: &str, prefix: &str) -> bool {
    s.starts_with(prefix)
}

/// Map a `--date-format=`/`feature date-format=` value onto a format.
///
/// `option_date_format()` (builtin/fast-import.c:3670): the four `whenspec` names.
fn date_format(name: &str) -> Result<DateFormat> {
    match name {
        "raw" => Ok(DateFormat::Raw),
        "raw-permissive" => Ok(DateFormat::RawPermissive),
        "rfc2822" => Ok(DateFormat::Rfc2822),
        "now" => Ok(DateFormat::Now),
        _ => crate::git_fatal!("unknown --date-format argument {name}"),
    }
}

/// The largest `--depth` git accepts: `MAX_DEPTH`, `(1 << DEPTH_BITS) - 1` with
/// `DEPTH_BITS` 13, because a delta depth is stored in a 13-bit bitfield
/// (`fast-import.c:36-37,56`).
const MAX_DEPTH: u64 = (1 << 13) - 1;

/// git's `ulong_arg`, used for `--depth`, `--active-branches` and
/// `--cat-blob-fd`: `strtoul(arg, &endptr, 0)`, rejected unless the whole
/// argument was consumed and it holds no `-` anywhere (`fast-import.c:3684`).
///
/// Base 0 means the prefix picks the radix — `0x`/`0X` hexadecimal, a leading
/// `0` octal, otherwise decimal — so `--depth=0x10` and `--depth=010` are
/// accepted, as is a leading `+` or leading whitespace. `strtoul` also saturates
/// at `ULONG_MAX` instead of failing, which is why an absurd
/// `--depth=99999999999999999999999` reaches `option_depth`'s ceiling and
/// reports `--depth cannot exceed 8191` rather than a parse error. All four
/// verified against git 2.55.0.
fn ulong_arg(flag: &str, value: &str) -> Result<u64> {
    let bad = || anyhow!("{flag}: argument must be a non-negative integer");
    // `strchr(arg, '-')`: any `-` at all, not just a leading sign.
    if value.contains('-') {
        return Err(bad());
    }
    let s = value.trim_start_matches([' ', '\t', '\n', '\u{b}', '\u{c}', '\r']);
    let s = s.strip_prefix('+').unwrap_or(s);
    let (digits, radix) = match s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
        Some(hex) => (hex, 16),
        None if s.len() > 1 && s.starts_with('0') => (&s[1..], 8),
        None => (s, 10),
    };
    if digits.is_empty() || !digits.chars().all(|c| c.is_digit(radix)) {
        return Err(bad());
    }
    Ok(u64::from_str_radix(digits, radix).unwrap_or(u64::MAX))
}

/// `strtoumax(data, NULL, 10)` as `parse_data()` calls it: leading blanks,
/// then as many decimal digits as there are, and no error for the rest.
fn strtoumax10(spec: &[u8]) -> usize {
    let mut i = 0;
    while matches!(spec.get(i), Some(b' ' | b'\t')) {
        i += 1;
    }
    let mut v: usize = 0;
    while let Some(d) = spec.get(i).filter(|b| b.is_ascii_digit()) {
        v = v.saturating_mul(10).saturating_add(usize::from(d - b'0'));
        i += 1;
    }
    v
}

/// `option_depth()` (builtin/fast-import.c:3746-3751): parsed, then held to
/// the bitfield's ceiling.
fn option_depth(value: &str) -> Result<u64> {
    let depth = ulong_arg("--depth", value)?;
    if depth > MAX_DEPTH {
        crate::git_fatal!("--depth cannot exceed {MAX_DEPTH}");
    }
    Ok(depth)
}

/// `option_max_pack_size()` (builtin/fast-import.c:3815-3829), shared by the
/// command line and `option git max-pack-size=`:
///
/// ```c
/// if (!git_parse_ulong(arg, &v))
///         die(_("--max-pack-size: argument must be a non-negative integer"));
/// if (v < 8192) {
///         warning(_("max-pack-size is now in bytes, assuming --max-pack-size=%lum"), v);
///         v *= 1024 * 1024;
/// } else if (v < 1024 * 1024) {
///         warning(_("minimum max-pack-size is 1 MiB"));
///         v = 1024 * 1024;
/// }
/// ```
///
/// Returns the clamped `max_packsize`, which `store_object()` and
/// `stream_blob()` cycle the pack on.
fn option_max_pack_size(value: &str) -> Result<u64> {
    let Some(v) = super::list_objects_filter::git_parse_ulong(value.as_bytes()) else {
        crate::git_fatal!("--max-pack-size: argument must be a non-negative integer");
    };
    if v < 8192 {
        eprintln!("warning: max-pack-size is now in bytes, assuming --max-pack-size={v}m");
        return Ok(v * 1024 * 1024);
    }
    if v < 1024 * 1024 {
        eprintln!("warning: minimum max-pack-size is 1 MiB");
        return Ok(1024 * 1024);
    }
    Ok(v)
}

/// `option_big_file_threshold()` (builtin/fast-import.c:3827-3834): the size
/// above which a counted `data` blob is streamed rather than deltified.
fn option_big_file_threshold(value: &str) -> Result<u64> {
    let Some(v) = super::list_objects_filter::git_parse_ulong(value.as_bytes()) else {
        crate::git_fatal!("--big-file-threshold: argument must be a non-negative integer");
    };
    Ok(v)
}

// ---------------------------------------------------------------------------
// Input
// ---------------------------------------------------------------------------

thread_local! {
    /// `static struct strbuf command_buf` (builtin/fast-import.c:253): the line
    /// most recently read off the stream. Nearly every `die()` in the parser
    /// quotes it back rather than the fragment it was handed, so the port keeps
    /// the same one-line memory instead of threading the line through each
    /// call.
    static COMMAND_BUF: std::cell::RefCell<Vec<u8>> =
        const { std::cell::RefCell::new(Vec::new()) };

    /// `cmd_hist` (builtin/fast-import.c:252-258): the last `cmd_save` lines
    /// `read_next_command()` read, oldest first, for the crash report.
    static CMD_HIST: std::cell::RefCell<std::collections::VecDeque<Vec<u8>>> =
        const { std::cell::RefCell::new(std::collections::VecDeque::new()) };
}

/// Append a line `read_next_command()` read to [`CMD_HIST`]
/// (builtin/fast-import.c:1889-1904), dropping the oldest once `cmd_save` are
/// held.
fn record_command(line: &[u8]) {
    CMD_HIST.with(|h| {
        let mut h = h.borrow_mut();
        if h.len() == CMD_SAVE {
            h.pop_front();
        }
        h.push_back(line.to_vec());
    });
}

/// The current [`COMMAND_BUF`], for the diagnostics that quote it.
fn command_buf() -> String {
    COMMAND_BUF.with(|b| String::from_utf8_lossy(&b.borrow()).into_owned())
}

/// The command stream on stdin, with git's one-line pushback and its
/// "consume a trailing LF if there is one" primitive.
struct Input {
    stdin: std::io::StdinLock<'static>,
    /// A command line that was read and handed back with [`Input::unread`].
    pending: Option<Vec<u8>>,
}

impl Input {
    fn new() -> Self {
        Input {
            stdin: std::io::stdin().lock(),
            pending: None,
        }
    }

    /// One line as `read_next_command()` reads it: without its LF, or `None`
    /// at EOF. A fresh line is also appended to the command history
    /// (builtin/fast-import.c:1889-1904), the oldest dropped once `cmd_save`
    /// are held; a line handed back with [`Input::unread`] is not recorded
    /// twice.
    fn line(&mut self) -> Result<Option<Vec<u8>>> {
        if let Some(line) = self.pending.take() {
            return Ok(Some(line));
        }
        let Some(buf) = self.raw_line()? else {
            return Ok(None);
        };
        record_command(&buf);
        Ok(Some(buf))
    }

    /// One line off the stream into `command_buf`, not recorded — the read
    /// `parse_data()` makes for each line of a `data <<delim` body (1953).
    fn raw_line(&mut self) -> Result<Option<Vec<u8>>> {
        let mut buf = Vec::new();
        if self.stdin.read_until(b'\n', &mut buf)? == 0 {
            return Ok(None);
        }
        if buf.last() == Some(&b'\n') {
            buf.pop();
        }
        // git reads every line into the one global `command_buf`, and a push-back
        // leaves it alone (`unread_command_buf` only suppresses the next read),
        // so this mirror is updated here and not in `unread`.
        COMMAND_BUF.with(|b| *b.borrow_mut() = buf.clone());
        Ok(Some(buf))
    }

    /// The next command line, skipping `#` comments as `read_next_command` does.
    fn command(&mut self) -> Result<Option<Vec<u8>>> {
        loop {
            match self.line()? {
                None => return Ok(None),
                Some(l) if l.first() == Some(&b'#') => continue,
                Some(l) => return Ok(Some(l)),
            }
        }
    }

    /// Hand a command line back, to be returned by the next [`Input::command`].
    fn unread(&mut self, line: Vec<u8>) {
        self.pending = Some(line);
    }

    /// Exactly `n` bytes of raw data.
    ///
    /// ```c
    /// while (n < length) {
    ///         size_t s = strbuf_fread(sb, length - n, stdin);
    ///         if (!s && feof(stdin))
    ///                 die(_("EOF in data (%lu bytes remaining)"),
    ///                         (unsigned long)(length - n));
    ///         n += s;
    /// }
    /// ```
    ///
    /// (`parse_data()`, builtin/fast-import.c:1950-1957.) The count git reports
    /// is what is *still* missing when the stream ends, not the whole request:
    /// `data 5` over a four-byte tail is `EOF in data (1 bytes remaining)`.
    fn exact(&mut self, n: usize) -> Result<Vec<u8>> {
        let mut buf = vec![0u8; n];
        let mut got = 0usize;
        while got < n {
            match self.stdin.read(&mut buf[got..]) {
                Ok(0) => crate::git_fatal!("EOF in data ({} bytes remaining)", n - got),
                Ok(k) => got += k,
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
                Err(e) => return Err(e.into()),
            }
        }
        Ok(buf)
    }

    /// git's `skip_optional_lf`: eat one LF if the next byte is one.
    fn skip_optional_lf(&mut self) -> Result<()> {
        debug_assert!(self.pending.is_none(), "a pushed-back line hides the stream");
        if self.stdin.fill_buf()?.first() == Some(&b'\n') {
            self.stdin.consume(1);
        }
        Ok(())
    }

    /// A `data` command's payload, in either the byte-count or delimiter form.
    fn data(&mut self, spec: &[u8]) -> Result<Vec<u8>> {
        let out = if let Some(delim) = spec.strip_prefix(b"<<") {
            let delim = delim.to_vec();
            let mut out = Vec::new();
            loop {
                let Some(line) = self.raw_line()? else {
                    crate::git_fatal!(
                        "EOF in data (terminator '{}' not found)",
                        String::from_utf8_lossy(&delim)
                    );
                };
                if line == delim {
                    break;
                }
                out.extend_from_slice(&line);
                out.push(b'\n');
            }
            out
        } else {
            // `uintmax_t len = strtoumax(data, NULL, 10);` — git never checks
            // for a conversion, so a `data` line whose argument is not a number
            // at all is a zero-length payload rather than an error, and a
            // trailing suffix is ignored.
            self.exact(strtoumax10(spec))?
        };
        self.skip_optional_lf()?;
        Ok(out)
    }
}

// ---------------------------------------------------------------------------
// Importer
// ---------------------------------------------------------------------------

/// The whole import: the repository, the mark table, and the refs in flight.
struct Importer {
    repo: gix::Repository,
    opts: Opts,
    marks: HashMap<u64, ObjectId>,
    /// The shape of `marks`, for the statistics block.
    mark_set: MarkSet,
    /// `marks_set_count`: slots set in any mark set, the submodule ones
    /// included.
    marks_set_count: u64,
    /// Branches in the order the stream first named them.
    branches: Vec<Branch>,
    by_name: HashMap<String, usize>,
    /// `active_branches`, the list's head first, and `max_active_branches`.
    active_branches: Vec<usize>,
    max_active_branches: u64,
    /// `branch_load_count`.
    branch_load_count: u64,
    /// `first_tag` .. `last_tag`, in stream order; written under `refs/tags/`
    /// at every checkpoint and at the end.
    tags: Vec<Tag>,
    /// git's pack: what `store_object()` counted, and what `end_packfile()` then
    /// does with it.
    pack: PackLog,
    /// `fi_mem_pool`, the tree-entry pool and the atom table.
    pools: Pools,
    /// `show_stats`: print the statistics block at the end. On by default;
    /// `--quiet` turns it off and `--stats` back on.
    show_stats: bool,
    /// Set when a ref update was declined; makes the process exit 1.
    failed: bool,
    /// Whether a command other than `feature`/`option` has been seen, which is
    /// what makes a later `option` command an error.
    seen_data_command: bool,
    /// Submodule names named by `--rewrite-submodules-from/-to`. Only their
    /// presence is used: a stream that carries a gitlink is refused, because
    /// rewriting the object id it names is not ported.
    submodule_rewrites: Vec<String>,
    /// The command line, held until `parse_argv()` reads it: at the first
    /// command that is not `feature` or `option`, or at the end of the stream.
    args: Vec<String>,
}

impl Importer {
    /// Drive the stream; the command line is read from inside it, where git
    /// reads it.
    ///
    /// ```c
    /// while (read_next_command(&state) != EOF) { … }
    /// /* argv hasn't been parsed yet, do so */
    /// if (!state.seen_data_command)
    ///         parse_argv(&state);
    /// if (require_explicit_termination && feof(stdin))
    ///         die(_("stream ends early"));
    /// ```
    ///
    /// (builtin/fast-import.c:4244-4290.)
    fn import(&mut self) -> Result<ExitCode> {
        let mut input = Input::new();
        let saw_done = self.stream(&mut input)?;
        if !self.seen_data_command {
            self.parse_argv()?;
        }
        if self.opts.require_done && !saw_done {
            crate::git_fatal!("stream ends early");
        }

        self.checkpoint()?;
        if self.show_stats {
            self.print_stats();
        }
        Ok(if self.failed {
            ExitCode::FAILURE
        } else {
            ExitCode::SUCCESS
        })
    }

    /// `read_next_command()` for the top-level loop (builtin/fast-import.c:
    /// 1868-1912): the first line that is neither `feature ` nor `option ` —
    /// a `#` comment included, since the test comes before the comment is
    /// skipped — is where the command line gets parsed. A line handed back with
    /// [`Input::unread`] is not read again, so it never triggers the parse.
    fn next_command(&mut self, input: &mut Input) -> Result<Option<Vec<u8>>> {
        loop {
            let unread = input.pending.is_some();
            let line = match input.pending.take() {
                Some(line) => Some(line),
                None => input.raw_line()?,
            };
            let Some(line) = line else {
                return Ok(None);
            };
            if !unread
                && !self.seen_data_command
                && !line.starts_with(b"feature ")
                && !line.starts_with(b"option ")
            {
                self.parse_argv()?;
            }
            // Recorded only once `parse_argv()` is through: a command line that
            // dies leaves the line that triggered it out of the crash report.
            if !unread {
                record_command(&line);
            }
            if line.first() == Some(&b'#') {
                continue;
            }
            return Ok(Some(line));
        }
    }

    /// `parse_argv()` (builtin/fast-import.c:3988-4000): `parse_options()`
    /// over `fast_import_options[]`, then the marks file it named.
    ///
    /// ```c
    /// int argc = parse_options(state->argc, state->argv, state->prefix,
    ///                          state->option, fast_import_usage,
    ///                          PARSE_OPT_KEEP_ARGV0);
    /// if (argc > 1)
    ///         usage_with_options(fast_import_usage, state->option);
    /// state->seen_data_command = 1;
    /// if (import_marks_file)
    ///         read_marks();
    /// ```
    ///
    /// `parse_options_step()` (parse-options.c:1048-1176) with this table: no
    /// short options at all, so every `-x` is a check for a long name typed
    /// with one dash, then help when it is `h`, then ``unknown switch``. A
    /// positional does not stop the sweep — options after it still take effect
    /// one by one, left to right — and is refused only once the sweep is over.
    /// `--` and `--end-of-options` end it, and whatever follows them counts as
    /// positional too.
    fn parse_argv(&mut self) -> Result<()> {
        use crate::parseopt::OptName;

        let args = std::mem::take(&mut self.args);
        let mut positional = 0usize;
        let mut i = 0;
        while i < args.len() {
            let arg = args[i].as_str();
            i += 1;
            if !arg.starts_with('-') || arg == "-" {
                positional += 1;
                continue;
            }
            if arg == "--" || arg == "--end-of-options" {
                positional += args.len() - i;
                break;
            }
            let Some(body) = arg.strip_prefix("--") else {
                let rest = &arg[1..];
                crate::parseopt::check_typos(rest, OPTIONS).map_err(printed)?;
                if rest.starts_with('h') {
                    return Err(help(USAGE));
                }
                crate::parseopt::unknown_option(arg, USAGE);
                return Err(printed(crate::parseopt::USAGE_ERROR));
            };
            if body == "help-all" {
                return Err(help(USAGE_ALL));
            }
            if body == "help" {
                return Err(help(USAGE));
            }
            let (opt, unset) = match super::resolve_long(OPTIONS, body) {
                super::Resolved::One(opt, unset) => (opt, unset),
                super::Resolved::Ambiguous(first, second) => {
                    super::ambiguous_option(arg, &first, &second, USAGE);
                    return Err(printed(crate::parseopt::USAGE_ERROR));
                }
                super::Resolved::Unknown => {
                    crate::parseopt::unknown_option(arg, USAGE);
                    return Err(printed(crate::parseopt::USAGE_ERROR));
                }
            };
            // `do_get_value()` (parse-options.c:130-143): an attached value on
            // the `no-` spelling, or on a `PARSE_OPT_NOARG` entry.
            let attached = body.split_once('=').map(|(_, v)| v);
            if attached.is_some() && (unset || opt.arg == super::Arg::None) {
                crate::parseopt::takes_no_value(match unset {
                    true => OptName::Unset(opt.name),
                    false => OptName::Long(opt.name),
                });
                return Err(printed(crate::parseopt::USAGE_ERROR));
            }
            let value = match (opt.arg, attached) {
                (super::Arg::None, _) => "",
                (_, Some(v)) => v,
                // `get_arg()`: the next argument, whatever it looks like.
                (_, None) => match args.get(i) {
                    Some(v) => {
                        i += 1;
                        v.as_str()
                    }
                    None => {
                        crate::parseopt::requires_value(OptName::Long(opt.name));
                        return Err(printed(crate::parseopt::USAGE_ERROR));
                    }
                },
            };
            self.apply_option(opt.name, unset, value)?;
        }
        if positional > 0 {
            // `usage_with_options()`: the block alone, on stderr.
            return Err(UsageError::refuse(USAGE.to_string()));
        }

        self.seen_data_command = true;
        self.read_marks()
    }

    /// One `fast_import_options[]` callback, run the moment `parse_options()`
    /// reaches its option (builtin/fast-import.c:4003-4120).
    fn apply_option(&mut self, name: &str, unset: bool, value: &str) -> Result<()> {
        match name {
            "date-format" => self.opts.date_format = date_format(value)?,
            "stats" => self.show_stats = true,
            // `option_quiet()`: `show_stats = 0; quiet = 1;`
            "quiet" => self.show_stats = false,
            "force" => self.opts.force = true,
            "done" => self.opts.require_done = true,
            "max-pack-size" => self.pack.max_packsize = option_max_pack_size(value)?,
            "big-file-threshold" => self.pack.big_file_threshold = option_big_file_threshold(value)?,
            "depth" => self.pack.max_depth = option_depth(value)?,
            "active-branches" => self.max_active_branches = ulong_arg("--active-branches", value)?,
            "import-marks" => self.option_import_marks(value, false, false)?,
            "import-marks-if-exists" => self.option_import_marks(value, false, true)?,
            "export-marks" => self.option_export_marks(value),
            "relative-marks" => self.opts.relative_marks = !unset,
            "rewrite-submodules-from" | "rewrite-submodules-to" => self.submodule_rewrite(value)?,
            "signed-commits" => self.opts.signed_commits = signed_mode("--signed-commits", value)?,
            "signed-tags" => self.opts.signed_tags = signed_mode("--signed-tags", value)?,
            "allow-unsafe-features" => self.opts.allow_unsafe = true,
            "export-pack-edges" => self.option_export_pack_edges(value)?,
            "cat-blob-fd" => {
                // `option_cat_blob_fd`: the fd has to survive the cast to int.
                let fd = ulong_arg("--cat-blob-fd", value)?;
                if fd > i32::MAX as u64 {
                    crate::git_fatal!("--cat-blob-fd cannot exceed {}", i32::MAX);
                }
                self.opts.cat_blob_fd = Some(fd as i32);
            }
            _ => unreachable!("--{name} is not in fast_import_options[]"),
        }
        Ok(())
    }

    /// `option_import_marks()` (builtin/fast-import.c:3703-3721): only one
    /// marks file is ever pending. Naming a second reads the first right away —
    /// unless the stream named it, in which case the command line replaces it
    /// unread — and a stream may name only one.
    fn option_import_marks(&mut self, marks: &str, from_stream: bool, ignore_missing: bool) -> Result<()> {
        if let Some(pending) = &self.opts.import_marks {
            if from_stream {
                crate::git_fatal!("only one import-marks command allowed per stream");
            }
            if !pending.from_stream {
                self.read_marks()?;
            }
        }
        self.opts.import_marks = Some(ImportMarks {
            path: self.marks_path(marks),
            from_stream,
            ignore_missing,
        });
        Ok(())
    }

    /// `read_marks()` (builtin/fast-import.c:1852-1865) for the pending file.
    /// `import_marks_file_done` is set and never cleared, so once one file has
    /// been read a later unread one no longer holds `dump_marks()` back.
    fn read_marks(&mut self) -> Result<()> {
        let Some(pending) = &self.opts.import_marks else {
            return Ok(());
        };
        let display = pending.path.display().to_string();
        for (mark, id) in read_mark_file(&pending.path, &display, pending.ignore_missing)? {
            // `insert_object_entry()` (builtin/fast-import.c:1806-1820): an
            // object the table does not hold yet must exist, and enters it as
            // one no pack of this run holds.
            if !self.pack.table.contains_key(&id) {
                let Some(header) = self.repo.try_find_header(id)? else {
                    crate::git_fatal!("object not found: {id}");
                };
                self.pack.table.insert(
                    id,
                    ObjectEntry {
                        kind: header.kind(),
                        pack_id: MAX_PACK_ID,
                        depth: 0,
                        offset: 1,
                    },
                );
            }
            self.insert_mark(mark, id);
        }
        self.opts.import_marks_done = true;
        Ok(())
    }

    /// `option_export_pack_edges()` (builtin/fast-import.c:3774-3781):
    /// `pack_edges = xfopen(fn, "a")`. Opened the moment the option is parsed —
    /// which is why it exists even when the run dies further along — and in
    /// *append* mode, so a second run adds its boundaries below the first's
    /// rather than replacing them.
    fn option_export_pack_edges(&mut self, path: &str) -> Result<()> {
        if let Err(e) = std::fs::OpenOptions::new().append(true).create(true).open(path) {
            crate::git_fatal!("could not open '{path}' for writing: {}", strerror(&e));
        }
        self.pack.edges = Some(std::path::PathBuf::from(path));
        Ok(())
    }

    /// `--rewrite-submodules-from/-to=<name>:<marks file>`: validate the spec and
    /// read the marks file, so both failures land exactly where git's do.
    fn submodule_rewrite(&mut self, spec: &str) -> Result<()> {
        let (name, file) = spec
            .split_once(':')
            .ok_or_else(|| anyhow!("expected format name:filename for submodule rewrite option"))?;
        // Parsed for its errors: a missing or corrupt marks file is the only
        // observable effect of this option until a gitlink actually appears.
        // `read_mark_file(&ms, fp, insert_oid_entry)` into a mark set of its own
        // (builtin/fast-import.c:3796-3802): `insert_mark()` allocates its levels
        // from the same pool and counts its slots in the same `marks_set_count`.
        let mut ms = MarkSet::new();
        for (mark, _) in read_mark_file(std::path::Path::new(file), file, false)? {
            if ms.insert(&mut self.pools, mark) {
                self.marks_set_count += 1;
            }
        }
        self.submodule_rewrites.push(name.to_string());
        Ok(())
    }

    /// The statistics block `cmd_fast_import()` prints on stderr unless
    /// `--quiet` asked it not to (builtin/fast-import.c:4301-4326), and the
    /// `pack_report()` it ends in (packfile.c:50-69).
    ///
    /// Every counter fast-import keeps is reproduced, the memory figures
    /// included: they are sums of the LP64 `sizeof`s of what it allocated,
    /// which the allocators above replay. The `pack_report` window counters
    /// count the mmap windows `packfile.c` opened to read objects back out of
    /// packs. This port reads objects through gitoxide's store and opens no
    /// such window, so it reports its own zeros there.
    fn print_stats(&self) {
        let p = &self.pack;
        let total: u64 = p.stored_by_type.iter().sum();
        let duplicates: u64 = p.duplicates_by_type.iter().sum();
        // `alloc_objects(object_entry_alloc)` at startup, and again whenever
        // `new_object()` finds the newest block full.
        let blocks = (p.table.len() as u64).div_ceil(OBJECT_ENTRY_ALLOC).max(1);
        let alloc_count = blocks * OBJECT_ENTRY_ALLOC;
        let objects_bytes = alloc_count * SIZEOF_OBJECT_ENTRY;
        let pools_bytes = self.pools.tree_entry_allocd + self.pools.pool_alloc;
        let rule = "---------------------------------------------------------------------";

        let mut out = String::new();
        out.push_str("fast-import statistics:\n");
        out.push_str(rule);
        out.push('\n');
        out.push_str(&format!("Alloc'd objects: {alloc_count:>10}\n"));
        out.push_str(&format!("Total objects:   {total:>10} ({duplicates:>10} duplicates                  )\n"));
        for (label, slot) in [("blobs  ", 0), ("trees  ", 1), ("commits", 2), ("tags   ", 3)] {
            out.push_str(&format!(
                "      {label}:   {:>10} ({:>10} duplicates {:>10} deltas of {:>10} attempts)\n",
                p.stored_by_type[slot],
                p.duplicates_by_type[slot],
                p.deltas_by_type[slot],
                p.delta_attempts_by_type[slot],
            ));
        }
        out.push_str(&format!(
            "Total branches:  {:>10} ({:>10} loads     )\n",
            self.branches.len(),
            self.branch_load_count
        ));
        let marks = 1024u64.checked_shl(self.mark_set.shift).unwrap_or(0);
        out.push_str(&format!("      marks:     {marks:>10} ({:>10} unique    )\n", self.marks_set_count));
        out.push_str(&format!("      atoms:     {:>10}\n", self.pools.atoms.len()));
        out.push_str(&format!("Memory total:    {:>10} KiB\n", (pools_bytes + objects_bytes) / 1024));
        out.push_str(&format!("       pools:    {:>10} KiB\n", pools_bytes / 1024));
        out.push_str(&format!("     objects:    {:>10} KiB\n", objects_bytes / 1024));
        out.push_str(rule);
        out.push('\n');

        // `pack_report()`: the page size, then the two window settings as
        // `prepare_repo_settings()` left them.
        // SAFETY: `sysconf` takes an int and returns a long; no pointers involved.
        let page_size = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
        let (window_size, limit) = match crate::repo_settings::RepoSettings::load(&self.repo) {
            Ok(s) => (s.packed_git_window_size, s.packed_git_limit),
            Err(_) => (
                crate::repo_settings::DEFAULT_PACKED_GIT_WINDOW_SIZE,
                crate::repo_settings::DEFAULT_PACKED_GIT_LIMIT,
            ),
        };
        out.push_str(&format!("pack_report: getpagesize()            = {page_size:>10}\n"));
        out.push_str(&format!("pack_report: core.packedGitWindowSize = {window_size:>10}\n"));
        out.push_str(&format!("pack_report: core.packedGitLimit      = {limit:>10}\n"));
        out.push_str(&format!("pack_report: pack_used_ctr            = {:>10}\n", 0));
        out.push_str(&format!("pack_report: pack_mmap_calls          = {:>10}\n", 0));
        out.push_str(&format!("pack_report: pack_open_windows        = {:>10} / {:>10}\n", 0, 0));
        out.push_str(&format!("pack_report: pack_mapped              = {:>10} / {:>10}\n", 0, 0));
        out.push_str(rule);
        out.push('\n');
        out.push('\n');
        eprint!("{out}");
    }

    /// `write_crash_report()` (builtin/fast-import.c:353-438): the state the
    /// failing command left behind, written to `$GIT_DIR/fast_import_crash_<pid>`
    /// and named on stderr. The branches are listed in `branch_table` order,
    /// all of them, under the heading git gives them.
    fn write_crash_report(&self, err: &str) {
        let pid = std::process::id();
        let loc = self.repo.git_dir().join(format!("fast_import_crash_{pid}"));
        let shown = crate::setup::git_path_display(&self.repo, &loc);
        let mut rpt = match std::fs::File::create(&loc) {
            Ok(f) => std::io::BufWriter::new(f),
            Err(e) => {
                eprintln!("error: can't write crash report {shown}: {}", strerror(&e));
                return;
            }
        };
        eprintln!("fast-import: dumping crash report to {shown}");

        let null = ObjectId::null(self.repo.object_hash());
        let mut out: Vec<u8> = Vec::new();
        out.extend_from_slice(b"fast-import crash report:\n");
        out.extend_from_slice(format!("    fast-import process: {pid}\n").as_bytes());
        // SAFETY: `getppid` takes no arguments and cannot fail.
        let ppid = unsafe { libc::getppid() };
        out.extend_from_slice(format!("    parent process     : {ppid}\n").as_bytes());
        let now = crate::date::now_seconds();
        let iso = crate::showdate::DateMode::new(crate::showdate::DateType::Iso8601);
        out.extend_from_slice(format!("    at {}\n", crate::showdate::show_date(now, 0, &iso, now)).as_bytes());
        out.push(b'\n');

        out.extend_from_slice(b"fatal: ");
        out.extend_from_slice(err.as_bytes());
        out.push(b'\n');

        out.push(b'\n');
        out.extend_from_slice(b"Most Recent Commands Before Crash\n");
        out.extend_from_slice(b"---------------------------------\n");
        CMD_HIST.with(|h| {
            let h = h.borrow();
            for (i, cmd) in h.iter().enumerate() {
                out.extend_from_slice(if i + 1 == h.len() { b"* " } else { b"  " });
                out.extend_from_slice(cmd);
                out.push(b'\n');
            }
        });

        out.push(b'\n');
        out.extend_from_slice(b"Active Branch LRU\n");
        out.extend_from_slice(b"-----------------\n");
        out.extend_from_slice(
            format!(
                "    active_branches = {} cur, {} max\n",
                self.active_branches.len(),
                self.max_active_branches
            )
            .as_bytes(),
        );
        out.push(b'\n');
        out.extend_from_slice(b"  pos  clock name\n");
        out.extend_from_slice(b"  ~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~\n");
        for (lu, &idx) in self.active_branches.iter().enumerate() {
            let b = &self.branches[idx];
            out.extend_from_slice(format!("  {:>2}) {:>6} {}\n", lu + 1, b.last_commit, b.name).as_bytes());
        }

        out.push(b'\n');
        out.extend_from_slice(b"Inactive Branches\n");
        out.extend_from_slice(b"-----------------\n");
        for idx in self.branch_table_order() {
            // `write_branch_report()` (builtin/fast-import.c:325-351).
            let b = &self.branches[idx];
            out.extend_from_slice(format!("{}:\n", b.name).as_bytes());
            out.extend_from_slice(b"  status      :");
            if b.active {
                out.extend_from_slice(b" active");
            }
            if b.tree.tree.is_some() {
                out.extend_from_slice(b" loaded");
            }
            if b.tree.versions[1].oid.is_null() {
                out.extend_from_slice(b" dirty");
            }
            out.push(b'\n');
            out.extend_from_slice(format!("  tip commit  : {}\n", b.head.unwrap_or(null)).as_bytes());
            out.extend_from_slice(format!("  old tree    : {}\n", b.tree.versions[0].oid).as_bytes());
            out.extend_from_slice(format!("  cur tree    : {}\n", b.tree.versions[1].oid).as_bytes());
            out.extend_from_slice(format!("  commit clock: {}\n", b.last_commit).as_bytes());
            out.extend_from_slice(b"  last pack   : ");
            if b.pack_id < MAX_PACK_ID {
                out.extend_from_slice(b.pack_id.to_string().as_bytes());
            }
            out.push(b'\n');
            out.push(b'\n');
        }

        if !self.tags.is_empty() {
            out.push(b'\n');
            out.extend_from_slice(b"Annotated Tags\n");
            out.extend_from_slice(b"--------------\n");
            for t in &self.tags {
                out.extend_from_slice(format!("{} {}\n", t.oid, t.name).as_bytes());
            }
        }

        out.push(b'\n');
        out.extend_from_slice(b"Marks\n");
        out.extend_from_slice(b"-----\n");
        match &self.opts.export_marks {
            Some(_) => {
                out.extend_from_slice(format!("  exported to {}\n", self.opts.export_marks_shown).as_bytes());
            }
            None => {
                let mut marks: Vec<_> = self.marks.iter().collect();
                marks.sort_by_key(|(m, _)| **m);
                for (mark, id) in marks {
                    out.extend_from_slice(format!(":{mark} {id}\n").as_bytes());
                }
            }
        }

        out.push(b'\n');
        out.extend_from_slice(b"-------------------\n");
        out.extend_from_slice(b"END OF CRASH REPORT\n");
        let _ = rpt.write_all(&out).and_then(|()| rpt.flush());
    }

    /// git's `die_nicely` path: a fatal error still writes the `--export-marks`
    /// file on the way out.
    fn dump_marks_on_fatal(&self) {
        let _ = self.export_marks();
    }

    /// `end_packfile()`: close the pack the log has accumulated, and record its
    /// boundary when `--export-pack-edges` asked for one.
    fn end_packfile(&mut self) -> Result<()> {
        self.pack.end_pack(&self.repo)?;
        self.drain_closed_packs()
    }

    /// The branch and tag half of every `end_packfile()` the log ran, whether
    /// here or inside `store_object()`'s auto-checkpoint: `invalidate_pack_id()`
    /// for a pack exploded loose, the `--export-pack-edges` line for one kept.
    ///
    /// The tips are collected here rather than in [`PackLog`] because they are
    /// branch and tag state: git reads `b->pack_id`/`t->pack_id` out of the
    /// branch table and the tag list at exactly this point (`913-927`). Every
    /// store is followed by this, before any of those fields moves, so it reads
    /// them as they were when the pack closed.
    fn drain_closed_packs(&mut self) -> Result<()> {
        for (pack_id, kept) in std::mem::take(&mut self.pack.closed) {
            let Some(pack) = kept else {
                // `loosen_small_pack()` exploded it: `invalidate_pack_id(pack_id)`
                // (540-563) — nothing points into a pack any more.
                for b in self.branches.iter_mut().filter(|b| b.pack_id == pack_id) {
                    b.pack_id = MAX_PACK_ID;
                }
                for t in self.tags.iter_mut().filter(|t| t.pack_id == pack_id) {
                    t.pack_id = MAX_PACK_ID;
                }
                continue;
            };
            let mut tips: Vec<ObjectId> = Vec::new();
            for idx in self.branch_table_order() {
                let b = &self.branches[idx];
                if b.pack_id == pack_id {
                    tips.push(b.head.unwrap_or_else(|| ObjectId::null(self.repo.object_hash())));
                }
            }
            for t in self.tags.iter().filter(|t| t.pack_id == pack_id) {
                tips.push(t.oid);
            }
            self.pack.write_edges(&pack, &tips)?;
        }
        Ok(())
    }

    /// Read commands until `done` or EOF. Returns whether `done` ended the stream.
    fn stream(&mut self, input: &mut Input) -> Result<bool> {
        while let Some(line) = self.next_command(input)? {
            let cmd = line.as_slice();
            if cmd == b"blob" {
                self.parse_blob(input)?;
            } else if let Some(v) = after(cmd, b"commit ") {
                let name = utf8(v, "ref name")?;
                self.parse_commit(input, &name)?;
            } else if let Some(v) = after(cmd, b"tag ") {
                let name = utf8(v, "tag name")?;
                self.parse_tag(input, &name)?;
            } else if let Some(v) = after(cmd, b"reset ") {
                let name = utf8(v, "ref name")?;
                self.parse_reset(input, &name)?;
            } else if cmd == b"alias" {
                self.parse_alias(input)?;
            } else if cmd == b"checkpoint" {
                self.checkpoint()?;
                input.skip_optional_lf()?;
            } else if let Some(v) = after(cmd, b"progress ") {
                let mut out = b"progress ".to_vec();
                out.extend_from_slice(v);
                out.push(b'\n');
                stdout_write(&out)?;
                input.skip_optional_lf()?;
            } else if let Some(v) = after(cmd, b"get-mark ") {
                // `if (*p != ':') die(_("not a mark: %s"), p);` — `parse_get_mark()`,
                // builtin/fast-import.c:3386-3387, the one caller that says so
                // before `parse_mark_ref()` asserts on the colon.
                if v.first() != Some(&b':') {
                    crate::git_fatal!("not a mark: {}", String::from_utf8_lossy(v));
                }
                let id = self.mark_ref(v)?;
                self.respond(format!("{id}\n").as_bytes())?;
            } else if let Some(v) = after(cmd, b"cat-blob ") {
                self.cat_blob(v)?;
            } else if let Some(v) = after(cmd, b"ls ") {
                self.parse_ls(v, None)?;
            } else if let Some(v) = after(cmd, b"feature ") {
                self.parse_feature(utf8(v, "feature")?.as_str())?;
            } else if let Some(v) = after(cmd, b"option git ") {
                self.parse_option(utf8(v, "option")?.as_str())?;
            } else if cmd.starts_with(b"option ") {
                // Options addressed at another importer are ignored.
            } else if cmd == b"done" {
                return Ok(true);
            } else {
                anyhow::bail!("unsupported command: {}", String::from_utf8_lossy(cmd));
            }
        }
        Ok(false)
    }

    // -- individual commands ------------------------------------------------

    /// `blob LF mark? original-oid? data`.
    fn parse_blob(&mut self, input: &mut Input) -> Result<()> {
        let mut mark = None;
        let mut line = input.command()?;
        if let Some(v) = field(&line, b"mark :") {
            mark = parse_mark_decl(&v);
            line = input.command()?;
        }
        if field(&line, b"original-oid ").is_some() {
            line = input.command()?;
        }
        let id = self.parse_and_store_blob(input, &line)?;
        if let Some(m) = mark {
            self.insert_mark(m, id);
        }
        Ok(())
    }

    /// `parse_and_store_blob(&last_blob, …)` (builtin/fast-import.c:2071-2090)
    /// for the `data` command in `line`: a payload above the big-file threshold
    /// goes through `stream_blob()`, which forgets the last blob and never
    /// deltifies; anything else is stored against `last_blob`.
    fn parse_and_store_blob(&mut self, input: &mut Input, line: &Option<Vec<u8>>) -> Result<ObjectId> {
        let Some(spec) = field(line, b"data ") else {
            crate::git_fatal!("expected 'data n' command, found: {}", command_buf());
        };
        // `parse_data()` returns 0 — "stream it" — only for the counted form:
        // `if (limit && limit < len)`.
        let limit = self.pack.big_file_threshold;
        let streamed = !spec.starts_with(b"<<") && limit != 0 && (limit as u128) < strtoumax10(&spec) as u128;
        let payload = input.data(&spec)?;
        let id = match streamed {
            true => {
                self.pack.last_blob = LastObject::default();
                self.pack.store_streamed(&self.repo, &payload)?
            }
            false => self.pack.store(&self.repo, Kind::Blob, &payload, Last::Blob)?.0,
        };
        self.drain_closed_packs()?;
        Ok(id)
    }

    /// `insert_mark(&marks, mark, e)` (builtin/fast-import.c:571-596).
    fn insert_mark(&mut self, mark: u64, id: ObjectId) {
        if self.mark_set.insert(&mut self.pools, mark) {
            self.marks_set_count += 1;
        }
        self.marks.insert(mark, id);
    }

    /// `commit <ref>` with its header block, ancestry and file changes.
    fn parse_commit(&mut self, input: &mut Input, name: &str) -> Result<()> {
        let idx = self.branch(name)?;

        let mut mark = None;
        let mut author: Option<Vec<u8>> = None;
        let mut encoding: Option<Vec<u8>> = None;

        let mut line = input.command()?;
        if let Some(v) = field(&line, b"mark :") {
            mark = parse_mark_decl(&v);
            line = input.command()?;
        }
        if field(&line, b"original-oid ").is_some() {
            line = input.command()?;
        }
        if let Some(v) = field(&line, b"author ") {
            author = Some(self.ident(&v)?);
            line = input.command()?;
        }
        let Some(v) = field(&line, b"committer ") else {
            crate::git_fatal!("expected committer but didn't get one");
        };
        let committer = self.ident(&v)?;
        line = input.command()?;

        // `gpgsig SP <git hash algo> SP <signature format> LF data`, at most one
        // per hash algorithm, sitting between `committer` and `encoding`.
        let mut signatures: Vec<(&'static str, Vec<u8>)> = Vec::new();
        while let Some(v) = field(&line, b"gpgsig ") {
            let spec = utf8(&v, "gpgsig")?;
            let algo = spec.split(' ').next().unwrap_or_default();
            let header = match algo {
                "sha1" => "gpgsig",
                "sha256" => "gpgsig-sha256",
                _ => crate::git_fatal!("unknown git hash algorithm in gpgsig: '{algo}'"),
            };
            line = input.command()?;
            let Some(spec) = field(&line, b"data ") else {
                crate::git_fatal!("expected 'data n' command, found: {}", command_buf());
            };
            signatures.push((header, input.data(&spec)?));
            line = input.command()?;
        }
        if !signatures.is_empty() {
            match self.opts.signed_commits {
                SignedMode::Verbatim => {}
                SignedMode::WarnVerbatim => {
                    eprintln!("warning: importing a commit signature verbatim");
                }
                SignedMode::Strip => signatures.clear(),
                SignedMode::WarnStrip => {
                    eprintln!("warning: stripping a commit signature");
                    signatures.clear();
                }
                SignedMode::Abort => {
                    crate::git_fatal!("encountered signed commit; use --signed-commits=<mode> to handle it")
                }
                SignedMode::NeedsVerification => bail!(
                    "unsupported flag \"--signed-commits=<mode>-if-invalid\" (the `-if-invalid` \
                     modes decide by verifying the signature, which needs a gpg driver the \
                     vendored crates do not provide)"
                ),
            }
        }

        if let Some(v) = field(&line, b"encoding ") {
            encoding = Some(v);
            line = input.command()?;
        }
        let Some(spec) = field(&line, b"data ") else {
            crate::git_fatal!("expected 'data n' command, found: {}", command_buf());
        };
        let message = input.data(&spec)?;

        // `from` resets the branch's ancestry and tree; `merge` only adds parents.
        let mut line = input.command()?;
        if let Some(from) = field(&line, b"from ") {
            self.parse_from(idx, &from)?;
            line = input.command()?;
        }
        let mut parents: Vec<ObjectId> = self.branches[idx].head.into_iter().collect();
        while let Some(spec) = field(&line, b"merge ") {
            parents.push(self.parse_merge(&spec)?);
            line = input.command()?;
        }

        // `/* ensure the branch is active/loaded */`
        if self.branches[idx].tree.tree.is_none() || self.max_active_branches == 0 {
            self.unload_one_branch();
            self.load_branch(idx)?;
        }
        let mut prev_fanout = convert_num_notes_to_fanout(self.branches[idx].notes);

        // File changes run until a blank line, EOF, or a line we do not own.
        while let Some(cmd) = line.clone() {
            if cmd.is_empty() {
                break;
            }
            if after(&cmd, b"M ").is_some() {
                self.file_modify(input, idx, &cmd)?;
            } else if let Some(v) = after(&cmd, b"D ") {
                // `file_change_d()` (builtin/fast-import.c:2494-2501).
                let path = unquote(v)?;
                let mut g = Ctx { repo: &self.repo, pack: &mut self.pack, pools: &mut self.pools };
                tree_content_remove(&mut g, &mut self.branches[idx].tree, &path, None, true)?;
            } else if let Some(v) = after(&cmd, b"C ") {
                let v = v.to_vec();
                self.file_copy(idx, &v, false)?;
            } else if let Some(v) = after(&cmd, b"R ") {
                let v = v.to_vec();
                self.file_copy(idx, &v, true)?;
            } else if let Some(v) = after(&cmd, b"N ") {
                let v = v.to_vec();
                self.note_change(input, idx, &v, &mut prev_fanout)?;
            } else if cmd == b"deleteall" {
                self.file_change_deleteall(idx)?;
            } else if let Some(v) = after(&cmd, b"ls ") {
                let v = v.to_vec();
                self.parse_ls(&v, Some(idx))?;
            } else if let Some(v) = after(&cmd, b"cat-blob ") {
                let v = v.to_vec();
                self.cat_blob(&v)?;
            } else {
                input.unread(cmd);
                break;
            }
            line = input.command()?;
        }

        let new_fanout = convert_num_notes_to_fanout(self.branches[idx].notes);
        if new_fanout != prev_fanout {
            let mut g = Ctx { repo: &self.repo, pack: &mut self.pack, pools: &mut self.pools };
            self.branches[idx].notes = change_note_fanout(&mut g, &mut self.branches[idx].tree, new_fanout)?;
        }

        // `/* build the tree and the commit */`
        let mut g = Ctx { repo: &self.repo, pack: &mut self.pack, pools: &mut self.pools };
        store_tree(&mut g, &mut self.branches[idx].tree)?;
        self.drain_closed_packs()?;
        let branch_tree = &mut self.branches[idx].tree;
        branch_tree.versions[0].oid = branch_tree.versions[1].oid;
        let tree = branch_tree.versions[1].oid;
        let mut buf = Vec::new();
        buf.extend_from_slice(format!("tree {tree}\n").as_bytes());
        for p in &parents {
            buf.extend_from_slice(format!("parent {p}\n").as_bytes());
        }
        buf.extend_from_slice(b"author ");
        buf.extend_from_slice(author.as_ref().unwrap_or(&committer));
        buf.extend_from_slice(b"\ncommitter ");
        buf.extend_from_slice(&committer);
        buf.push(b'\n');
        for (header, payload) in &signatures {
            // git folds a multi-line header value by prefixing every line after
            // the first with one space, and the payload's trailing LF makes a
            // final empty — so folded to a bare space — line.
            buf.extend_from_slice(header.as_bytes());
            buf.push(b' ');
            let mut lines = payload.split(|&b| b == b'\n');
            if let Some(first) = lines.next() {
                buf.extend_from_slice(first);
            }
            buf.push(b'\n');
            for rest in lines {
                buf.push(b' ');
                buf.extend_from_slice(rest);
                buf.push(b'\n');
            }
        }
        if let Some(enc) = &encoding {
            buf.extend_from_slice(b"encoding ");
            buf.extend_from_slice(enc);
            buf.push(b'\n');
        }
        buf.push(b'\n');
        buf.extend_from_slice(&message);

        // `if (!store_object(OBJ_COMMIT, &new_data, NULL, &b->oid, next_mark))
        //          b->pack_id = pack_id;` (builtin/fast-import.c:3100-3101).
        let (id, stored) = self.pack.store(&self.repo, Kind::Commit, &buf, Last::None)?;
        self.drain_closed_packs()?;
        if let Some(m) = mark {
            self.insert_mark(m, id);
        }
        let pack_id = self.pack.pack_id;
        let branch = &mut self.branches[idx];
        branch.head = Some(id);
        branch.delete = false;
        if stored {
            branch.pack_id = pack_id;
        }
        // `b->last_commit = object_count_by_type[OBJ_COMMIT];`
        branch.last_commit = self.pack.stored_by_type[type_slot(Kind::Commit)];
        Ok(())
    }

    /// `tag <name> LF mark? from original-oid? tagger? data`.
    fn parse_tag(&mut self, input: &mut Input, name: &str) -> Result<()> {
        // `t = mem_pool_calloc(&fi_mem_pool, 1, sizeof(struct tag));` and its
        // name, appended to the tag list before anything is read
        // (builtin/fast-import.c:3236-3242): a tag whose command dies is still
        // listed, with a null id, in the crash report.
        self.pools.alloc(SIZEOF_TAG);
        self.pools.strdup(name);
        let slot = self.tags.len();
        self.tags.push(Tag {
            name: name.to_string(),
            oid: ObjectId::null(self.repo.object_hash()),
            pack_id: 0,
        });

        let mut mark = None;
        let mut line = input.command()?;
        if let Some(v) = field(&line, b"mark :") {
            mark = parse_mark_decl(&v);
            line = input.command()?;
        }
        let Some(spec) = field(&line, b"from ") else {
            crate::git_fatal!("expected 'from' command, got '{}'", command_buf());
        };
        // `parse_new_tag()` reads the mark's own type out of the mark table and
        // tags whatever it finds (builtin/fast-import.c:3220-3225), so a tag of
        // a blob or of another tag is legal where a `<commit-ish>` is not.
        let object = match spec.strip_prefix(b":") {
            Some(v) => self.mark_ref(v)?,
            None => self.commitish(&spec)?,
        };
        line = input.command()?;

        if field(&line, b"original-oid ").is_some() {
            line = input.command()?;
        }
        let mut tagger = None;
        if let Some(v) = field(&line, b"tagger ") {
            tagger = Some(self.ident(&v)?);
            line = input.command()?;
        }
        let Some(spec) = field(&line, b"data ") else {
            crate::git_fatal!("expected 'data n' command, found: {}", command_buf());
        };
        let mut message = input.data(&spec)?;

        // A tag's signature lives in its message, so the mode is applied by
        // truncating at the marker git's `parse_signature` would find.
        if let Some(at) = signature_offset(&message) {
            match self.opts.signed_tags {
                SignedMode::Verbatim => {}
                SignedMode::WarnVerbatim => {
                    eprintln!("warning: importing a tag signature verbatim for tag '{name}'");
                }
                SignedMode::Strip => message.truncate(at),
                SignedMode::WarnStrip => {
                    eprintln!("warning: stripping a tag signature for tag '{name}'");
                    message.truncate(at);
                }
                SignedMode::Abort => {
                    crate::git_fatal!("encountered signed tag; use --signed-tags=<mode> to handle it")
                }
                SignedMode::NeedsVerification => bail!(
                    "unsupported flag \"--signed-tags=<mode>-if-invalid\" (the `-if-invalid` \
                     modes decide by verifying the signature, which needs a gpg driver the \
                     vendored crates do not provide)"
                ),
            }
        }

        let kind = self.repo.find_header(object)?.kind();
        let mut buf = Vec::new();
        buf.extend_from_slice(format!("object {object}\ntype {kind}\ntag {name}\n").as_bytes());
        if let Some(t) = &tagger {
            buf.extend_from_slice(b"tagger ");
            buf.extend_from_slice(t);
            buf.push(b'\n');
        }
        buf.push(b'\n');
        buf.extend_from_slice(&message);

        // `if (store_object(OBJ_TAG, …)) t->pack_id = MAX_PACK_ID; else
        //  t->pack_id = pack_id;` (builtin/fast-import.c:3272-3275).
        let (id, stored) = self.pack.store(&self.repo, Kind::Tag, &buf, Last::None)?;
        self.drain_closed_packs()?;
        if let Some(m) = mark {
            self.insert_mark(m, id);
        }
        self.tags[slot].oid = id;
        self.tags[slot].pack_id = match stored {
            true => self.pack.pack_id,
            false => MAX_PACK_ID,
        };
        Ok(())
    }

    /// `reset <ref> LF from? LF?`.
    ///
    /// `parse_reset_branch()` (builtin/fast-import.c:3317-3360): an existing
    /// branch loses its tip and both tree versions and lets go of its loaded
    /// tree, but stays on the active list and keeps its note count.
    fn parse_reset(&mut self, input: &mut Input, name: &str) -> Result<()> {
        let idx = match self.by_name.get(name) {
            Some(&idx) => {
                let null = ObjectId::null(self.repo.object_hash());
                let b = &mut self.branches[idx];
                b.head = None;
                b.tree.versions[0].oid = null;
                b.tree.versions[1].oid = null;
                if let Some(t) = b.tree.tree.take() {
                    release_tree_content_recursive(&mut self.pools, t);
                }
                idx
            }
            None => self.branch(name)?,
        };

        if let Some(line) = input.command()? {
            // Take an owned copy so the pushback below is not holding a borrow.
            match after(&line, b"from ").map(<[u8]>::to_vec) {
                // `parse_objectish()` ends in `read_next_command()`, and
                // `if (command_buf.len > 0) unread_command_buf = 1;` hands a
                // line that is not the blank terminator back.
                Some(from) => {
                    self.parse_from(idx, &from)?;
                    if let Some(next) = input.command()? {
                        if !next.is_empty() {
                            input.unread(next);
                        }
                    }
                }
                // The optional blank line that ends the command, or something
                // that belongs to whoever reads next.
                None if line.is_empty() => {}
                None => input.unread(line),
            }
        }

        // A `reset refs/tags/<name>` to the null id deletes the ref, and
        // `dump_branches()` runs before `dump_tags()`, so the first tag of that
        // name comes off the tag list or it would write the ref straight back.
        if self.branches[idx].delete {
            if let Some(tag_name) = self.branches[idx].name.strip_prefix("refs/tags/") {
                if let Some(at) = self.tags.iter().position(|t| t.name == tag_name) {
                    self.tags.remove(at);
                }
            }
        }
        Ok(())
    }

    /// `alias LF mark 'to' SP <commit-ish> LF`.
    ///
    /// ```c
    /// static void parse_alias(void)
    /// {
    ///         struct object_entry *e;
    ///         struct branch b;
    ///
    ///         skip_optional_lf();
    ///         read_next_command();
    ///
    ///         /* mark ... */
    ///         parse_mark();
    ///         if (!next_mark)
    ///                 die(_("expected 'mark' command, got %s"), command_buf.buf);
    ///
    ///         /* to ... */
    ///         memset(&b, 0, sizeof(b));
    ///         if (!parse_objectish_with_prefix(&b, "to "))
    ///                 die(_("expected 'to' command, got %s"), command_buf.buf);
    /// ```
    ///
    /// (builtin/fast-import.c:3623-3642.) The optional LF is eaten *before* the
    /// block, not after it, and `parse_objectish()` ends with its own
    /// `read_next_command()` whose result nothing here puts back — so the line
    /// after `to <objectish>` is consumed and discarded. In a stream that
    /// separates its blocks with a blank line that line is the blank; in one
    /// that does not, the next command is swallowed, which is why stock 2.55.0
    /// answers `fatal: unsupported command: from :9` for an `alias` block
    /// followed immediately by `reset`/`from`.
    fn parse_alias(&mut self, input: &mut Input) -> Result<()> {
        input.skip_optional_lf()?;
        let line = input.command()?;
        let Some(v) = field(&line, b"mark :") else {
            // `command_buf` is what git quotes, and at EOF it still holds the
            // previous line rather than an empty one.
            crate::git_fatal!("expected 'mark' command, got {}", command_buf());
        };
        // `if (!next_mark) die(_("expected 'mark' command, got %s"), …)`: a
        // `mark :` whose argument is not a positive number is no mark at all,
        // and `alias` is one of the callers that insists on one.
        let Some(mark) = parse_mark_decl(&v) else {
            crate::git_fatal!("expected 'mark' command, got {}", command_buf());
        };
        let line = input.command()?;
        let Some(spec) = field(&line, b"to ") else {
            crate::git_fatal!("expected 'to' command, got {}", command_buf());
        };
        let id = self.commitish(&spec)?;
        self.insert_mark(mark, id);
        input.command()?;
        Ok(())
    }

    /// `parse_feature()` and `parse_one_feature()` (builtin/fast-import.c:
    /// 3884-3961). Every name is matched exactly, or as a `<name>=` prefix
    /// where it takes an argument, so `feature force=1` is unsupported and
    /// `feature date-format` without its `=` is too.
    fn parse_feature(&mut self, feature: &str) -> Result<()> {
        if self.seen_data_command {
            crate::git_fatal!("got feature command '{feature}' after data command");
        }
        if let Some(arg) = feature.strip_prefix("date-format=") {
            self.opts.date_format = date_format(arg)?;
        } else if let Some(arg) = feature.strip_prefix("import-marks=") {
            self.check_unsafe_feature("import-marks")?;
            self.option_import_marks(arg, true, false)?;
        } else if let Some(arg) = feature.strip_prefix("import-marks-if-exists=") {
            self.check_unsafe_feature("import-marks-if-exists")?;
            self.option_import_marks(arg, true, true)?;
        } else if let Some(arg) = feature.strip_prefix("export-marks=") {
            // git names the whole feature here, argument and all.
            self.check_unsafe_feature(feature)?;
            self.option_export_marks(arg);
        } else if let Some(arg) = feature
            .strip_prefix("rewrite-submodules-to=")
            .or_else(|| feature.strip_prefix("rewrite-submodules-from="))
        {
            self.submodule_rewrite(arg)?;
        } else {
            match feature {
                // Capability probes for commands this port implements.
                "alias" | "get-mark" | "cat-blob" | "notes" | "ls" => {}
                "relative-marks" => self.opts.relative_marks = true,
                "no-relative-marks" => self.opts.relative_marks = false,
                "done" => self.opts.require_done = true,
                "force" => self.opts.force = true,
                _ => crate::git_fatal!("this version of fast-import does not support feature {feature}."),
            }
        }
        Ok(())
    }

    /// `check_unsafe_feature()` (builtin/fast-import.c:3876-3882).
    fn check_unsafe_feature(&self, feature: &str) -> Result<()> {
        if !self.opts.allow_unsafe {
            crate::git_fatal!("feature '{feature}' forbidden in input without --allow-unsafe-features");
        }
        Ok(())
    }

    /// `option git <option>`: `parse_option()` and `parse_one_option()`
    /// (builtin/fast-import.c:3850-3874, 3974-3983). The same callbacks the
    /// command line runs, so a bad value fails the same way.
    fn parse_option(&mut self, option: &str) -> Result<()> {
        if self.seen_data_command {
            crate::git_fatal!("got option command '{option}' after data command");
        }
        if let Some(v) = option.strip_prefix("max-pack-size=") {
            self.pack.max_packsize = option_max_pack_size(v)?;
        } else if let Some(v) = option.strip_prefix("big-file-threshold=") {
            self.pack.big_file_threshold = option_big_file_threshold(v)?;
        } else if let Some(v) = option.strip_prefix("depth=") {
            self.pack.max_depth = option_depth(v)?;
        } else if let Some(v) = option.strip_prefix("active-branches=") {
            self.max_active_branches = ulong_arg("--active-branches", v)?;
        } else if let Some(v) = option.strip_prefix("export-pack-edges=") {
            self.option_export_pack_edges(v)?;
        } else if let Some(v) = option.strip_prefix("signed-commits=") {
            self.opts.signed_commits = signed_mode("--signed-commits", v)?;
        } else if let Some(v) = option.strip_prefix("signed-tags=") {
            self.opts.signed_tags = signed_mode("--signed-tags", v)?;
        } else if option == "quiet" {
            self.show_stats = false;
        } else if option == "stats" {
            self.show_stats = true;
        } else if option != "allow-unsafe-features" {
            crate::git_fatal!("this version of fast-import does not support option: {option}");
        }
        Ok(())
    }

    // -- file changes -------------------------------------------------------

    /// `M SP <mode> SP <dataref> SP <path>`, with the dataref possibly `inline`.
    ///
    /// Port of `file_change_m()` (builtin/fast-import.c:2370-2469) including its
    /// order: mode, dataref, path, the empty-directory shortcut, then the
    /// gitlink and `inline` refusals, then the type check. Every diagnostic in
    /// that function names `command_buf.buf` — the whole `M …` line as typed —
    /// so `line` is carried in rather than the remainder being reassembled.
    fn file_modify(&mut self, input: &mut Input, idx: usize, line: &[u8]) -> Result<()> {
        let shown = String::from_utf8_lossy(line).into_owned();
        let rest = &line[b"M ".len()..];
        // ```c
        // p = parse_mode(p, &mode);
        // if (!p)
        //         die(_("corrupt mode: %s"), command_buf.buf);
        // ```
        //
        // …and the `switch (mode)` below it, whose `default:` is the same die,
        // so an octal that parses but is not one git tracks reads identically.
        let (mode_word, rest) =
            split_space(rest).ok_or_else(|| anyhow!("corrupt mode: {shown}"))?;
        let mode = canonical_mode(mode_word).ok_or_else(|| anyhow!("corrupt mode: {shown}"))?;

        let hexsz = self.repo.object_hash().len_in_hex();
        let mut inline_data = false;
        let (oid, path) = if rest.first() == Some(&b':') {
            let (oid, rest) = self.mark_ref_space(rest)?;
            (oid, unquote(rest)?)
        } else if let Some(after_inline) = after(rest, b"inline ") {
            inline_data = true;
            (ObjectId::null(self.repo.object_hash()), unquote(after_inline)?)
        } else {
            // `parse_mapped_oid_hex()` consumes exactly `hexsz` characters and
            // leaves `p` on whatever follows; the space is checked separately,
            // which is why a 41-character id is `missing space after SHA1` and
            // not `invalid dataref`.
            if rest.len() < hexsz {
                crate::git_fatal!("invalid dataref: {shown}");
            }
            let Ok(oid) = ObjectId::from_hex(&rest[..hexsz]) else {
                crate::git_fatal!("invalid dataref: {shown}");
            };
            let tail = &rest[hexsz..];
            if tail.first() != Some(&b' ') {
                crate::git_fatal!("missing space after SHA1: {shown}");
            }
            (oid, unquote(&tail[1..])?)
        };

        // `if (S_ISDIR(mode) && is_empty_tree_oid(&oid) && *path.buf)`: git does
        // not track empty directories below the root, and the shortcut returns
        // before any of the checks below.
        if mode == 0o40000
            && !inline_data
            && oid == ObjectId::empty_tree(self.repo.object_hash())
            && !path.is_empty()
        {
            let mut g = Ctx { repo: &self.repo, pack: &mut self.pack, pools: &mut self.pools };
            tree_content_remove(&mut g, &mut self.branches[idx].tree, &path, None, false)?;
            return Ok(());
        }

        if mode == 0o160000 && inline_data {
            crate::git_fatal!("Git links cannot be specified 'inline': {shown}");
        }
        if mode == 0o40000 && inline_data {
            crate::git_fatal!("directories cannot be specified 'inline': {shown}");
        }

        let oid = if inline_data {
            // `while (read_next_command() != EOF)`: a `cat-blob` may come
            // between the `M` and its `data` (builtin/fast-import.c:2460-2468).
            loop {
                let dline = input.command()?;
                if let Some(v) = field(&dline, b"cat-blob ") {
                    self.cat_blob(&v)?;
                    continue;
                }
                break self.parse_and_store_blob(input, &dline)?;
            }
        } else {
            oid
        };

        // A gitlink is the only thing `--rewrite-submodules-from/-to` would touch,
        // and mapping its object id through the two marks files is not ported, so
        // the run stops here rather than record the un-rewritten id.
        if mode == 0o160000 && !self.submodule_rewrites.is_empty() {
            bail!(
                "unsupported flag \"--rewrite-submodules-{{from,to}}={}\" for a stream that \
                 carries a gitlink (mapping its object id through the submodule marks files \
                 is not ported)",
                self.submodule_rewrites.join(",")
            );
        }

        // The object must actually be what the mode claims it is. A gitlink is
        // exempt from the presence check: the commit it names lives in the
        // submodule's repository, not this one, so git never looks for it here.
        let want = match mode {
            0o40000 => Kind::Tree,
            0o160000 => Kind::Commit,
            _ => Kind::Blob,
        };
        // ```c
        // if (type < 0)
        //         die(_("%s not found: %s"),
        //             S_ISDIR(mode) ?  _("tree") : _("blob"),
        //             command_buf.buf);
        // if (type != expected)
        //         die(_("not a %s (actually a %s): %s"),
        //                 type_name(expected), type_name(type),
        //                 command_buf.buf);
        // ```
        //
        // (builtin/fast-import.c:2446-2459.) The object just written for an
        // `inline` payload is a blob by construction and git never re-reads it,
        // so the check is skipped there as it is in the C.
        if !inline_data {
            match self.repo.try_find_header(oid)? {
                None if mode == 0o160000 => {}
                None => crate::git_fatal!("{want} not found: {shown}"),
                Some(header) if header.kind() != want => crate::git_fatal!(
                    "not a {want} (actually a {}): {shown}",
                    header.kind()
                ),
                Some(_) => {}
            }
        }

        // ```c
        // if (!*path.buf) {
        //         tree_content_replace(&b->branch_tree, &oid, mode, NULL);
        //         return;
        // }
        // …
        // tree_content_set(&b->branch_tree, path.buf, &oid, mode, NULL);
        // ```
        //
        // A directory is set by id alone; its entries are read when something
        // walks into it.
        let root = &mut self.branches[idx].tree;
        if path.is_empty() {
            return tree_content_replace(&mut self.pools, root, oid, mode, None);
        }
        let mut g = Ctx { repo: &self.repo, pack: &mut self.pack, pools: &mut self.pools };
        tree_content_set(&mut g, root, &path, oid, mode, None)?;
        Ok(())
    }

    /// `file_change_deleteall()` (builtin/fast-import.c:2622-2630).
    fn file_change_deleteall(&mut self, idx: usize) -> Result<()> {
        let null = ObjectId::null(self.repo.object_hash());
        let b = &mut self.branches[idx];
        if let Some(t) = b.tree.tree.take() {
            release_tree_content_recursive(&mut self.pools, t);
        }
        b.tree.versions[0].oid = null;
        b.tree.versions[1].oid = null;
        let mut g = Ctx { repo: &self.repo, pack: &mut self.pack, pools: &mut self.pools };
        load_tree(&mut g, &mut b.tree)?;
        b.notes = 0;
        Ok(())
    }

    /// `C SP <src> SP <dst>` and `R SP <src> SP <dst>` (`remove_source` for `R`).
    ///
    /// `file_change_cr()` (builtin/fast-import.c:2503-2535): the source entry
    /// is taken out (`R`) or copied (`C`) into a leaf, with a dirty subtree
    /// carried along, and set at the destination — or made the root when the
    /// destination is empty.
    fn file_copy(&mut self, idx: usize, rest: &[u8], remove_source: bool) -> Result<()> {
        let (src, dst) = split_two_paths(rest)?;
        let mut leaf = TreeEntry::detached(self.repo.object_hash(), 0);
        let root = &mut self.branches[idx].tree;
        let mut g = Ctx { repo: &self.repo, pack: &mut self.pack, pools: &mut self.pools };
        match remove_source {
            true => tree_content_remove(&mut g, root, &src, Some(&mut leaf), true)?,
            false => tree_content_get(&mut g, root, &src, &mut leaf, true)?,
        };
        if leaf.versions[1].mode == 0 {
            crate::git_fatal!("path {} not in branch", String::from_utf8_lossy(&src));
        }
        let subtree = leaf.tree.take();
        if dst.is_empty() {
            return tree_content_replace(g.pools, root, leaf.versions[1].oid, leaf.versions[1].mode, subtree);
        }
        tree_content_set(&mut g, root, &dst, leaf.versions[1].oid, leaf.versions[1].mode, subtree)?;
        Ok(())
    }

    /// `N SP <dataref> SP <commit-ish>` or `N SP inline SP <commit-ish>` + data.
    ///
    /// `note_change_n()` (builtin/fast-import.c:2537-2620). A branch whose note
    /// count is still unknown has its notes counted first, in
    /// `change_note_fanout()`'s counting mode; the note is then removed from
    /// the path the old fanout gives it and set at the one the new count gives.
    /// The fanout of the notes already there is fixed up once the commit's
    /// changes are all read.
    fn note_change(&mut self, input: &mut Input, idx: usize, rest: &[u8], old_fanout: &mut u8) -> Result<()> {
        if self.branches[idx].notes == 0 && *old_fanout == 0 {
            let mut g = Ctx { repo: &self.repo, pack: &mut self.pack, pools: &mut self.pools };
            let notes = change_note_fanout(&mut g, &mut self.branches[idx].tree, 0xff)?;
            self.branches[idx].notes = notes;
            *old_fanout = convert_num_notes_to_fanout(notes);
        }

        // `<dataref>` or `inline`.
        let (inline_data, oid, oe, target) = if let Some(after_inline) = after(rest, b"inline ") {
            (true, ObjectId::null(self.repo.object_hash()), None, after_inline.to_vec())
        } else {
            let (oid, target) = self.dataref_space(rest, "SHA1")?;
            (false, oid, self.pack.table.get(&oid).copied(), target.to_vec())
        };

        // `<commit-ish>`.
        let text = utf8(&target, "commit-ish")?;
        let commit = if let Some(&s) = self.by_name.get(&text) {
            match self.branches[s].head {
                Some(id) => id,
                None => crate::git_fatal!("can't add a note on empty branch."),
            }
        } else if let Some(v) = target.strip_prefix(b":") {
            let mark = parse_mark_ref_eol(v)?;
            let id = self.find_mark(mark)?;
            if self.pack.table.get(&id).map(|e| e.kind) != Some(Kind::Commit) {
                crate::git_fatal!("mark :{mark} not a commit");
            }
            id
        } else {
            let Some(id) = self.get_oid(&text) else {
                crate::git_fatal!("invalid ref name or SHA1 expression: {text}");
            };
            // `odb_read_object_peeled(…, OBJ_COMMIT, …)`.
            match self.repo.find_object(id).ok().and_then(|o| o.peel_to_commit().ok()) {
                Some(commit) => commit.id,
                None => crate::git_fatal!("not a valid commit: {text}"),
            }
        };

        let oid = if inline_data {
            let line = input.command()?;
            self.parse_and_store_blob(input, &line)?
        } else {
            match oe {
                Some(oe) if oe.kind != Kind::Blob => {
                    crate::git_fatal!("not a blob (actually a {}): {}", oe.kind, command_buf())
                }
                Some(_) => {}
                None if oid.is_null() => {}
                None => match self.repo.try_find_header(oid)? {
                    None => crate::git_fatal!("blob not found: {}", command_buf()),
                    Some(h) if h.kind() != Kind::Blob => {
                        crate::git_fatal!("not a blob (actually a {}): {}", h.kind(), command_buf())
                    }
                    Some(_) => {}
                },
            }
            oid
        };

        let rawsz = self.repo.object_hash().len_in_bytes();
        let hex = commit.to_string().into_bytes();
        let path = construct_path_with_fanout(&hex, *old_fanout, rawsz)?;
        let mut g = Ctx { repo: &self.repo, pack: &mut self.pack, pools: &mut self.pools };
        let b = &mut self.branches[idx];
        if tree_content_remove(&mut g, &mut b.tree, &path, None, false)? {
            b.notes = b.notes.wrapping_sub(1);
        }
        if oid.is_null() {
            // Nothing to insert: the note is deleted.
            return Ok(());
        }
        b.notes += 1;
        let new_fanout = convert_num_notes_to_fanout(b.notes);
        let path = construct_path_with_fanout(&hex, new_fanout, rawsz)?;
        tree_content_set(&mut g, &mut b.tree, &path, oid, S_IFREG | 0o644, None)?;
        Ok(())
    }

    // -- queries ------------------------------------------------------------

    /// `cat-blob <dataref>` → `<oid> SP blob SP <size> LF <contents> LF`.
    /// ```c
    /// if (type <= 0) {
    ///         strbuf_reset(&line);
    ///         strbuf_addf(&line, "%s missing\n", oid_to_hex(oid));
    ///         cat_blob_write(line.buf, line.len);
    ///         [...]
    ///         return;
    /// }
    /// if (!buf)
    ///         die(_("can't read object %s"), oid_to_hex(oid));
    /// if (type != OBJ_BLOB)
    ///         die(_("object %s is a %s but a blob was expected."),
    ///             oid_to_hex(oid), type_name(type));
    /// ```
    ///
    /// (`cat_blob()`, builtin/fast-import.c:3332-3363.) An object that is not
    /// there is answered on the cat-blob channel the way `cat-file --batch`
    /// answers it, not as a fatal error.
    fn cat_blob(&mut self, spec: &[u8]) -> Result<()> {
        let oid = self.dataref(spec)?;
        let Some(header) = self.repo.try_find_header(oid)? else {
            return self.respond(format!("{oid} missing\n").as_bytes());
        };
        if header.kind() != Kind::Blob {
            crate::git_fatal!("object {oid} is a {} but a blob was expected.", header.kind());
        }
        let data = self.repo.find_object(oid)?.data.clone();
        let mut out = format!("{oid} blob {}\n", data.len()).into_bytes();
        out.extend_from_slice(&data);
        out.push(b'\n');
        self.respond(&out)?;
        // A blob of the pack being written becomes the next blob's delta base
        // (builtin/fast-import.c:3416-3421).
        if let Some(oe) = self.pack.table.get(&oid).copied().filter(|oe| oe.pack_id == self.pack.pack_id) {
            self.pack.last_blob = LastObject { data, offset: oe.offset, depth: oe.depth };
        }
        Ok(())
    }

    /// `ls [<dataref> SP] <path>` → an `ls-tree` line, or `missing <path>`.
    ///
    /// `parse_ls()` (builtin/fast-import.c:3609-3644): a quoted path reads the
    /// commit's own tree; a tree-ish is peeled into a root entry of its own. A
    /// directory still being built is stored first, so its id can be printed.
    fn parse_ls(&mut self, rest: &[u8], branch: Option<usize>) -> Result<()> {
        let hash = self.repo.object_hash();
        let (mut own_root, path) = if rest.first() == Some(&b'"') {
            if branch.is_none() {
                crate::git_fatal!("not in a commit: {}", command_buf());
            }
            (None, unquote(rest)?)
        } else {
            // `parse_ls()` takes the tree-ish through `parse_mark_ref_space()`
            // for a mark (builtin/fast-import.c:3518), so a mark with trailing
            // junk is `missing space after mark`, not `garbage after mark`.
            let (oid, rest) = self.dataref_space(rest, "tree-ish")?;
            let tree = self.parse_treeish(oid)?;
            // `root = new_tree_entry(); …; load_tree(root);`
            self.pools.new_tree_entry();
            let mut root = TreeEntry::detached(hash, 0);
            root.versions[1].oid = tree;
            if !tree.is_null() {
                root.versions[1].mode = S_IFDIR;
            }
            let mut g = Ctx { repo: &self.repo, pack: &mut self.pack, pools: &mut self.pools };
            load_tree(&mut g, &mut root)?;
            (Some(root), unquote(rest)?)
        };

        let mut leaf = TreeEntry::detached(hash, 0);
        let mut g = Ctx { repo: &self.repo, pack: &mut self.pack, pools: &mut self.pools };
        let root = match (&mut own_root, branch) {
            (Some(root), _) => root,
            (None, Some(idx)) => &mut self.branches[idx].tree,
            (None, None) => unreachable!("refused above"),
        };
        tree_content_get(&mut g, root, &path, &mut leaf, true)?;
        // `A directory in preparation would have a sha1 of zero until it is
        // saved.  Save, for simplicity.`
        if s_isdir(leaf.versions[1].mode) {
            store_tree(&mut g, &mut leaf)?;
        }
        self.drain_closed_packs()?;

        // `print_ls()` (builtin/fast-import.c:3584-3607).
        let mode = leaf.versions[1].mode;
        let mut line = match mode {
            0 => b"missing ".to_vec(),
            _ => {
                let kind = match mode & S_IFMT {
                    0o160000 => "commit",
                    S_IFDIR => "tree",
                    _ => "blob",
                };
                format!("{:06o} {kind} {}\t", mode & !NO_DELTA, leaf.versions[1].oid).into_bytes()
            }
        };
        line.extend_from_slice(&crate::quote::quoted_name_bytes(&path));
        line.push(b'\n');

        if let Some(t) = leaf.tree.take() {
            release_tree_content_recursive(&mut self.pools, t);
        }
        if let Some(root) = own_root {
            release_tree_entry(&mut self.pools, root);
        }
        self.respond(&line)
    }

    /// `parse_treeish_dataref()`'s peel (builtin/fast-import.c:3440-3502,
    /// 3558-3582): commits and tags are dereferenced one layer at a time until
    /// a tree is reached, and an object the table did not know is entered into
    /// it on the way.
    fn parse_treeish(&mut self, mut oid: ObjectId) -> Result<ObjectId> {
        loop {
            let kind = match self.pack.table.get(&oid) {
                Some(e) => e.kind,
                None => {
                    // `dereference()`: `/* cache it! */`
                    let Some(header) = self.repo.try_find_header(oid)? else {
                        crate::git_fatal!("object not found: {oid}");
                    };
                    let kind = header.kind();
                    self.pack.table.insert(oid, ObjectEntry { kind, pack_id: MAX_PACK_ID, depth: 0, offset: 1 });
                    kind
                }
            };
            let hexsz = oid.kind().len_in_hex();
            let (prefix, what) = match kind {
                Kind::Tree => return Ok(oid),
                Kind::Commit => (&b"tree "[..], "commit"),
                Kind::Tag => (&b"object "[..], "tag"),
                Kind::Blob => crate::git_fatal!("not a tree-ish: {}", command_buf()),
            };
            let Ok(object) = self.repo.find_object(oid) else {
                crate::git_fatal!("can't load object {oid}");
            };
            // The id is read at a fixed offset, past `tree ` or `object `, as
            // `get_oid_hex(buf + strlen("tree "), oid)` reads it.
            let peeled = object
                .data
                .get(prefix.len()..prefix.len() + hexsz)
                .and_then(|hex| ObjectId::from_hex(hex).ok());
            match peeled {
                Some(next) => oid = next,
                None => crate::git_fatal!("invalid SHA1 in {what}: {}", command_buf()),
            }
        }
    }

    /// Write a query response to `--cat-blob-fd`, or stdout when unset.
    fn respond(&mut self, buf: &[u8]) -> Result<()> {
        match self.opts.cat_blob_fd {
            None => stdout_write(buf),
            Some(fd) => {
                // SAFETY: the caller promised `fd` is open for writing; the
                // `ManuallyDrop` keeps the descriptor open for the next response
                // and for whoever owns it after this process is done with it.
                use std::os::fd::FromRawFd;
                let file = unsafe { std::fs::File::from_raw_fd(fd) };
                let mut file = std::mem::ManuallyDrop::new(file);
                file.write_all(buf)
                    .with_context(|| format!("unable to write to --cat-blob-fd={fd}"))?;
                file.flush()?;
                Ok(())
            }
        }
    }

    // -- resolution ---------------------------------------------------------

    /// The index of the branch called `name`, creating the in-memory entry on
    /// first mention. A newly named branch starts empty even when the ref exists
    /// — git only consults the repository when a `from` command asks it to.
    fn branch(&mut self, name: &str) -> Result<usize> {
        if let Some(idx) = self.by_name.get(name) {
            return Ok(*idx);
        }
        if !refname_ok_onelevel(name) {
            crate::git_fatal!("branch name doesn't conform to Git standards: {name}");
        }
        // `new_branch()` (builtin/fast-import.c:647-667).
        self.pools.alloc(SIZEOF_BRANCH);
        self.pools.strdup(name);
        let idx = self.branches.len();
        self.branches.push(Branch {
            name: name.to_string(),
            head: None,
            tree: TreeEntry::detached(self.repo.object_hash(), S_IFDIR),
            last_commit: 0,
            notes: 0,
            active: false,
            delete: false,
            pack_id: MAX_PACK_ID,
        });
        self.by_name.insert(name.to_string(), idx);
        Ok(idx)
    }

    /// The branches in `branch_table` order: by `hc_str(name) % branch_table_sz`
    /// bucket, the most recently created first within a bucket
    /// (builtin/fast-import.c:565-570, 647-667). `dump_branches()`, the pack
    /// edges and the crash report all walk the table this way.
    fn branch_table_order(&self) -> Vec<usize> {
        let mut order: Vec<usize> = (0..self.branches.len()).collect();
        order.sort_by_key(|&idx| (hc_str(self.branches[idx].name.as_bytes()) % BRANCH_TABLE_SZ, std::cmp::Reverse(idx)));
        order
    }

    /// `unload_one_branch()` (builtin/fast-import.c:2097-2130): while the
    /// active list is full, drop the branch with the oldest commit clock — the
    /// first such on the list — and let go of its tree.
    fn unload_one_branch(&mut self) {
        while !self.active_branches.is_empty() && self.active_branches.len() as u64 >= self.max_active_branches {
            let mut min_commit = u64::MAX;
            let mut at = 0;
            for (pos, &idx) in self.active_branches.iter().enumerate() {
                if self.branches[idx].last_commit < min_commit {
                    at = pos;
                    min_commit = self.branches[idx].last_commit;
                }
            }
            let idx = self.active_branches.remove(at);
            let e = &mut self.branches[idx];
            e.active = false;
            if let Some(t) = e.tree.tree.take() {
                release_tree_content_recursive(&mut self.pools, t);
            }
        }
    }

    /// `load_branch()` (builtin/fast-import.c:2132-2142).
    fn load_branch(&mut self, idx: usize) -> Result<()> {
        let mut g = Ctx { repo: &self.repo, pack: &mut self.pack, pools: &mut self.pools };
        load_tree(&mut g, &mut self.branches[idx].tree)?;
        if !self.branches[idx].active {
            self.branches[idx].active = true;
            self.active_branches.insert(0, idx);
            self.branch_load_count += 1;
        }
        Ok(())
    }

    /// Apply a `from <commit-ish>` to branch `idx`: adopt its tip and its tree.
    ///
    /// `parse_objectish()` (builtin/fast-import.c:2654-2697): another branch
    /// lends its tip and its stored tree id; a mark, its commit; anything else
    /// `get_oid()` resolves, the commit it peels to. Only the tree's id is
    /// taken — a loaded tree that no longer matches it is let go and read again
    /// when the commit needs it.
    fn parse_from(&mut self, idx: usize, spec: &[u8]) -> Result<()> {
        let text = utf8(spec, "commit-ish")?;
        let saved = self.branches[idx].tree.versions[1].oid;

        if let Some(&s) = self.by_name.get(&text) {
            if s == idx {
                crate::git_fatal!("can't create a branch from itself: {}", self.branches[idx].name);
            }
            let (head, t) = (self.branches[s].head, self.branches[s].tree.versions[1].oid);
            let b = &mut self.branches[idx];
            b.head = head;
            b.tree.versions[0].oid = t;
            b.tree.versions[1].oid = t;
        } else if let Some(v) = spec.strip_prefix(b":") {
            let mark = parse_mark_ref_eol(v)?;
            let id = self.find_mark(mark)?;
            if self.pack.table.get(&id).map(|e| e.kind) != Some(Kind::Commit) {
                crate::git_fatal!("mark :{mark} not a commit");
            }
            if self.branches[idx].head != Some(id) {
                self.branches[idx].head = Some(id);
                self.parse_from_existing(idx)?;
            }
        } else if let Some(id) = self.get_oid(&text) {
            self.branches[idx].head = (!id.is_null()).then_some(id);
            self.parse_from_existing(idx)?;
            if id.is_null() {
                self.branches[idx].delete = true;
            }
        } else {
            crate::git_fatal!("invalid ref name or SHA1 expression: {text}");
        }

        let b = &mut self.branches[idx];
        if b.tree.tree.is_some() && saved != b.tree.versions[1].oid {
            if let Some(t) = b.tree.tree.take() {
                release_tree_content_recursive(&mut self.pools, t);
            }
        }
        Ok(())
    }

    /// `parse_from_existing()` and `parse_from_commit()`
    /// (builtin/fast-import.c:2632-2652): both tree versions become the tree of
    /// the commit the tip peels to, and the tip becomes that commit.
    fn parse_from_existing(&mut self, idx: usize) -> Result<()> {
        let null = ObjectId::null(self.repo.object_hash());
        let Some(id) = self.branches[idx].head else {
            self.branches[idx].tree.versions[0].oid = null;
            self.branches[idx].tree.versions[1].oid = null;
            return Ok(());
        };
        let peeled = self.repo.find_object(id).ok().and_then(|o| o.peel_to_commit().ok());
        let Some(commit) = peeled else {
            crate::git_fatal!("not a valid commit: {id}");
        };
        let Ok(tree) = commit.tree_id() else {
            crate::git_fatal!("the commit {} is corrupt", commit.id);
        };
        let b = &mut self.branches[idx];
        b.head = Some(commit.id);
        b.tree.versions[0].oid = tree.detach();
        b.tree.versions[1].oid = b.tree.versions[0].oid;
        Ok(())
    }

    /// `parse_merge()` (builtin/fast-import.c:2717-2749) for one `merge` line:
    /// a branch's tip as it stands, a mark's commit, or the commit anything
    /// else peels to.
    fn parse_merge(&mut self, spec: &[u8]) -> Result<ObjectId> {
        let text = utf8(spec, "commit-ish")?;
        if let Some(&s) = self.by_name.get(&text) {
            return Ok(self.branches[s].head.unwrap_or_else(|| ObjectId::null(self.repo.object_hash())));
        }
        if let Some(v) = spec.strip_prefix(b":") {
            let mark = parse_mark_ref_eol(v)?;
            let id = self.find_mark(mark)?;
            if self.pack.table.get(&id).map(|e| e.kind) != Some(Kind::Commit) {
                crate::git_fatal!("mark :{mark} not a commit");
            }
            return Ok(id);
        }
        let Some(id) = self.get_oid(&text) else {
            crate::git_fatal!("invalid ref name or SHA1 expression: {text}");
        };
        match self.repo.find_object(id).ok().and_then(|o| o.peel_to_commit().ok()) {
            Some(commit) => Ok(commit.id),
            None => crate::git_fatal!("not a valid commit: {text}"),
        }
    }

    /// `repo_get_oid()` for a commit-ish that is neither a branch of the
    /// stream nor a mark: a full hex id as it is, or any revision expression.
    fn get_oid(&self, text: &str) -> Option<ObjectId> {
        if text.len() == self.repo.object_hash().len_in_hex() {
            if let Ok(id) = ObjectId::from_hex(text.as_bytes()) {
                return Some(id);
            }
        }
        self.repo.rev_parse_single(text).ok().map(|id| id.detach())
    }

    /// Resolve a `<commit-ish>`: a mark, a branch, or any revision expression.
    fn commitish(&self, spec: &[u8]) -> Result<ObjectId> {
        let id = self.commitish_allow_null(spec)?;
        if id.is_null() {
            crate::git_fatal!("invalid null object id in commit-ish");
        }
        Ok(id)
    }

    /// As [`Importer::commitish`], but the all-zero id is a legal answer.
    fn commitish_allow_null(&self, spec: &[u8]) -> Result<ObjectId> {
        if let Some(v) = spec.strip_prefix(b":") {
            // ```c
            // uintmax_t idnum = parse_mark_ref_eol(objectish);
            // struct object_entry *oe = find_mark(marks, idnum);
            // if (oe->type != OBJ_COMMIT)
            //         die(_("mark :%" PRIuMAX " not a commit"), idnum);
            // ```
            //
            // (`parse_objectish()`, builtin/fast-import.c:2662-2666, with the
            // same three lines in `parse_merge()` at 2727-2730 and in
            // `note_change_n()` at 2562-2566.) `parse_new_tag()` is the one
            // caller that takes a mark of any type, so it resolves the mark
            // itself rather than coming through here.
            let mark = parse_mark_ref_eol(v)?;
            let id = self.find_mark(mark)?;
            if self.repo.try_find_header(id)?.map(|h| h.kind()) != Some(Kind::Commit) {
                crate::git_fatal!("mark :{mark} not a commit");
            }
            return Ok(id);
        }
        let text = utf8(spec, "commit-ish")?;
        if let Some(&idx) = self.by_name.get(&text) {
            return self.branches[idx]
                .head
                .ok_or_else(|| anyhow!("Branch {text} has no commits"));
        }
        if text.len() == self.repo.object_hash().len_in_hex() {
            if let Ok(id) = ObjectId::from_hex(text.as_bytes()) {
                return Ok(id);
            }
        }
        Ok(self
            .repo
            .rev_parse_single(text.as_str())
            .map_err(|_| anyhow!("invalid ref name or SHA1 expression: {text}"))?
            .detach())
    }

    /// Resolve an `M`/`N`/`cat-blob`/`ls` dataref: a mark or a full hex id only.
    fn dataref(&self, spec: &[u8]) -> Result<ObjectId> {
        if let Some(v) = spec.strip_prefix(b":") {
            return self.mark_ref(v);
        }
        ObjectId::from_hex(spec).map_err(|_| anyhow!("invalid dataref: {}", command_buf()))
    }

    /// A dataref that is followed by a space: a mark through
    /// `parse_mark_ref_space()`, or a full hex id with the space checked after
    /// it (`file_change_m()`/`note_change_n()`/`parse_ls()`).
    fn dataref_space<'a>(&self, spec: &'a [u8], field: &str) -> Result<(ObjectId, &'a [u8])> {
        if spec.first() == Some(&b':') {
            return self.mark_ref_space(spec);
        }
        let hexsz = self.repo.object_hash().len_in_hex();
        if spec.len() < hexsz {
            crate::git_fatal!("invalid dataref: {}", command_buf());
        }
        let Ok(oid) = ObjectId::from_hex(&spec[..hexsz]) else {
            crate::git_fatal!("invalid dataref: {}", command_buf());
        };
        let Some(rest) = spec[hexsz..].strip_prefix(b" ") else {
            crate::git_fatal!("missing space after {field}: {}", command_buf());
        };
        Ok((oid, rest))
    }

    /// `find_mark(marks, parse_mark_ref_eol(p))`: a mark reference that runs to
    /// the end of the line.
    fn mark_ref(&self, spec: &[u8]) -> Result<ObjectId> {
        self.find_mark(parse_mark_ref_eol(spec)?)
    }

    /// `find_mark(marks, parse_mark_ref_space(&p))`: a mark reference followed
    /// by a space, with the rest of the line handed back.
    fn mark_ref_space<'a>(&self, spec: &'a [u8]) -> Result<(ObjectId, &'a [u8])> {
        let (mark, rest) = parse_mark_ref_space(spec)?;
        Ok((self.find_mark(mark)?, rest))
    }

    /// `find_mark()`: the object a declared mark stands for.
    fn find_mark(&self, mark: u64) -> Result<ObjectId> {
        self.marks
            .get(&mark)
            .copied()
            .ok_or_else(|| anyhow!("mark :{mark} not declared"))
    }

    /// git's `parse_ident` (builtin/fast-import.c:2001-2046): validate and copy an
    /// identity line, keeping git's exact bytes.
    ///
    /// git stores the `<name> SP LT <email> GT SP <when>` text verbatim once the
    /// date validates, so the committer line in the object is the stream's own
    /// bytes; only the date is checked.
    ///
    /// The scan is deliberately the C one rather than "find the last `>`": git
    /// takes the first byte that is either `<` or `>` and insists it is `<`, then
    /// the next such byte and insists it is `>`. A name containing an angle
    /// bracket is therefore rejected with a named diagnostic instead of quietly
    /// producing an ident whose email is whatever came last.
    fn ident(&self, raw: &[u8]) -> Result<Vec<u8>> {
        // ```c
        // /* ensure there is a space delimiter even if there is no name */
        // if (*buf == '<')
        //         --buf;
        // ```
        //
        // `buf` points just past the `committer `/`author `/`tagger ` keyword, so
        // stepping back lands on that keyword's space. From here on `buf` *is*
        // the ident: the stored object reads `committer  <email> …` with two
        // spaces, and every diagnostic below quotes the ident from that space.
        let stepped_back;
        let raw: &[u8] = if raw.first() == Some(&b'<') {
            stepped_back = [b" ", raw].concat();
            &stepped_back
        } else {
            raw
        };
        let lt = raw
            .iter()
            .position(|&b| b == b'<' || b == b'>')
            .filter(|&i| raw[i] == b'<')
            .ok_or_else(|| {
                anyhow!(
                    "missing < in ident string: {}",
                    String::from_utf8_lossy(raw)
                )
            })?;
        if lt != 0 && raw[lt - 1] != b' ' {
            bail!(
                "missing space before < in ident string: {}",
                String::from_utf8_lossy(raw)
            );
        }
        let gt = raw[lt + 1..]
            .iter()
            .position(|&b| b == b'<' || b == b'>')
            .map(|i| lt + 1 + i)
            .filter(|&i| raw[i] == b'>')
            .ok_or_else(|| {
                anyhow!(
                    "missing > in ident string: {}",
                    String::from_utf8_lossy(raw)
                )
            })?;
        let date = raw
            .get(gt + 1..)
            .and_then(|d| d.strip_prefix(b" "))
            .ok_or_else(|| {
                anyhow!(
                    "missing space after > in ident string: {}",
                    String::from_utf8_lossy(raw)
                )
            })?;
        // `name_len = ltgt - buf; strbuf_add(&ident, buf, name_len);`: everything
        // through the space after `>`, which the two converting formats follow
        // with the date they compute.
        let name_part = &raw[..raw.len() - date.len()];
        let strict = match self.opts.date_format {
            DateFormat::Raw => true,
            DateFormat::RawPermissive => false,
            // ```c
            // case WHENSPEC_RFC2822:
            //         if (parse_date(ltgt, &ident) < 0)
            //                 die(_("invalid rfc2822 date \"%s\" in ident: %s"), ltgt, buf);
            // ```
            DateFormat::Rfc2822 => {
                let text = String::from_utf8_lossy(date);
                let Some(stamp) = crate::date::parse_date(&text) else {
                    crate::git_fatal!(
                        "invalid rfc2822 date \"{text}\" in ident: {}",
                        String::from_utf8_lossy(raw)
                    );
                };
                return Ok([name_part, stamp.as_bytes()].concat());
            }
            // ```c
            // case WHENSPEC_NOW:
            //         if (strcmp("now", ltgt))
            //                 die(_("date in ident must be 'now': %s"), buf);
            //         datestamp(&ident);
            // ```
            DateFormat::Now => {
                if date != b"now" {
                    crate::git_fatal!(
                        "date in ident must be 'now': {}",
                        String::from_utf8_lossy(raw)
                    );
                }
                return Ok([name_part, crate::date::datestamp().as_bytes()].concat());
            }
        };
        if !valid_raw_date(date, strict) {
            crate::git_fatal!(
                "invalid raw date \"{}\" in ident: {}",
                String::from_utf8_lossy(date),
                String::from_utf8_lossy(raw)
            );
        }
        Ok(raw.to_vec())
    }

    // -- marks and refs -----------------------------------------------------

    /// `make_fast_import_path()` (builtin/fast-import.c:3695-3701): resolved
    /// when the option or feature is read, under the `--relative-marks` in
    /// effect at that point.
    fn marks_path(&self, path: &str) -> std::path::PathBuf {
        if self.opts.relative_marks {
            self.repo.git_dir().join("info").join("fast-import").join(path)
        } else {
            std::path::PathBuf::from(path)
        }
    }

    /// `option_export_marks()` (builtin/fast-import.c:3758-3763).
    fn option_export_marks(&mut self, marks: &str) {
        self.opts.export_marks = Some(self.marks_path(marks));
        self.opts.export_marks_shown = match self.opts.relative_marks && !std::path::Path::new(marks).is_absolute() {
            true => {
                let path = self.repo.git_dir().join("info").join("fast-import").join(marks);
                crate::setup::git_path_display(&self.repo, &path)
            }
            false => crate::setup::prefix_filename(crate::setup::startup_prefix(&self.repo).as_deref(), marks),
        };
    }

    /// `dump_marks()`: write out the mark table, ascending by mark number.
    ///
    /// It declines while an import file is named but no marks file has been
    /// read yet, so a run that died before loading it does not overwrite the
    /// export with a half-populated table.
    fn export_marks(&self) -> Result<()> {
        if self.opts.import_marks.is_some() && !self.opts.import_marks_done {
            return Ok(());
        }
        let Some(full) = &self.opts.export_marks else {
            return Ok(());
        };
        if let Some(parent) = full.parent() {
            std::fs::create_dir_all(parent).ok();
        }
        let mut marks: Vec<_> = self.marks.iter().collect();
        marks.sort_by_key(|(m, _)| **m);
        let mut out = String::new();
        for (mark, id) in marks {
            out.push_str(&format!(":{mark} {id}\n"));
        }
        std::fs::write(full, out).with_context(|| format!("cannot write {}", full.display()))
    }

    /// Flush the packfile, every pending ref update and the marks file —
    /// `checkpoint`, and the end of the stream.
    ///
    /// ```c
    /// static void checkpoint(void) {
    ///         checkpoint_requested = 0;
    ///         if (object_count) cycle_packfile();
    ///         dump_branches();
    ///         dump_tags();
    ///         dump_marks();
    /// }
    /// ```
    ///
    /// (`builtin/fast-import.c:3598-3606`; the end of `cmd_fast_import` is the
    /// same sequence with a bare `end_packfile()` in place of the cycle, 4025.)
    /// The pack goes first, so `--export-pack-edges` names the tips of the pack
    /// that just closed rather than of the one the next command opens.
    fn checkpoint(&mut self) -> Result<()> {
        self.end_packfile()?;
        for idx in self.branch_table_order() {
            self.update_branch(idx)?;
        }
        // `dump_tags()` writes every tag of the run, at every checkpoint.
        for i in 0..self.tags.len() {
            let (full, id) = (format!("refs/tags/{}", self.tags[i].name), self.tags[i].oid);
            let old = self.current(&full)?;
            self.write_ref(&full, id, old)?;
        }
        self.export_marks()
    }

    /// Update one branch ref, applying git's fast-forward guard unless `--force`.
    fn update_branch(&mut self, idx: usize) -> Result<()> {
        let name = self.branches[idx].name.clone();
        let old = self.current(&name)?;

        let Some(new) = self.branches[idx].head else {
            if self.branches[idx].delete && old.is_some() {
                // A deletion is the one update git validates with
                // `refname_is_safe()` rather than `check_refname_format()`
                // (`refs.c:1199-1205`), so a ref `commit main` was allowed to
                // *create* cannot be deleted again. git reports that through
                // `delete_ref()`, whose return `update_branch()` ignores, so the
                // run neither fails nor stops.
                if !refname_is_safe(&name) {
                    eprintln!("error: refusing to update ref with bad name '{name}'");
                    self.branches[idx].delete = false;
                    return Ok(());
                }
                self.repo.edit_reference(RefEdit {
                    change: Change::Delete {
                        expected: PreviousValue::Any,
                        log: RefLog::AndReference,
                        message: Default::default(),
                    },
                    name: name.as_str().try_into()?,
                    deref: false,
                })?;
                self.branches[idx].delete = false;
            }
            return Ok(());
        };

        if !self.opts.force {
            if let Some(old) = old {
                match self.is_ancestor(old, new) {
                    // `return error(_("branch %s is missing commits."), b->name)`
                    // (builtin/fast-import.c:1674) — one of the two ids does not
                    // name a commit at all, so there is no ancestry to test.
                    None => {
                        eprintln!("error: branch {name} is missing commits.");
                        self.failed = true;
                        return Ok(());
                    }
                    Some(false) => {
                        eprintln!(
                            "warning: not updating {name} (new tip {new} does not contain {old})"
                        );
                        self.failed = true;
                        return Ok(());
                    }
                    Some(true) => {}
                }
            }
        }
        self.write_ref(&name, new, old)
    }

    /// The id `name` currently points at, or `None` when the ref does not exist.
    ///
    /// A name outside `refs/` is read straight out of the git directory. git's
    /// `read_ref()` is a literal lookup, while gix's `try_find_reference` takes a
    /// *partial* name and searches the usual prefixes — which finds neither the
    /// `main` that `commit main` writes nor the `foo/bar` that `commit foo/bar`
    /// does, and would answer for `refs/heads/v1` when the stream said `v1`.
    ///
    /// It also stops at the first object rather than peeling to a commit.
    /// `refs_read_ref()` resolves symrefs (`RESOLVE_REF_READING`) and hands back
    /// whatever id the file holds, and that id is what `update_branch()` passes as
    /// `old_oid` to `ref_transaction_update()` (builtin/fast-import.c:1663,
    /// `:1690`) — so a `reset refs/tags/v0.2.0` over an *annotated* tag has to
    /// expect the tag object, not the commit under it. Peeling here made every
    /// such update fail its own expected-value check with `should have content
    /// <commit>, actual content was <tag>` and abort the import. The peel git does
    /// do is `lookup_commit_reference_gently()` inside the fast-forward guard
    /// alone (`:1669-1672`), which is where [`Self::is_ancestor`] does it.
    fn current(&self, name: &str) -> Result<Option<ObjectId>> {
        if !name.starts_with("refs/") {
            return self.read_onelevel_ref(name);
        }
        Ok(match self.repo.try_find_reference(name)? {
            Some(mut r) => Some(r.follow_to_object()?.detach()),
            None => None,
        })
    }

    /// Read `$GIT_DIR/<name>` — a one-level ref such as the `main` that
    /// `commit main` creates — following `ref: ` indirections the way git's
    /// `refs_resolve_ref_unsafe()` does, and giving up after as many of them as
    /// git's `SYMREF_MAXDEPTH` allows rather than chasing a cycle forever. A file
    /// that is absent or holds something that is not an object id reads as "no
    /// such ref", which is the answer `read_ref()` gives for both.
    fn read_onelevel_ref(&self, name: &str) -> Result<Option<ObjectId>> {
        let mut name = name.to_string();
        for _ in 0..5 {
            if name.starts_with("refs/") {
                return self.current(&name);
            }
            let Ok(text) = std::fs::read_to_string(self.repo.path().join(&name)) else {
                return Ok(None);
            };
            let text = text.trim();
            let Some(target) = text.strip_prefix("ref: ") else {
                return Ok(ObjectId::from_hex(text.as_bytes()).ok());
            };
            name = target.trim().to_string();
        }
        Ok(None)
    }

    /// Write `$GIT_DIR/<name>` for a one-level ref, through a `.lock` sibling and
    /// a rename so a reader never sees a half-written id.
    ///
    /// git writes no reflog here: `should_autocreate_reflog()` (`refs.c:959`)
    /// auto-creates one only for `HEAD` and for `refs/{heads,remotes,notes}/`,
    /// and a one-level name is none of those — which is why the loose write is
    /// all there is to match.
    fn write_onelevel_ref(&self, name: &str, new: ObjectId) -> Result<()> {
        let path = self.repo.path().join(name);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut lock = path.clone().into_os_string();
        lock.push(".lock");
        let lock = std::path::PathBuf::from(lock);
        std::fs::write(&lock, format!("{new}\n")).with_context(|| format!("cannot write {}", lock.display()))?;
        std::fs::rename(&lock, &path).with_context(|| format!("cannot write {}", path.display()))
    }

    /// Whether `old` is reachable from `new`, i.e. the update loses no commits.
    ///
    /// Both sides go through `lookup_commit_reference_gently()` first
    /// (builtin/fast-import.c:1669-1672), so an annotated tag on either end is
    /// compared as the commit it names; `None` is git's `!old_cmit || !new_cmit`,
    /// which is the `branch %s is missing commits.` error rather than a refusal.
    fn is_ancestor(&self, old: ObjectId, new: ObjectId) -> Option<bool> {
        let peel = |id: ObjectId| -> Option<ObjectId> {
            Some(self.repo.find_object(id).ok()?.peel_to_commit().ok()?.id)
        };
        let (old, new) = (peel(old)?, peel(new)?);
        Some(matches!(self.repo.merge_base(old, new), Ok(base) if base.detach() == old))
    }

    /// Point `name` at `new`, with git's `fast-import` reflog message.
    ///
    /// A ref that already holds `new` still goes through the transaction, because
    /// git's does. `update_branch()` runs `ref_transaction_update()` unconditionally
    /// (builtin/fast-import.c:1687-1697) and the backend decides what that means: a
    /// no-op update writes no ref file and no reflog line of its own
    /// (`oideq(&lock->old_oid, &update->new_oid)` leaves `REF_NEEDS_COMMIT` unset,
    /// refs/files-backend.c:2807-2812), while the `REF_LOG_ONLY` mirror
    /// `split_head_update()` added for `HEAD` is written regardless. So re-importing
    /// a stream a repository already holds appends exactly one line to
    /// `.git/logs/HEAD` — `<oid> <oid> <ident>\tfast-import` — and nothing to the
    /// branch's own log. Measured on stock 2.55.0 with `reset refs/heads/main` to
    /// the value main already had.
    ///
    /// An earlier short-circuit here returned before the transaction and so wrote
    /// neither, which is why every "import a stream back into its own repository"
    /// case diverged by that one line.
    fn write_ref(&self, name: &str, new: ObjectId, old: Option<ObjectId>) -> Result<()> {
        if old == Some(new) && FullName::try_from(name).is_err() {
            // A one-level ref has no reflog to write (see `write_onelevel_ref`),
            // so a no-op update on one really is nothing at all.
            return Ok(());
        }
        if FullName::try_from(name).is_err() {
            return self.write_onelevel_ref(name, new);
        }
        self.repo.edit_reference(RefEdit {
            change: Change::Update {
                log: LogChange {
                    mode: RefLog::AndReference,
                    force_create_reflog: false,
                    message: "fast-import".into(),
                },
                expected: match old {
                    Some(id) => PreviousValue::MustExistAndMatch(Target::Object(id)),
                    None => PreviousValue::MustNotExist,
                },
                new: Target::Object(new),
            },
            name: name.try_into()?,
            // `update_branch()` calls `ref_transaction_update()` without
            // `REF_NO_DEREF`, so a symbolic ref is followed and its target is
            // what moves: `commit HEAD` advances the branch HEAD points at and
            // leaves HEAD symbolic.
            deref: true,
        })?;
        Ok(())
    }
}

/// git's `refname_disposition` table (`refs.c:49-58`), which classifies a byte
/// for the two functions below: 0 acceptable, 1 end-of-component, 2 `.`, 3 `{`,
/// 4 forbidden, 5 `*`.
fn refname_disposition(byte: u8) -> u8 {
    match byte {
        0 | b'/' => 1,
        b'.' => 2,
        b'{' => 3,
        b'*' => 5,
        0x01..=0x20 | b':' | b'?' | b'[' | b'\\' | b'^' | b'~' | 0x7f => 4,
        _ => 0,
    }
}

/// git's `check_refname_component()` (`refs.c:161`): the length of the component
/// at the front of `s`, or `None` when the component itself is malformed.
///
/// A zero length is a legal answer — `check_or_sanitize_refname` is the one that
/// rejects an empty component — and mirrors the `cp == refname` early return.
fn refname_component_len(s: &[u8]) -> Option<usize> {
    let mut last = 0u8;
    let mut cp = 0;
    while cp < s.len() {
        match refname_disposition(s[cp]) {
            1 => break,
            2 if last == b'.' => return None,  // ".."
            3 if last == b'@' => return None,  // "@{"
            4 | 5 => return None,              // forbidden byte, or "*" without a pattern flag
            _ => {}
        }
        last = s[cp];
        cp += 1;
    }
    if cp == 0 {
        return Some(0);
    }
    if s[0] == b'.' || s[..cp].ends_with(b".lock") {
        return None;
    }
    Some(cp)
}

/// git's `check_refname_format(name, REFNAME_ALLOW_ONELEVEL)`
/// (`refs.c:289` into `check_or_sanitize_refname` at 245), which is exactly how
/// `fast-import.c:617` validates a branch name in `new_branch()`.
///
/// The `ALLOW_ONELEVEL` flag is the whole point: a stream may say `commit main`,
/// and git accepts it, writing `$GIT_DIR/main`. gix's `FullName` implements the
/// flagless form instead — `gix-validate`'s `reference::name` requires a slash
/// unless the name is all-uppercase — so it rejects `main`, `v1.2` and every
/// other lowercase one-level name that git takes.
fn refname_ok_onelevel(name: &str) -> bool {
    if name == "@" {
        return false;
    }
    let mut rest = name.as_bytes();
    loop {
        let Some(len) = refname_component_len(rest) else { return false };
        if len == 0 {
            return false;
        }
        if len == rest.len() {
            // The last component, and the only one whose trailing '.' is checked.
            return rest[len - 1] != b'.';
        }
        rest = &rest[len + 1..];
    }
}

/// git's `refname_is_safe()` (`refs.c:300`), which guards a *deletion* rather
/// than an update: `ref_transaction_update()` picks it over
/// `check_refname_format()` when the new id is null (`refs.c:1199-1205`).
///
/// Outside `refs/` a name must be all-uppercase or `_`, so the one-level refs
/// this file can create are deletable only when they are shouted.
fn refname_is_safe(name: &str) -> bool {
    if let Some(rest) = name.strip_prefix("refs/") {
        if rest.is_empty() || rest.starts_with('/') || rest.ends_with('/') {
            return false;
        }
        // `normalize_path_copy()` leaves a path alone unless it has a "." or
        // ".." component to resolve, so "unchanged" means "does not escape".
        return !rest.split('/').any(|c| c == "." || c == ".." || c.is_empty());
    }
    !name.is_empty() && name.bytes().all(|b| b.is_ascii_uppercase() || b == b'_')
}

/// Where a cryptographic signature starts inside a tag message, or `None` when
/// the message carries none.
///
/// git's `parse_signature` knows four armor headers, one per signature format it
/// supports, and each has to begin a line. When a message contains more than one
/// the last wins, so a body that merely quotes an armor header keeps everything
/// up to the real signature.
fn signature_offset(message: &[u8]) -> Option<usize> {
    const MARKERS: [&[u8]; 4] = [
        b"-----BEGIN PGP SIGNATURE-----",
        b"-----BEGIN PGP MESSAGE-----",
        b"-----BEGIN SIGNED MESSAGE-----",
        b"-----BEGIN SSH SIGNATURE-----",
    ];
    let starts_line = |at: usize| at == 0 || message[at - 1] == b'\n';
    (0..message.len())
        .rev()
        .find(|&at| {
            starts_line(at) && MARKERS.iter().any(|m| message[at..].starts_with(m))
        })
}

/// The bare `strerror` text of an I/O error. Rust appends ` (os error <n>)` to
/// its `Display`; git's `die_errno` does not, so it is trimmed off here.
fn strerror(e: &std::io::Error) -> String {
    let text = e.to_string();
    match text.find(" (os error ") {
        Some(i) => text[..i].to_string(),
        None => text,
    }
}

/// Read a `:<idnum> SP <oid>` marks file, as git's `read_mark_file` does.
///
/// `display` is the name to put in the error text — git reports the path it was
/// handed, which is the resolved one for `--import-marks` and the raw one for
/// `--rewrite-submodules-from/-to`. With `if_exists`, a missing file is not an
/// error and yields no marks; a corrupt line always is.
fn read_mark_file(
    path: &std::path::Path,
    display: &str,
    if_exists: bool,
) -> Result<Vec<(u64, ObjectId)>> {
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) if if_exists && e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => crate::git_fatal!("cannot read '{display}': {}", strerror(&e)),
    };
    let mut out = Vec::new();
    for line in text.lines() {
        if line.is_empty() {
            continue;
        }
        let corrupt = || anyhow!("corrupt mark line: {line}");
        let rest = line.strip_prefix(':').ok_or_else(&corrupt)?;
        let (mark, hex) = rest.split_once(' ').ok_or_else(&corrupt)?;
        let mark: u64 = mark.parse().map_err(|_| corrupt())?;
        let id = ObjectId::from_hex(hex.as_bytes()).map_err(|_| corrupt())?;
        out.push((mark, id));
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// git's in-memory trees
// ---------------------------------------------------------------------------
//
// A port of the tree half of builtin/fast-import.c: `struct tree_entry` with its
// two versions, `struct tree_content`, and the functions that load, edit and
// store them (657-1615). Version 0 is the tree as it was last stored or
// loaded, version 1 the tree as the stream has made it; a null version-1 id is
// "dirty", a version-1 mode of 0 is "deleted, still needed for the old tree".
// Subtrees are read lazily, the first time a path walks into them. The shape
// matters beyond the ids it produces: which trees are stored, which of them git
// tried to deltify against their old version, which names it interned, and how
// many entries and contents it allocated are all printed by the statistics
// block, and the loaded/dirty state of each branch by the crash report.

/// `S_IFMT`, `S_IFDIR` and `S_IFREG` as git's `cache.h` spells them.
const S_IFMT: u32 = 0o170000;
const S_IFDIR: u32 = 0o040000;
const S_IFREG: u32 = 0o100000;
/// `#define NO_DELTA S_ISUID` (builtin/fast-import.c:40-43): set on a
/// version-0 directory mode whose old tree can no longer be a delta base.
const NO_DELTA: u32 = 0o4000;
/// `MAX_PACK_ID` (builtin/fast-import.c:35-36): "in no pack this run wrote".
const MAX_PACK_ID: u32 = (1 << 16) - 1;
/// What `write_pack_header()` writes ahead of the first entry: `PACK`, the
/// version and the object count `fixup_pack_header_footer()` fills in.
const PACK_HEADER_LEN: u64 = 12;

/// The entry type `encode_in_pack_object_header()` is given for a whole object.
fn pack_entry_header(kind: Kind) -> gix::odb::pack::data::entry::Header {
    use gix::odb::pack::data::entry::Header;
    match kind {
        Kind::Blob => Header::Blob,
        Kind::Tree => Header::Tree,
        Kind::Commit => Header::Commit,
        Kind::Tag => Header::Tag,
    }
}

fn s_isdir(mode: u32) -> bool {
    mode & S_IFMT == S_IFDIR
}

/// `struct atom_str`: an interned path component.
type Atom = std::rc::Rc<[u8]>;

/// One `struct object_entry` of git's object table, reduced to what is read
/// back out of it.
#[derive(Clone, Copy)]
struct ObjectEntry {
    kind: Kind,
    /// `pack_id`: the pack it was stored into, or [`MAX_PACK_ID`].
    pack_id: u32,
    /// `depth`: its delta chain length in that pack.
    depth: u32,
    /// `idx.offset`: where its entry starts in that pack, or `1` ("just not
    /// zero!") for an object the run did not write.
    offset: u64,
}

/// `struct last_object`: the object the next one of its kind is deltified
/// against.
#[derive(Default)]
struct LastObject {
    data: Vec<u8>,
    /// `offset`: the entry an `OBJ_OFS_DELTA` against `data` counts back to.
    offset: u64,
    depth: u32,
}

/// Which `struct last_object` a [`PackLog::store`] call passes.
enum Last<'a> {
    None,
    /// `&last_blob`.
    Blob,
    /// A tree's old version, built by `store_tree()`.
    Tree(&'a mut LastObject),
}

/// The index of an object type in git's `*_count_by_type[]` arrays, in the
/// order the statistics block prints them.
fn type_slot(kind: Kind) -> usize {
    match kind {
        Kind::Blob => 0,
        Kind::Tree => 1,
        Kind::Commit => 2,
        Kind::Tag => 3,
    }
}

/// `struct tree_entry_ms`.
#[derive(Clone, Copy)]
struct Version {
    mode: u32,
    oid: ObjectId,
}

/// `struct tree_entry`.
struct TreeEntry {
    /// `tree`: the loaded content, or `None` until something walks into it.
    tree: Option<Box<TreeContent>>,
    name: Atom,
    versions: [Version; 2],
}

impl TreeEntry {
    /// A `struct tree_entry` git keeps outside the entry pool: a branch's
    /// `branch_tree`, or the `leaf` a caller zeroes on its stack.
    fn detached(hash: gix::hash::Kind, mode: u32) -> TreeEntry {
        let null = ObjectId::null(hash);
        TreeEntry {
            tree: None,
            name: Atom::from(&[][..]),
            versions: [Version { mode, oid: null }, Version { mode, oid: null }],
        }
    }
}

/// `struct tree_content`.
struct TreeContent {
    entry_capacity: u32,
    delta_depth: u32,
    /// `entries[0 .. entry_count]`.
    entries: Vec<TreeEntry>,
}

/// `GIT_MAX_ALIGNMENT` on the LP64 platforms git and this port build for.
const MAX_ALIGNMENT: u64 = 8;
/// `sizeof(struct mp_block)`: three pointers ahead of the flexible array.
const MP_BLOCK: u64 = 24;
/// `fi_mem_pool.block_alloc` (builtin/fast-import.c:194-196).
const POOL_BLOCK_ALLOC: u64 = 2 * 1024 * 1024 - MP_BLOCK;
/// `tree_entry_alloc` (builtin/fast-import.c:230).
const TREE_ENTRY_ALLOC: u64 = 1000;
/// `avail_tree_table_sz` (builtin/fast-import.c:232).
const AVAIL_TREE_TABLE_SZ: u32 = 100;
/// `sizeof` of the structures fast-import allocates, on LP64: a 36-byte
/// `struct object_id` (`GIT_MAX_RAWSZ` + the `algo` int) aligned to 4, and
/// pointers and `uintmax_t` aligned to 8.
const SIZEOF_TREE_ENTRY: u64 = 96;
const SIZEOF_TREE_CONTENT: u64 = 16;
const SIZEOF_ATOM_STR: u64 = 16;
const SIZEOF_BRANCH: u64 = 176;
const SIZEOF_TAG: u64 = 56;
const SIZEOF_MARK_SET: u64 = 8200;
const SIZEOF_RECENT_COMMAND: u64 = 24;
const SIZEOF_OBJECT_ENTRY: u64 = 72;
/// `object_entry_alloc` (builtin/fast-import.c:213).
const OBJECT_ENTRY_ALLOC: u64 = 5000;
/// `cmd_save` (builtin/fast-import.c:258).
const CMD_SAVE: usize = 100;

/// fast-import's allocators, as the statistics block measures them:
/// `fi_mem_pool` (mem-pool.c:83-104), the tree-entry free list
/// (builtin/fast-import.c:728-752), the `avail_tree_table` that recycles tree
/// contents by capacity (677-726), and the atom table (617-632).
struct Pools {
    /// `fi_mem_pool.pool_alloc`.
    pool_alloc: u64,
    /// `end - next_free` of the pool's head block; `None` before the first
    /// block.
    head_free: Option<u64>,
    /// `tree_entry_allocd`.
    tree_entry_allocd: u64,
    /// Entries handed out by `new_tree_entry()` and not yet released.
    entries_live: u64,
    /// Entries allocated in all, live or on the free list.
    entries_capacity: u64,
    /// `avail_tree_table[]`: per bucket, the released capacities, the list's
    /// head last.
    avail_tree_table: Vec<Vec<u32>>,
    /// `atom_table`; its length is `atom_cnt`.
    atoms: HashMap<Vec<u8>, Atom>,
    /// `fspathncmp()` is `strncasecmp()` under `core.ignorecase`.
    ignore_case: bool,
}

impl Pools {
    fn new() -> Pools {
        Pools {
            pool_alloc: 0,
            head_free: None,
            tree_entry_allocd: 0,
            entries_live: 0,
            entries_capacity: 0,
            avail_tree_table: vec![Vec::new(); AVAIL_TREE_TABLE_SZ as usize],
            atoms: HashMap::new(),
            ignore_case: false,
        }
    }

    /// `mem_pool_alloc(&fi_mem_pool, len)` (mem-pool.c:83-104): carve `len`,
    /// rounded up to the alignment, out of the head block, or start a new block
    /// — a block of its own, linked behind the head, for a request of half a
    /// block or more.
    fn alloc(&mut self, len: u64) {
        let len = len.div_ceil(MAX_ALIGNMENT) * MAX_ALIGNMENT;
        if let Some(free) = self.head_free.filter(|free| *free >= len) {
            self.head_free = Some(free - len);
            return;
        }
        if len >= POOL_BLOCK_ALLOC / 2 {
            self.pool_alloc += MP_BLOCK + len;
            self.head_free.get_or_insert(0);
        } else {
            self.pool_alloc += MP_BLOCK + POOL_BLOCK_ALLOC;
            self.head_free = Some(POOL_BLOCK_ALLOC - len);
        }
    }

    /// `mem_pool_strdup()`: the string and its NUL.
    fn strdup(&mut self, s: &str) {
        self.alloc(s.len() as u64 + 1);
    }

    /// `new_tree_entry()` (builtin/fast-import.c:728-746): the free list grows
    /// by `tree_entry_alloc` entries only when it is empty.
    fn new_tree_entry(&mut self) {
        if self.entries_live == self.entries_capacity {
            self.entries_capacity += TREE_ENTRY_ALLOC;
            self.tree_entry_allocd += TREE_ENTRY_ALLOC * SIZEOF_TREE_ENTRY;
        }
        self.entries_live += 1;
    }

    /// `new_tree_content()` (builtin/fast-import.c:677-704): the first released
    /// content of at least `cnt` entries in its bucket, or a pool allocation
    /// rounded up to a multiple of 8 entries.
    fn new_tree_content(&mut self, cnt: u32) -> Box<TreeContent> {
        let list = &mut self.avail_tree_table[hc_entries(cnt) as usize];
        let entry_capacity = match list.iter().rposition(|capacity| *capacity >= cnt) {
            Some(at) => list.remove(at),
            None => {
                let cnt = if cnt & 7 != 0 { (cnt / 8 + 1) * 8 } else { cnt };
                self.alloc(SIZEOF_TREE_CONTENT + 8 * u64::from(cnt));
                cnt
            }
        };
        Box::new(TreeContent {
            entry_capacity,
            delta_depth: 0,
            entries: Vec::new(),
        })
    }

    /// `release_tree_content()` (builtin/fast-import.c:706-712): the content
    /// goes back on the list for its capacity; its entries are the caller's.
    fn release_tree_content(&mut self, entry_capacity: u32) {
        self.avail_tree_table[hc_entries(entry_capacity) as usize].push(entry_capacity);
    }

    /// `to_atom()` (builtin/fast-import.c:617-632).
    fn to_atom(&mut self, s: &[u8]) -> Atom {
        if let Some(atom) = self.atoms.get(s) {
            return atom.clone();
        }
        self.alloc(SIZEOF_ATOM_STR + s.len() as u64 + 1);
        let atom = Atom::from(s);
        self.atoms.insert(s.to_vec(), atom.clone());
        atom
    }

    /// `fspathncmp(p, e->name->str_dat, n) == 0`.
    fn name_eq(&self, a: &[u8], b: &[u8]) -> bool {
        match self.ignore_case {
            true => a.eq_ignore_ascii_case(b),
            false => a == b,
        }
    }
}

/// `hc_entries()` (builtin/fast-import.c:671-675).
fn hc_entries(cnt: u32) -> u32 {
    let cnt = if cnt & 7 != 0 { cnt / 8 + 1 } else { cnt / 8 };
    cnt.min(AVAIL_TREE_TABLE_SZ - 1)
}

/// `branch_table_sz` (builtin/fast-import.c:240).
const BRANCH_TABLE_SZ: u32 = 1039;

/// `hc_str()` (builtin/fast-import.c:565-570): `r = r * 31 + *s++` over the
/// platform's `char`, which is signed on x86-64 and Apple arm64 and unsigned
/// on Linux arm64 — so a name with a byte above 0x7f lands in a different
/// bucket there, in git as here.
fn hc_str(s: &[u8]) -> u32 {
    s.iter()
        .fold(0u32, |r, &c| r.wrapping_mul(31).wrapping_add(c as std::ffi::c_char as i32 as u32))
}

/// What the tree functions need of the importer: the repository to read
/// objects from, the pack bookkeeping `store_object()` updates, and the
/// allocators.
struct Ctx<'a> {
    repo: &'a gix::Repository,
    pack: &'a mut PackLog,
    pools: &'a mut Pools,
}

/// `release_tree_content_recursive()` (builtin/fast-import.c:714-720).
fn release_tree_content_recursive(pools: &mut Pools, t: Box<TreeContent>) {
    let TreeContent { entry_capacity, entries, .. } = *t;
    for e in entries {
        release_tree_entry(pools, e);
    }
    pools.release_tree_content(entry_capacity);
}

/// `release_tree_entry()` (builtin/fast-import.c:748-754).
fn release_tree_entry(pools: &mut Pools, e: TreeEntry) {
    if let Some(t) = e.tree {
        release_tree_content_recursive(pools, t);
    }
    pools.entries_live -= 1;
}

/// `grow_tree_content()` (builtin/fast-import.c:722-732).
fn grow_tree_content(pools: &mut Pools, t: Box<TreeContent>, amt: u32) -> Box<TreeContent> {
    let mut r = pools.new_tree_content(t.entries.len() as u32 + amt);
    r.delta_depth = t.delta_depth;
    let TreeContent { entry_capacity, entries, .. } = *t;
    r.entries = entries;
    pools.release_tree_content(entry_capacity);
    r
}

/// `if (t->entry_count == t->entry_capacity) root->tree = t = grow_tree_content(t, t->entry_count);`,
/// which every caller runs before it appends an entry.
fn grow_if_full(pools: &mut Pools, slot: &mut Option<Box<TreeContent>>) {
    let t = slot.take().expect("a loaded tree");
    *slot = Some(match t.entries.len() as u32 == t.entry_capacity {
        true => {
            let count = t.entries.len() as u32;
            grow_tree_content(pools, t, count)
        }
        false => t,
    });
}

/// `dup_tree_content()` (builtin/fast-import.c:756-779): a loaded subtree is
/// copied only while it is dirty; a clean one is read again when needed.
fn dup_tree_content(pools: &mut Pools, s: &TreeContent) -> Box<TreeContent> {
    let mut d = pools.new_tree_content(s.entries.len() as u32);
    for a in &s.entries {
        pools.new_tree_entry();
        let tree = match &a.tree {
            Some(t) if a.versions[1].oid.is_null() => Some(dup_tree_content(pools, t)),
            _ => None,
        };
        d.entries.push(TreeEntry {
            tree,
            name: a.name.clone(),
            versions: a.versions,
        });
    }
    d.delta_depth = s.delta_depth;
    d
}

/// `parse_mode()` (object.h:200-215): octal digits up to a space, into a
/// `uint16_t`.
fn parse_mode(s: &[u8]) -> Option<(u32, usize)> {
    if s.first() == Some(&b' ') {
        return None;
    }
    let mut mode: u32 = 0;
    for (i, &c) in s.iter().enumerate() {
        if c == b' ' {
            return Some((mode & 0xffff, i + 1));
        }
        if !(b'0'..=b'7').contains(&c) {
            return None;
        }
        mode = (mode << 3).wrapping_add(u32::from(c - b'0'));
    }
    None
}

/// `load_tree()` (builtin/fast-import.c:1293-1351): read `root`'s version-1
/// tree into a fresh content, one entry per tree entry. A tree this run stored
/// into a pack carries that object's delta depth.
fn load_tree(g: &mut Ctx<'_>, root: &mut TreeEntry) -> Result<()> {
    let oid = root.versions[1].oid;
    // `root->tree = t = new_tree_content(8);` — a content already there is
    // overwritten, not released, exactly as the C overwrites the pointer.
    root.tree = Some(g.pools.new_tree_content(8));
    if oid.is_null() {
        return Ok(());
    }

    let buf = match g.pack.table.get(&oid).copied() {
        Some(myoe) if myoe.pack_id != MAX_PACK_ID => {
            if myoe.kind != Kind::Tree {
                crate::git_fatal!("not a tree: {oid}");
            }
            root.tree.as_mut().expect("just set").delta_depth = myoe.depth;
            match g.repo.try_find_object(oid) {
                Ok(Some(object)) => object.data.clone(),
                _ => crate::git_fatal!("can't load tree {oid}"),
            }
        }
        _ => match g.repo.try_find_object(oid) {
            Ok(Some(object)) if object.kind == Kind::Tree => object.data.clone(),
            _ => crate::git_fatal!("can't load tree {oid}"),
        },
    };

    let rawsz = g.repo.object_hash().len_in_bytes();
    let mut c = 0;
    while c != buf.len() {
        // `e = new_tree_entry();`, the grow, and only then the parse.
        g.pools.new_tree_entry();
        grow_if_full(g.pools, &mut root.tree);
        let Some((mode, used)) = parse_mode(&buf[c..]) else {
            crate::git_fatal!("corrupt mode in {oid}");
        };
        c += used;
        let name_len = buf[c..].iter().position(|&b| b == 0).unwrap_or(buf.len() - c);
        let name = g.pools.to_atom(&buf[c..c + name_len]);
        c += name_len + 1;
        let Some(raw) = buf.get(c..c + rawsz) else {
            crate::git_fatal!("can't load tree {oid}");
        };
        let entry_oid = ObjectId::from_bytes_or_panic(raw);
        c += rawsz;
        let version = Version { mode, oid: entry_oid };
        root.tree.as_mut().expect("loaded above").entries.push(TreeEntry {
            tree: None,
            name,
            versions: [version, version],
        });
    }
    Ok(())
}

/// `mktree()` (builtin/fast-import.c:1371-1401): sort the entries by version
/// `v` — the order is kept, as the C sorts in place — and write that version.
fn mktree(t: &mut TreeContent, v: usize, rawsz: usize) -> Vec<u8> {
    t.entries.sort_by(|a, b| {
        base_name_compare(&a.name, a.versions[v].mode, &b.name, b.versions[v].mode)
    });
    let mut buf = Vec::new();
    for e in &t.entries {
        if e.versions[v].mode == 0 {
            continue;
        }
        buf.extend_from_slice(format!("{:o} ", e.versions[v].mode & !NO_DELTA).as_bytes());
        buf.extend_from_slice(&e.name);
        buf.push(0);
        buf.extend_from_slice(&e.versions[v].oid.as_slice()[..rawsz]);
    }
    buf
}

/// `store_tree()` (builtin/fast-import.c:1403-1446): store every dirty tree
/// below `root` and then `root` itself, deltified against its old version
/// when that version sits in the pack now being built, and let go of the
/// entries deleted since the last store.
fn store_tree(g: &mut Ctx<'_>, root: &mut TreeEntry) -> Result<()> {
    if !root.versions[1].oid.is_null() {
        return Ok(());
    }
    if root.tree.is_none() {
        load_tree(g, root)?;
    }
    let rawsz = g.repo.object_hash().len_in_bytes();
    let t = root.tree.as_mut().expect("loaded above");
    for e in t.entries.iter_mut() {
        if e.tree.is_some() {
            store_tree(g, e)?;
        }
    }
    // Read after the subtrees, any of which may have cycled the pack.
    let pack_id = g.pack.pack_id;

    // Looked up after the subtrees are stored, as the C does: one of them may
    // be the object the old version names.
    let le = match root.versions[0].mode & NO_DELTA {
        0 => g.pack.table.get(&root.versions[0].oid).copied(),
        _ => None,
    };
    let mut lo = LastObject::default();
    if s_isdir(root.versions[0].mode) && le.is_some_and(|le| le.pack_id == pack_id) {
        lo.data = mktree(t, 0, rawsz);
        lo.offset = le.map_or(0, |le| le.offset);
        lo.depth = t.delta_depth;
    }
    let new_tree = mktree(t, 1, rawsz);
    let (oid, _) = g.pack.store(g.repo, Kind::Tree, &new_tree, Last::Tree(&mut lo))?;
    root.versions[1].oid = oid;

    let t = root.tree.as_mut().expect("loaded above");
    t.delta_depth = lo.depth;
    for mut e in std::mem::take(&mut t.entries) {
        if e.versions[1].mode != 0 {
            e.versions[0] = e.versions[1];
            t.entries.push(e);
        } else {
            release_tree_entry(g.pools, e);
        }
    }
    Ok(())
}

/// `tree_content_replace()` (builtin/fast-import.c:1448-1461).
fn tree_content_replace(
    pools: &mut Pools,
    root: &mut TreeEntry,
    oid: ObjectId,
    mode: u32,
    newtree: Option<Box<TreeContent>>,
) -> Result<()> {
    if !s_isdir(mode) {
        crate::git_fatal!("root cannot be a non-directory");
    }
    root.versions[0].oid = ObjectId::null(oid.kind());
    root.versions[1].oid = oid;
    if let Some(t) = root.tree.take() {
        release_tree_content_recursive(pools, t);
    }
    root.tree = newtree;
    Ok(())
}

/// The first component of `p` and whether more follow: `strchrnul(p, '/')`.
fn first_component(p: &[u8]) -> (usize, bool) {
    match p.iter().position(|&b| b == b'/') {
        Some(n) => (n, true),
        None => (p.len(), false),
    }
}

/// The index of the entry of `root`'s loaded content named `name`.
fn find_entry(pools: &Pools, t: &TreeContent, name: &[u8]) -> Option<usize> {
    t.entries.iter().position(|e| e.name.len() == name.len() && pools.name_eq(name, &e.name))
}

/// `tree_content_set()` (builtin/fast-import.c:1463-1543). Returns whether
/// anything changed, which is what marks the parents dirty.
fn tree_content_set(
    g: &mut Ctx<'_>,
    root: &mut TreeEntry,
    p: &[u8],
    oid: ObjectId,
    mode: u32,
    subtree: Option<Box<TreeContent>>,
) -> Result<bool> {
    let (n, more) = first_component(p);
    if n == 0 {
        crate::git_fatal!("empty path component found in input");
    }
    if !more && !s_isdir(mode) && subtree.is_some() {
        crate::git_fatal!("non-directories cannot have subtrees");
    }
    if root.tree.is_none() {
        load_tree(g, root)?;
    }
    let null = ObjectId::null(oid.kind());
    let t = root.tree.as_mut().expect("loaded above");
    if let Some(i) = find_entry(g.pools, t, &p[..n]) {
        let e = &mut t.entries[i];
        if !more {
            if !s_isdir(mode) && e.versions[1].mode == mode && e.versions[1].oid == oid {
                return Ok(false);
            }
            e.versions[1] = Version { mode, oid };
            if let Some(old) = e.tree.take() {
                release_tree_content_recursive(g.pools, old);
            }
            e.tree = subtree;
            // The old subtree is no delta base any more: its entries are gone.
            if s_isdir(e.versions[0].mode) {
                e.versions[0].mode |= NO_DELTA;
            }
            root.versions[1].oid = null;
            return Ok(true);
        }
        if !s_isdir(e.versions[1].mode) {
            // A content the C overwrites rather than releases.
            e.tree = Some(g.pools.new_tree_content(8));
            e.versions[1].mode = S_IFDIR;
        }
        if e.tree.is_none() {
            load_tree(g, e)?;
        }
        if tree_content_set(g, e, &p[n + 1..], oid, mode, subtree)? {
            root.versions[1].oid = null;
            return Ok(true);
        }
        return Ok(false);
    }

    grow_if_full(g.pools, &mut root.tree);
    g.pools.new_tree_entry();
    let name = g.pools.to_atom(&p[..n]);
    let mut e = TreeEntry {
        tree: None,
        name,
        versions: [Version { mode: 0, oid: null }, Version { mode: 0, oid: null }],
    };
    if more {
        e.tree = Some(g.pools.new_tree_content(8));
        e.versions[1].mode = S_IFDIR;
        tree_content_set(g, &mut e, &p[n + 1..], oid, mode, subtree)?;
    } else {
        e.tree = subtree;
        e.versions[1] = Version { mode, oid };
    }
    root.tree.as_mut().expect("loaded above").entries.push(e);
    root.versions[1].oid = null;
    Ok(true)
}

/// The `del_entry:` tail of `tree_content_remove()`: hand the entry to
/// `backup_leaf` or release its subtree, and mark it deleted.
fn del_entry(pools: &mut Pools, e: &mut TreeEntry, backup_leaf: Option<&mut TreeEntry>) {
    match backup_leaf {
        Some(leaf) => {
            leaf.tree = e.tree.take();
            leaf.name = e.name.clone();
            leaf.versions = e.versions;
        }
        None => {
            if let Some(t) = e.tree.take() {
                release_tree_content_recursive(pools, t);
            }
        }
    }
    e.versions[1] = Version {
        mode: 0,
        oid: ObjectId::null(e.versions[1].oid.kind()),
    };
}

/// `tree_content_remove()` (builtin/fast-import.c:1545-1601). A directory the
/// removal leaves with no live entry is removed with it.
fn tree_content_remove(
    g: &mut Ctx<'_>,
    root: &mut TreeEntry,
    p: &[u8],
    backup_leaf: Option<&mut TreeEntry>,
    allow_root: bool,
) -> Result<bool> {
    let (n, more) = first_component(p);
    if root.tree.is_none() {
        load_tree(g, root)?;
    }
    let null = ObjectId::null(root.versions[1].oid.kind());
    if p.is_empty() && allow_root {
        del_entry(g.pools, root, backup_leaf);
        root.versions[1].oid = null;
        return Ok(true);
    }

    let t = root.tree.as_mut().expect("loaded above");
    let Some(i) = find_entry(g.pools, t, &p[..n]) else {
        return Ok(false);
    };
    let e = &mut t.entries[i];
    if more && !s_isdir(e.versions[1].mode) {
        // A file stands where the path's parent directory would be, so the
        // path cannot exist and needs no deleting.
        return Ok(true);
    }
    if !more {
        del_entry(g.pools, e, backup_leaf);
        root.versions[1].oid = null;
        return Ok(true);
    }
    if e.tree.is_none() {
        load_tree(g, e)?;
    }
    if !tree_content_remove(g, e, &p[n + 1..], backup_leaf, false)? {
        return Ok(false);
    }
    let live = e
        .tree
        .as_ref()
        .is_some_and(|t| t.entries.iter().any(|child| child.versions[1].mode != 0));
    if !live {
        del_entry(g.pools, e, None);
    }
    root.versions[1].oid = null;
    Ok(true)
}

/// `tree_content_get()` (builtin/fast-import.c:1603-1646): copy the entry at
/// `p` into `leaf`, with a private copy of its subtree while that is dirty.
fn tree_content_get(
    g: &mut Ctx<'_>,
    root: &mut TreeEntry,
    p: &[u8],
    leaf: &mut TreeEntry,
    allow_root: bool,
) -> Result<bool> {
    let (n, more) = first_component(p);
    if n == 0 && !allow_root {
        crate::git_fatal!("empty path component found in input");
    }
    if root.tree.is_none() {
        load_tree(g, root)?;
    }
    let found = |pools: &mut Pools, e: &TreeEntry, leaf: &mut TreeEntry| {
        leaf.name = e.name.clone();
        leaf.versions = e.versions;
        leaf.tree = match &e.tree {
            Some(t) if e.versions[1].oid.is_null() => Some(dup_tree_content(pools, t)),
            _ => None,
        };
    };
    if n == 0 {
        found(g.pools, root, leaf);
        return Ok(true);
    }
    let t = root.tree.as_mut().expect("loaded above");
    let Some(i) = find_entry(g.pools, t, &p[..n]) else {
        return Ok(false);
    };
    let e = &mut t.entries[i];
    if !more {
        found(g.pools, e, leaf);
        return Ok(true);
    }
    if !s_isdir(e.versions[1].mode) {
        return Ok(false);
    }
    if e.tree.is_none() {
        load_tree(g, e)?;
    }
    tree_content_get(g, e, &p[n + 1..], leaf, false)
}

/// `convert_num_notes_to_fanout()` (builtin/fast-import.c:2144-2150).
fn convert_num_notes_to_fanout(mut num_notes: u64) -> u8 {
    let mut fanout = 0;
    loop {
        num_notes >>= 8;
        if num_notes == 0 {
            return fanout;
        }
        fanout += 1;
    }
}

/// `construct_path_with_fanout()` (builtin/fast-import.c:2152-2167).
fn construct_path_with_fanout(hex: &[u8], fanout: u8, rawsz: usize) -> Result<Vec<u8>> {
    if usize::from(fanout) >= rawsz {
        crate::git_fatal!("too large fanout ({fanout})");
    }
    let mut path = Vec::with_capacity(hex.len() + usize::from(fanout));
    let mut j = 0;
    for _ in 0..fanout {
        path.extend_from_slice(&hex[j..j + 2]);
        path.push(b'/');
        j += 2;
    }
    path.extend_from_slice(&hex[j..]);
    Ok(path)
}

/// The entry `path` names below `root`, walking loaded contents only — the
/// pointer `do_change_note_fanout()` holds for the directory it is in.
fn entry_at<'a>(pools: &Pools, root: &'a mut TreeEntry, path: &[Atom]) -> Option<&'a mut TreeEntry> {
    let mut cur = root;
    for name in path {
        let t = cur.tree.as_mut()?;
        let i = find_entry(pools, t, name)?;
        cur = &mut t.entries[i];
    }
    Some(cur)
}

/// `do_change_note_fanout()` (builtin/fast-import.c:2169-2246): count the
/// notes below the directory at `dir` and, unless `fanout` is the counting
/// mode `0xff`, move each one to the path that fanout gives it.
fn do_change_note_fanout(
    g: &mut Ctx<'_>,
    orig_root: &mut TreeEntry,
    dir: &[Atom],
    hex_oid: &[u8],
    fullpath: &[u8],
    fanout: u8,
) -> Result<u64> {
    let hexsz = g.repo.object_hash().len_in_hex();
    let rawsz = g.repo.object_hash().len_in_bytes();
    let mut num_notes = 0;
    if let Some(root) = entry_at(g.pools, orig_root, dir) {
        if root.tree.is_none() {
            load_tree(g, root)?;
        }
    }

    // `for (i = 0; t && i < t->entry_count; i++)`, with `t = root->tree`
    // re-read after every entry: a rename may have grown it.
    let mut i = 0;
    loop {
        let Some(root) = entry_at(g.pools, orig_root, dir) else { break };
        let Some(t) = root.tree.as_ref() else { break };
        let Some(e) = t.entries.get(i) else { break };
        i += 1;
        let (name, mode) = (e.name.clone(), e.versions[1].mode);

        let tmp_hex_oid_len = hex_oid.len() + name.len();
        if mode == 0 || tmp_hex_oid_len > hexsz || name.len() % 2 != 0 {
            continue;
        }
        let hex = [hex_oid, &name[..]].concat();
        let mut path = fullpath.to_vec();
        if !path.is_empty() {
            path.push(b'/');
        }
        path.extend_from_slice(&name);

        if tmp_hex_oid_len == hexsz && hex.iter().all(u8::is_ascii_hexdigit) {
            // A note entry.
            if fanout == 0xff {
                num_notes += 1;
                continue;
            }
            let realpath = construct_path_with_fanout(&hex, fanout, rawsz)?;
            if path == realpath {
                num_notes += 1;
                continue;
            }
            let mut leaf = TreeEntry::detached(g.repo.object_hash(), 0);
            if !tree_content_remove(g, orig_root, &path, Some(&mut leaf), false)? {
                crate::git_fatal!("failed to remove path {}", String::from_utf8_lossy(&path));
            }
            let subtree = leaf.tree.take();
            tree_content_set(g, orig_root, &realpath, leaf.versions[1].oid, leaf.versions[1].mode, subtree)?;
        } else if s_isdir(mode) {
            let mut sub = dir.to_vec();
            sub.push(name);
            num_notes += do_change_note_fanout(g, orig_root, &sub, &hex, &path, fanout)?;
        }
    }
    Ok(num_notes)
}

/// `change_note_fanout()` (builtin/fast-import.c:2248-2259).
fn change_note_fanout(g: &mut Ctx<'_>, root: &mut TreeEntry, fanout: u8) -> Result<u64> {
    do_change_note_fanout(g, root, &[], &[], &[], fanout)
}

/// git's `base_name_compare`: unsigned byte order, except that a directory's
/// name compares as though it ended in `/`.
fn base_name_compare(n1: &[u8], m1: u32, n2: &[u8], m2: u32) -> std::cmp::Ordering {
    let common = n1.len().min(n2.len());
    match n1[..common].cmp(&n2[..common]) {
        std::cmp::Ordering::Equal => {}
        other => return other,
    }
    trailing_byte(n1, common, m1).cmp(&trailing_byte(n2, common, m2))
}

/// The byte just past the common prefix: the next real byte, or the implicit
/// terminator (`/` for a tree, NUL otherwise).
fn trailing_byte(name: &[u8], common: usize, mode: u32) -> u8 {
    match name.get(common) {
        Some(&byte) => byte,
        None if s_isdir(mode) => b'/',
        None => 0,
    }
}

/// Accept the mode words `fast-import` accepts, canonicalized as it canonicalizes
/// them (`644` and `755` gain their file bits; everything else must already be a
/// mode git can store).
fn canonical_mode(word: &[u8]) -> Option<u32> {
    match word {
        b"644" | b"100644" => Some(0o100644),
        b"755" | b"100755" => Some(0o100755),
        b"120000" => Some(0o120000),
        b"040000" | b"40000" => Some(0o40000),
        b"160000" => Some(0o160000),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Small parsing helpers
// ---------------------------------------------------------------------------

/// The remainder of `line` after `prefix`, or `None` when it does not match.
fn after<'a>(line: &'a [u8], prefix: &[u8]) -> Option<&'a [u8]> {
    line.strip_prefix(prefix)
}

/// The *owned* remainder of a header line that starts with `prefix`.
///
/// Header parsing reads one command, decides whether it is the header it is
/// looking for, and then reads the next one — so the argument has to outlive the
/// line it came from. Returning it owned keeps that reassignment legal.
fn field(line: &Option<Vec<u8>>, prefix: &[u8]) -> Option<Vec<u8>> {
    line.as_deref()
        .and_then(|l| after(l, prefix))
        .map(<[u8]>::to_vec)
}

/// Split at the first space: `(before, after)`.
fn split_space(s: &[u8]) -> Option<(&[u8], &[u8])> {
    let i = s.iter().position(|&b| b == b' ')?;
    Some((&s[..i], &s[i + 1..]))
}

/// Split a `C`/`R` argument into its source and destination paths. The source
/// must be quoted when it contains a space, which is what makes this decidable.
fn split_two_paths(rest: &[u8]) -> Result<(Vec<u8>, Vec<u8>)> {
    if rest.first() == Some(&b'"') {
        let (src, consumed) = unquote_prefix(rest)?;
        let tail = rest
            .get(consumed..)
            .and_then(|t| t.strip_prefix(b" "))
            .ok_or_else(|| anyhow!("Missing space after source: {}", String::from_utf8_lossy(rest)))?;
        Ok((src, unquote(tail)?))
    } else {
        let (src, tail) = split_space(rest)
            .ok_or_else(|| anyhow!("Missing space after source: {}", String::from_utf8_lossy(rest)))?;
        Ok((src.to_vec(), unquote(tail)?))
    }
}

/// Decode a path: C-style unquoting when it is quoted, raw bytes otherwise.
fn unquote(path: &[u8]) -> Result<Vec<u8>> {
    if path.first() != Some(&b'"') {
        return Ok(path.to_vec());
    }
    let (out, consumed) = unquote_prefix(path)?;
    if consumed != path.len() {
        crate::git_fatal!("Garbage after path: {}", String::from_utf8_lossy(path));
    }
    Ok(out)
}

/// Unquote a leading `"…"`, returning the bytes and how much of the input the
/// quoted string occupied. Implements git's `unquote_c_style` escape set.
fn unquote_prefix(input: &[u8]) -> Result<(Vec<u8>, usize)> {
    let mut out = Vec::new();
    let mut i = 1; // past the opening quote
    while i < input.len() {
        match input[i] {
            b'"' => return Ok((out, i + 1)),
            b'\\' => {
                i += 1;
                let c = *input
                    .get(i)
                    .ok_or_else(|| anyhow!("Invalid quoting: {}", String::from_utf8_lossy(input)))?;
                i += 1;
                match c {
                    b'a' => out.push(0x07),
                    b'b' => out.push(0x08),
                    b'f' => out.push(0x0c),
                    b'n' => out.push(b'\n'),
                    b'r' => out.push(b'\r'),
                    b't' => out.push(b'\t'),
                    b'v' => out.push(0x0b),
                    b'"' | b'\\' => out.push(c),
                    b'0'..=b'7' => {
                        // Up to three octal digits, counting the one just read.
                        let mut value = u32::from(c - b'0');
                        for _ in 0..2 {
                            match input.get(i) {
                                Some(&d @ b'0'..=b'7') => {
                                    value = value * 8 + u32::from(d - b'0');
                                    i += 1;
                                }
                                _ => break,
                            }
                        }
                        out.push(value as u8);
                    }
                    _ => crate::git_fatal!("Invalid quoting: {}", String::from_utf8_lossy(input)),
                }
            }
            b => {
                out.push(b);
                i += 1;
            }
        }
    }
    crate::git_fatal!("Invalid quoting: {}", String::from_utf8_lossy(input))
}

/// `parse_mark()` (builtin/fast-import.c:1901-1910): the `mark :<idnum>`
/// declaration, whose argument goes through a bare `strtoumax(v, NULL, 10)`.
/// There is no failure mode — a `mark :` line whose argument is not a number
/// leaves `next_mark` at 0, which every caller reads as "no mark was given" —
/// so `mark :x` is accepted and declares nothing.
fn parse_mark_decl(spec: &[u8]) -> Option<u64> {
    let mark = strtoumax10(spec) as u64;
    (mark != 0).then_some(mark)
}

/// `parse_mark_ref()` (builtin/fast-import.c:2274-2285): the digits after the
/// `:` of a mark *reference*, and whatever follows them.
///
/// ```c
/// assert(*p == ':');
/// p++;
/// mark = strtoumax(p, endptr, 10);
/// if (*endptr == p)
///         die(_("no value after ':' in mark: %s"), command_buf.buf);
/// ```
fn parse_mark_ref(spec: &[u8]) -> Result<(u64, &[u8])> {
    let spec = spec.strip_prefix(b":").unwrap_or(spec);
    let n = spec.iter().take_while(|b| b.is_ascii_digit()).count();
    if n == 0 {
        crate::git_fatal!("no value after ':' in mark: {}", command_buf());
    }
    let mut mark: u64 = 0;
    for b in &spec[..n] {
        mark = mark.saturating_mul(10).saturating_add(u64::from(b - b'0'));
    }
    Ok((mark, &spec[n..]))
}

/// `parse_mark_ref_eol()` (builtin/fast-import.c:2290-2299): a mark reference
/// that has to be the whole of the rest of the line.
fn parse_mark_ref_eol(spec: &[u8]) -> Result<u64> {
    let (mark, rest) = parse_mark_ref(spec)?;
    if !rest.is_empty() {
        crate::git_fatal!("garbage after mark: {}", command_buf());
    }
    Ok(mark)
}

/// `parse_mark_ref_space()` (builtin/fast-import.c:2305-2316): a mark reference
/// followed by a space, and the rest of the line after it.
fn parse_mark_ref_space(spec: &[u8]) -> Result<(u64, &[u8])> {
    let (mark, rest) = parse_mark_ref(spec)?;
    let Some(rest) = rest.strip_prefix(b" ") else {
        crate::git_fatal!("missing space after mark: {}", command_buf());
    };
    Ok((mark, rest))
}

/// git's `validate_raw_date` (builtin/fast-import.c:1966-1999): `<seconds> SP
/// [+-]<digits>`, with the whole of the sanity checking — the `1400 < num` cap on
/// the timezone — behind the `strict` flag.
///
/// `--date-format=raw-permissive` exists for importers whose source repository
/// records offsets git would never write, so under it *any* parsable offset is
/// accepted: git's own `-1800` test case is the point. The two `NEEDSWORK`
/// comments in that function name checks git considered and did not make —
/// `(num % 100) >= 60` and `((num % 100) % 15) != 0` — so neither is applied here
/// either, in strict mode included. An earlier version of this function enforced
/// the minutes one under `strict` and applied the 1400 cap unconditionally, which
/// rejected both `--date-format=raw +0090` and the permissive format's whole
/// reason for existing.
///
/// The `errno` check around each `strtoul` is what rejects an offset too large to
/// represent, which is the parse failure below.
fn valid_raw_date(date: &[u8], strict: bool) -> bool {
    let Some((secs, tz)) = split_space(date) else {
        return false;
    };
    if secs.is_empty() || !secs.iter().all(u8::is_ascii_digit) {
        return false;
    }
    if std::str::from_utf8(secs)
        .unwrap_or("x")
        .parse::<u64>()
        .is_err()
    {
        return false;
    }
    let Some((&sign, digits)) = tz.split_first() else {
        return false;
    };
    if sign != b'+' && sign != b'-' {
        return false;
    }
    if digits.is_empty() || !digits.iter().all(u8::is_ascii_digit) {
        return false;
    }
    let Ok(offset) = std::str::from_utf8(digits).unwrap_or("x").parse::<u64>() else {
        return false;
    };
    !(strict && offset > 1400)
}

/// Decode a stream field that git requires to be text (a ref name, a spec).
fn utf8(bytes: &[u8], what: &str) -> Result<String> {
    String::from_utf8(bytes.to_vec()).map_err(|_| anyhow!("invalid UTF-8 in {what}"))
}

/// Write to stdout and flush, so query responses reach a frontend that is
/// waiting on them before we read more of the stream.
fn stdout_write(buf: &[u8]) -> Result<()> {
    let mut out = std::io::stdout().lock();
    out.write_all(buf)?;
    out.flush()?;
    Ok(())
}

/// Object-write errors are boxed and not `Sync`, so give them an owned message.
fn to_anyhow(e: gix::objs::write::Error) -> anyhow::Error {
    anyhow!("unable to write object: {e}")
}

//! `git commit-graph verify` — a port of `graph_verify()` (builtin/commit-graph.c:65-141)
//! and of everything it reaches in commit-graph.c: the loaders (`load_commit_graph_one_fd_st()`,
//! `parse_commit_graph()`, `load_commit_graph_chain_fd_st()`, chunk-format.c's
//! `read_table_of_contents()`), the graph-side commit parser (`fill_commit_in_graph()` and its
//! helpers) and `verify_commit_graph()` / `verify_one_commit_graph()` (commit-graph.c:2745-2948),
//! which compare every commit of the graph with the object database.
//!
//! The C keeps its state in process-wide structures that this port reproduces as one [`Verifier`]:
//!
//! * the parsed-object table (`lookup_commit()` / `lookup_tree()`, object.c), so a commit parsed
//!   from one layer of a chain is already parsed when another layer reaches it, and a parent's
//!   graph position recorded by `insert_parent_or_die()` is what `find_commit_pos_in_graph()`
//!   later uses instead of searching;
//! * the `commit_graph_data_slab` (graph position and generation per commit);
//! * the file-static `verify_commit_graph_error` (commit-graph.c:2746), which is never reset, so
//!   an error found in the tip layer makes every base layer stop after its first loop.
//!
//! Messages go to stderr the way the C writes them: `graph_report()` with no prefix, `error()`,
//! `warning()` and `die()` with theirs, interleaved with the progress meter's redraws. The
//! loaders queue their `error()` / `warning()` lines for the caller to print, because the other
//! reader of the graph, [`ProcessGraph`] (`prepare_commit_graph()` and the graph-side commit parse
//! `git fsck`'s own process does), has to place them among output it is still collecting.

use gix::hash::ObjectId;
use gix::objs::Find as _;
use std::collections::HashMap;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

/// `GRAPH_SIGNATURE` (commit-graph.c:44), "CGPH".
const GRAPH_SIGNATURE: u32 = 0x4347_5048;
const GRAPH_CHUNKID_OIDFANOUT: u32 = 0x4f49_4446;
const GRAPH_CHUNKID_OIDLOOKUP: u32 = 0x4f49_444c;
const GRAPH_CHUNKID_DATA: u32 = 0x4344_4154;
const GRAPH_CHUNKID_GENERATION_DATA: u32 = 0x4744_4132;
const GRAPH_CHUNKID_GENERATION_DATA_OVERFLOW: u32 = 0x4744_4f32;
const GRAPH_CHUNKID_EXTRAEDGES: u32 = 0x4544_4745;
const GRAPH_CHUNKID_BLOOMINDEXES: u32 = 0x4249_4458;
const GRAPH_CHUNKID_BLOOMDATA: u32 = 0x4244_4154;
const GRAPH_CHUNKID_BASE: u32 = 0x4241_5345;
/// `GRAPH_VERSION` (commit-graph.c:55-56).
const GRAPH_VERSION: u8 = 1;
const GRAPH_EXTRA_EDGES_NEEDED: u32 = 0x8000_0000;
const GRAPH_EDGE_LAST_MASK: u32 = 0x7fff_ffff;
const GRAPH_PARENT_NONE: u32 = 0x7000_0000;
const GRAPH_LAST_EDGE: u32 = 0x8000_0000;
const GRAPH_HEADER_SIZE: usize = 8;
const GRAPH_FANOUT_SIZE: usize = 4 * 256;
/// `CHUNK_TOC_ENTRY_SIZE` (chunk-format.h:9): a 4-byte id and an 8-byte offset.
const CHUNK_TOC_ENTRY_SIZE: usize = 12;
const CORRECTED_COMMIT_DATE_OFFSET_OVERFLOW: u64 = 1 << 31;
/// `BLOOMDATA_CHUNK_HEADER_SIZE` (bloom.h): hash version, hash count, bits per entry.
const BLOOMDATA_CHUNK_HEADER_SIZE: usize = 3 * 4;
/// commit.h:12-13.
const GENERATION_NUMBER_INFINITY: u64 = (1 << 63) - 1;
const GENERATION_NUMBER_V1_MAX: u64 = 0x3FFF_FFFF;
/// `COMMIT_NOT_FROM_GRAPH` (commit-graph.h).
const COMMIT_NOT_FROM_GRAPH: u32 = 0xFFFF_FFFF;
/// commit-graph.c:2745.
const VERIFY_COMMIT_GRAPH_ERROR_HASH: i32 = 2;

/// `die()`: the message, printed as `fatal: <msg>` and exit 128 by [`graph_verify`].
type Die = String;

/// One `struct odb_source`: the object directory as git spells it in a message, and where this
/// process reads it.
pub(super) struct Source {
    pub(super) spelled: PathBuf,
    pub(super) real: PathBuf,
}

/// `repo->objects->sources`: the repository's own object directory, then every alternate in the
/// order `prepare_alt_odb()` links them, transitively (`gix-odb`'s alternate walk is the port of
/// that walk, odb.c).
///
/// The primary source's `path` is `repo_get_object_directory()`: `$GIT_OBJECT_DIRECTORY`
/// verbatim, else `<commondir>/objects` as setup left the git directory spelled
/// (`repo_set_gitdir()`, repository.c). An alternate's is the normalized absolute path
/// `link_alt_odb_entry()` records.
pub(super) fn odb_sources(repo: &gix::Repository) -> Vec<Source> {
    let real = repo.objects.store_ref().path().to_owned();
    let spelled = match std::env::var_os("GIT_OBJECT_DIRECTORY") {
        Some(env) => PathBuf::from(env),
        None if repo.common_dir() == repo.git_dir() => crate::setup::git_path_spelled(repo, "objects"),
        None => crate::setup::realpath(repo.common_dir()).join("objects"),
    };
    let mut out = vec![Source { spelled, real }];
    for alt in repo.objects.store_ref().alternate_db_paths().unwrap_or_default() {
        out.push(Source {
            spelled: alt.clone(),
            real: alt,
        });
    }
    out
}

/// `odb_find_source()` (odb.c:359-375): the source whose `strbuf_realpath()` equals `want`, which
/// the caller has already resolved with `real_pathdup(obj_dir, 1)`.
pub(super) fn find_source(sources: Vec<Source>, want: &Path) -> Option<Source> {
    sources
        .into_iter()
        .find(|s| std::fs::canonicalize(&s.real).is_ok_and(|c| c == want))
}

/// One loaded commit-graph file, `struct commit_graph` (commit-graph.h). Chunk positions are
/// offsets into `data`; an absent chunk is `None` where the C tests the pointer and `(0, 0)`
/// where it only ever tests the size.
struct Graph {
    data: Vec<u8>,
    num_commits: u32,
    num_commits_in_base: u32,
    fanout: usize,
    oid_lookup: usize,
    commit_data: usize,
    generation_data: Option<usize>,
    generation_data_overflow: Option<(usize, usize)>,
    extra_edges: (usize, usize),
    base_graphs: Option<(usize, usize)>,
    read_generation_data: bool,
    /// `bloom_filter_settings`: hash version, number of hashes, bits per entry.
    bloom: Option<(u32, u32, u32)>,
    /// The trailing checksum, `g->oid`.
    oid: ObjectId,
}

fn be32(data: &[u8], at: usize) -> u32 {
    u32::from_be_bytes(data[at..at + 4].try_into().expect("four bytes"))
}

fn be64(data: &[u8], at: usize) -> u64 {
    u64::from_be_bytes(data[at..at + 8].try_into().expect("eight bytes"))
}

/// The settings `parse_commit_graph()` consults through `prepare_repo_settings()`
/// (repo-settings.c:68-72).
struct Settings {
    generation_version: i64,
    changed_paths_version: i64,
}

fn settings(repo: &gix::Repository) -> Result<Settings, Die> {
    crate::repo_settings::RepoSettings::load(repo)?;
    let generation_version = crate::config::config_int(repo, "commitgraph.generationversion")?.unwrap_or(2);
    let read_changed_paths =
        crate::repo_settings::config_bool_strict(repo, "commitgraph.readchangedpaths")?.unwrap_or(true);
    let changed_paths_version = crate::config::config_int(repo, "commitgraph.changedpathsversion")?
        .unwrap_or(if read_changed_paths { -1 } else { 0 });
    Ok(Settings {
        generation_version,
        changed_paths_version,
    })
}

/// `graph_min_size()` (commit-graph.c:83-86).
fn graph_min_size(rawsz: usize) -> usize {
    GRAPH_HEADER_SIZE + 4 * CHUNK_TOC_ENTRY_SIZE + GRAPH_FANOUT_SIZE + rawsz
}

/// `oid_version()` (commit-graph.c): 1 for SHA-1, 2 for SHA-256.
fn oid_version(kind: gix::hash::Kind) -> u8 {
    match kind {
        gix::hash::Kind::Sha1 => 1,
        _ => 2,
    }
}

/// `struct chunkfile` after `read_table_of_contents()`: `(id, start, size)` per chunk.
type Chunks = Vec<(u32, usize, usize)>;

/// `read_table_of_contents()` (chunk-format.c:104-161) with `expected_alignment` 1, so the
/// alignment check never fires.
fn read_table_of_contents(data: &[u8], toc_offset: usize, toc_length: usize, rawsz: usize, out: &mut Vec<String>) -> Option<Chunks> {
    let mut chunks: Chunks = Vec::new();
    let mut toc = toc_offset;
    for _ in 0..toc_length {
        let chunk_id = be32(data, toc);
        let chunk_offset = be64(data, toc + 4);
        if chunk_id == 0 {
            out.push("error: terminating chunk id appears earlier than expected".into());
            return None;
        }
        toc += CHUNK_TOC_ENTRY_SIZE;
        let next_chunk_offset = be64(data, toc + 4);
        if next_chunk_offset < chunk_offset || next_chunk_offset > (data.len() - rawsz) as u64 {
            out.push(format!("error: improper chunk offset(s) {chunk_offset:x} and {next_chunk_offset:x}"));
            return None;
        }
        if chunks.iter().any(|&(id, _, _)| id == chunk_id) {
            out.push(format!("error: duplicate chunk ID {chunk_id:x} found"));
            return None;
        }
        chunks.push((chunk_id, chunk_offset as usize, (next_chunk_offset - chunk_offset) as usize));
    }
    let chunk_id = be32(data, toc);
    if chunk_id != 0 {
        out.push(format!("error: final chunk has non-zero id {chunk_id:x}"));
        return None;
    }
    Some(chunks)
}

/// `read_chunk()`'s lookup (chunk-format.c:190-202): the chunk's start and size, or `None` for
/// `CHUNK_NOT_FOUND`.
fn chunk(chunks: &Chunks, id: u32) -> Option<(usize, usize)> {
    chunks.iter().find(|&&(c, _, _)| c == id).map(|&(_, start, size)| (start, size))
}

/// `load_commit_graph_one_fd_st()` (commit-graph.c:256-280): the size from `fstat()` is checked
/// before the file is mapped, so a short file, or a directory, is only "too small".
fn load_commit_graph_one_fd_st(repo: &gix::Repository, f: std::fs::File, size: u64, out: &mut Vec<String>) -> Result<Option<Graph>, Die> {
    if size < graph_min_size(repo.object_hash().len_in_bytes()) as u64 {
        out.push("error: commit-graph file is too small".into());
        return Ok(None);
    }
    parse_commit_graph(repo, map_file(f)?, out)
}

/// `parse_commit_graph()` (commit-graph.c:373-491) over the bytes of one file. `None` after the
/// `error()` that explains why.
fn parse_commit_graph(repo: &gix::Repository, data: Vec<u8>, out: &mut Vec<String>) -> Result<Option<Graph>, Die> {
    let hash = repo.object_hash();
    let rawsz = hash.len_in_bytes();
    // The silent re-check of the minimum size (:384-385).
    if data.len() < graph_min_size(rawsz) {
        return Ok(None);
    }

    let graph_signature = be32(&data, 0);
    if graph_signature != GRAPH_SIGNATURE {
        out.push(format!("error: commit-graph signature {graph_signature:X} does not match signature {GRAPH_SIGNATURE:X}"));
        return Ok(None);
    }
    let graph_version = data[4];
    if graph_version != GRAPH_VERSION {
        out.push(format!("error: commit-graph version {graph_version:X} does not match version {GRAPH_VERSION:X}"));
        return Ok(None);
    }
    let hash_version = data[5];
    if hash_version != oid_version(hash) {
        out.push(format!(
            "error: commit-graph hash version {hash_version:X} does not match version {:X}",
            oid_version(hash)
        ));
        return Ok(None);
    }

    let num_chunks = data[6] as usize;
    if data.len() < GRAPH_HEADER_SIZE + (num_chunks + 1) * CHUNK_TOC_ENTRY_SIZE + GRAPH_FANOUT_SIZE + rawsz {
        out.push(format!("error: commit-graph file is too small to hold {num_chunks} chunks"));
        return Ok(None);
    }

    let Some(chunks) = read_table_of_contents(&data, GRAPH_HEADER_SIZE, num_chunks, rawsz, out) else {
        return Ok(None);
    };

    // graph_read_oid_fanout() (commit-graph.c:282-304).
    let fanout = match chunk(&chunks, GRAPH_CHUNKID_OIDFANOUT) {
        Some((_, size)) if size != 256 * 4 => {
            out.push("error: commit-graph oid fanout chunk is wrong size".into());
            None
        }
        Some((start, _)) => {
            let ordered = (0..255).all(|i| be32(&data, start + 4 * i) <= be32(&data, start + 4 * (i + 1)));
            if ordered {
                Some(start)
            } else {
                out.push("error: commit-graph fanout values out of order".into());
                None
            }
        }
        None => None,
    };
    let Some(fanout) = fanout else {
        out.push("error: commit-graph required OID fanout chunk missing or corrupted".into());
        return Ok(None);
    };
    let num_commits = be32(&data, fanout + 4 * 255);

    // graph_read_oid_lookup() (commit-graph.c:306-314).
    let oid_lookup = match chunk(&chunks, GRAPH_CHUNKID_OIDLOOKUP) {
        Some((start, size)) if (size / rawsz) as u64 == num_commits as u64 => Some(start),
        Some(_) => {
            out.push("error: commit-graph OID lookup chunk is the wrong size".into());
            None
        }
        None => None,
    };
    let Some(oid_lookup) = oid_lookup else {
        out.push("error: commit-graph required OID lookup chunk missing or corrupted".into());
        return Ok(None);
    };

    // graph_read_commit_data() (commit-graph.c:316-324).
    let commit_data = match chunk(&chunks, GRAPH_CHUNKID_DATA) {
        Some((start, size)) if (size / (rawsz + 16)) as u64 == num_commits as u64 => Some(start),
        Some(_) => {
            out.push("error: commit-graph commit data chunk is wrong size".into());
            None
        }
        None => None,
    };
    let Some(commit_data) = commit_data else {
        out.push("error: commit-graph required commit data chunk missing or corrupted".into());
        return Ok(None);
    };

    let extra_edges = chunk(&chunks, GRAPH_CHUNKID_EXTRAEDGES).unwrap_or((0, 0));
    let base_graphs = chunk(&chunks, GRAPH_CHUNKID_BASE);

    let settings = settings(repo)?;

    let mut generation_data = None;
    let mut generation_data_overflow = None;
    if settings.generation_version >= 2 {
        // graph_read_generation_data() (commit-graph.c:326-334); its failure is not fatal.
        if let Some((start, size)) = chunk(&chunks, GRAPH_CHUNKID_GENERATION_DATA) {
            if (size / 4) as u64 != num_commits as u64 {
                out.push("error: commit-graph generations chunk is wrong size".into());
            } else {
                generation_data = Some(start);
            }
        }
        generation_data_overflow = chunk(&chunks, GRAPH_CHUNKID_GENERATION_DATA_OVERFLOW);
    }

    let mut bloom_indexes = false;
    let mut bloom = None;
    if settings.changed_paths_version != 0 {
        // graph_read_bloom_index() (commit-graph.c:336-346).
        if let Some((_, size)) = chunk(&chunks, GRAPH_CHUNKID_BLOOMINDEXES) {
            if (size / 4) as u64 != num_commits as u64 {
                out.push("warning: commit-graph changed-path index chunk is too small".into());
            } else {
                bloom_indexes = true;
            }
        }
        // graph_read_bloom_data() (commit-graph.c:348-371).
        if let Some((start, size)) = chunk(&chunks, GRAPH_CHUNKID_BLOOMDATA) {
            if size < BLOOMDATA_CHUNK_HEADER_SIZE {
                out.push(format!(
                    "warning: ignoring too-small changed-path chunk ({size} < {BLOOMDATA_CHUNK_HEADER_SIZE}) in commit-graph file"
                ));
            } else {
                bloom = Some((be32(&data, start), be32(&data, start + 4), be32(&data, start + 8)));
            }
        }
    }
    // "We need both the bloom chunks to exist together. Else ignore the data" (:463-470).
    if !bloom_indexes {
        bloom = None;
    }

    let oid = ObjectId::from_bytes_or_panic(&data[data.len() - rawsz..]);
    Ok(Some(Graph {
        num_commits,
        num_commits_in_base: 0,
        fanout,
        oid_lookup,
        commit_data,
        read_generation_data: generation_data.is_some(),
        generation_data,
        generation_data_overflow,
        extra_edges,
        base_graphs,
        bloom,
        oid,
        data,
    }))
}

/// How `open_commit_graph()` / `git_open()` failed, for the caller's `errno` test.
enum Opened {
    File(std::fs::File, u64),
    Missing,
    Failed(std::io::Error),
}

/// `open_commit_graph()` (commit-graph.c:244-254): `git_open()` and `fstat()`.
fn open_commit_graph(path: &Path) -> Opened {
    match std::fs::File::open(path) {
        Ok(f) => match f.metadata() {
            Ok(m) => Opened::File(f, m.len()),
            Err(e) => Opened::Failed(e),
        },
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Opened::Missing,
        Err(e) => Opened::Failed(e),
    }
}

/// `xmmap()` of the whole file; a failure is `die_errno("mmap failed")` (wrapper.c).
fn map_file(mut f: std::fs::File) -> Result<Vec<u8>, Die> {
    let mut data = Vec::new();
    match f.read_to_end(&mut data) {
        Ok(_) => Ok(data),
        Err(e) => Err(format!("mmap failed: {}", crate::external::strerror(&e))),
    }
}

/// `load_commit_graph_one()` (commit-graph.c:493-509): a file that cannot be opened is skipped
/// silently.
fn load_commit_graph_one(repo: &gix::Repository, path: &Path, out: &mut Vec<String>) -> Result<Option<Graph>, Die> {
    match open_commit_graph(path) {
        Opened::File(f, size) => load_commit_graph_one_fd_st(repo, f, size, out),
        Opened::Missing | Opened::Failed(_) => Ok(None),
    }
}

/// `add_graph_to_chain()` (commit-graph.c:570-614): whether `g` may sit on `chain` (tip last),
/// given the ids the chain file named below it.
fn add_graph_to_chain(g: &mut Graph, chain: &[Graph], oids: &[ObjectId], rawsz: usize, out: &mut Vec<String>) -> bool {
    let mut n = oids.len();
    if n != 0 && g.base_graphs.is_none() {
        out.push("warning: commit-graph has no base graphs chunk".into());
        return false;
    }
    let (base_start, base_size) = g.base_graphs.unwrap_or((0, 0));
    if base_size / rawsz < n {
        out.push("warning: commit-graph base graphs chunk is too small".into());
        return false;
    }
    let mut cur = chain.len();
    while n != 0 {
        n -= 1;
        let matches = cur != 0
            && oids[n] == chain[cur - 1].oid
            && oids[n].as_bytes() == &g.data[base_start + rawsz * n..base_start + rawsz * (n + 1)];
        if !matches {
            out.push("warning: commit-graph chain does not match".into());
            return false;
        }
        cur -= 1;
    }
    if let Some(tip) = chain.last() {
        let Some(total) = tip.num_commits.checked_add(tip.num_commits_in_base) else {
            out.push(format!("warning: commit count in base graph too high: {}", tip.num_commits_in_base));
            return false;
        };
        g.num_commits_in_base = total;
    }
    true
}

/// `load_commit_graph_chain_fd_st()` (commit-graph.c:641-701): the layers that loaded, base first,
/// and whether the chain is incomplete.
fn load_commit_graph_chain(repo: &gix::Repository, sources: &[Source], body: &[u8], size: u64, out: &mut Vec<String>) -> Result<(Vec<Graph>, bool), Die> {
    let kind = repo.object_hash();
    let rawsz = kind.len_in_bytes();
    let count = size / (kind.len_in_hex() as u64 + 1);
    let mut chain: Vec<Graph> = Vec::new();
    let mut oids: Vec<ObjectId> = Vec::new();
    let mut valid = true;
    let mut lines = body.split_inclusive(|&b| b == b'\n');
    for _ in 0..count {
        // strbuf_getline_lf(): the line without its '\n'.
        let Some(line) = lines.next() else { break };
        let line = line.strip_suffix(b"\n").unwrap_or(line);
        let Some(oid) = get_oid_hex(line, kind) else {
            out.push(format!(
                "warning: invalid commit-graph chain: line '{}' not a hash",
                String::from_utf8_lossy(line)
            ));
            valid = false;
            break;
        };

        valid = false;
        for source in sources {
            // get_split_graph_filename() (commit-graph.c:204-209) names the file by the whole line.
            let name = format!("graph-{}.graph", String::from_utf8_lossy(line));
            let path = source.real.join("info").join("commit-graphs").join(name);
            if let Some(mut g) = load_commit_graph_one(repo, &path, out)? {
                if add_graph_to_chain(&mut g, &chain, &oids, rawsz, out) {
                    chain.push(g);
                    valid = true;
                }
                break;
            }
        }
        oids.push(oid);

        if !valid {
            out.push("warning: unable to find all commit-graph files".into());
            break;
        }
    }

    // validate_mixed_generation_chain() (commit-graph.c:524-543).
    if !chain.iter().all(|g| g.read_generation_data) {
        for g in &mut chain {
            g.read_generation_data = false;
        }
    }
    // validate_mixed_bloom_settings() (commit-graph.c:545-568), from the tip down.
    let mut settings: Option<(u32, u32, u32)> = None;
    for g in chain.iter_mut().rev() {
        let Some(mine) = g.bloom else { continue };
        let Some(first) = settings else {
            settings = Some(mine);
            continue;
        };
        if mine != first {
            g.bloom = None;
            out.push(format!(
                "warning: disabling Bloom filters for commit-graph layer '{}' due to incompatible settings",
                g.oid
            ));
        }
    }
    Ok((chain, !valid))
}

/// A `struct commit` as verify sees it, plus its `commit_graph_data_slab` entry.
#[derive(Clone)]
struct Commit {
    oid: ObjectId,
    parsed: bool,
    date: u64,
    parents: Vec<usize>,
    maybe_tree: Option<ObjectId>,
    graph_pos: u32,
    generation: u64,
}

impl Commit {
    fn new(oid: ObjectId) -> Self {
        Commit {
            oid,
            parsed: false,
            date: 0,
            parents: Vec::new(),
            maybe_tree: None,
            graph_pos: COMMIT_NOT_FROM_GRAPH,
            generation: 0,
        }
    }
}

/// What the parsed-object table holds for an id.
#[derive(Clone, Copy)]
enum Known {
    Commit(usize),
    Tree,
}

/// The process state `verify_commit_graph()` runs against.
struct Verifier<'r> {
    repo: &'r gix::Repository,
    /// The loaded layers, base first; the tip is the last.
    graphs: Vec<Graph>,
    rawsz: usize,
    objects: HashMap<ObjectId, Known>,
    commits: Vec<Commit>,
    /// The shallow file's commits, which `prepare_commit_graft()` registers as grafts the first
    /// time `lookup_commit_graft()` is asked; `None` once they are registered.
    shallow_pending: Option<Vec<ObjectId>>,
    /// The commits registered so far, `r->parsed_objects->grafts`.
    grafts: Vec<ObjectId>,
    /// `verify_commit_graph_error` (commit-graph.c:2746).
    error: i32,
}

/// `display_progress()`'s meter, `NULL` when progress is off.
type Meter = crate::progress::Meter;

impl Verifier<'_> {
    /// `graph_report()` (commit-graph.c:2748-2758).
    fn report(&mut self, msg: String) {
        self.error = 1;
        eprintln!("{msg}");
    }

    /// `lookup_commit()` (commit.c) through `object_as_type()`: a known non-commit is
    /// `error("object %s is a %s, not a %s")` and `NULL`.
    fn lookup_commit(&mut self, oid: ObjectId) -> Option<usize> {
        match self.objects.get(&oid) {
            Some(Known::Commit(i)) => Some(*i),
            Some(Known::Tree) => {
                eprintln!("error: object {oid} is a tree, not a commit");
                None
            }
            None => {
                self.commits.push(Commit::new(oid));
                let i = self.commits.len() - 1;
                self.objects.insert(oid, Known::Commit(i));
                Some(i)
            }
        }
    }

    /// `lookup_tree()` (tree.c): whether `oid` may be a tree.
    fn lookup_tree(&mut self, oid: ObjectId) -> bool {
        match self.objects.get(&oid) {
            Some(Known::Tree) => true,
            Some(Known::Commit(_)) => {
                eprintln!("error: object {oid} is a commit, not a tree");
                false
            }
            None => {
                self.objects.insert(oid, Known::Tree);
                true
            }
        }
    }

    fn oid_at(&self, g: usize, lex_index: u32) -> ObjectId {
        let graph = &self.graphs[g];
        let at = graph.oid_lookup + self.rawsz * lex_index as usize;
        ObjectId::from_bytes_or_panic(&graph.data[at..at + self.rawsz])
    }

    /// The layer holding chain position `pos`, walking down from `g` as
    /// `while (pos < g->num_commits_in_base) g = g->base_graph;` does.
    fn layer_of(&self, mut g: usize, pos: u32) -> usize {
        while pos < self.graphs[g].num_commits_in_base && g > 0 {
            g -= 1;
        }
        g
    }

    /// `bsearch_hash()` (hash-lookup.c:107-135) over one layer.
    fn bsearch_graph(&self, g: usize, oid: &ObjectId) -> Option<u32> {
        let graph = &self.graphs[g];
        let first = oid.as_bytes()[0] as usize;
        let mut hi = be32(&graph.data, graph.fanout + 4 * first);
        let mut lo = if first == 0 {
            0
        } else {
            be32(&graph.data, graph.fanout + 4 * (first - 1))
        };
        while lo < hi {
            let mi = lo + (hi - lo) / 2;
            let at = graph.oid_lookup + self.rawsz * mi as usize;
            match graph.data[at..at + self.rawsz].cmp(oid.as_bytes()) {
                std::cmp::Ordering::Equal => return Some(mi),
                std::cmp::Ordering::Greater => hi = mi,
                std::cmp::Ordering::Less => lo = mi + 1,
            }
        }
        None
    }

    /// `search_commit_pos_in_graph()` (commit-graph.c:981-995).
    fn search_commit_pos_in_graph(&self, oid: &ObjectId, g: usize) -> Option<u32> {
        let mut cur = Some(g);
        while let Some(c) = cur {
            if let Some(lex_index) = self.bsearch_graph(c, oid) {
                return Some(lex_index + self.graphs[c].num_commits_in_base);
            }
            cur = c.checked_sub(1);
        }
        None
    }

    /// `load_oid_from_graph()` (commit-graph.c:837-856).
    fn load_oid_from_graph(&self, g: usize, pos: u32) -> Result<ObjectId, Die> {
        let g = self.layer_of(g, pos);
        let graph = &self.graphs[g];
        if pos as u64 >= graph.num_commits as u64 + graph.num_commits_in_base as u64 {
            return Err("invalid commit position. commit-graph is likely corrupt".into());
        }
        Ok(self.oid_at(g, pos - graph.num_commits_in_base))
    }

    /// `insert_parent_or_die()` (commit-graph.c:858-874).
    fn insert_parent_or_die(&mut self, g: usize, pos: u32, item: usize) -> Result<(), Die> {
        let graph = &self.graphs[g];
        if pos as u64 >= graph.num_commits as u64 + graph.num_commits_in_base as u64 {
            return Err(format!("invalid parent position {pos}"));
        }
        let oid = self.load_oid_from_graph(g, pos)?;
        let Some(c) = self.lookup_commit(oid) else {
            return Err(format!("could not find commit {oid}"));
        };
        self.commits[c].graph_pos = pos;
        self.commits[item].parents.push(c);
        Ok(())
    }

    /// `fill_commit_graph_info()` (commit-graph.c:876-918).
    fn fill_commit_graph_info(&mut self, item: usize, g: usize, pos: u32) -> Result<(), Die> {
        let g = self.layer_of(g, pos);
        let graph = &self.graphs[g];
        if pos as u64 >= graph.num_commits as u64 + graph.num_commits_in_base as u64 {
            return Err("invalid commit position. commit-graph is likely corrupt".into());
        }
        let lex_index = (pos - graph.num_commits_in_base) as usize;
        let at = graph.commit_data + (self.rawsz + 16) * lex_index;
        let date_high = (be32(&graph.data, at + self.rawsz + 8) & 0x3) as u64;
        let date_low = be32(&graph.data, at + self.rawsz + 12) as u64;
        let date = (date_high << 32) | date_low;
        let generation = match graph.generation_data {
            Some(gda) if graph.read_generation_data => {
                let offset = be32(&graph.data, gda + 4 * lex_index) as u64;
                if offset & CORRECTED_COMMIT_DATE_OFFSET_OVERFLOW != 0 {
                    let Some((gdo, gdo_size)) = graph.generation_data_overflow else {
                        return Err("commit-graph requires overflow generation data but has none".into());
                    };
                    let offset_pos = (offset ^ CORRECTED_COMMIT_DATE_OFFSET_OVERFLOW) as usize;
                    if gdo_size / 8 <= offset_pos {
                        return Err("commit-graph overflow generation data is too small".into());
                    }
                    date.wrapping_add(be64(&graph.data, gdo + 8 * offset_pos))
                } else {
                    date.wrapping_add(offset)
                }
            }
            _ => (be32(&graph.data, at + self.rawsz + 8) >> 2) as u64,
        };
        let c = &mut self.commits[item];
        c.graph_pos = pos;
        c.date = date;
        c.generation = generation;
        Ok(())
    }

    /// `fill_commit_in_graph()` (commit-graph.c:925-979).
    fn fill_commit_in_graph(&mut self, item: usize, g: usize, pos: u32) -> Result<bool, Die> {
        let g = self.layer_of(g, pos);
        self.fill_commit_graph_info(item, g, pos)?;

        let graph = &self.graphs[g];
        let lex_index = (pos - graph.num_commits_in_base) as usize;
        let at = graph.commit_data + (self.rawsz + 16) * lex_index;
        let parent1 = be32(&graph.data, at + self.rawsz);
        let parent2 = be32(&graph.data, at + self.rawsz + 4);

        self.commits[item].parsed = true;
        self.commits[item].maybe_tree = None;
        self.commits[item].parents.clear();

        if parent1 == GRAPH_PARENT_NONE {
            return Ok(true);
        }
        self.insert_parent_or_die(g, parent1, item)?;

        if parent2 == GRAPH_PARENT_NONE {
            return Ok(true);
        }
        if parent2 & GRAPH_EXTRA_EDGES_NEEDED == 0 {
            self.insert_parent_or_die(g, parent2, item)?;
            return Ok(true);
        }

        let mut parent_data_pos = (parent2 & GRAPH_EDGE_LAST_MASK) as usize;
        loop {
            let (edges, edges_size) = self.graphs[g].extra_edges;
            if edges_size / 4 <= parent_data_pos {
                eprintln!("error: commit-graph extra-edges pointer out of bounds");
                let c = &mut self.commits[item];
                c.parents.clear();
                c.parsed = false;
                return Ok(false);
            }
            let edge_value = be32(&self.graphs[g].data, edges + 4 * parent_data_pos);
            self.insert_parent_or_die(g, edge_value & GRAPH_EDGE_LAST_MASK, item)?;
            parent_data_pos += 1;
            if edge_value & GRAPH_LAST_EDGE != 0 {
                return Ok(true);
            }
        }
    }

    /// `parse_commit_in_graph_one()` (commit-graph.c:1050-1062) with `find_commit_pos_in_graph()`.
    fn parse_commit_in_graph_one(&mut self, g: usize, item: usize) -> Result<bool, Die> {
        if self.commits[item].parsed {
            return Ok(true);
        }
        let pos = match self.commits[item].graph_pos {
            COMMIT_NOT_FROM_GRAPH => self.search_commit_pos_in_graph(&self.commits[item].oid.clone(), g),
            pos => Some(pos),
        };
        match pos {
            Some(pos) => self.fill_commit_in_graph(item, g, pos),
            None => Ok(false),
        }
    }

    /// `get_commit_tree_in_graph_one()` + `load_tree_for_commit()` (commit-graph.c:1091-1120).
    fn commit_tree_in_graph(&mut self, g: usize, item: usize) -> ObjectId {
        if let Some(tree) = self.commits[item].maybe_tree {
            return tree;
        }
        let pos = self.commits[item].graph_pos;
        let g = self.layer_of(g, pos);
        let graph = &self.graphs[g];
        let at = graph.commit_data + (self.rawsz + 16) * (pos - graph.num_commits_in_base) as usize;
        let tree = ObjectId::from_bytes_or_panic(&graph.data[at..at + self.rawsz]);
        self.lookup_tree(tree);
        self.commits[item].maybe_tree = Some(tree);
        tree
    }

    /// `commit_graph_generation()` (commit-graph.c:126-135).
    fn generation(&self, c: usize) -> u64 {
        match self.commits[c].generation {
            0 => GENERATION_NUMBER_INFINITY,
            g => g,
        }
    }

    /// `commit_graph_generation_from_graph()` (commit-graph.c:137-145).
    fn generation_from_graph(&self, c: usize) -> u64 {
        match self.commits[c].graph_pos {
            COMMIT_NOT_FROM_GRAPH => GENERATION_NUMBER_INFINITY,
            _ => self.commits[c].generation,
        }
    }

    /// `repo_parse_commit_internal(r, odb_commit, 0, 0)` (commit.c:600-655) on a fresh
    /// `create_object()`'d commit: its parent ids, tree and date as the object database has them,
    /// or `None` after the `error()` saying why.
    fn parse_from_odb(&mut self, oid: ObjectId) -> Option<(Vec<ObjectId>, ObjectId, u64)> {
        let mut buf = Vec::new();
        // OBJECT_INFO_LOOKUP_REPLACE: the store answers through `refs/replace/`.
        let data = match self.repo.objects.try_find(&oid, &mut buf) {
            Ok(Some(data)) => data,
            _ => {
                eprintln!("error: Could not read {oid}");
                return None;
            }
        };
        if data.kind != gix::object::Kind::Commit {
            eprintln!("error: Object {oid} not a commit");
            return None;
        }
        let body = data.data.to_vec();
        self.parse_commit_buffer(oid, &body)
    }

    /// `parse_commit_buffer()` (commit.c:516-598) with `check_graph` 0.
    fn parse_commit_buffer(&mut self, oid: ObjectId, buf: &[u8]) -> Option<(Vec<ObjectId>, ObjectId, u64)> {
        let kind = self.repo.object_hash();
        let hexsz = kind.len_in_hex();
        let tree_entry_len = hexsz + 5;
        let parent_entry_len = hexsz + 7;
        let tail = buf.len();

        if tail <= tree_entry_len + 1 || !buf.starts_with(b"tree ") || buf[tree_entry_len] != b'\n' {
            eprintln!("error: bogus commit object {oid}");
            return None;
        }
        let Some(tree) = get_oid_hex(&buf[5..], kind) else {
            eprintln!("error: bad tree pointer in commit {oid}");
            return None;
        };
        if !self.lookup_tree(tree) {
            eprintln!("error: bad tree pointer {tree} in commit {oid}");
            return None;
        }
        let mut at = tree_entry_len + 1;

        // lookup_commit_graft(): a shallow commit is registered as a graft with
        // `nr_parent = -1` (shallow.c:32-45), so its parents are skipped.
        self.prepare_commit_graft();
        let grafted = self.grafts.contains(&oid);
        let mut parents = Vec::new();
        while at + parent_entry_len < tail && buf[at..].starts_with(b"parent ") {
            let parent = get_oid_hex(&buf[at + 7..], kind);
            let (Some(parent), false, b'\n') = (parent, tail <= at + parent_entry_len + 1, buf[at + parent_entry_len])
            else {
                eprintln!("error: bad parents in commit {oid}");
                return None;
            };
            at += parent_entry_len + 1;
            if grafted {
                continue;
            }
            if self.lookup_commit(parent).is_none() {
                eprintln!("error: bad parent {parent} in commit {oid}");
                return None;
            }
            parents.push(parent);
        }
        Some((parents, tree, parse_commit_date(&buf[at..])))
    }

    /// `prepare_commit_graft()` (commit.c:316-330), run by the first `lookup_commit_graft()`,
    /// which is the first object-database parse of the second loop: `is_repository_shallow()`
    /// `register_shallow()`s every line of the shallow file (shallow.c:61-93), and
    /// `register_shallow()` drops the parents of a commit that is already parsed
    /// (shallow.c:32-45). Every commit the graph has parsed by then therefore reaches the
    /// comparison with no parents on either side.
    ///
    /// `read_graft_file()` for `info/grafts` is not ported.
    fn prepare_commit_graft(&mut self) {
        let Some(shallow) = self.shallow_pending.take() else {
            return;
        };
        for oid in shallow {
            if let Some(c) = self.lookup_commit(oid) {
                if self.commits[c].parsed {
                    self.commits[c].parents.clear();
                }
            }
            self.grafts.push(oid);
        }
    }

    /// `verify_one_commit_graph()` (commit-graph.c:2766-2916).
    fn verify_one_commit_graph(&mut self, g: usize, meter: &mut Meter, seen: &mut usize) -> Result<i32, Die> {
        let mut cur_fanout_pos: u32 = 0;
        let mut seen_gen_zero: Option<usize> = None;
        let mut seen_gen_non_zero: Option<usize> = None;

        if !self.checksum_valid(g) {
            self.report("the commit-graph file has incorrect checksum and is likely corrupt".into());
            self.error = VERIFY_COMMIT_GRAPH_ERROR_HASH;
        }

        let num_commits = self.graphs[g].num_commits;
        let mut prev_oid: Option<ObjectId> = None;
        let mut i: u32 = 0;
        while i < num_commits {
            let cur_oid = self.oid_at(g, i);
            if let Some(prev) = prev_oid.filter(|_| i != 0) {
                if prev >= cur_oid {
                    self.report(format!("commit-graph has incorrect OID order: {prev} then {cur_oid}"));
                }
            }
            prev_oid = Some(cur_oid);

            while u32::from(cur_oid.as_bytes()[0]) > cur_fanout_pos {
                let graph = &self.graphs[g];
                let fanout_value = be32(&graph.data, graph.fanout + 4 * cur_fanout_pos as usize);
                if i != fanout_value {
                    self.report(format!(
                        "commit-graph has incorrect fanout value: fanout[{cur_fanout_pos}] = {fanout_value} != {i}"
                    ));
                }
                cur_fanout_pos += 1;
            }

            let parsed = match self.lookup_commit(cur_oid) {
                Some(c) => self.parse_commit_in_graph_one(g, c)?,
                None => false,
            };
            if !parsed {
                self.report(format!("failed to parse commit {cur_oid} from commit-graph"));
            }
            i += 1;
        }

        while cur_fanout_pos < 256 {
            let graph = &self.graphs[g];
            let fanout_value = be32(&graph.data, graph.fanout + 4 * cur_fanout_pos as usize);
            if num_commits != fanout_value {
                self.report(format!(
                    "commit-graph has incorrect fanout value: fanout[{cur_fanout_pos}] = {fanout_value} != {i}"
                ));
            }
            cur_fanout_pos += 1;
        }

        if self.error & !VERIFY_COMMIT_GRAPH_ERROR_HASH != 0 {
            return Ok(self.error);
        }

        for i in 0..num_commits {
            *seen += 1;
            meter.set(*seen);
            let cur_oid = self.oid_at(g, i);

            let Some(graph_commit) = self.lookup_commit(cur_oid) else {
                continue;
            };
            let Some((odb_parents, odb_tree, odb_date)) = self.parse_from_odb(cur_oid) else {
                self.report(format!(
                    "failed to parse commit {cur_oid} from object database for commit-graph"
                ));
                continue;
            };

            let graph_tree = self.commit_tree_in_graph(g, graph_commit);
            if graph_tree != odb_tree {
                self.report(format!(
                    "root tree OID for commit {cur_oid} in commit-graph is {graph_tree} != {odb_tree}"
                ));
            }

            let graph_parents = self.commits[graph_commit].parents.clone();
            let mut max_generation: u64 = 0;
            let mut odb_next = 0;
            for &parent in &graph_parents {
                let Some(&odb_parent) = odb_parents.get(odb_next) else {
                    self.report(format!("commit-graph parent list for commit {cur_oid} is too long"));
                    break;
                };
                // "parse parent in case it is in a base graph"
                self.parse_commit_in_graph_one(g, parent)?;
                let parent_oid = self.commits[parent].oid;
                if parent_oid != odb_parent {
                    self.report(format!("commit-graph parent for {cur_oid} is {parent_oid} != {odb_parent}"));
                }
                let generation = self.generation_from_graph(parent);
                if generation > max_generation {
                    max_generation = generation;
                }
                odb_next += 1;
            }
            if odb_next < odb_parents.len() {
                self.report(format!("commit-graph parent list for commit {cur_oid} terminates early"));
            }

            if self.generation_from_graph(graph_commit) != 0 {
                seen_gen_non_zero = Some(graph_commit);
            } else {
                seen_gen_zero = Some(graph_commit);
            }
            if seen_gen_zero.is_some() {
                continue;
            }

            // "If we are using topological level and one of our parents has generation
            // GENERATION_NUMBER_V1_MAX, then our generation is also GENERATION_NUMBER_V1_MAX."
            if !self.graphs[g].read_generation_data && max_generation == GENERATION_NUMBER_V1_MAX {
                max_generation -= 1;
            }
            let generation = self.generation(graph_commit);
            if generation < max_generation.wrapping_add(1) {
                self.report(format!(
                    "commit-graph generation for commit {cur_oid} is {generation} < {}",
                    max_generation.wrapping_add(1)
                ));
            }

            let graph_date = self.commits[graph_commit].date;
            if graph_date != odb_date {
                self.report(format!(
                    "commit date for commit {cur_oid} in commit-graph is {graph_date} != {odb_date}"
                ));
            }
        }

        if let (Some(zero), Some(non_zero)) = (seen_gen_zero, seen_gen_non_zero) {
            let (zero, non_zero) = (self.commits[zero].oid, self.commits[non_zero].oid);
            self.report(format!(
                "commit-graph has both zero and non-zero generations (e.g., commits '{zero}' and '{non_zero}')"
            ));
        }
        Ok(self.error)
    }

    /// `commit_graph_checksum_valid()` → `hashfile_checksum_valid()` (csum-file.c): the trailing
    /// hash is the hash of everything before it.
    fn checksum_valid(&self, g: usize) -> bool {
        let data = &self.graphs[g].data;
        let body = &data[..data.len() - self.rawsz];
        let mut hasher = gix::hash::hasher(self.repo.object_hash());
        hasher.update(body);
        hasher
            .try_finalize()
            .is_ok_and(|id| id.as_bytes() == &data[data.len() - self.rawsz..])
    }
}

/// `read_commit_graph_one()` (commit-graph.c:720-728): `load_commit_graph_v1()`, a file that
/// cannot be opened skipped silently, then `load_commit_graph_chain()` (:704-718), whose
/// `open_commit_graph_chain()` (:616-639) treats an empty chain file as a missing one and warns
/// about a short one. The layers that loaded, base first; empty when there is no graph.
fn read_commit_graph_one(
    repo: &gix::Repository,
    sources: &[Source],
    source: &Source,
    out: &mut Vec<String>,
) -> Result<Vec<Graph>, Die> {
    let info = source.real.join("info");
    if let Some(g) = load_commit_graph_one(repo, &info.join("commit-graph"), out)? {
        return Ok(vec![g]);
    }
    match open_commit_graph(&info.join("commit-graphs").join("commit-graph-chain")) {
        Opened::File(_, 0) | Opened::Missing | Opened::Failed(_) => Ok(Vec::new()),
        Opened::File(_, size) if size < repo.object_hash().len_in_hex() as u64 => {
            out.push("warning: commit-graph chain file too small".into());
            Ok(Vec::new())
        }
        Opened::File(f, size) => {
            let body = map_file(f).unwrap_or_default();
            Ok(load_commit_graph_chain(repo, sources, &body, size, out)?.0)
        }
    }
}

/// How the graph half of `repo_parse_commit_internal()` (commit.c:625-640) answered.
pub(in crate::porcelain) enum GraphParse {
    /// `parse_commit_in_graph()` filled the commit in: the tree `get_commit_tree()` reads from the
    /// graph (`get_commit_tree_in_graph()`, commit-graph.c:1122-1125) and the parents
    /// `fill_commit_in_graph()` inserted, in order.
    Parsed { tree: ObjectId, parents: Vec<ObjectId> },
    /// No graph, or the commit is not in it: the caller reads the object database.
    NotInGraph,
    /// `GIT_COMMIT_GRAPH_PARANOIA` found the commit in the graph but not in the object database,
    /// so it was unparsed again after this `error()`, and the parse failed.
    Paranoid(String),
}

/// `r->objects->commit_graph` for a command that reads the graph instead of verifying it, with
/// the parsed-object table its graph-side parses share: `prepare_commit_graph()`
/// (commit-graph.c:736-773), `load_commit_graph_info()` (:1081-1089) and the graph half of
/// `repo_parse_commit_internal()` (commit.c:625-640).
///
/// `commit_graph_compatible()` (commit-graph.c:223-242) is ported for a process that ran
/// `disable_replace_refs()` first, as `cmd_fsck()` does (builtin/fsck.c:879), so `refs/replace/`
/// never makes the graph incompatible here.
pub(in crate::porcelain) struct ProcessGraph<'r> {
    repo: &'r gix::Repository,
    /// `r->objects->commit_graph_attempted`.
    attempted: bool,
    /// `r->objects->commit_graph`, with the objects its parses created.
    graph: Option<Verifier<'r>>,
    /// `parse_commit_in_graph()`'s `checked_env` (commit-graph.c:1066).
    checked_env: bool,
}

impl<'r> ProcessGraph<'r> {
    pub(in crate::porcelain) fn new(repo: &'r gix::Repository) -> Self {
        ProcessGraph {
            repo,
            attempted: false,
            graph: None,
            checked_env: false,
        }
    }

    /// `prepare_commit_graph()` (commit-graph.c:736-773): the first call loads the graph of the
    /// first source that has one, queueing the loader's `error()` / `warning()` lines on `out`;
    /// every later call answers from that attempt.
    pub(in crate::porcelain) fn prepare(&mut self, out: &mut Vec<String>) -> Result<bool, Die> {
        if self.attempted {
            return Ok(self.graph.is_some());
        }
        self.attempted = true;
        let repo = self.repo;
        let enabled = crate::repo_settings::config_bool_strict(repo, "core.commitgraph")?.unwrap_or(true);
        if !crate::setup::git_env_bool("GIT_TEST_COMMIT_GRAPH", false) && !enabled {
            return Ok(false);
        }
        // commit_graph_compatible(): prepare_commit_graft() reads info/grafts and registers the
        // shallow file, and either leaves the graph unused.
        if !repo.commit_grafts().is_empty() {
            return Ok(false);
        }
        let sources = odb_sources(repo);
        for source in &sources {
            let graphs = read_commit_graph_one(repo, &sources, source, out)?;
            if !graphs.is_empty() {
                self.graph = Some(Verifier {
                    repo,
                    rawsz: repo.object_hash().len_in_bytes(),
                    graphs,
                    objects: HashMap::new(),
                    commits: Vec::new(),
                    shallow_pending: None,
                    grafts: Vec::new(),
                    error: 0,
                });
                break;
            }
        }
        Ok(self.graph.is_some())
    }

    /// `load_commit_graph_info()` (commit-graph.c:1081-1089), which `parse_commit_buffer()` runs
    /// with `check_graph` set (commit.c:593-594) for a commit parsed from the object database:
    /// the commit's graph position and generation are filled in, which can die on a corrupt
    /// generation chunk. The commit is parsed from then on, so no graph parse revisits it.
    pub(in crate::porcelain) fn load_commit_graph_info(&mut self, oid: ObjectId, out: &mut Vec<String>) -> Result<(), Die> {
        if !self.prepare(out)? {
            return Ok(());
        }
        let v = self.graph.as_mut().expect("prepared");
        let tip = v.graphs.len() - 1;
        let Some(c) = v.lookup_commit(oid) else {
            return Ok(());
        };
        v.commits[c].parsed = true;
        // find_commit_pos_in_graph() (commit-graph.c:997-1006).
        let pos = match v.commits[c].graph_pos {
            COMMIT_NOT_FROM_GRAPH => v.search_commit_pos_in_graph(&oid, tip),
            pos => Some(pos),
        };
        match pos {
            Some(pos) => v.fill_commit_graph_info(c, tip, pos),
            None => Ok(()),
        }
    }

    /// `repo_parse_commit_internal(r, item, 0, 1)` up to its object-database read
    /// (commit.c:621-640) for a commit not parsed yet: `parse_commit_in_graph()`
    /// (commit-graph.c:1064-1079), then the `GIT_COMMIT_GRAPH_PARANOIA` re-check.
    pub(in crate::porcelain) fn parse_commit(&mut self, oid: ObjectId, out: &mut Vec<String>) -> Result<GraphParse, Die> {
        if !self.checked_env && crate::setup::git_env_bool("GIT_TEST_COMMIT_GRAPH_DIE_ON_PARSE", false) {
            return Err(
                "dying as requested by the 'GIT_TEST_COMMIT_GRAPH_DIE_ON_PARSE' variable on commit-graph parse!".into(),
            );
        }
        self.checked_env = true;
        if !self.prepare(out)? {
            return Ok(GraphParse::NotInGraph);
        }
        let repo = self.repo;
        let v = self.graph.as_mut().expect("prepared");
        let tip = v.graphs.len() - 1;
        let Some(c) = v.lookup_commit(oid) else {
            return Ok(GraphParse::NotInGraph);
        };
        if !v.parse_commit_in_graph_one(tip, c)? {
            return Ok(GraphParse::NotInGraph);
        }
        if crate::setup::git_env_bool("GIT_COMMIT_GRAPH_PARANOIA", false) && !repo.has_object(oid) {
            // unparse_commit() (commit.c:209-218).
            let item = &mut v.commits[c];
            item.parsed = false;
            item.parents.clear();
            return Ok(GraphParse::Paranoid(format!(
                "error: commit {oid} exists in commit-graph but not in the object database"
            )));
        }
        let tree = v.commit_tree_in_graph(tip, c);
        let parents = v.commits[c].parents.iter().map(|&p| v.commits[p].oid).collect();
        Ok(GraphParse::Parsed { tree, parents })
    }
}

/// `get_oid_hex()` / `get_oid_hex_algop()`: the first `hexsz` characters of `s` as hex digits of
/// either case, whatever follows them.
fn get_oid_hex(s: &[u8], kind: gix::hash::Kind) -> Option<ObjectId> {
    let hex = s.get(..kind.len_in_hex())?;
    if !hex.iter().all(u8::is_ascii_hexdigit) {
        return None;
    }
    ObjectId::from_hex(&hex.to_ascii_lowercase()).ok()
}

/// git's `isspace()` (sane_ctype, git-compat-util.h): space, `\t`, `\n` and `\r` only.
fn git_isspace(b: u8) -> bool {
    matches!(b, b' ' | b'\t' | b'\n' | b'\r')
}

/// `parse_commit_date()` (commit.c:128-194) over the buffer after the parent lines.
fn parse_commit_date(buf: &[u8]) -> u64 {
    let tail = buf.len();
    if 6 >= tail || !buf.starts_with(b"author") {
        return 0;
    }
    let mut at = 0;
    while at < tail {
        at += 1;
        if buf[at - 1] == b'\n' {
            break;
        }
    }
    if at + 9 >= tail || !buf[at..].starts_with(b"committer") {
        return 0;
    }
    let Some(eol) = buf[at..].iter().position(|&b| b == b'\n').map(|p| at + p) else {
        return 0;
    };
    let mut dateptr = eol;
    while dateptr > at && buf[dateptr - 1] != b'>' {
        dateptr -= 1;
    }
    if dateptr == at {
        return 0;
    }
    while dateptr < eol && git_isspace(buf[dateptr]) {
        dateptr += 1;
    }
    if !buf[dateptr].is_ascii_digit() && buf[dateptr] != b'-' {
        return 0;
    }
    strtoumax(&buf[dateptr..])
}

/// `parse_timestamp()` = `strtoumax(s, NULL, 10)`: an optional sign, then digits; an overflow is
/// `UINTMAX_MAX`, and a `-` negates the magnitude in unsigned arithmetic.
fn strtoumax(s: &[u8]) -> u64 {
    let (negative, digits) = match s.first() {
        Some(b'-') => (true, &s[1..]),
        Some(b'+') => (false, &s[1..]),
        _ => (false, s),
    };
    let mut value: u64 = 0;
    let mut overflow = false;
    for &b in digits.iter().take_while(|b| b.is_ascii_digit()) {
        match value.checked_mul(10).and_then(|v| v.checked_add((b - b'0') as u64)) {
            Some(v) => value = v,
            None => overflow = true,
        }
    }
    if overflow {
        return u64::MAX;
    }
    if negative {
        value.wrapping_neg()
    } else {
        value
    }
}

/// `graph_verify()` (builtin/commit-graph.c:94-140) once the options are parsed and
/// `odb_find_source_or_die()` has answered: open the single file or the chain of `source`, load
/// it, and run `verify_commit_graph()` (commit-graph.c:2918-2948) over it.
pub(super) fn graph_verify(
    repo: &gix::Repository,
    sources: &[Source],
    source: &Source,
    shallow: bool,
    progress: bool,
) -> ExitCode {
    match verify_inner(repo, sources, source, shallow, progress) {
        Ok(code) => ExitCode::from(code),
        Err(msg) => {
            eprintln!("fatal: {msg}");
            ExitCode::from(128)
        }
    }
}

/// Run one loader and print what it queued, in order, before its result is acted on: `verify`
/// writes each `error()` / `warning()` the loader meets straight to stderr, ahead of any meter.
fn emit<T>(load: impl FnOnce(&mut Vec<String>) -> T) -> T {
    let mut out = Vec::new();
    let result = load(&mut out);
    for line in out {
        eprintln!("{line}");
    }
    result
}

fn verify_inner(repo: &gix::Repository, sources: &[Source], source: &Source, shallow: bool, progress: bool) -> Result<u8, Die> {
    let graph_name = |base: &Path| base.join("info").join("commit-graph");
    let chain_name = |base: &Path| base.join("info").join("commit-graphs").join("commit-graph-chain");

    let mut incomplete_chain = false;
    let graphs = match open_commit_graph(&graph_name(&source.real)) {
        Opened::File(f, size) => match emit(|out| load_commit_graph_one_fd_st(repo, f, size, out))? {
            Some(g) => vec![g],
            None => return Ok(1),
        },
        Opened::Failed(e) => {
            return Err(format!(
                "Could not open commit-graph '{}': {}",
                graph_name(&source.spelled).display(),
                crate::external::strerror(&e)
            ))
        }
        Opened::Missing => {
            let chain_path = chain_name(&source.real);
            let die_chain = |reason: &str| {
                format!(
                    "could not open commit-graph chain '{}': {reason}",
                    chain_name(&source.spelled).display()
                )
            };
            match open_commit_graph(&chain_path) {
                Opened::Missing => return Ok(0),
                Opened::Failed(e) => return Err(die_chain(&crate::external::strerror(&e))),
                // open_commit_graph_chain() (commit-graph.c:616-639): an empty file is a missing
                // one, a short one warns and fails with EINVAL.
                Opened::File(_, 0) => return Ok(0),
                Opened::File(_, size) if size < repo.object_hash().len_in_hex() as u64 => {
                    eprintln!("warning: commit-graph chain file too small");
                    return Err(die_chain(&crate::external::strerror(&std::io::Error::from_raw_os_error(
                        libc::EINVAL,
                    ))));
                }
                Opened::File(f, size) => {
                    // xfdopen() + strbuf_getline_lf(): a read error is an early EOF.
                    let body = map_file(f).unwrap_or_default();
                    let (chain, incomplete) = emit(|out| load_commit_graph_chain(repo, sources, &body, size, out))?;
                    incomplete_chain = incomplete;
                    if chain.is_empty() {
                        return Ok(1);
                    }
                    chain
                }
            }
        }
    };

    let mut verifier = Verifier {
        repo,
        rawsz: repo.object_hash().len_in_bytes(),
        graphs,
        objects: HashMap::new(),
        commits: Vec::new(),
        shallow_pending: Some(crate::porcelain::fetch_progress::shallow_grafts(repo)),
        grafts: Vec::new(),
        error: 0,
    };

    let tip = verifier.graphs.len() - 1;
    let mut total = verifier.graphs[tip].num_commits as usize;
    if !shallow {
        total += verifier.graphs[tip].num_commits_in_base as usize;
    }
    let mut meter = Meter::counted("Verifying commits in commit graph", total, progress);
    let mut seen = 0usize;
    let mut local_error = 0;
    for g in (0..=tip).rev() {
        local_error |= verifier.verify_one_commit_graph(g, &mut meter, &mut seen)?;
        if shallow {
            break;
        }
    }
    meter.stop("done");

    let mut ret = local_error;
    if incomplete_chain {
        eprintln!("error: one or more commit-graph chain files could not be loaded");
        ret |= 1;
    }
    Ok(ret as u8)
}

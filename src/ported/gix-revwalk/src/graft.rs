//! The commit **graft table** — git's `struct commit_graft` set (`commit.c`).
//!
//! A graft substitutes a commit's parent list with one given by the user, so
//! every walk over the history sees the substituted parents instead of the ones
//! recorded in the commit object. The object itself is never touched: `git
//! cat-file -p <grafted-commit>` still prints the original `parent` header, and
//! only the *parsed* parent list changes. That is exactly where git applies it —
//! `parse_commit_buffer()` (commit.c:554-590) looks the commit up in the table
//! and, when it hits, drops every decoded `parent` line and appends the table's
//! parents instead:
//!
//! ```text
//! graft = lookup_commit_graft(r, &item->object.oid);          /* commit.c:554 */
//! if (graft)
//!         r->parsed_objects->substituted_parent = 1;
//! while (... !memcmp(bufptr, "parent ", 7)) {
//!         ...
//!         if (graft && (graft->nr_parent < 0 || !grafts_keep_true_parents))
//!                 continue;                                   /* commit.c:569 */
//!         ...
//! }
//! if (graft) {
//!         for (i = 0; i < graft->nr_parent; i++) {            /* commit.c:581 */
//!                 new_parent = lookup_commit(r, &graft->parent[i]);
//!                 ...
//!                 pptr = &commit_list_insert(new_parent, pptr)->next;
//!         }
//! }
//! ```
//!
//! Two files feed the same table, which is why they live in one type here:
//!
//! * `<GIT_DIR>/info/grafts` (or `$GIT_GRAFT_FILE`) — one line per graft,
//!   `<commit> [<parent>...]`, read by `read_graft_file()` (commit.c:287-314).
//!   A line with no parents makes the commit a root.
//! * `<GIT_DIR>/shallow` — `register_shallow()` (shallow.c:32-45) enters every
//!   listed commit with `nr_parent = -1`, i.e. [`Graft::Shallow`]. The sign is
//!   what tells the two apart: commit.c:569 refuses to keep the true parents of
//!   a shallow commit even under `--keep-true-parents`, because a shallow
//!   clone's boundary parents are genuinely absent from the object database.
//!
//! Grafts have **no gating switch**. `--no-replace-objects`,
//! `GIT_NO_REPLACE_OBJECTS` and `core.useReplaceRefs` all reach
//! `disable_replace_refs()`/`replace_refs_enabled()` (replace-object.c:83-108),
//! which only guards `lookup_replace_object()`; `lookup_commit_graft()`
//! (commit.c:332-340) consults nothing of the sort. Measured against git 2.55.0:
//! all three leave a grafted `git log` truncated exactly as an ungated run.
//!
//! The one place the two features do meet is `commit_graph_compatible()`
//! (commit-graph.c:223-242), which refuses to open a commit-graph at all when
//! the graft table is non-empty — a graph is written from the real parents and
//! would contradict the table.

use gix_hash::{ObjectId, oid};
use smallvec::SmallVec;

/// One entry of the table — what `struct commit_graft` records for a commit.
///
/// `nr_parent` in git is an `int` that doubles as a tag: `-1` means "shallow",
/// and any value `>= 0` is the length of the substituted parent list.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Graft {
    /// `nr_parent >= 0`: the commit's parents are exactly these, possibly none.
    Parents(SmallVec<[ObjectId; 1]>),
    /// `nr_parent < 0`, as [`register_shallow()`](https://github.com/git/git/blob/master/shallow.c)
    /// enters it: the commit is a shallow boundary and has no reachable parents.
    ///
    /// Kept apart from an empty [`Graft::Parents`] because commit.c:569 tests
    /// the sign, not the count, when deciding whether `--keep-true-parents` may
    /// resurrect the real parent list.
    Shallow,
}

impl Graft {
    /// The parents this graft substitutes — empty for a shallow boundary.
    pub fn parents(&self) -> &[ObjectId] {
        match self {
            Graft::Parents(ids) => ids,
            Graft::Shallow => &[],
        }
    }
}

/// The graft table: commit ids mapped to their substituted parents, sorted by id.
///
/// git keeps `r->parsed_objects->grafts` sorted and binary-searches it with
/// `commit_graft_pos()` (commit.c:202-207); [`Table::get`] is that search, and
/// [`Table::register`] is `register_commit_graft()` (commit.c:220-246) including
/// its insertion into the sorted position.
#[derive(Clone, Debug, Default)]
pub struct Table {
    /// Sorted by `.0`, so a lookup is a binary search.
    entries: Vec<(ObjectId, Graft)>,
}

impl Table {
    /// `true` when no graft is registered, i.e. `grafts_nr == 0`.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// The number of registered grafts — git's `grafts_nr`.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// `lookup_commit_graft()` (commit.c:332-340): the graft registered for `id`, if any.
    pub fn get(&self, id: &oid) -> Option<&Graft> {
        self.entries
            .binary_search_by(|(candidate, _)| candidate.as_ref().cmp(id))
            .ok()
            .map(|pos| &self.entries[pos].1)
    }

    /// The parents `id` must be walked with, or `None` when no graft covers it
    /// and the commit's own `parent` headers stand.
    pub fn parents_of(&self, id: &oid) -> Option<&[ObjectId]> {
        self.get(id).map(Graft::parents)
    }

    /// Apply the table to a decoded parent list in place, as `parse_commit_buffer()`
    /// does after decoding the `parent` headers (commit.c:557-590).
    ///
    /// Returns `true` when a graft applied, which is git's `substituted_parent`.
    pub fn substitute(&self, id: &oid, parents: &mut SmallVec<[ObjectId; 1]>) -> bool {
        match self.get(id) {
            Some(graft) => {
                parents.clear();
                parents.extend_from_slice(graft.parents());
                true
            }
            None => false,
        }
    }

    /// `register_commit_graft()` (commit.c:220-246).
    ///
    /// With `ignore_dups` an already-registered commit keeps its entry and `true`
    /// is returned, which is what makes `read_graft_file()` report
    /// `duplicate graft data`. Without it the new entry replaces the old one —
    /// the mode `register_shallow()` uses (shallow.c:44), so a `.git/shallow`
    /// line wins over a graft-file line naming the same commit.
    pub fn register(&mut self, id: ObjectId, graft: Graft, ignore_dups: bool) -> bool {
        match self.entries.binary_search_by(|(candidate, _)| candidate.cmp(&id)) {
            Ok(pos) => {
                if !ignore_dups {
                    self.entries[pos].1 = graft;
                }
                true
            }
            Err(pos) => {
                self.entries.insert(pos, (id, graft));
                false
            }
        }
    }

    /// Every entry, in sorted order — git's `for_each_commit_graft()`.
    pub fn iter(&self) -> impl Iterator<Item = (&ObjectId, &Graft)> {
        self.entries.iter().map(|(id, graft)| (id, graft))
    }
}

/// `read_graft_line()` (commit.c:249-285): decode one line of a graft file.
///
/// The format is `"Commit Parent1 Parent2 ...\n"`. git `strbuf_rtrim()`s the
/// line first — which is what makes a CRLF file and trailing blanks work — then
/// skips it entirely when it is empty or starts with `#`. Anything else that
/// does not parse as a hash followed by whitespace-separated hashes is
/// `bad graft data`.
///
/// Returns `Ok(None)` for a line git skips, and `Err(())` for one it rejects;
/// the caller owns the `error: bad graft data: <line>` report because git prints
/// the *trimmed* line there.
#[expect(clippy::result_unit_err)]
pub fn parse_line(line: &[u8], hash_kind: gix_hash::Kind) -> Result<Option<(ObjectId, Graft)>, ()> {
    let line = rtrim(line);
    if line.is_empty() || line[0] == b'#' {
        return Ok(None);
    }
    let hex_len = hash_kind.len_in_hex();
    // `parse_oid_hex()` reads exactly `hexsz` characters and leaves `tail` on the
    // rest; the loop then demands `isspace(*tail++)` before each further hash, so
    // exactly one separator character is consumed per parent and a run of two
    // blanks is a parse failure just as it is in git.
    if line.len() < hex_len {
        return Err(());
    }
    let id = ObjectId::from_hex(&line[..hex_len]).map_err(|_| ())?;
    let mut parents = SmallVec::<[ObjectId; 1]>::new();
    let mut tail = &line[hex_len..];
    while !tail.is_empty() {
        if !tail[0].is_ascii_whitespace() {
            return Err(());
        }
        tail = &tail[1..];
        if tail.len() < hex_len {
            return Err(());
        }
        parents.push(ObjectId::from_hex(&tail[..hex_len]).map_err(|_| ())?);
        tail = &tail[hex_len..];
    }
    Ok(Some((id, Graft::Parents(parents))))
}

/// `strbuf_rtrim()`: drop trailing whitespace, which includes the `\n` and the
/// `\r` of a CRLF file.
fn rtrim(mut line: &[u8]) -> &[u8] {
    while let Some((last, rest)) = line.split_last() {
        if last.is_ascii_whitespace() {
            line = rest;
        } else {
            break;
        }
    }
    line
}

/// What [`read()`] observed while parsing, so the caller can report it the way
/// `read_graft_file()` does — with `error()`, on stderr, without failing the
/// command.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Complaint {
    /// `error("bad graft data: %s", line->buf)` (commit.c:281), carrying the
    /// **trimmed** line git prints.
    BadData(String),
    /// `error("duplicate graft data: %s", buf.buf)` (commit.c:309), carrying the
    /// line as read, which is what git's `buf.buf` still holds at that point.
    Duplicate(String),
}

/// `read_graft_file()` (commit.c:287-314): register every line of `graft_file`
/// into `table`, collecting the complaints git would have printed.
///
/// A missing file is `Ok(None)` — git's `fopen_or_warn()` is silent for `ENOENT`
/// and `prepare_commit_graft()` simply carries on with an empty table.
/// Registration uses `ignore_dups = 1`, so the *first* line naming a commit
/// wins and every later one is a [`Complaint::Duplicate`].
pub fn read(
    graft_file: &std::path::Path,
    hash_kind: gix_hash::Kind,
    table: &mut Table,
) -> Result<Option<Vec<Complaint>>, read::Error> {
    let buf = match std::fs::read(graft_file) {
        Ok(buf) => buf,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(err) => return Err(read::Error::Io(err)),
    };
    let mut complaints = Vec::new();
    // `strbuf_getwholeline(&buf, fp, '\n')` keeps the separator and stops at EOF,
    // so a file without a trailing newline still yields its last line.
    for line in buf.split(|b| *b == b'\n') {
        match parse_line(line, hash_kind) {
            Ok(None) => {}
            Ok(Some((id, graft))) => {
                if table.register(id, graft, true) {
                    complaints.push(Complaint::Duplicate(String::from_utf8_lossy(rtrim(line)).into_owned()));
                }
            }
            Err(()) => complaints.push(Complaint::BadData(String::from_utf8_lossy(rtrim(line)).into_owned())),
        }
    }
    Ok(Some(complaints))
}

///
pub mod read {
    /// The error returned by [`read()`](super::read()).
    #[derive(Debug, thiserror::Error)]
    #[expect(missing_docs)]
    pub enum Error {
        #[error("Could not read the graft file")]
        Io(#[from] std::io::Error),
    }
}

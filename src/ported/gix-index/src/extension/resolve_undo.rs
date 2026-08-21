//! The `REUC` (resolve-undo) index extension — git's `resolve-undo.c`.
//!
//! It is the only record of a conflict that survives the conflict's resolution.
//! When an unmerged entry (stage 1, 2 or 3) leaves the index — because a stage-0
//! entry replaced it, or because the path was removed outright — git first
//! copies that stage's mode and blob id into `istate->resolve_undo`
//! (`record_resolve_undo()`, called from `remove_index_entry_at()`,
//! read-cache.c:1370-1371), and writes the accumulated records out as `REUC`
//! (`resolve_undo_write()`, do_write_index() read-cache.c:2222).
//!
//! Nothing recomputes it. A stock `git write-tree` on an index missing `REUC`
//! rebuilds the tree-cache but cannot rebuild this, because the stages it
//! describes no longer exist anywhere: they were the *inputs* to a resolution
//! the working tree has already replaced. Losing it silently costs
//! `git checkout --merge <path>` and `git checkout --conflict=<style> <path>`
//! their ability to put the conflict back (`unmerge_index_entry()`,
//! resolve-undo.c:104-128), and `git update-index --unresolve` its input.
//!
//! On-disk shape, one record per path, in `string_list` order — which is sorted
//! by path, since `string_list_insert()` binary-searches (resolve-undo.c:23):
//!
//! ```text
//! <path> NUL <mode1> NUL <mode2> NUL <mode3> NUL <oid1?> <oid2?> <oid3?>
//! ```
//!
//! The three modes are octal ASCII with no `0` prefix, `0` standing for "this
//! stage was absent"; an object id follows, raw, only for the stages whose mode
//! is non-zero (resolve-undo.c:41-46). A `REUC` extension therefore has no fixed
//! record size and must be parsed sequentially.

use bstr::{BStr, BString, ByteSlice};
use gix_hash::ObjectId;

use crate::{extension::Signature, util::split_at_byte_exclusive};

pub type Paths = Vec<ResolvePath>;

#[derive(Clone)]
pub struct ResolvePath {
    /// relative to the root of the repository, or what would be stored in the index
    name: BString,

    /// 0 = ancestor/common, 1 = ours, 2 = theirs
    stages: [Option<Stage>; 3],
}

impl ResolvePath {
    /// The path this resolve-undo record applies to, relative to the repository root.
    pub fn name(&self) -> &bstr::BStr {
        self.name.as_bstr()
    }

    /// The three recorded stages, in order: `[stage1 (base), stage2 (ours), stage3 (theirs)]`.
    /// A `None` means that stage was absent (mode `0`) in the recorded conflict.
    pub fn stages(&self) -> &[Option<Stage>; 3] {
        &self.stages
    }
}

#[derive(Clone, Copy)]
pub struct Stage {
    mode: u32,
    id: ObjectId,
}

impl Stage {
    /// The raw file mode recorded for this stage (e.g. `0o100644`).
    pub fn mode(&self) -> u32 {
        self.mode
    }

    /// The blob id recorded for this stage.
    pub fn id(&self) -> ObjectId {
        self.id
    }
}

pub const SIGNATURE: Signature = *b"REUC";

pub fn decode(mut data: &[u8], object_hash: gix_hash::Kind) -> Option<Paths> {
    let hash_len = object_hash.len_in_bytes();
    let mut out = Vec::new();

    while !data.is_empty() {
        let (path, rest) = split_at_byte_exclusive(data, 0)?;
        data = rest;

        let mut modes = [0u32; 3];
        for mode in &mut modes {
            let (mode_ascii, rest) = split_at_byte_exclusive(data, 0)?;
            data = rest;
            *mode = u32::from_str_radix(std::str::from_utf8(mode_ascii).ok()?, 8).ok()?;
        }

        let mut stages = [None, None, None];
        for (mode, stage) in modes.iter().zip(stages.iter_mut()) {
            if *mode == 0 {
                continue;
            }
            let (hash, rest) = data.split_at_checked(hash_len)?;
            data = rest;
            *stage = Some(Stage {
                mode: *mode,
                id: ObjectId::from_bytes_or_panic(hash),
            });
        }

        out.push(ResolvePath {
            name: path.into(),
            stages,
        });
    }
    out.into()
}

/// Serialize `paths` to `out` including the extension's signature and size header,
/// git's `resolve_undo_write()` (resolve-undo.c:33-50).
///
/// git builds the body in a `strbuf` first and hands its length to
/// `add_index_extension()`, which is why the body is assembled here before a
/// single byte reaches `out`: the size prefix cannot be known any other way.
pub fn write_to(paths: &Paths, mut out: impl std::io::Write) -> std::io::Result<()> {
    let mut body = Vec::new();
    for path in paths {
        // `strbuf_addstr(sb, item->string); strbuf_addch(sb, 0);` (resolve-undo.c:39-40)
        body.extend_from_slice(&path.name);
        body.push(0);
        // `strbuf_addf(sb, "%o%c", ui->mode[i], 0);` for all three stages, present or
        // not — an absent stage is the literal `0` (resolve-undo.c:41-42).
        for stage in &path.stages {
            let mode = stage.map_or(0, |stage| stage.mode);
            body.extend_from_slice(format!("{mode:o}").as_bytes());
            body.push(0);
        }
        // `if (!ui->mode[i]) continue;` — only the stages that existed contribute an
        // object id, and they contribute it raw (resolve-undo.c:43-46).
        for stage in path.stages.iter().flatten() {
            body.extend_from_slice(stage.id.as_slice());
        }
    }

    out.write_all(&SIGNATURE)?;
    let size = u32::try_from(body.len())
        .map_err(|_| std::io::Error::other("resolve-undo extension exceeds 4 gigabytes"))?;
    out.write_all(&size.to_be_bytes())?;
    out.write_all(&body)
}

/// git's `record_resolve_undo()` (resolve-undo.c:10-31): remember `stage`'s `mode`
/// and `id` for `name`, allocating the record on first use.
///
/// `stage` is `ce_stage(ce)`, so `0` — an already-merged entry — records nothing
/// (`if (!stage) return;`, resolve-undo.c:17) and the three slots are indexed
/// `stage - 1` (resolve-undo.c:30-31). Recording the same path and stage twice
/// overwrites, exactly as the `string_list` `util` payload does.
///
/// The order of `paths` is git's `string_list` order: `string_list_insert()`
/// binary-searches and inserts, so the list stays sorted by path and the `REUC`
/// extension is written sorted.
pub(crate) fn record(paths: &mut Paths, name: &BStr, stage: u32, mode: u32, id: ObjectId) {
    if stage == 0 || stage > 3 {
        return;
    }
    let at = match paths.binary_search_by(|path| path.name.as_bstr().cmp(name)) {
        Ok(at) => at,
        Err(at) => {
            paths.insert(
                at,
                ResolvePath {
                    name: name.to_owned(),
                    stages: [None, None, None],
                },
            );
            at
        }
    };
    paths[at].stages[stage as usize - 1] = Some(Stage { mode, id });
}

/// Record `entry`'s stage into `paths`, creating the record list on first need.
///
/// This is the shape `remove_index_entry_at()` uses it in (read-cache.c:1370-1371):
/// every entry on its way out of the index is offered, and only the unmerged ones
/// leave a trace.
pub(crate) fn record_entry(paths: &mut Option<Paths>, path: &BStr, entry: &crate::Entry) {
    let stage = entry.stage_raw();
    if stage == 0 {
        return;
    }
    record(
        paths.get_or_insert_with(Default::default),
        path,
        stage,
        entry.mode.bits(),
        entry.id,
    );
}

/// Drop the record for `name`, returning whether there was one.
///
/// git's `unmerge_index_entry()` does this through
/// `string_list_remove(istate->resolve_undo, ce->name, 1)` once the stages have
/// been put back (resolve-undo.c:151-152) — a conflict that has been recreated is
/// no longer a conflict that was undone.
pub(crate) fn remove(paths: &mut Paths, name: &BStr) -> bool {
    match paths.binary_search_by(|path| path.name.as_bstr().cmp(name)) {
        Ok(at) => {
            paths.remove(at);
            true
        }
        Err(_) => false,
    }
}

//! `git pack-redundant` — find pack files whose objects are fully covered elsewhere.
//!
//! A faithful port of `builtin/pack-redundant.c` (git 2.55.0). The command reads
//! every pack index reachable from the repository, computes the smallest
//! (bytewise) set of local packs that still holds every object, and prints the
//! packs left over — index path first, pack path second, one per line — which is
//! what makes `git pack-redundant --all | xargs rm` work.
//!
//! ### Covered (byte-identical stdout/stderr and exit code against stock git)
//!
//! * `--all`, explicit `<pack-filename>...` (substring match against the pack
//!   path, exactly like `strstr(p->pack_name, filename)`), `--verbose`,
//!   `--alt-odb`, `--i-still-use-this`, and the `--` terminator.
//! * The deprecation gate: without `--i-still-use-this` the command prints the
//!   `you_still_use_that()` block on stderr and exits 128, before any pack is
//!   touched. `-h` / `--help-all` as the sole argument still print usage on
//!   stdout and exit 129 (`show_usage_if_asked`), ahead of the gate.
//! * `usage()` on an unknown dash-argument: usage line on stderr, exit 129.
//! * The `fatal:` paths — `Zero packs found!`, `Bad pack filename: <name>`,
//!   `Filename <name> not found in packed_git`, `Bad object ID on stdin: <line>`
//!   — all exit 128, with git's exact wording (including the embedded newline
//!   the stdin one inherits from `fgets`).
//! * Object ids on stdin (read only when stdin is not a terminal) are subtracted
//!   from consideration, with git's `fgets` framing: at most `hexsz + 1` bytes
//!   per record, and only the leading `hexsz` hex characters are parsed.
//! * The `--verbose` report on stderr: alt-odb pack count, the chosen minimal
//!   set, the duplicate-object count and kb total, the unique object count, and
//!   the MB total of the redundant set.
//! * Pack ordering: per object source, packs are sorted by pack-file mtime,
//!   newest first (git's `sort_pack`, which also puts local sources first — we
//!   walk sources in order, so that falls out); sources are the primary object
//!   directory followed by the alternates. Within one source, git's `local`
//!   flag is constant, so only mtime matters.
//! * Path rendering matches git's `get_object_directory()`: `.git/objects/...`
//!   for a repository with a work tree (git chdir's to the top level first, so
//!   the form does not depend on the caller's directory), `./objects/...` for a
//!   bare repository entered at its own root, and an absolute path otherwise.
//!
//! ### Honest limitations
//!
//! * `sort_pack_list` uses C `qsort`, which is unstable; this port uses a stable
//!   sort. The two can disagree only when two packs tie on *both* remaining and
//!   total object count, in which case git's own answer is platform-dependent.
//! * Packs listed in a `multi-pack-index` are enumerated from their `.idx`
//!   files, not through the midx, so a pack with no `.idx` on disk is invisible
//!   here while git would still see it.
//! * `.keep`/`.promisor` marks are ignored, matching `pack-redundant.c`, which
//!   never consults them.
//! * Outside a repository git fails in `RUN_SETUP` with `not a git repository`
//!   before the builtin runs at all, whereas the deprecation gate here is
//!   checked first. In a repository — the only case worth diffing — the order
//!   is unobservable.

use anyhow::Result;
use std::io::{IsTerminal, Read, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use gix::hash::ObjectId;

/// `pack_redundant_usage` — the string both `usage()` and `-h` print.
const USAGE: &str = "git pack-redundant [--verbose] [--alt-odb] (--all | <pack-filename>...)";

/// One entry of git's `struct pack_list` fused with its `struct packed_git`.
struct Pack {
    /// `p->pack_name`: the `.pack` path exactly as git would render it.
    pack_name: String,
    /// `p->hash`: the hash parsed out of the file name, or all-zero hex when the
    /// name does not end in a full hash (git's `hashclr` fallback).
    hash_hex: String,
    /// `p->pack_size` + `p->index_size`, the two files `pack_set_bytecount` adds.
    pack_size: u64,
    index_size: u64,
    /// `p->pack_local`.
    local: bool,
    /// The index's object ids, ascending — git reads these straight out of the
    /// mmapped index in `cmp_two_packs`/`sizeof_union`, never from the
    /// (mutated) `remaining_objects` list.
    index_oids: Vec<ObjectId>,
    /// `remaining_objects`, whittled down by alternates, stdin and `minimize`.
    remaining: Vec<ObjectId>,
    /// `unique_objects`, filled lazily by `cmp_two_packs`.
    unique: Option<Vec<ObjectId>>,
    /// `all_objects_size`: the index's object count, captured before any pruning.
    all_objects_size: usize,
}

/// `git pack-redundant` — list packs that can be deleted without losing objects.
pub fn pack_redundant(args: &[String]) -> Result<ExitCode> {
    // show_usage_if_asked(): fires before every other check, including the gate.
    if args.len() == 1 && (args[0] == "-h" || args[0] == "--help-all") {
        println!("usage: {USAGE}");
        return Ok(ExitCode::from(129));
    }

    let mut load_all_packs = false;
    let mut verbose = false;
    let mut alt_odb = false;
    let mut i_still_use_this = false;

    let mut i = 0;
    while i < args.len() {
        let arg = args[i].as_str();
        if arg == "--" {
            i += 1;
            break;
        }
        match arg {
            "--all" => load_all_packs = true,
            "--verbose" => verbose = true,
            "--alt-odb" => alt_odb = true,
            "--i-still-use-this" => i_still_use_this = true,
            _ if arg.starts_with('-') => {
                eprintln!("usage: {USAGE}");
                return Ok(ExitCode::from(129));
            }
            _ => break,
        }
        i += 1;
    }

    if !i_still_use_this {
        you_still_use_that("git pack-redundant");
        return Ok(ExitCode::from(128));
    }

    let repo = gix::discover(".")?;
    let hash_kind = repo.object_hash();
    let hexsz = hash_kind.len_in_hex();

    // repo_for_each_pack(): the primary object directory, then each alternate.
    let mut all_packs: Vec<Pack> = Vec::new();
    let primary_fs = repo.objects.store_ref().path().to_owned();
    let objdir = object_dir_display(&repo);
    all_packs.extend(source_packs(&primary_fs, &objdir, true, hash_kind));
    for alt in repo.objects.store_ref().alternate_db_paths()? {
        let disp = alt.display().to_string();
        all_packs.extend(source_packs(&alt, &disp, false, hash_kind));
    }

    // `pack_list_insert` prepends, so both lists come out reversed relative to
    // the iteration order above.
    let mut local_packs: Vec<usize> = Vec::new();
    let mut altodb_packs: Vec<usize> = Vec::new();
    let want_altodb = alt_odb || verbose;

    if load_all_packs {
        for idx in 0..all_packs.len() {
            add_pack(
                idx,
                &all_packs,
                want_altodb,
                &mut local_packs,
                &mut altodb_packs,
            );
        }
    } else {
        for filename in &args[i..] {
            // add_pack_file(): the 40 is hardcoded in git, sha256 included.
            if filename.len() < 40 {
                eprintln!("fatal: Bad pack filename: {filename}");
                return Ok(ExitCode::from(128));
            }
            match all_packs.iter().position(|p| p.pack_name.contains(filename.as_str())) {
                Some(idx) => add_pack(
                    idx,
                    &all_packs,
                    want_altodb,
                    &mut local_packs,
                    &mut altodb_packs,
                ),
                None => {
                    eprintln!("fatal: Filename {filename} not found in packed_git");
                    return Ok(ExitCode::from(128));
                }
            }
        }
    }

    if local_packs.is_empty() {
        eprintln!("fatal: Zero packs found!");
        return Ok(ExitCode::from(128));
    }

    // load_all_objects(): union of the local packs, minus everything the
    // alternates already carry. Note git subtracts the alternates here whenever
    // they were loaded at all — that is, under `--verbose` too, not just
    // `--alt-odb`.
    let mut all_objects: Vec<ObjectId> = Vec::new();
    for &p in &local_packs {
        all_objects.extend_from_slice(&all_packs[p].remaining);
    }
    all_objects.sort_unstable();
    all_objects.dedup();
    for &a in &altodb_packs {
        let alt = std::mem::take(&mut all_packs[a].remaining);
        difference_inplace(&mut all_objects, &alt);
        all_packs[a].remaining = alt;
    }

    // scan_alt_odb_packs(): only `--alt-odb` prunes the local packs themselves.
    if alt_odb {
        for &a in &altodb_packs {
            let alt = std::mem::take(&mut all_packs[a].remaining);
            for &p in &local_packs {
                difference_inplace(&mut all_packs[p].remaining, &alt);
            }
            all_packs[a].remaining = alt;
        }
    }

    // Objects named on stdin are ignored when deciding what a pack must hold.
    let ignore = match read_ignored_objects(hexsz)? {
        Ok(list) => list,
        Err(bad_line) => {
            eprintln!("fatal: Bad object ID on stdin: {bad_line}");
            return Ok(ExitCode::from(128));
        }
    };
    difference_inplace(&mut all_objects, &ignore);
    for &p in &local_packs {
        difference_inplace(&mut all_packs[p].remaining, &ignore);
    }

    cmp_local_packs(&mut all_packs, &local_packs);
    let min = minimize(&mut all_packs, &local_packs, &all_objects);

    if verbose {
        let mut err = std::io::stderr().lock();
        let _ = writeln!(
            err,
            "There are {} packs available in alt-odbs.",
            altodb_packs.len()
        );
        let _ = writeln!(err, "The smallest (bytewise) set of packs is:");
        for &p in &min {
            let _ = writeln!(err, "\t{}", all_packs[p].pack_name);
        }
        let _ = writeln!(
            err,
            "containing {} duplicate objects with a total size of {}kb.",
            pack_redundancy(&all_packs, &min),
            bytecount(&all_packs, &min) / 1024
        );
        let _ = writeln!(
            err,
            "A total of {} unique objects were considered.",
            all_objects.len()
        );
        let _ = writeln!(err, "Redundant packs (with indexes):");
    }

    // pack_list_difference(local_packs, min) — keeps local_packs order.
    let red: Vec<usize> = local_packs
        .iter()
        .copied()
        .filter(|p| !min.contains(p))
        .collect();

    let mut out = String::new();
    for &p in &red {
        let pack = &all_packs[p];
        // odb_pack_name(): rebuilt from the object directory and the pack hash,
        // not read off disk, so it can differ from the actual `.idx` name.
        out.push_str(&format!(
            "{}/pack/pack-{}.idx\n{}\n",
            objdir, pack.hash_hex, pack.pack_name
        ));
    }
    print!("{out}");

    if verbose {
        let _ = writeln!(
            std::io::stderr().lock(),
            "{}MB of redundant packs in total.",
            bytecount(&all_packs, &red) / (1024 * 1024)
        );
    }

    Ok(ExitCode::SUCCESS)
}

/// `you_still_use_that("git pack-redundant", NULL)` — the deprecation block plus
/// `die()`. The mailing-list query is percent-encoded, so the space becomes `%20`.
fn you_still_use_that(command_name: &str) {
    let encoded = percent_encode(command_name);
    let mut err = std::io::stderr().lock();
    let _ = writeln!(err, "'{command_name}' is nominated for removal.");
    let _ = write!(
        err,
        "If you still use this command, here's what you can do:\n\
         \n\
         - read https://git-scm.com/docs/BreakingChanges.html\n\
         - check if anyone has discussed this on the mailing\n  \
           list and if they came up with something that can\n  \
           help you: https://lore.kernel.org/git/?q={encoded}\n\
         - send an email to <git@vger.kernel.org> to let us\n  \
           know that you still use this command and were unable\n  \
           to determine a suitable replacement\n\
         \n"
    );
    let _ = writeln!(err, "fatal: refusing to run without --i-still-use-this");
}

/// `strbuf_add_percentencode(..., STRBUF_ENCODE_SLASH)`: everything outside the
/// URL-unreserved set is escaped, and `/` is escaped as well.
fn percent_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// `get_object_directory()` as git renders it for output: relative to the work
/// tree top (git chdir's there first), `./objects` for a bare repository entered
/// at its own root, absolute otherwise.
fn object_dir_display(repo: &gix::Repository) -> String {
    let common = repo.common_dir();
    if let Some(work_dir) = repo.work_dir() {
        if let Ok(rel) = common.strip_prefix(work_dir) {
            return format!("{}/objects", rel.display());
        }
    }
    let same_as_cwd = std::env::current_dir()
        .ok()
        .and_then(|cwd| Some((cwd.canonicalize().ok()?, common.canonicalize().ok()?)))
        .is_some_and(|(cwd, common)| cwd == common);
    if same_as_cwd {
        return "./objects".to_string();
    }
    format!("{}/objects", common.display())
}

/// `add_pack()`: file the pack under its object source, or drop it when it comes
/// from an alternate we were not asked to look at. `pack_list_insert` prepends,
/// which is why both lists end up reversed relative to the iteration order.
fn add_pack(
    idx: usize,
    packs: &[Pack],
    want_altodb: bool,
    local_packs: &mut Vec<usize>,
    altodb_packs: &mut Vec<usize>,
) {
    if packs[idx].local {
        local_packs.insert(0, idx);
    } else if want_altodb {
        altodb_packs.insert(0, idx);
    }
}

/// `prepare_packed_git_one` + `sort_packs`: every `.idx` in `<objdir>/pack`, in
/// readdir order, then sorted by the pack file's mtime with the newest first.
fn source_packs(
    objdir_fs: &Path,
    objdir_display: &str,
    local: bool,
    hash_kind: gix::hash::Kind,
) -> Vec<Pack> {
    let pack_dir: PathBuf = objdir_fs.join("pack");
    let Ok(entries) = std::fs::read_dir(&pack_dir) else {
        return Vec::new();
    };

    let mut found: Vec<(i64, Pack)> = Vec::new();
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        let Some(stem) = name.strip_suffix(".idx") else {
            continue;
        };

        // add_packed_git(): a `.idx` without a readable regular `.pack` is skipped.
        let pack_fs = pack_dir.join(format!("{stem}.pack"));
        let Ok(pack_meta) = std::fs::metadata(&pack_fs) else {
            continue;
        };
        if !pack_meta.is_file() {
            continue;
        }
        let Ok(index_meta) = entry.metadata() else {
            continue;
        };

        // open_pack_index(): a broken index makes add_pack give up on this pack.
        let Ok(index) = gix::odb::pack::index::File::at(entry.path(), hash_kind) else {
            continue;
        };
        let index_oids: Vec<ObjectId> = (0..index.num_objects())
            .map(|n| index.oid_at_index(n).to_owned())
            .collect();

        let pack_name = format!("{objdir_display}/pack/{stem}.pack");
        let hash_hex = hash_from_pack_path(&pack_name, hash_kind);
        let mtime = pack_meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map_or(0, |d| d.as_secs() as i64);

        found.push((
            mtime,
            Pack {
                pack_name,
                hash_hex,
                pack_size: pack_meta.len(),
                index_size: index_meta.len(),
                local,
                all_objects_size: index_oids.len(),
                remaining: index_oids.clone(),
                index_oids,
                unique: None,
            },
        ));
    }

    found.sort_by(|a, b| b.0.cmp(&a.0));
    found.into_iter().map(|(_, p)| p).collect()
}

/// `add_packed_git`'s hash recovery: the last `hexsz` characters of the path with
/// its extension removed, if they are hex; otherwise `hashclr` — all zeroes.
fn hash_from_pack_path(pack_name: &str, hash_kind: gix::hash::Kind) -> String {
    let hexsz = hash_kind.len_in_hex();
    let base = pack_name.strip_suffix(".pack").unwrap_or(pack_name);
    if base.len() >= hexsz {
        let tail = &base[base.len() - hexsz..];
        if tail.bytes().all(|b| b.is_ascii_hexdigit()) {
            return tail.to_ascii_lowercase();
        }
    }
    "0".repeat(hexsz)
}

/// The stdin object list. `Ok(Err(line))` carries the offending record for the
/// `Bad object ID on stdin` message, newline included, exactly as `fgets` left it.
fn read_ignored_objects(hexsz: usize) -> Result<std::result::Result<Vec<ObjectId>, String>> {
    let mut stdin = std::io::stdin();
    if stdin.is_terminal() {
        return Ok(Ok(Vec::new()));
    }

    let mut buf = Vec::new();
    stdin.read_to_end(&mut buf)?;

    // fgets(buf, GIT_MAX_HEXSZ + 2, stdin): at most 65 bytes per record, cut
    // short at the first newline.
    const FGETS_MAX: usize = 65;
    let mut ignore = Vec::new();
    let mut pos = 0;
    while pos < buf.len() {
        let limit = (pos + FGETS_MAX).min(buf.len());
        let end = buf[pos..limit]
            .iter()
            .position(|&b| b == b'\n')
            .map_or(limit, |at| pos + at + 1);
        let record = &buf[pos..end];
        pos = end;

        // get_oid_hex(): exactly hexsz hex characters, anything after is ignored.
        let Some(hex) = record.get(..hexsz) else {
            return Ok(Err(String::from_utf8_lossy(record).into_owned()));
        };
        let Ok(oid) = ObjectId::from_hex(hex) else {
            return Ok(Err(String::from_utf8_lossy(record).into_owned()));
        };
        ignore.push(oid);
    }

    ignore.sort_unstable();
    ignore.dedup();
    Ok(Ok(ignore))
}

/// `cmp_local_packs`: every local pack keeps only the objects no other local
/// pack has. A lone pack gets an empty unique set, straight from `llist_init`.
fn cmp_local_packs(packs: &mut [Pack], local_packs: &[usize]) {
    if local_packs.len() < 2 {
        if let Some(&only) = local_packs.first() {
            packs[only].unique = Some(Vec::new());
        }
        return;
    }

    for a in 0..local_packs.len() {
        for b in (a + 1)..local_packs.len() {
            let (pa, pb) = (local_packs[a], local_packs[b]);
            if packs[pa].unique.is_none() {
                packs[pa].unique = Some(packs[pa].remaining.clone());
            }
            if packs[pb].unique.is_none() {
                packs[pb].unique = Some(packs[pb].remaining.clone());
            }
            let common = intersection(&packs[pa].index_oids, &packs[pb].index_oids);
            if let Some(u) = packs[pa].unique.as_mut() {
                difference_inplace(u, &common);
            }
            if let Some(u) = packs[pb].unique.as_mut() {
                difference_inplace(u, &common);
            }
        }
    }
}

/// `minimize()`: the packs holding unique objects are mandatory; the rest are
/// picked greedily, largest remaining contribution first, until nothing is left
/// to cover. Returns the chosen set in git's list order.
fn minimize(packs: &mut [Pack], local_packs: &[usize], all_objects: &[ObjectId]) -> Vec<usize> {
    // pack_list_insert prepends, so both partitions come out reversed.
    let mut unique: Vec<usize> = Vec::new();
    let mut non_unique: Vec<usize> = Vec::new();
    for &p in local_packs {
        if packs[p].unique.as_ref().is_some_and(|u| !u.is_empty()) {
            unique.insert(0, p);
        } else {
            non_unique.insert(0, p);
        }
    }

    let mut missing = all_objects.to_vec();
    for &p in &unique {
        let remaining = std::mem::take(&mut packs[p].remaining);
        difference_inplace(&mut missing, &remaining);
        packs[p].remaining = remaining;
    }

    let mut min = unique;
    if missing.is_empty() {
        return min;
    }

    let mut unique_pack_objects = all_objects.to_vec();
    difference_inplace(&mut unique_pack_objects, &missing);
    for &p in &non_unique {
        difference_inplace(&mut packs[p].remaining, &unique_pack_objects);
    }

    while !non_unique.is_empty() {
        // sort_pack_list(): most remaining objects first, ties broken by the
        // larger pack. git's qsort is unstable; a stable sort only differs when
        // both keys tie, where git's own order is not defined either.
        non_unique.sort_by(|&a, &b| {
            packs[b]
                .remaining
                .len()
                .cmp(&packs[a].remaining.len())
                .then(packs[b].all_objects_size.cmp(&packs[a].all_objects_size))
        });

        let head = non_unique[0];
        if packs[head].remaining.is_empty() {
            break;
        }
        min.insert(0, head);

        let chosen = std::mem::take(&mut packs[head].remaining);
        for &p in &non_unique[1..] {
            if packs[p].remaining.is_empty() {
                break;
            }
            difference_inplace(&mut packs[p].remaining, &chosen);
        }
        packs[head].remaining = chosen;
        non_unique.remove(0);
    }

    min
}

/// `get_pack_redundancy()`: how many objects the chosen set stores more than once,
/// counted over every unordered pair.
fn pack_redundancy(packs: &[Pack], set: &[usize]) -> usize {
    let mut total = 0;
    for a in 0..set.len() {
        for b in (a + 1)..set.len() {
            total += intersection(&packs[set[a]].index_oids, &packs[set[b]].index_oids).len();
        }
    }
    total
}

/// `pack_set_bytecount()`: pack plus index bytes over a set of packs.
fn bytecount(packs: &[Pack], set: &[usize]) -> u64 {
    set.iter()
        .map(|&p| packs[p].pack_size + packs[p].index_size)
        .sum()
}

/// `llist_sorted_difference_inplace(a, b)` — `a` becomes `a \ b`. Both sides must
/// be sorted ascending, which pack indices and every list derived from them are.
fn difference_inplace(a: &mut Vec<ObjectId>, b: &[ObjectId]) {
    if b.is_empty() || a.is_empty() {
        return;
    }
    let mut j = 0;
    a.retain(|x| {
        while j < b.len() && b[j] < *x {
            j += 1;
        }
        !(j < b.len() && b[j] == *x)
    });
}

/// The objects two sorted id lists share — `sizeof_union()`'s counting walk, kept
/// as a list because `cmp_two_packs` needs the ids themselves.
fn intersection(a: &[ObjectId], b: &[ObjectId]) -> Vec<ObjectId> {
    let mut out = Vec::new();
    let (mut i, mut j) = (0, 0);
    while i < a.len() && j < b.len() {
        match a[i].cmp(&b[j]) {
            std::cmp::Ordering::Equal => {
                out.push(a[i]);
                i += 1;
                j += 1;
            }
            std::cmp::Ordering::Less => i += 1,
            std::cmp::Ordering::Greater => j += 1,
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn oid(n: u8) -> ObjectId {
        ObjectId::from_hex(format!("{n:02x}").repeat(20).as_bytes()).unwrap()
    }

    #[test]
    fn difference_removes_exactly_the_shared_ids() {
        let mut a = vec![oid(1), oid(3), oid(5), oid(7)];
        difference_inplace(&mut a, &[oid(0), oid(3), oid(4), oid(7), oid(9)]);
        assert_eq!(a, vec![oid(1), oid(5)]);
    }

    #[test]
    fn difference_with_an_empty_side_is_a_no_op() {
        let mut a = vec![oid(1), oid(2)];
        difference_inplace(&mut a, &[]);
        assert_eq!(a, vec![oid(1), oid(2)]);

        let mut empty: Vec<ObjectId> = Vec::new();
        difference_inplace(&mut empty, &[oid(1)]);
        assert!(empty.is_empty());
    }

    #[test]
    fn intersection_walks_both_lists_once() {
        let a = vec![oid(1), oid(2), oid(6), oid(8)];
        let b = vec![oid(2), oid(3), oid(8)];
        assert_eq!(intersection(&a, &b), vec![oid(2), oid(8)]);
        assert!(intersection(&a, &[]).is_empty());
    }

    /// git percent-encodes the command name for the lore.kernel.org query, which
    /// is why the deprecation text says `?q=git%20pack-redundant`.
    #[test]
    fn command_name_is_percent_encoded_for_the_mailing_list_url() {
        assert_eq!(percent_encode("git pack-redundant"), "git%20pack-redundant");
        assert_eq!(percent_encode("git merge-base"), "git%20merge-base");
    }

    #[test]
    fn pack_hash_comes_from_the_file_name_and_falls_back_to_zeroes() {
        let sha1 = gix::hash::Kind::Sha1;
        let name = format!("/o/pack/pack-{}.pack", "ab".repeat(20));
        assert_eq!(hash_from_pack_path(&name, sha1), "ab".repeat(20));
        assert_eq!(
            hash_from_pack_path("/o/pack/short.pack", sha1),
            "0".repeat(40)
        );
    }

    /// A remote pack is filed only when `--alt-odb` or `--verbose` asked for it;
    /// otherwise `add_pack` drops it and it never reaches either list.
    #[test]
    fn alternate_packs_are_dropped_unless_wanted() {
        let packs = [pack_stub(true), pack_stub(false)];
        let (mut local, mut alt) = (Vec::new(), Vec::new());
        add_pack(0, &packs, false, &mut local, &mut alt);
        add_pack(1, &packs, false, &mut local, &mut alt);
        assert_eq!((local.as_slice(), alt.as_slice()), (&[0usize][..], &[][..]));

        let (mut local, mut alt) = (Vec::new(), Vec::new());
        add_pack(0, &packs, true, &mut local, &mut alt);
        add_pack(1, &packs, true, &mut local, &mut alt);
        assert_eq!((local.as_slice(), alt.as_slice()), (&[0usize][..], &[1usize][..]));
    }

    fn pack_stub(local: bool) -> Pack {
        Pack {
            pack_name: String::new(),
            hash_hex: String::new(),
            pack_size: 0,
            index_size: 0,
            local,
            index_oids: Vec::new(),
            remaining: Vec::new(),
            unique: None,
            all_objects_size: 0,
        }
    }
}

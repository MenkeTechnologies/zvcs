//! `git count-objects` — count loose objects and the disk space they consume.
//!
//! Covered: the whole documented surface — `-v`/`--verbose`/`--no-verbose`,
//! `-H`/`--human-readable`/`--no-human-readable`, clustered short flags (`-vH`),
//! `--`, and `-h`. Both the terse (`N objects, K kilobytes`) and the verbose
//! key/value report are reproduced byte-for-byte, including git's exact
//! `strbuf_humanise_bytes` rounding, the `alternate:` lines, and the usage text
//! and exit code (129) for a bad invocation. Nothing is written to the
//! repository, so post-command state is trivially unchanged.
//!
//! Sizes follow git exactly: loose objects are measured in *on-disk* bytes
//! (`st_blocks * 512`, `git-compat-util.h`'s `on_disk_bytes`), packs in
//! `.pack` + `.idx` apparent size, and garbage in apparent size — each divided
//! by 1024 and truncated for the non-`-H` form.
//!
//! Not covered exactly: the `warning:` lines that accompany garbage are written
//! to stderr with paths made relative to the worktree root rather than to git's
//! internal post-`chdir` object directory, and they are grouped loose-first then
//! pack-second instead of interleaving with the loose scan. Every number printed
//! on stdout — including `garbage` and `size-garbage` — still matches. A repo
//! whose multi-pack-index references packs whose `.idx` files have been removed
//! would under-count `in-pack`/`packs`, as this port enumerates `objects/pack/*.idx`
//! the way `prepare_packed_git_one()` does rather than reading the midx. Running
//! outside a repository propagates the discovery error to the central handler
//! rather than emitting git's own `fatal: not a git repository` / exit 128,
//! matching every other module in this directory.

use anyhow::Result;
use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use gix::hash::ObjectId;
use gix::odb::pack;

/// Stock git's `count-objects` usage block, byte-for-byte (186 bytes), including
/// the trailing blank line. Printed on `-h` (stdout) and for a usage error (stderr).
const USAGE: &str = "usage: git count-objects [-v] [-H | --human-readable]\n\
                     \n\
                     \x20   -v, --[no-]verbose    be verbose\n\
                     \x20   -H, --[no-]human-readable\n\
                     \x20                         print sizes in human readable format\n\
                     \n";

/// Bits mirroring `packfile.h`'s `PACKDIR_FILE_*`, used to classify a stray file
/// in `objects/pack` before reporting it.
const FILE_PACK: u32 = 1;
const FILE_IDX: u32 = 2;
const FILE_GARBAGE: u32 = 4;

/// Suffixes `prepare_packed_git_one()` recognises as belonging to a pack; files
/// carrying one are grouped by basename before being judged, everything else in
/// the pack directory is garbage on sight.
const PACK_SUFFIXES: [&str; 7] = [".idx", ".rev", ".pack", ".bitmap", ".keep", ".promisor", ".mtimes"];

/// `git count-objects` — report loose-object count and disk usage.
///
/// Supported forms (matching stock git byte-for-byte, including exit codes):
///   * `git count-objects`                  → `N objects, K kilobytes`
///   * `-v` / `--verbose`                   → the eight-field report plus `alternate:` lines
///   * `-H` / `--human-readable`            → IEC sizes instead of raw KiB counts
///   * `-vH`, `--no-verbose`, `--no-human-readable`, `--`
///   * `-h`                                 → usage on stdout, exit 129
///
/// Packs are only opened in verbose mode, exactly as git only installs its
/// garbage reporter and consults the pack directory when `-v` is given.
pub fn count_objects(args: &[String]) -> Result<ExitCode> {
    // Dispatch includes the verb at index 0; `count-objects` takes no positional
    // of its own, so dropping a leading copy is unambiguous.
    let args = match args.first().map(String::as_str) {
        Some("count-objects") => &args[1..],
        _ => args,
    };

    let mut verbose = false;
    let mut human = false;
    let mut end_of_opts = false;

    for a in args {
        let a = a.as_str();
        if end_of_opts {
            // Any positional is a usage error, with no `error:` line — git goes
            // straight to `usage_with_options()`.
            return Ok(usage_error(None));
        }
        match a {
            "--" => end_of_opts = true,
            "-h" => {
                print!("{USAGE}");
                return Ok(ExitCode::from(129));
            }
            "--verbose" => verbose = true,
            "--no-verbose" => verbose = false,
            "--human-readable" => human = true,
            "--no-human-readable" => human = false,
            s if s.starts_with("--") => {
                return Ok(usage_error(Some(&format!("unknown option `{}'", &s[2..]))));
            }
            s if s.len() > 1 && s.starts_with('-') => {
                // Clustered short switches, e.g. `-vH`.
                for c in s[1..].chars() {
                    match c {
                        'v' => verbose = true,
                        'H' => human = true,
                        'h' => {
                            print!("{USAGE}");
                            return Ok(ExitCode::from(129));
                        }
                        _ => return Ok(usage_error(Some(&format!("unknown switch `{c}'")))),
                    }
                }
            }
            _ => return Ok(usage_error(None)),
        }
    }

    let repo = gix::discover(".")?;
    let hash = repo.object_hash();
    let objdir = repo.objects.store_ref().path().to_path_buf();
    // git prints garbage paths relative to the top-level it chdir'd into.
    let display_root = repo.workdir().map(Path::to_path_buf);

    let mut garbage = Garbage::new(verbose, display_root);

    // --- loose objects -----------------------------------------------------
    // `for_each_loose_file_in_objdir()` walks the 256 `00`..`ff` fan-out
    // directories by name, so `info/`, `pack/` and any other entry directly
    // under `objects/` is never visited at all.
    let mut loose: u64 = 0;
    let mut loose_size: u64 = 0;
    let mut loose_ids: Vec<ObjectId> = Vec::new();
    let name_len = hash.len_in_hex() - 2;

    for fanout in 0u16..256 {
        let prefix = format!("{fanout:02x}");
        let sub = objdir.join(&prefix);
        for name in sorted_dir(&sub) {
            let path = sub.join(&name);
            let name = name.to_string_lossy().into_owned();
            // `hex_to_bytes()` accepts either case, so mixed-case names count too.
            let is_object =
                name.len() == name_len && name.bytes().all(|b| b.is_ascii_hexdigit());
            if !is_object {
                garbage.report(FILE_GARBAGE, &path);
                continue;
            }
            // A directory or symlink bearing a valid object name is garbage, not
            // an object — `count_loose()` requires a regular file.
            match fs::symlink_metadata(&path) {
                Ok(md) if md.is_file() => {
                    loose_size += on_disk_bytes(&md);
                    loose += 1;
                    if verbose {
                        if let Ok(id) = ObjectId::from_hex(format!("{prefix}{name}").as_bytes()) {
                            loose_ids.push(id);
                        }
                    }
                }
                _ => garbage.report(FILE_GARBAGE, &path),
            }
        }
    }

    if !verbose {
        let size = if human {
            humanise(loose_size)
        } else {
            format!("{} kilobytes", loose_size / 1024)
        };
        println!("{loose} objects, {size}");
        return Ok(ExitCode::SUCCESS);
    }

    // --- packs -------------------------------------------------------------
    // Only local packs feed `in-pack`/`packs`/`size-pack`; alternates are opened
    // solely so `has_object_pack()` — and hence `prune-packable` — sees them.
    let mut packs: u64 = 0;
    let mut in_pack: u64 = 0;
    let mut size_pack: u64 = 0;
    let mut indices: Vec<pack::index::File> = Vec::new();

    for idx in scan_pack_dir(&objdir, hash, &mut garbage) {
        packs += 1;
        in_pack += u64::from(idx.file.num_objects());
        size_pack += idx.pack_size + idx.index_size;
        indices.push(idx.file);
    }

    let alternates = repo.objects.store_ref().alternate_db_paths()?;
    for alt in &alternates {
        for idx in scan_pack_dir(alt, hash, &mut garbage) {
            indices.push(idx.file);
        }
    }

    let prune_packable = loose_ids
        .iter()
        .filter(|id| indices.iter().any(|f| f.lookup(**id).is_some()))
        .count();

    // --- report ------------------------------------------------------------
    let size = |bytes: u64| -> String {
        if human {
            humanise(bytes)
        } else {
            (bytes / 1024).to_string()
        }
    };

    println!("count: {loose}");
    println!("size: {}", size(loose_size));
    println!("in-pack: {in_pack}");
    println!("packs: {packs}");
    println!("size-pack: {}", size(size_pack));
    println!("prune-packable: {prune_packable}");
    println!("garbage: {}", garbage.count);
    println!("size-garbage: {}", size(garbage.size));
    for alt in &alternates {
        println!("alternate: {}", quote_c_style(alt.to_string_lossy().as_bytes()));
    }

    Ok(ExitCode::SUCCESS)
}

/// git's parse-options failure shape: an optional `error: <msg>` line followed by
/// the usage block, both on stderr, exit 129. A stray positional produces the
/// usage block alone.
fn usage_error(msg: Option<&str>) -> ExitCode {
    match msg {
        Some(m) => eprint!("error: {m}\n{USAGE}"),
        None => eprint!("{USAGE}"),
    }
    ExitCode::from(129)
}

/// One successfully opened local pack: its index plus the two file sizes git
/// sums into `size-pack` (the `.rev`, `.keep` and `.bitmap` are excluded).
struct OpenPack {
    file: pack::index::File,
    index_size: u64,
    pack_size: u64,
}

/// Scan an `objects/pack` directory the way `prepare_packed_git_one()` does:
/// open every `.idx` whose `.pack` sibling is a readable regular file, and route
/// everything else through the garbage classifier.
fn scan_pack_dir(objdir: &Path, hash: gix::hash::Kind, garbage: &mut Garbage) -> Vec<OpenPack> {
    let dir = objdir.join("pack");
    let mut open = Vec::new();
    // Files with a pack-ish suffix are judged as a group once the whole
    // directory has been seen, so a `.rev` next to its pair is not garbage.
    let mut deferred: Vec<PathBuf> = Vec::new();

    for name in sorted_dir(&dir) {
        let name = name.to_string_lossy().into_owned();
        // The multi-pack-index and its companions are handled separately by git
        // and are never candidates for garbage.
        if name.starts_with("multi-pack-index") {
            continue;
        }
        let path = dir.join(&name);

        if let Some(base) = name.strip_suffix(".idx") {
            let pack_path = dir.join(format!("{base}.pack"));
            // `add_packed_git()` refuses a pack it cannot stat as a regular file.
            let pack_size = match fs::metadata(&pack_path) {
                Ok(md) if md.is_file() => Some(md.len()),
                _ => None,
            };
            if let Some(pack_size) = pack_size {
                // A corrupt or unsupported index is skipped, as `open_pack_index()` does.
                if let Ok(file) = pack::index::File::at(&path, hash) {
                    let index_size = fs::metadata(&path).map(|md| md.len()).unwrap_or(0);
                    open.push(OpenPack {
                        file,
                        index_size,
                        pack_size,
                    });
                }
            }
        }

        if !garbage.enabled {
            continue;
        }
        if PACK_SUFFIXES.iter().any(|s| name.ends_with(s)) {
            deferred.push(path);
        } else {
            garbage.report(FILE_GARBAGE, &path);
        }
    }

    // `report_pack_garbage()`: sort, group by the path up to and including the
    // final `.`, and stay silent for any group holding both a `.pack` and `.idx`.
    deferred.sort();
    let paths: Vec<String> = deferred
        .iter()
        .map(|p| p.to_string_lossy().into_owned())
        .collect();
    let mut i = 0;
    while i < paths.len() {
        let Some(dot) = paths[i].rfind('.') else {
            garbage.report(FILE_GARBAGE, &deferred[i]);
            i += 1;
            continue;
        };
        let base = &paths[i][..=dot];
        let mut seen = 0u32;
        let mut j = i;
        while j < paths.len() && paths[j].starts_with(base) {
            match &paths[j][base.len()..] {
                "pack" => seen |= FILE_PACK,
                "idx" => seen |= FILE_IDX,
                _ => {}
            }
            j += 1;
        }
        if seen != (FILE_PACK | FILE_IDX) {
            for k in i..j {
                garbage.report(seen, &deferred[k]);
            }
        }
        i = j;
    }

    open
}

/// Accumulator behind git's `report_garbage` hook, which is only installed in
/// verbose mode — without `-v` stray files are silently skipped and never
/// contribute to any count.
struct Garbage {
    enabled: bool,
    /// Worktree root used to shorten reported paths, mirroring git's own chdir.
    root: Option<PathBuf>,
    count: u64,
    size: u64,
}

impl Garbage {
    fn new(enabled: bool, root: Option<PathBuf>) -> Self {
        Garbage {
            enabled,
            root,
            count: 0,
            size: 0,
        }
    }

    /// `real_report_garbage()`: add the file's apparent size, warn, and count it.
    fn report(&mut self, seen: u32, path: &Path) {
        if !self.enabled {
            return;
        }
        if let Ok(md) = fs::metadata(path) {
            self.size += md.len();
        }
        let desc = if seen == FILE_GARBAGE {
            "garbage found"
        } else if (seen & (FILE_IDX | FILE_PACK)) == 0 {
            "no corresponding .idx or .pack"
        } else if (seen & FILE_IDX) == 0 {
            "no corresponding .idx"
        } else {
            "no corresponding .pack"
        };
        let shown = self
            .root
            .as_deref()
            .and_then(|r| path.strip_prefix(r).ok())
            .unwrap_or(path);
        eprintln!("warning: {desc}: {}", shown.display());
        self.count += 1;
    }
}

/// Entry names of `dir` in sorted order; a missing or unreadable directory
/// yields nothing, matching git's tolerance of an absent fan-out directory.
fn sorted_dir(dir: &Path) -> Vec<OsString> {
    let Ok(rd) = fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut names: Vec<OsString> = rd.filter_map(|e| e.ok()).map(|e| e.file_name()).collect();
    names.sort();
    names
}

/// Bytes a file actually occupies, matching `git-compat-util.h`'s
/// `on_disk_bytes(st)` — `st_blocks * 512`, falling back to the apparent size
/// where the platform has no block count (git's `NO_ST_BLOCKS_IN_STRUCT_STAT`).
#[cfg(unix)]
fn on_disk_bytes(md: &fs::Metadata) -> u64 {
    use std::os::unix::fs::MetadataExt;
    md.blocks() * 512
}

#[cfg(not(unix))]
fn on_disk_bytes(md: &fs::Metadata) -> u64 {
    md.len()
}

/// `strbuf_humanise_bytes()` from `strbuf.c`, including its truncating fraction
/// arithmetic and the `>` (not `>=`) unit boundaries, so `12288` renders as
/// `12.00 KiB` and `1344` as `1.31 KiB` exactly as git does.
fn humanise(bytes: u64) -> String {
    if bytes > 1 << 30 {
        format!(
            "{}.{:02} GiB",
            bytes >> 30,
            (bytes & ((1 << 30) - 1)) / 10_737_419
        )
    } else if bytes > 1 << 20 {
        let x = bytes + 5243; // git's rounding nudge
        format!("{}.{:02} MiB", x >> 20, ((x & ((1 << 20) - 1)) * 100) >> 20)
    } else if bytes > 1 << 10 {
        let x = bytes + 5;
        format!("{}.{:02} KiB", x >> 10, ((x & ((1 << 10) - 1)) * 100) >> 10)
    } else if bytes == 1 {
        "1 byte".to_string()
    } else {
        format!("{bytes} bytes")
    }
}

/// `quote_c_style()`: emit the bytes verbatim unless they contain a control
/// byte, a quote, a backslash or anything >= 0x80, in which case wrap in double
/// quotes with C-style escapes.
fn quote_c_style(bytes: &[u8]) -> String {
    let needs = bytes
        .iter()
        .any(|&b| b < 0x20 || b == 0x7f || b == b'"' || b == b'\\' || b >= 0x80);
    if !needs {
        return String::from_utf8_lossy(bytes).into_owned();
    }
    let mut out = String::from("\"");
    for &b in bytes {
        match b {
            b'"' => out.push_str("\\\""),
            b'\\' => out.push_str("\\\\"),
            0x07 => out.push_str("\\a"),
            0x08 => out.push_str("\\b"),
            0x09 => out.push_str("\\t"),
            0x0a => out.push_str("\\n"),
            0x0b => out.push_str("\\v"),
            0x0c => out.push_str("\\f"),
            0x0d => out.push_str("\\r"),
            b if b < 0x20 || b == 0x7f || b >= 0x80 => out.push_str(&format!("\\{b:03o}")),
            b => out.push(b as char),
        }
    }
    out.push('"');
    out
}

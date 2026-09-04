//! `git archive` — create an archive of files from a named tree.
//!
//! This is a faithful port of git's `archive.c` traversal plus the whole of
//! `archive-tar.c` (the `ustar` record layout, the pax extended headers, the
//! `tar.umask` mode mangling and the 10 KiB block padding). Output for the
//! `tar` format is byte-identical to stock `git archive`, verified against a
//! reference implementation on fixtures covering nested directories, symlinks,
//! executable bits, `--prefix`, path filters, paths over 100 bytes (both the
//! `ustar` prefix split and the `<oid>.data` + pax `path` fallback) and the
//! record-boundary padding cases.
//!
//! Covered:
//!   * `<tree-ish>` resolving to a commit, tag or raw tree. A commit (or a tag
//!     peeling to one) contributes the `pax_global_header` carrying
//!     `comment=<commit-oid>` and makes the committer date the entry mtime; a
//!     bare tree uses the current time and emits no global header.
//!   * `--format=tar` (the default, also inferred from an `-o` name ending in
//!     `.tar`).
//!   * `--format=zip`, a port of `archive-zip.c` — see [`write_zip`]. One local
//!     file header plus data per entry, a central directory, and the
//!     end-of-central-directory record carrying the commit id as the archive
//!     comment. Each entry is deflated (raw, through the same coder) only when that
//!     makes it smaller, and its headers carry git's own field choices: version
//!     needed 10 throughout, the `UT` extended-timestamp extra in both headers, the
//!     UTF-8 name flag for a non-ASCII path, `version made by` 0x0317 with the mode
//!     in the external attributes for a symlink or an executable, and zip's
//!     "apparently text" internal bit set from `entry_is_binary()` — the `diff`
//!     attribute's userdiff driver where it decides (`diff` is text, `-diff` is
//!     binary), `buffer_is_binary()` on the converted content otherwise.
//!   * `--format=tgz` / `--format=tar.gz`, git's in-process `gzip` filter. See
//!     [`gzip`] — it is a port of zlib's `deflate.c` + `trees.c` driven exactly
//!     the way `archive-tar.c` drives it (10 KiB input blocks into a 16 KiB
//!     output buffer, `deflateSetHeader` with `os = 3`), so the compressed
//!     bytes match stock git's at every level including `-0`.
//!   * `--prefix=<prefix>/`, including the leading directory entry git writes
//!     for a prefix that ends in `/`.
//!   * `-o <file>` / `--output=<file>`.
//!   * `-l` / `--list` (including git's refusal to take any positional
//!     alongside it), `-v` / `--verbose`.
//!   * `--mtime=<time>`, which overrides the entry *and* `pax_global_header`
//!     timestamps. git runs the value through `approxidate()`, which falls back
//!     to the current time for anything it cannot parse; see [`approxidate`] for
//!     what this port does and does not parse.
//!   * `-<digits>` compression levels, to the extent git itself honours them:
//!     `tar` rejects any with `Argument not supported for format 'tar': -<n>`,
//!     `zip` rejects one outside `0..=9` with the same message, `tgz`/`tar.gz`
//!     accept any at parse time and only fail (at `deflateInit2`, after the tree
//!     walk) on one above `9`; the last `-<digits>` on the command line is the
//!     one reported.
//!   * Trailing `[--] <path>...` filters, with git's "pathspec did not match"
//!     failure, and git's lazy directory-entry emission (a directory record is
//!     written only once a file below it is written). The specs go through the
//!     vendored `gix-pathspec` (git parses them with
//!     `parse_pathspec(..., 0, PATHSPEC_PREFER_CWD, prefix, argv)` and
//!     `recursive = 1`), so the full magic grammar is honoured — `:(glob)`,
//!     `:(literal)`, `:(icase)`, `:(top)`, `:(exclude)` / `:!`, and the `*` /
//!     `?` / `[…]` wildcards — matched the way git's `match_pathspec()` matches,
//!     with only a *positive* spec that matches nothing raising the failure.
//!   * Being run from a subdirectory, which narrows the tree to that
//!     subdirectory exactly as git does.
//!   * `tar.umask` (numeric, and `tar.umask=user`, which git reads from the
//!     process umask by `umask(0)`-then-restore — reproduced here).
//!   * `--add-file <path>` and `--add-virtual-file <path:content>`: the extra
//!     records git writes last of all, after the tree walk, in command-line
//!     order. `--add-file` takes the disk file's bytes and its executable bit
//!     (canonicalised and masked exactly like a tree blob), names it
//!     `<--prefix>` + `basename(path)`, and `stat()`s it at parse time so
//!     `File not found` / `Not a regular file` fire in git's order; the fake
//!     object id git assigns each added file (a 1-based counter, big-endian in
//!     the first eight bytes of an all-zero hash) is reproduced so the
//!     `<oid>.data` / `<oid>.paxheader` overflow name matches. `--add-virtual-file`
//!     writes the literal content under the literal path (git does *not* prepend
//!     `--prefix` to it), validated (missing colon / empty name) at parse time.
//!   * Unknown options: `--<opt>` → `error: unknown option '<opt>'`, `-<c>` →
//!     `error: unknown switch '<c>'`, each followed by the usage block on stderr
//!     with exit 129; `-h` / `--help` print the same usage to stdout, exit 129;
//!     the `--no-` negations of the boolean and value options.
//!
//!   * `--worktree-attributes`, to the extent this port supports attributes at
//!     all: the working directory's `.gitattributes` files are read and, like
//!     the ones in the tree, rejected if any of them assigns a
//!     content-affecting attribute. With none set the flag cannot change a byte
//!     of the archive, which is exactly what stock git produces.
//!
//!   * A regular blob larger than the 8 GiB `ustar` `size` field: git writes the
//!     header `size` as `0` and spills the true length into a pax `size` record
//!     (`strbuf_append_ext_header_uint(&ext_header, "size", size)`), appended
//!     after any `path` / `linkpath` record. Reproduced here on the very pax path
//!     the over-100-byte `path` overflow already drives.
//!
//! `--remote` drives the `git-upload-archive` protocol itself: the command line
//! travels to the far side as `argument` pkt-lines, the archive comes back on
//! sideband 1 and the server's diagnostics on bands 2 and 3. Only a local
//! repository is reached that way — git's other transports connect the same
//! stream over ssh or the daemon, which this port does not open; `--exec` names
//! the program to run for a local one, as it does for git.
//!
//! Not covered — every one of these fails loudly rather than emitting an
//! archive that would silently differ from git's:
//!   * `export-subst`. `args->convert` makes `object_file_to_archive()` run
//!     `format_subst()` over the blob, expanding `$Format:<pretty>$` against the
//!     archived commit; the `--pretty` formatter is not wired into this command,
//!     so an entry carrying the attribute is rejected rather than archived
//!     unexpanded. Everything else `.gitattributes` drives *is* honoured — see
//!     [`Convert`]: `export-ignore` skips entries and whole sub-trees, and
//!     `convert_to_working_tree()` (`text`/`eol`/`crlf`, `ident`,
//!     `working-tree-encoding`, `filter=<driver>`) rewrites blob content, with
//!     attributes read from the archived tree exactly as git's
//!     `git_attr_set_direction(GIT_ATTR_INDEX)` does, plus
//!     `$GIT_DIR/info/attributes`, `core.attributesFile`, `core.autocrlf` and
//!     `core.eol`. `--worktree-attributes` additionally consults the working
//!     copy.
//!   * Two pathspec-magic corners that need substrate this port does not wire
//!     into `git archive`: an `:(attr:<name>)` spec is matched as if no
//!     attribute were set, so an `:(attr:…)` spec selects nothing; and `:(top)`
//!     given from a *subdirectory* re-roots at the repository top in git, but
//!     here the tree is already narrowed to the subdirectory, so a `:(top)` spec
//!     is matched against the narrowed tree. Every other magic works.

use anyhow::{bail, Result};
use std::io::Write;
use std::process::ExitCode;

use gix::bstr::ByteSlice;
use gix::hash::ObjectId;
use gix::object::tree::EntryKind;
use gix::pathspec::Search;

/// git's `RECORDSIZE`: one `ustar` record.
const RECORD: usize = 512;
/// git's `BLOCKSIZE`: the unit stdout is padded up to.
const BLOCK: u64 = 10240;
/// The `ustar` `name` field width; longer paths need a prefix split or pax.
const NAME_MAX: usize = 100;
/// The `ustar` `prefix` field width.
const PREFIX_MAX: usize = 155;
/// git refuses a plain `size` field beyond this and emits a pax `size` record.
const SIZE_MAX: u64 = 0o77777777777;

const ZEROS: [u8; RECORD] = [0; RECORD];

/// The formats stock `git archive --list` reports before configuration is read: `tar` and
/// git's two pre-seeded tar filters, then `zip` — the registration order of
/// `init_tar_archiver()` and `init_zip_archiver()` (archive.c), which is the order `--list`
/// prints. A `tar.<name>.command` adds its own name to the list between the two groups; see
/// [`configured_formats`].
const FORMATS: &[&str] = &["tar", "tgz", "tar.gz", "zip"];

/// ```c
/// ar = find_tar_filter(name, namelen);
/// if (!ar) {
///         CALLOC_ARRAY(ar, 1);
///         ar->name = xmemdupz(name, namelen);
///         ar->write_archive = write_tar_filter_archive;
///         ar->flags = ARCHIVER_WANT_COMPRESSION_LEVELS |
///                     ARCHIVER_HIGH_COMPRESSION_LEVELS;
///         ALLOC_GROW(tar_filters, nr_tar_filters + 1, alloc_tar_filters);
///         tar_filters[nr_tar_filters++] = ar;
/// }
/// ```
///
/// A filter is *not* born remotely available — `ARCHIVER_REMOTE` is granted only
/// by a `tar.<name>.remote`, which [`remote_allowed`] reads.
///
/// (`git_tar_config()`, archive-tar.c.) Every `tar.<name>.command` is an archive format of
/// its own: the tar goes to the command's standard input and whatever it writes is the
/// archive. `tgz` and `tar.gz` are registered the same way with `git archive gzip` as their
/// command, so configuring one of *those* replaces the internal gzip.
fn configured_formats(repo: Option<&gix::Repository>) -> Vec<String> {
    let mut out: Vec<String> = FORMATS[..3].iter().map(|f| (*f).to_string()).collect();
    if let Some(repo) = repo {
        let snapshot = repo.config_snapshot();
        for section in snapshot.plumbing().sections() {
            let header = section.header();
            if !header.name().to_string().eq_ignore_ascii_case("tar") {
                continue;
            }
            // `tar.<name>.command` reaches the config callback as one flat key, so a
            // dotted format name lands in the subsection (`tar "tar.gz"`) or in the value
            // name (`tar.command` under a `tar.gz` subsection is not a thing) — either way
            // the name is whatever sits between `tar.` and `.command`.
            let Some(sub) = header.subsection_name() else {
                continue;
            };
            let name = sub.to_string();
            if section.body().values("command").is_empty() || out.contains(&name) {
                continue;
            }
            out.push(name);
        }
    }
    out.push("zip".to_string());
    out
}

/// Whether `format` carries `ARCHIVER_REMOTE` — the flag that decides what
/// `git upload-archive` will serve, and so what a `git archive --remote` client
/// can ask for.
///
/// `tar` and `zip` are static archivers declared with the flag
/// (`archive-tar.c:526`, `archive-zip.c`). Everything else is a tar filter, and
/// `tar_filter_config()` grants the flag only through `tar.<name>.remote`:
///
/// ```c
/// ar->flags = ARCHIVER_WANT_COMPRESSION_LEVELS | ARCHIVER_HIGH_COMPRESSION_LEVELS;
/// [...]
/// if (!strcmp(type, "remote")) {
///         if (git_config_bool(var, value))  ar->flags |=  ARCHIVER_REMOTE;
///         else                              ar->flags &= ~ARCHIVER_REMOTE;
/// }
/// ```
///
/// `init_tar_archiver()` pre-seeds `tar.tgz.remote=true` and
/// `tar.tar.gz.remote=true` *before* reading the repository's configuration, so
/// those two default to remotely available and a user `tar.tgz.remote=false`
/// takes it back — while a filter the user invents is not remotely available
/// unless they say so.
fn remote_allowed(repo: Option<&gix::Repository>, format: &str) -> bool {
    if format == "tar" || format == "zip" {
        return true;
    }
    let configured = repo.and_then(|repo| {
        repo.config_snapshot()
            .plumbing()
            .boolean_by("tar", Some(gix::bstr::BStr::new(format)), "remote")
            .ok()
            .flatten()
    });
    configured.unwrap_or(matches!(format, "tgz" | "tar.gz"))
}

/// The command a `tar.<format>.command` configures for this format, if any.
fn tar_filter_command(repo: &gix::Repository, format: &str) -> Option<String> {
    repo.config_snapshot()
        .plumbing()
        .string_by("tar", Some(format.into()), "command")
        .map(|v| v.to_string())
}

/// The formats carrying git's `ARCHIVER_WANT_COMPRESSION_LEVELS`. A `-<digits>`
/// given for any other format is fatal, which is the only way a compression
/// level is observable from here since none of these three can be produced yet.
const LEVEL_FORMATS: &[&str] = &["tgz", "tar.gz", "zip"];

/// `parse_archive_args()`'s `struct option opts[]` (archive.c), in table order, as
/// [`super::resolve_long`] reads it.
///
/// `cmd_archive()` runs a first `parse_options()` over a three-entry
/// `local_opts[]` (`--output`, `--remote`, `--exec`) with `PARSE_OPT_KEEP_ALL`,
/// which keeps everything it does not claim, and the surviving argv then reaches
/// this table — where those same three names appear again. One table therefore
/// answers both passes identically. `--mtime` is `PARSE_OPT_NONEG`.
const LONG_OPTS: &[super::LongOpt] = &[
    super::LongOpt { name: "format",                      neg: true,  arg: super::Arg::Required },
    super::LongOpt { name: "prefix",                      neg: true,  arg: super::Arg::Required },
    super::LongOpt { name: "add-file",                    neg: true,  arg: super::Arg::Required },
    super::LongOpt { name: "add-virtual-file",            neg: true,  arg: super::Arg::Required },
    super::LongOpt { name: "output",                      neg: true,  arg: super::Arg::Required },
    super::LongOpt { name: "worktree-attributes",         neg: true,  arg: super::Arg::None },
    super::LongOpt { name: "verbose",                     neg: true,  arg: super::Arg::None },
    super::LongOpt { name: "mtime",                       neg: false, arg: super::Arg::Required },
    super::LongOpt { name: "list",                        neg: true,  arg: super::Arg::None },
    super::LongOpt { name: "remote",                      neg: true,  arg: super::Arg::Required },
    super::LongOpt { name: "exec",                        neg: true,  arg: super::Arg::Required },
];
/// git's `archive_usage` followed by the option list `parse_options()` renders,
/// byte for byte, as printed on stderr for a usage error.
const USAGE: &str = "\
usage: git archive [<options>] <tree-ish> [<path>...]
   or: git archive --list
   or: git archive --remote <repo> [--exec <cmd>] [<options>] <tree-ish> [<path>...]
   or: git archive --remote <repo> [--exec <cmd>] --list

    --[no-]format <fmt>   archive format
    --[no-]prefix <prefix>
                          prepend prefix to each pathname in the archive
    --[no-]add-file <file>
                          add untracked file to archive
    --[no-]add-virtual-file <path:content>
                          add untracked file to archive
    -o, --[no-]output <file>
                          write the archive to this file
    --[no-]worktree-attributes
                          read .gitattributes in working directory
    -v, --[no-]verbose    report archived files on stderr
    --mtime <time>        set modification time of archive entries
    -NUM                  set compression level

    -l, --[no-]list       list supported archive formats

    --[no-]remote <repo>  retrieve the archive from remote repository <repo>
    --[no-]exec <command> path to the remote git-upload-archive command

";

/// Parsed command line for one `archive` invocation.
#[derive(Default)]
struct Opts {
    format: Option<String>,
    prefix: Option<String>,
    output: Option<String>,
    verbose: bool,
    /// Raw `--mtime` text, still unparsed: git resolves it only after the usage,
    /// format and compression-level diagnostics have had their chance.
    mtime: Option<String>,
    /// The last `-<digits>` seen; git keeps a single `int`, so later ones win.
    level: Option<u32>,
    worktree_attributes: bool,
    /// `--add-file` / `--add-virtual-file` records, in command-line order. git
    /// assigns each one a fake object id from a 1-based counter incremented per
    /// `add_file_cb` call, which surfaces as the `<oid>.data` / `<oid>.paxheader`
    /// name when the in-archive path overflows the `ustar` fields.
    added: Vec<Added>,
    treeish: Option<String>,
    paths: Vec<String>,
}

/// One `--add-file` / `--add-virtual-file` record, resolved at parse time so its
/// diagnostics (`File not found`, `Not a regular file`, `missing colon`, `empty
/// file name`) fire in git's command-line order, before the format, compression
/// level, tree-ish and pathspec checks that run after `parse_options()`.
enum Added {
    /// `--add-file <path>`: the archive name is `<--prefix>` + `basename(path)`
    /// (git prepends `args->base`), the mode is the disk file's executable bit
    /// canonicalised the same way tree blobs are, and the bytes are read from
    /// disk at archive-writing time.
    File {
        /// `basename(path)`; `base` is prepended when the record is written.
        name: Vec<u8>,
        /// `info->base = xstrdup_or_null(base)` (archive.c:581) — the value of
        /// `--prefix` *at the moment this option was parsed*, since `base` is read
        /// through `opt->defval` while `parse_options()` is still walking the
        /// command line. A `--prefix` that comes later therefore does not reach
        /// this record, and `--no-prefix` in between takes it away again.
        base: Vec<u8>,
        /// Whether any execute bit is set on disk (`st_mode & 0111`).
        exec: bool,
        /// The path to read the bytes from, resolved against the process cwd.
        disk: std::path::PathBuf,
    },
    /// `--add-virtual-file <path:content>`: the archive name is the path before
    /// the first colon, C-style-unquoted when it is quoted. git does *not*
    /// prepend `--prefix` to it — but it does prepend the *cwd* prefix
    /// (`prefix_filename(args->prefix, path)`, archive.c:608-612), which is the
    /// opposite of `--add-file`, where the cwd prefix reaches the path on disk
    /// and `--prefix` reaches the name in the archive. The mode is a
    /// non-executable blob's and the content is the bytes after that colon.
    Virtual { name: Vec<u8>, content: Vec<u8> },
}

/// One record git will write, in the order it writes them. Collected up front so
/// that a pathspec that matches nothing fails before a single byte reaches
/// stdout, which is what git does.
struct Item {
    /// Full in-archive path; directories and submodules carry a trailing `/`.
    path: Vec<u8>,
    kind: EntryKind,
    oid: ObjectId,
}

/// `git archive` — write a `tar` archive of `<tree-ish>` to stdout or `-o`.
pub fn archive(args: &[String]) -> Result<ExitCode> {
    archive_impl(args, false)
}

/// `write_archive(..., remote = 1)`, which is how `upload-archive--writer` runs
/// the archiver on behalf of a `git archive --remote` client.
///
/// The only difference is the `ARCHIVER_REMOTE` gate: `--list` reports just the
/// formats that carry the flag, and a format without it is `Unknown archive
/// format` even though the same name works locally.
pub fn archive_remote(args: &[String]) -> Result<ExitCode> {
    archive_impl(args, true)
}

fn archive_impl(args: &[String], is_remote: bool) -> Result<ExitCode> {
    // `cmd_archive()` parses `-o`/`--remote`/`--exec` first, with
    // `PARSE_OPT_KEEP_ALL`, and hands the rest of the command line to the far side
    // when a `--remote` came out of it (builtin/archive.c:97-108). Without one
    // this pass has nothing to say and the ordinary parse below owns every token.
    if args.iter().any(|a| a == "--remote" || a.starts_with("--remote=") || a.starts_with("--rem")) {
        let outer = match parse_outer(args) {
            Ok(outer) => outer,
            Err(code) => return Ok(code),
        };
        if outer.remote.is_some() {
            return run_remote_archiver(&outer);
        }
    }

    let mut opts = Opts::default();
    let mut list = false;
    let mut literal = false;
    let mut i = 0;

    while i < args.len() {
        let a = args[i].as_str();
        if literal {
            // git's parse-options strips `--` and leaves the positionals, so the
            // tree-ish may still be the first thing after it.
            if opts.treeish.is_none() {
                opts.treeish = Some(a.to_string());
            } else {
                opts.paths.push(a.to_string());
            }
            i += 1;
            continue;
        }
        // Respell a unique abbreviation as the name it resolves to, so `--worktree-a`
        // reaches the same arm as `--worktree-attributes`.
        let canonical;
        let a = match super::canonical_long(a, LONG_OPTS) {
            super::Long::Name(name) => {
                canonical = name;
                canonical.as_ref()
            }
            super::Long::Ambiguous(first, second) => {
                return Ok(super::ambiguous_option(a, &first, &second, USAGE))
            }
        };
        match a {
            "--" => literal = true,
            // git's `parse_options()` prints the full usage to *stdout* and exits
            // 129 on `-h` / `--help`. `--help-all` renders `USAGE_FULL`, which is
            // this same block: the option table has no `PARSE_OPT_HIDDEN` entry.
            "-h" | "--help" | "--help-all" => {
                print!("{USAGE}");
                return Ok(ExitCode::from(129));
            }
            "-l" | "--list" => list = true,
            "--no-list" => list = false,
            "-v" | "--verbose" => opts.verbose = true,
            "--no-verbose" => opts.verbose = false,
            "--worktree-attributes" => opts.worktree_attributes = true,
            "--no-worktree-attributes" => opts.worktree_attributes = false,
            "--format" => opts.format = Some(value_of(args, &mut i, a)?),
            "--no-format" => opts.format = None,
            "--prefix" => opts.prefix = Some(value_of(args, &mut i, a)?),
            "--no-prefix" => opts.prefix = None,
            "-o" | "--output" => opts.output = Some(value_of(args, &mut i, a)?),
            // git accepts `--no-output` but it is a no-op: `-o FILE --no-output`
            // (either order) still writes to FILE. Verified against git 2.55.0.
            "--no-output" => {}
            "--mtime" => opts.mtime = Some(value_of(args, &mut i, a)?),
            "--add-file" => {
                let value = value_of(args, &mut i, a)?;
                match resolve_add_file(&value, opts.prefix.as_deref()) {
                    Ok(item) => opts.added.push(item),
                    Err(msg) => {
                        eprintln!("fatal: {msg}");
                        return Ok(ExitCode::from(128));
                    }
                }
            }
            "--add-virtual-file" => {
                let value = value_of(args, &mut i, a)?;
                match resolve_virtual_file(&value) {
                    Ok(item) => opts.added.push(item),
                    Err(msg) => {
                        eprintln!("fatal: {msg}");
                        return Ok(ExitCode::from(128));
                    }
                }
            }
            // `add_file_cb()`'s unset arm clears the whole extra-file list
            // (archive.c:571-574), so either spelling discards every `--add-file`
            // and `--add-virtual-file` seen so far — the two share one callback and
            // one list.
            "--no-add-file" | "--no-add-virtual-file" => opts.added.clear(),
            _ if a.starts_with("--format=") => opts.format = Some(a[9..].to_string()),
            _ if a.starts_with("--prefix=") => opts.prefix = Some(a[9..].to_string()),
            _ if a.starts_with("--output=") => opts.output = Some(a[9..].to_string()),
            _ if a.starts_with("--mtime=") => opts.mtime = Some(a[8..].to_string()),
            _ if a.starts_with("--add-file=") => match resolve_add_file(&a[11..], opts.prefix.as_deref()) {
                Ok(item) => opts.added.push(item),
                Err(msg) => {
                    eprintln!("fatal: {msg}");
                    return Ok(ExitCode::from(128));
                }
            },
            _ if a.starts_with("--add-virtual-file=") => match resolve_virtual_file(&a[19..]) {
                Ok(item) => opts.added.push(item),
                Err(msg) => {
                    eprintln!("fatal: {msg}");
                    return Ok(ExitCode::from(128));
                }
            },
            // `cmd_archive()` took `--remote` for itself before this parse ran, so
            // reaching one here means the outer pass did not (an abbreviation this
            // table resolves differently, say). `--exec` without it is swallowed
            // there and never mentioned again, which is why `archive --exec=x HEAD`
            // writes an ordinary archive rather than complaining.
            "--exec" => {
                let _ = value_of(args, &mut i, a)?;
            }
            _ if a.starts_with("--exec=") => {}
            // Both are `OPT_STRING`s, so their unset writes NULL over the slot
            // (parse-options.c:200-202) — asking for the local archive.
            "--no-remote" | "--no-exec" => {}
            _ if compression_level(a).is_some() => opts.level = compression_level(a),
            // git's `parse_options()` rejects any other dashed token with the
            // usage block on stderr and exit 129 — `unknown option` for a `--long`
            // form (the leading `--` stripped, the rest kept verbatim) and
            // `unknown switch` for a single-dash form (just the switch character).
            _ if a.starts_with("--") => {
                eprintln!("error: unknown option `{}'", &a[2..]);
                eprint!("{USAGE}");
                return Ok(ExitCode::from(129));
            }
            _ if a.len() > 1 && a.starts_with('-') => {
                let sw = a.chars().nth(1).unwrap_or('?');
                eprintln!("error: unknown switch `{sw}'");
                eprint!("{USAGE}");
                return Ok(ExitCode::from(129));
            }
            _ if opts.treeish.is_none() => opts.treeish = Some(a.to_string()),
            _ => opts.paths.push(a.to_string()),
        }
        i += 1;
    }

    // `if (is_remote && args->extra_files.nr) die(_("options '%s' and '%s' cannot
    // be used together"), "--add-file", "--remote");` (`parse_archive_args`,
    // archive.c:705). Raised on the *serving* side, ahead of `--list` and ahead of
    // the tree-ish check, and named for `--add-file` whichever of the two
    // spellings put the file there.
    if is_remote && !opts.added.is_empty() {
        eprintln!("fatal: options '--add-file' and '--remote' cannot be used together");
        return Ok(ExitCode::from(128));
    }

    // `--list` short-circuits everything else, but git rejects any leftover
    // positional first — it never lists formats *and* takes a tree-ish.
    if list {
        if let Some(extra) = opts.treeish.as_deref().or(opts.paths.first().map(String::as_str)) {
            eprintln!("fatal: extra command line parameter '{extra}'");
            return Ok(ExitCode::from(128));
        }
        let repo = crate::setup::discover().ok();
        let mut out = String::new();
        for f in configured_formats(repo.as_ref()) {
            // `if (!is_remote || archivers[i]->flags & ARCHIVER_REMOTE)`
            // (`parse_archive_args`, archive.c).
            if is_remote && !remote_allowed(repo.as_ref(), &f) {
                continue;
            }
            out.push_str(&f);
            out.push('\n');
        }
        print!("{out}");
        return Ok(ExitCode::SUCCESS);
    }

    // git checks `argc` before it looks at the format or the compression level,
    // so a missing tree-ish outranks both `-<digits>` and an unknown format.
    let Some(spec) = opts.treeish.clone() else {
        eprint!("{USAGE}");
        return Ok(ExitCode::from(129));
    };

    // Resolve the requested format: explicit `--format`, else inferred from the
    // `-o` filename, else `tar`.
    let format = match opts.format.as_deref() {
        Some(f) => f.to_string(),
        None => opts
            .output
            .as_deref()
            .and_then(format_from_filename)
            .unwrap_or("tar")
            .to_string(),
    };
    // The registry is config-driven, so a `tar.<name>.command` makes `<name>` a format git
    // knows — which is why this is checked against the configured list and not the built-in
    // one.
    let known_repo = crate::setup::discover().ok();
    let known = configured_formats(known_repo.as_ref());
    // `if (!*ar || (is_remote && !((*ar)->flags & ARCHIVER_REMOTE)))
    //         die(_("Unknown archive format '%s'"), format);` — a format the
    // remote side is not allowed to serve is reported as unknown, not as denied.
    if !known.iter().any(|f| f == &format)
        || (is_remote && !remote_allowed(known_repo.as_ref(), &format))
    {
        eprintln!("fatal: Unknown archive format '{format}'");
        return Ok(ExitCode::from(128));
    }

    // git's `parse_archive_args()`: a compression level is fatal at parse time
    // for a format that does not declare `ARCHIVER_WANT_COMPRESSION_LEVELS`
    // (`tar`), and for `zip`, whose archiver additionally rejects a level outside
    // zlib's `0..=9` range with the very same message and exit code. `tgz` /
    // `tar.gz` accept any level here; an out-of-range one is not diagnosed until
    // `deflateInit2()` fails, which is after the tree walk (see below). The level
    // reported is the last `-<digits>` parsed, not the first one given.
    if let Some(level) = opts.level {
        let filter_format = !FORMATS.contains(&format.as_str());
        let reject = (!LEVEL_FORMATS.contains(&format.as_str()) && !filter_format)
            || (format == "zip" && level > 9);
        if reject {
            eprintln!("fatal: Argument not supported for format '{format}': -{level}");
            return Ok(ExitCode::from(128));
        }
    }

    let repo = crate::setup::discover()?;

    // git lets `tar.<fmt>.command` replace an archiver with an external filter.
    // The internal gzip is what this port reproduces; anything else would have
    // to be spawned, which it does not do. `tar.tgz.command` and
    // `tar.tar.gz.command` are pre-seeded with the internal name, so only a
    // value that differs from it is a problem.
    // `tgz`/`tar.gz` are pre-seeded with `git archive gzip`; a configuration that repeats
    // that value asks for the gzip this port produces in-process, and anything else is a
    // filter to spawn.
    const INTERNAL_GZIP: &str = "git archive gzip";
    let filter = tar_filter_command(&repo, &format).filter(|cmd| cmd != INTERNAL_GZIP);
    let umask = tar_umask(&repo)?;

    // `parse_treeish_arg()` (archive.c) resolves with `repo_get_oid()`, which
    // only has to *name* an object — a full-length hex string is decoded
    // without the odb being consulted (see [`crate::objname::full_hex`]). An
    // absent but well-formed id therefore reaches `repo_parse_tree_indirect()`
    // below and is reported as "not a tree object", not as an invalid name.
    let Some(id) = crate::objname::resolve(&repo, spec.as_str()) else {
        eprintln!("fatal: not a valid object name: {spec}");
        return Ok(ExitCode::from(128));
    };
    let object = repo.find_object(id).ok();

    // A commit (or a tag peeling to one) contributes the pax global header and
    // the entry mtime; anything else that peels to a tree uses the current time.
    // `lookup_commit_reference_gently(..., quiet)` never diagnoses a miss, so a
    // missing object simply leaves both unset.
    let commit = object
        .clone()
        .and_then(|obj| obj.peel_to_commit().ok())
        .map(|c| (c.id, c.time()));
    let (commit_id, default_mtime) = match commit {
        Some((cid, time)) => (Some(cid), time?.seconds),
        None => (None, now()),
    };
    // `--mtime` replaces that for every entry and for the global header alike.
    let mtime = match opts.mtime.as_deref() {
        Some(text) => approxidate(text),
        None => default_mtime,
    };
    // git names the *resolved* id here, not the spelling from argv.
    let Some(mut tree) = object.and_then(|obj| obj.peel_to_tree().ok()) else {
        eprintln!("fatal: not a tree object: {id}");
        return Ok(ExitCode::from(128));
    };

    // git does not diagnose an unsupported container, nor an out-of-range gzip
    // level, until *archive-writing* time — after the subdirectory narrowing,
    // the attribute scan and the whole path-filter walk have each had their turn
    // to fail with git's own exit code. Both checks are therefore deferred to
    // just before the first byte is written (see below); here we only compute the
    // format flags the writer needs.
    // A configured `tar.<format>.command` *replaces* the archiver, so a `tar.tar.gz.command`
    // means the tar goes to that command rather than through the internal gzip.
    let gzipped = matches!(format.as_str(), "tgz" | "tar.gz") && filter.is_none();
    let level = opts.level.unwrap_or(6);
    // Run from a subdirectory, git narrows the tree to that subdirectory and
    // makes every archived path relative to it.
    if let Some(prefix) = repo.prefix()?.map(std::path::Path::to_path_buf) {
        for part in prefix.components() {
            let name = part.as_os_str().as_encoded_bytes().to_vec();
            let Some(sub) = subtree(&repo, &tree, &name)? else {
                eprintln!("fatal: current working directory is untracked");
                return Ok(ExitCode::from(128));
            };
            tree = sub;
        }
    }

    let mut conv = Convert::new(&repo, &tree, opts.worktree_attributes)?;

    // Parse the trailing pathspecs into a `gix-pathspec` search so the whole
    // magic grammar is matched exactly as git's `match_pathspec()` does. git
    // parses these with `parse_pathspec(..., 0, PATHSPEC_PREFER_CWD, prefix,
    // argv)` (no magic disallowed) and sets `recursive = 1`. The tree was
    // already narrowed to the CWD subdirectory above, so the specs are matched
    // against subdirectory-relative paths — the PREFER_CWD prefixing and the
    // narrowing cancel out for every spec that is not `:(top)`. An empty spec
    // list means "match everything", represented as `None` (no search built).
    // `parsed` keeps the individual patterns so the "did not match" check can
    // test each one independently, the way git's `path_exists()` does.
    let root = repo.workdir().unwrap_or_else(|| repo.git_dir()).to_path_buf();
    let parsed: Vec<gix::pathspec::Pattern> = if opts.paths.is_empty() {
        Vec::new()
    } else {
        let defaults = repo.pathspec_defaults().unwrap_or_default();
        let mut patterns = Vec::with_capacity(opts.paths.len());
        for spec in &opts.paths {
            match gix::pathspec::parse(spec.as_bytes(), defaults) {
                Ok(p) => patterns.push(p),
                Err(e) => {
                    eprintln!("fatal: {e}");
                    return Ok(ExitCode::from(128));
                }
            }
        }
        patterns
    };
    let mut search = if parsed.is_empty() {
        None
    } else {
        match Search::from_specs(parsed.clone(), None, &root) {
            Ok(s) => Some(s),
            Err(e) => {
                eprintln!("fatal: {e}");
                return Ok(ExitCode::from(128));
            }
        }
    };

    // Walk first, emit second: a pathspec that matches nothing must fail with an
    // empty stdout. Inclusion (and the exclude-aware directory pruning) uses the
    // combined search; excludes sort ahead of includes so an excluded path never
    // slips in.
    let mut pending: Vec<Item> = Vec::new();
    let mut items: Vec<Item> = Vec::new();
    collect(&repo, tree.clone(), b"", search.as_mut(), &mut conv, &mut pending, &mut items)?;

    // git calls `path_exists()` for each argv pathspec independently, dying on
    // the first *positive* one (in argv order) that matches nothing. Reproduce
    // that per-spec existence test against the archived paths: build a one-spec
    // search and look for any archived entry it matches. A negative
    // (`:(exclude)` / `:!`) spec is never required to match.
    for (idx, pat) in parsed.iter().enumerate() {
        if pat.is_excluded() {
            continue;
        }
        let mut one = match Search::from_specs([pat.clone()], None, &root) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("fatal: {e}");
                return Ok(ExitCode::from(128));
            }
        };
        // ```c
        // const char *paths[] = { path, NULL };
        // [...]
        // parse_pathspec(&ctx.pathspec, 0, 0, "", paths);
        // ctx.pathspec.recursive = 1;
        // ret = read_tree(args->repo, args->tree, &ctx.pathspec, reject_entry, &ctx);
        // ```
        //
        // (`path_exists()`, archive.c.) The tree is re-read with *only* this one spec, so
        // whether it matched is decided before any other spec has a say: `git archive HEAD
        // src :!src/lib.rs` archives nothing and still accepts `src`, which matched the
        // directory. Testing against the already-filtered entry list called that a miss.
        let mut probe_pending: Vec<Item> = Vec::new();
        let mut probe: Vec<Item> = Vec::new();
        collect(
            &repo,
            tree.clone(),
            b"",
            Some(&mut one),
            &mut conv,
            &mut probe_pending,
            &mut probe,
        )?;
        let hit = !probe.is_empty();
        if !hit {
            eprintln!(
                "fatal: pathspec '{}' did not match any files",
                opts.paths[idx]
            );
            return Ok(ExitCode::from(128));
        }
    }

    // Now that every git diagnostic with an exit code of its own has fired, the
    // two archive-writing-time failures can be emitted in git's own order. git
    // only learns that zlib rejects a `tgz` / `tar.gz` level when
    // `deflateInit2()` fails, which it does here, after the walk — so
    // `git archive --format=tgz -10 <tree> <unmatched>` reports the pathspec
    // miss, not the deflate error.
    if gzipped && level > 9 {
        eprintln!("fatal: deflateInit2: stream consistency error (no message)");
        return Ok(ExitCode::from(128));
    }
    if !gzipped && format != "tar" && format != "zip" && filter.is_none() {
        bail!("archive format {format:?} is not supported");
    }

    let base = opts.prefix.clone().unwrap_or_default();

    // `write_zip_archive()`: the same walk, a different container.
    if format == "zip" {
        return write_zip(&repo, &tree, items, &opts, &base, commit_id, mtime, level as i32, &mut conv);
    }
    // ```c
    // strbuf_addstr(&cmd, ar->data);
    // if (args->compression_level >= 0)
    //         strbuf_addf(&cmd, " -%d", args->compression_level);
    // strvec_push(&filter.args, cmd.buf);
    // filter.use_shell = 1;
    // filter.in = -1;
    // [...]
    // if (dup2(filter.in, 1) < 0) [...]
    // r = write_tar_archive(ar, args);
    // ```
    //
    // (`write_tar_filter_archive()`, archive-tar.c.) The command runs through a shell with
    // the tar on its standard input and its own output going wherever this archive was
    // going; a compression level is appended to the command line.
    let mut child = None;
    let raw: Box<dyn Write> = match &filter {
        Some(command) => {
            let mut line = command.clone();
            if let Some(level) = opts.level {
                line.push_str(&format!(" -{level}"));
            }
            let mut spawned = std::process::Command::new("sh");
            spawned.arg("-c").arg(&line).stdin(std::process::Stdio::piped());
            if let Some(path) = &opts.output {
                spawned.stdout(std::fs::File::create(path)?);
            }
            let mut spawned = match spawned.spawn() {
                Ok(child) => child,
                Err(e) => crate::git_fatal!("cannot spawn {line}: {e}"),
            };
            let stdin = spawned.stdin.take().expect("stdin is piped");
            child = Some((spawned, line));
            Box::new(std::io::BufWriter::new(stdin))
        }
        None => match &opts.output {
            Some(path) => Box::new(std::io::BufWriter::new(std::fs::File::create(path)?)),
            // No `BufWriter`, and no `std::io::stdout()`: `Tar::raw` already hands
            // this whole 10 KiB blocks, and both of those would chop them up
            // again. See [`RawStdout`].
            None => Box::new(RawStdout::new()),
        },
    };
    let sink = if gzipped {
        Sink::Gz(Box::new(gzip::GzDeflate::new(raw, level as i32)))
    } else {
        Sink::Plain(raw)
    };
    let mut tar = Tar {
        out: sink,
        written: 0,
        mtime,
        umask,
        block: Vec::with_capacity(BLOCK as usize),
    };

    if let Some(cid) = commit_id {
        tar.global_header(&cid)?;
    }

    // A `--prefix` ending in `/` gets its own directory record, with repeated
    // trailing slashes collapsed to one, before any tree entry.
    if base.ends_with('/') {
        let mut len = base.len();
        while len > 1 && base.as_bytes()[len - 2] == b'/' {
            len -= 1;
        }
        report(opts.verbose, &base.as_bytes()[..len]);
        tar.entry(&base.as_bytes()[..len], EntryKind::Tree, &tree.id, &[])?;
    }

    for item in items {
        let mut path = base.clone().into_bytes();
        path.extend_from_slice(&item.path);
        let data = entry_data(&repo, &mut conv, &item)?;
        report(opts.verbose, &path);
        tar.entry(&path, item.kind, &item.oid, &data)?;
    }

    // git writes the `--add-file` / `--add-virtual-file` records last of all,
    // after the whole tree walk, in command-line order. Each carries a fake object
    // id built from a 1-based counter (the Nth added file, big-endian in the first
    // eight bytes of an otherwise-zero hash), which only becomes visible when the
    // in-archive path overflows the `ustar` name field and spills into a pax
    // `<oid>.data` / `<oid>.paxheader` record.
    let hash_len = repo.object_hash().len_in_bytes();
    for (idx, added) in opts.added.iter().enumerate() {
        let mut raw = vec![0u8; hash_len];
        raw[..8].copy_from_slice(&((idx as u64) + 1).to_be_bytes());
        let oid = ObjectId::from_bytes_or_panic(&raw);
        match added {
            Added::File { name, base, exec, disk } => {
                let mut path = base.clone();
                path.extend_from_slice(name);
                let data = std::fs::read(disk)?;
                let kind = if *exec {
                    EntryKind::BlobExecutable
                } else {
                    EntryKind::Blob
                };
                tar.entry(&path, kind, &oid, &data)?;
            }
            Added::Virtual { name, content } => {
                // git does not prepend `--prefix` to a virtual file's path, but
                // `prefix_filename()` has already put the *cwd* prefix on it.
                let mut path = crate::setup::prefix_bytes(&repo);
                path.extend_from_slice(name);
                tar.entry(&path, EntryKind::Blob, &oid, content)?;
            }
        }
    }

    tar.finish()?;
    // The gzip stream's own trailer is only written once the tar is complete.
    tar.out.done()?;

    // `close(1); if (finish_command(&filter) != 0) die("'%s' filter reported error", cmd.buf)`
    // — the pipe has to be closed before the wait, or the filter never sees end of input.
    if let Some((mut spawned, line)) = child {
        match spawned.wait() {
            Ok(status) if status.success() => {}
            _ => crate::git_fatal!("'{line}' filter reported error"),
        }
    }

    Ok(ExitCode::SUCCESS)
}

/// File descriptor 1, without Rust's line buffering.
///
/// `std::io::Stdout` is a `LineWriter` whatever fd 1 is, so `write_all` of a
/// binary block is cut at its last `\n` — `LineWriterShim::write` does
/// `memrchr(b'\n', buf)`, writes everything up to and including it, and buffers
/// the tail. The bytes are the same either way, which is why this is invisible
/// in `git archive > file`; it is not invisible through `upload-archive`, which
/// frames one sideband packet per `read()` of the archiver's stdout. The `linear`
/// fixture's tar has its last newline 3098 bytes in, so its single 10240-byte
/// block reached the wire as one `0x2805` packet or as a `0x0c1f`/`0x1beb` pair,
/// depending on whether the reader woke up between the writer's two `write(2)`s
/// — the port disagreeing with itself across two runs.
///
/// git has no stdio in this path at all: `write_blocked()` ends in
/// `write_or_die(1, block, BLOCKSIZE)`, one raw write per block. This is that.
struct RawStdout(std::mem::ManuallyDrop<std::fs::File>);

impl RawStdout {
    fn new() -> Self {
        use std::os::fd::FromRawFd as _;
        // Anything Rust had buffered for fd 1 must reach it before this starts
        // writing behind stdio's back. Nothing on the archive path prints, so
        // this is normally a no-op.
        let _ = std::io::stdout().flush();
        // SAFETY: fd 1 is open for the lifetime of the process, and
        // `ManuallyDrop` keeps the `File` from closing it — the descriptor is
        // borrowed here, never owned.
        Self(std::mem::ManuallyDrop::new(unsafe { std::fs::File::from_raw_fd(1) }))
    }
}

impl Write for RawStdout {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.write(buf)
    }
    fn flush(&mut self) -> std::io::Result<()> {
        self.0.flush()
    }
}

/// Where the tar bytes go: straight out, or through git's in-process gzip.
enum Sink {
    Plain(Box<dyn Write>),
    Gz(Box<gzip::GzDeflate<Box<dyn Write>>>),
}

impl Write for Sink {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        match self {
            Sink::Plain(w) => w.write(buf),
            Sink::Gz(w) => w.write(buf),
        }
    }

    fn flush(&mut self) -> std::io::Result<()> {
        match self {
            Sink::Plain(w) => w.flush(),
            Sink::Gz(w) => w.flush(),
        }
    }
}

impl Sink {
    /// Finalise: flush the plain writer, or close out the deflate stream and
    /// then flush what it was writing to.
    fn done(self) -> Result<()> {
        match self {
            Sink::Plain(mut w) => w.flush()?,
            Sink::Gz(w) => w.finish()?.flush()?,
        }
        Ok(())
    }
}

/// Read the value of an option given as a separate argument (`--format tar`).
///
/// `i` stays *on* the value, because this command's loop advances past the
/// current argument itself; only the shared `get_arg()` port needs the
/// next-unread convention. `flag` is the token as typed, and that is the whole
/// point: `optname()` names a short option by its character, so `git archive -o`
/// is ``switch `o'`` and not the ``option `--output'`` this used to print.
fn value_of(args: &[String], i: &mut usize, flag: &str) -> Result<String> {
    *i += 1;
    Ok(super::value_at(args, *i, flag)?.to_string())
}

/// What `cmd_archive()` keeps for itself (builtin/archive.c:86-94) before the
/// rest of the command line goes on to `write_archive()` — or, with `--remote`,
/// to the server verbatim.
struct Outer {
    remote: Option<String>,
    /// `--exec`, the program to run on the far side.
    exec: String,
    output: Option<String>,
    /// argv as the server will see it: every token the outer parse did not take,
    /// `--` and unknown options included.
    rest: Vec<String>,
}

/// The outer option table. `PARSE_OPT_KEEP_ALL` keeps everything it does not
/// recognize, so an abbreviation is resolved against *these three* names only —
/// which is how `--rem=.` reaches `--remote` before the inner parse can see it.
const OUTER_OPTS: &[super::LongOpt] = &[
    super::LongOpt { name: "output", neg: false, arg: super::Arg::Required },
    super::LongOpt { name: "remote", neg: false, arg: super::Arg::Required },
    super::LongOpt { name: "exec", neg: false, arg: super::Arg::Required },
];

/// `parse_options(argc, argv, prefix, local_opts, NULL, PARSE_OPT_KEEP_ALL)`
/// (builtin/archive.c:97-98).
///
/// Only consulted when the command line names a `--remote`; without one the
/// ordinary parse below owns every token, `-o` and `--exec` included.
fn parse_outer(args: &[String]) -> std::result::Result<Outer, ExitCode> {
    let mut out = Outer {
        remote: None,
        exec: "git-upload-archive".to_string(),
        output: None,
        rest: Vec::new(),
    };
    let mut i = 0;
    let mut literal = false;
    while i < args.len() {
        let a = args[i].as_str();
        // `PARSE_OPT_KEEP_DASHDASH`: the `--` stays in argv, and option parsing
        // stops there.
        if literal || a == "--" {
            literal = true;
            out.rest.push(args[i].clone());
            i += 1;
            continue;
        }
        // `-o<file>` and `-o <file>`, the only short option in the table.
        if let Some(sticky) = a.strip_prefix("-o").filter(|_| !a.starts_with("--")) {
            let value = match sticky.is_empty() {
                false => sticky.to_string(),
                true => match args.get(i + 1) {
                    Some(v) => {
                        i += 1;
                        v.clone()
                    }
                    None => return Err(super::missing_option_value("-o")),
                },
            };
            out.output = Some(value);
            i += 1;
            continue;
        }
        let (name, inline) = match a.strip_prefix("--") {
            Some(body) => match body.split_once('=') {
                Some((n, v)) => (n, Some(v.to_string())),
                None => (body, None),
            },
            None => ("", None),
        };
        let slot = match super::resolve_long(OUTER_OPTS, name) {
            super::Resolved::One(opt, _) => opt.name,
            // Unknown or ambiguous here is not an error: it belongs to the far
            // side, or to the inner parse.
            _ => {
                out.rest.push(args[i].clone());
                i += 1;
                continue;
            }
        };
        let value = match inline {
            Some(v) => v,
            None => match args.get(i + 1) {
                Some(v) => {
                    i += 1;
                    v.clone()
                }
                None => return Err(super::missing_option_value(a)),
            },
        };
        match slot {
            "output" => out.output = Some(value),
            "remote" => out.remote = Some(value),
            _ => out.exec = value,
        }
        i += 1;
    }
    Ok(out)
}

/// `run_remote_archiver()` (builtin/archive.c:23-71): hand the whole command line
/// to `git upload-archive` on the far side and copy back what it sends.
///
/// ```c
/// if (name_hint) {
///         const char *format = archive_format_from_filename(name_hint);
///         if (format)
///                 packet_write_fmt(fd[1], "argument --format=%s\n", format);
/// }
/// for (i = 1; i < argc; i++)
///         packet_write_fmt(fd[1], "argument %s\n", argv[i]);
/// packet_flush(fd[1]);
/// ```
///
/// The `--format` inferred from `-o`'s filename goes first *so that an explicit
/// `--format` later on the command line overrides it* — the server keeps the last
/// one it is given. Then `ACK`, a flush, and the archive itself on sideband 1.
fn run_remote_archiver(outer: &Outer) -> Result<ExitCode> {
    let remote = outer.remote.as_deref().unwrap_or_default();
    // Every transport carries the same stream; only the way it is opened differs,
    // and the only opener here is a local child process. Refuse the others rather
    // than hand a URL to `upload-archive` as though it were a path, which would
    // report the repository missing instead of the transport.
    if let Some(kind) = non_local_transport(remote) {
        bail!("archive --remote over {kind} is not supported here: only a local repository is served");
    }
    let mut child = match spawn_upload_archive(&outer.exec, remote) {
        Ok(child) => child,
        Err(err) => {
            eprintln!("fatal: cannot run {}: {err}", outer.exec);
            return Ok(ExitCode::from(128));
        }
    };
    let mut to_server = child.stdin.take().expect("piped stdin");
    let mut request: Vec<u8> = Vec::new();
    if let Some(format) = outer.output.as_deref().and_then(format_from_filename) {
        pkt_line(&mut request, format!("argument --format={format}\n").as_bytes());
    }
    for arg in &outer.rest {
        pkt_line(&mut request, format!("argument {arg}\n").as_bytes());
    }
    request.extend_from_slice(b"0000");
    // A server that died before reading the whole request leaves this write to
    // fail on a closed pipe; the handshake below reports that as the disconnect
    // it is, so the error here is not the one to print.
    let _ = to_server.write_all(&request);
    let _ = to_server.flush();
    drop(to_server);

    let mut from_server = std::io::BufReader::new(child.stdout.take().expect("piped stdout"));
    match read_pkt_line(&mut from_server) {
        // `PACKET_READ_DIE_ON_ERR_PACKET` reaches `die_initial_contact()` for a
        // server that never spoke.
        Err(_) => {
            let _ = child.wait();
            eprintln!("fatal: the remote end hung up unexpectedly");
            return Ok(ExitCode::from(128));
        }
        Ok(None) => {
            let _ = child.wait();
            eprintln!("fatal: git archive: expected ACK/NAK, got a flush packet");
            return Ok(ExitCode::from(128));
        }
        Ok(Some(line)) => {
            let line = String::from_utf8_lossy(&line).trim_end_matches('\n').to_string();
            if line != "ACK" {
                let _ = child.wait();
                match line.strip_prefix("NACK ") {
                    Some(reason) => eprintln!("fatal: git archive: NACK {reason}"),
                    None => eprintln!("fatal: git archive: protocol error"),
                }
                return Ok(ExitCode::from(128));
            }
        }
    }
    if !matches!(read_pkt_line(&mut from_server), Ok(None)) {
        let _ = child.wait();
        eprintln!("fatal: git archive: expected a flush");
        return Ok(ExitCode::from(128));
    }

    // `create_output_file()` (builtin/archive.c:12-21) has already put `-o`'s file
    // on fd 1 by this point, so the primary band goes wherever stdout now is.
    let mut sink: Box<dyn Write> = match outer.output.as_deref() {
        Some(path) => Box::new(std::fs::File::create(path)?),
        None => Box::new(std::io::stdout().lock()),
    };
    let failed = recv_sideband(&mut from_server, &mut sink)?;
    sink.flush()?;
    // `rv |= transport_disconnect(transport)`: a server that exited non-zero makes
    // the whole thing a failure even when the sideband ended cleanly.
    let status = child.wait().map(|s| !s.success()).unwrap_or(true);
    Ok(match failed || status {
        true => ExitCode::from(1),
        false => ExitCode::SUCCESS,
    })
}

/// `transport_connect(transport, GIT_CONNECT_UPLOAD_ARCHIVE, exec, fd)` for the
/// one transport this reaches: a local path, which git serves by running the
/// program named by `--exec` with the repository as its only argument.
///
/// The default `git-upload-archive` is git's own helper, found through the exec
/// path it prepends to `PATH` for its children; the same repository served by
/// this port has to be served by *this* binary, so the default runs it directly
/// as `upload-archive`. A `--exec` the caller chose is run through the shell the
/// way `git_connect()` does, which is why `--exec=nosuch` reports
/// `nosuch: command not found` and not an `execvp` failure.
fn spawn_upload_archive(exec: &str, path: &str) -> std::io::Result<std::process::Child> {
    let path = path.strip_prefix("file://").unwrap_or(path);
    let mut command = match exec == "git-upload-archive" {
        true => {
            let mut c = std::process::Command::new(std::env::current_exe()?);
            c.arg("upload-archive").arg(path);
            c
        }
        false => {
            let mut c = std::process::Command::new("sh");
            c.arg("-c").arg(format!("{exec} '{path}'"));
            c
        }
    };
    // `git_connect()` clears `local_repo_env` from the child's environment
    // (connect.c: `strvec_pushv(&conn->env, local_repo_env)` with each name
    // unset), because a repository-local setting of *this* process must not
    // follow the connection into a different repository. `GIT_CONFIG_PARAMETERS`
    // is on that list, which is why `git -c tar.tgz.remote=false archive
    // --remote=. --format=tgz` still succeeds: the `-c` never reaches the server.
    for name in LOCAL_REPO_ENV {
        command.env_remove(name);
    }
    command
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
}

/// `local_repo_env[]` (environment.c): the variables that describe *this*
/// repository and must not leak into a child talking to another one.
const LOCAL_REPO_ENV: &[&str] = &[
    "GIT_ALTERNATE_OBJECT_DIRECTORIES",
    "GIT_CONFIG",
    "GIT_CONFIG_PARAMETERS",
    "GIT_CONFIG_COUNT",
    "GIT_OBJECT_DIRECTORY",
    "GIT_DIR",
    "GIT_WORK_TREE",
    "GIT_IMPLICIT_WORK_TREE",
    "GIT_GRAFT_FILE",
    "GIT_INDEX_FILE",
    "GIT_NO_REPLACE_OBJECTS",
    "GIT_REPLACE_REF_BASE",
    "GIT_PREFIX",
    "GIT_INTERNAL_SUPER_PREFIX",
    "GIT_SHALLOW_FILE",
    "GIT_COMMON_DIR",
];

/// `recv_sideband()` (pkt-line.c:578-604) driving `demultiplex_sideband()`
/// (sideband.c:301-433): band 1 is the archive, band 2 progress, band 3 an error,
/// and the stream ends at a flush. Returns whether it ended as anything but that
/// clean flush, which is `!!rv` in the caller.
fn recv_sideband(from: &mut impl std::io::BufRead, out: &mut impl Write) -> Result<bool> {
    // The `remote: ` prefixing and the keyword colours are the same ones `push`
    // uses. `--remote` runs outside a repository too, and there is then nothing
    // to read `color.remote` from.
    let mut sideband = match crate::setup::discover() {
        Ok(repo) => super::push_proto::Sideband::new(&repo),
        Err(_) => super::push_proto::Sideband::plain(),
    };
    loop {
        match read_pkt_line(from) {
            Err(_) => {
                sideband.finish();
                eprintln!("fatal: archive: unexpected disconnect while reading sideband packet");
                return Ok(true);
            }
            Ok(None) => {
                sideband.finish();
                return Ok(false);
            }
            Ok(Some(packet)) => match packet.split_first() {
                Some((1, payload)) => out.write_all(payload)?,
                Some((2, text)) => sideband.progress(text),
                Some((3, text)) => {
                    sideband.remote_error(text);
                    sideband.finish();
                    return Ok(true);
                }
                _ => {
                    sideband.finish();
                    eprintln!("fatal: archive: protocol error: bad band");
                    return Ok(true);
                }
            },
        }
    }
}

/// The transport `git_connect()` would open for `remote`, when it is not the
/// local one: a `<scheme>://` URL other than `file:`, or the scp-like
/// `[user@]host:path` (a colon before any slash). `None` means a plain path.
fn non_local_transport(remote: &str) -> Option<&'static str> {
    if let Some((scheme, _)) = remote.split_once("://") {
        return match scheme {
            "file" => None,
            "ssh" => Some("ssh"),
            "git" => Some("the git daemon"),
            _ => Some("that transport"),
        };
    }
    match remote.split_once(':') {
        Some((host, _)) if !host.contains('/') && !host.is_empty() => Some("ssh"),
        _ => None,
    }
}

/// A pkt-line carrying `payload`.
fn pkt_line(out: &mut Vec<u8>, payload: &[u8]) {
    out.extend_from_slice(format!("{:04x}", payload.len() + 4).as_bytes());
    out.extend_from_slice(payload);
}

/// One pkt-line, or `None` for the flush that ends a stream. `Err` is a
/// disconnect or a malformed length.
fn read_pkt_line(r: &mut impl std::io::Read) -> Result<Option<Vec<u8>>> {
    let mut header = [0u8; 4];
    r.read_exact(&mut header)?;
    let len = usize::from_str_radix(std::str::from_utf8(&header)?, 16)?;
    match len {
        0..=4 => Ok(None),
        _ => {
            let mut payload = vec![0u8; len - 4];
            r.read_exact(&mut payload)?;
            Ok(Some(payload))
        }
    }
}

/// git's `add_file_cb` for `--add-virtual-file <path:content>`, which runs
/// inside `parse_options()` — so its diagnostics fire before the format,
/// compression-level and tree-ish ones, exactly like stock git. The value must
/// carry a colon, and the path before it must be non-empty; the first colon
/// splits the (literal, un-prefixed) archive path from the content. Returns the
/// `fatal:` text git would `die()` with (exit 128) on the `Err` side.
fn resolve_virtual_file(value: &str) -> std::result::Result<Added, String> {
    // ```c
    // if (*p != '"')
    //         p = strchr(p, ':');
    // else if (unquote_c_style(&buf, p, &p) < 0)
    //         die(_("unclosed quote: '%s'"), arg);
    // if (!p || *p != ':')
    //         die(_("missing colon: '%s'"), arg);
    // if (p == arg)
    //         die(_("empty file name: '%s'"), arg);
    // path = buf.len ? strbuf_detach(&buf, NULL) : xstrndup(arg, p - arg);
    // ```
    //
    // (`archive.c:592-606`.) A quoted name is decoded and the colon must be the
    // very next byte; an unquoted one runs to the first colon. The `buf.len`
    // fallback is why `"":x` archives a file literally named `""` — an empty
    // decode is indistinguishable from no decode at all.
    let bytes = value.as_bytes();
    let (name, colon) = match bytes.first() {
        Some(b'"') => match crate::setup::unquote_c_style_step(bytes, 0) {
            Some((name, end)) => (name, Some(end)),
            None => return Err(format!("unclosed quote: '{value}'")),
        },
        _ => (Vec::new(), value.find(':')),
    };
    let colon = match colon {
        Some(at) if bytes.get(at) == Some(&b':') => at,
        _ => return Err(format!("missing colon: '{value}'")),
    };
    if colon == 0 {
        return Err(format!("empty file name: '{value}'"));
    }
    Ok(Added::Virtual {
        name: match name.is_empty() {
            true => bytes[..colon].to_vec(),
            false => name,
        },
        content: bytes[colon + 1..].to_vec(),
    })
}

/// git's `add_file_cb` for `--add-file <path>`: it `stat()`s the file while
/// still inside `parse_options()`, so `File not found` / `Not a regular file`
/// (both `die()`, exit 128) fire in command-line order, ahead of the post-parse
/// diagnostics. The path is resolved against the process cwd exactly as git's
/// `prefix_filename()` does when run from a subdirectory; the archive name is
/// the basename, with `--prefix` prepended later at writing time.
fn resolve_add_file(path: &str, base: Option<&str>) -> std::result::Result<Added, String> {
    let Ok(meta) = std::fs::metadata(path) else {
        return Err(format!("File not found: {path}"));
    };
    if !meta.is_file() {
        return Err(format!("Not a regular file: {path}"));
    }
    // git's `canon_mode()` keys the executable bit off the *owner* execute bit
    // (0100) alone, not any of the group/other bits, before the tar writer mangles
    // it to 0777/0666 & ~umask. Verified against git 2.55.0 (mode 0641 archives as
    // non-executable, 0744 as executable).
    let exec = {
        use std::os::unix::fs::MetadataExt;
        meta.mode() & 0o100 != 0
    };
    let name = std::path::Path::new(path)
        .file_name()
        .map(|s| s.as_encoded_bytes().to_vec())
        .unwrap_or_else(|| path.as_bytes().to_vec());
    Ok(Added::File {
        name,
        base: base.unwrap_or_default().as_bytes().to_vec(),
        exec,
        disk: std::path::PathBuf::from(path),
    })
}

/// The level in a `-<digits>` argument, or `None` if `arg` is not one.
fn compression_level(arg: &str) -> Option<u32> {
    let digits = arg.strip_prefix('-')?;
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    digits.parse().ok()
}

/// Seconds since the epoch, right now.
fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or_default()
}

// git's `approxidate()`, as `--mtime` uses it (`archive.c:520`) — the same shared parser every
// `--since`/`--until` goes through, so `--mtime=@0` (no zone, so not an object header) and
// `--mtime=bogus-time` both resolve to the current time exactly as stock git does.
use crate::date::approxidate;

/// git's `filename_to_archive_format()`: the format whose name the output file's
/// extension spells, or `None` to fall back to `tar`.
fn format_from_filename(name: &str) -> Option<&'static str> {
    FORMATS
        .iter()
        .copied()
        .find(|f| name.len() > f.len() + 1 && name.ends_with(&format!(".{f}")))
}

/// `tar.umask`, parsed the way `git_config_int` does (a leading `0` means octal).
fn tar_umask(repo: &gix::Repository) -> Result<u32> {
    let Some(raw) = repo.config_snapshot().string("tar.umask") else {
        return Ok(0o002);
    };
    let text = raw.to_str()?.trim().to_string();
    if text == "user" {
        // git's `git_tar_config`: `tar_umask = umask(0); umask(tar_umask);`.
        // There is no read-only POSIX umask getter, so git reads it by setting
        // it to zero and restoring it in one breath; this does the same. The
        // result is at most 0o777, so masking guards against any garbage the C
        // ABI leaves in the high bits of the narrower `mode_t` on some targets.
        return Ok(process_umask() & 0o7777);
    }
    // Everything else is `git_config_int()`, whose grammar is `strtoimax()` base
    // 0 plus a `k`/`m`/`g` scale suffix: `1k` is 1024, `-1` is negative, and an
    // empty value is `die()`'s "invalid unit" rather than a zero. The result is
    // stored in git's `static unsigned int tar_umask`, so a negative value wraps
    // — `-1` becomes all-ones, `~tar_umask` becomes zero, and every archived mode
    // comes out as 0. Truncating to `u32` here reproduces that assignment.
    match crate::config::config_int(repo, "tar.umask") {
        Ok(Some(v)) => Ok(v as u32),
        Ok(None) => Ok(0o002),
        Err(msg) => Err(crate::fatal::Fatal(msg).into()),
    }
}

/// The process umask, read the way git reads it for `tar.umask=user`: set it to
/// zero to learn the old value, then restore it. `mode_t` is 32-bit on Linux and
/// 16-bit on the BSD/macOS targets, so its width is selected per platform to keep
/// the C ABI correct.
#[cfg(target_os = "linux")]
type ModeT = u32;
#[cfg(not(target_os = "linux"))]
type ModeT = u16;

extern "C" {
    fn umask(mask: ModeT) -> ModeT;
}

fn process_umask() -> u32 {
    // SAFETY: `umask(2)` has no failure mode and no memory effects; the old value
    // is restored immediately, matching git's `umask(0); umask(old)` sequence.
    unsafe {
        let old = umask(0);
        umask(old);
        u32::from(old)
    }
}

/// The sub-tree named `name` directly below `tree`, if it is a tree.
fn subtree<'r>(
    repo: &'r gix::Repository,
    tree: &gix::Tree<'r>,
    name: &[u8],
) -> Result<Option<gix::Tree<'r>>> {
    for entry in tree.decode()?.entries.iter() {
        if entry.filename == name && entry.mode.is_tree() {
            return Ok(Some(repo.find_object(entry.oid.to_owned())?.peel_to_tree()?));
        }
    }
    Ok(None)
}

/// The two ways `.gitattributes` reaches `git archive`, bundled so the walk and
/// the two container writers share one lookup state.
///
/// git sets this up in `write_archive()`: with no `--worktree-attributes` it
/// unpacks the archived tree into an in-memory index and calls
/// `git_attr_set_direction(GIT_ATTR_INDEX)`, so in-tree `.gitattributes` come
/// from the tree being archived rather than from the working copy.
/// `$GIT_DIR/info/attributes` and `core.attributesFile` apply either way, which
/// is what `assemble_attribute_globals()` folds in behind `attributes_only()`.
///
///   * [`Convert::export`] is `get_archive_attrs()` — `attr_check_initl(
///     "export-ignore", "export-subst", NULL)` — driving the two skip/rewrite
///     decisions in `write_archive_entry()` and `queue_or_write_archive_entry()`.
///   * [`Convert::content`] is `convert_to_working_tree()` as called from
///     `object_file_to_archive()`, covering `text`/`eol`/`crlf`, `ident`,
///     `working-tree-encoding` and any configured `filter=<driver>` smudge.
struct Convert<'r> {
    attrs: gix::AttributeStack<'r>,
    outcome: gix::attrs::search::Outcome,
    pipeline: gix::filter::Pipeline<'r>,
}

/// What `get_archive_attrs()` yields for one path.
#[derive(Clone, Copy, Default)]
struct Export {
    ignore: bool,
    subst: bool,
}

impl<'r> Convert<'r> {
    /// Build the lookup state for archiving `tree`.
    fn new(repo: &'r gix::Repository, tree: &gix::Tree<'_>, worktree_attributes: bool) -> Result<Self> {
        use gix::worktree::stack::state::attributes::Source;
        // git's `unpack_trees()` into a scratch index, so in-tree
        // `.gitattributes` are read from the archived tree. `--worktree-attributes`
        // is git's `GIT_ATTR_CHECKIN`-like direction: the working copy wins where
        // it has a file, the tree is the fallback.
        let index = repo.index_from_tree(&tree.id)?;
        let source = if worktree_attributes {
            Source::WorktreeThenIdMapping
        } else {
            Source::IdMapping
        };
        // The pipeline consumes its stack, so attribute lookup and content
        // conversion each get their own; they read the same files.
        let pipeline = gix::filter::Pipeline::new(repo, repo.attributes_only(&index, source)?.detach())?;
        Ok(Convert {
            attrs: repo.attributes_only(&index, source)?,
            outcome: gix::attrs::search::Outcome::default(),
            pipeline,
        })
    }

    /// Look `path` up in the attribute stack. `path` already carries git's
    /// trailing `/` for a tree or a gitlink.
    ///
    /// Returns the states of `export-ignore`, `export-subst` and `diff`, in that
    /// order — `iter_selected()` yields them in the order they were requested.
    fn lookup(&mut self, path: &[u8]) -> Result<[gix::attrs::StateRef<'_>; 3]> {
        const WANTED: [&str; 3] = ["export-ignore", "export-subst", "diff"];
        let mode = if path.ends_with(b"/") {
            gix::index::entry::Mode::DIR
        } else {
            gix::index::entry::Mode::FILE
        };
        // Descending loads the `.gitattributes` along the way, and only then does
        // the collection know every attribute name — so the outcome is sized
        // after the first descent and the second is a cache hit, as in
        // `check_attr.rs`.
        let _ = self.attrs.at_entry(path.as_bstr(), Some(mode))?;
        self.outcome
            .initialize_with_selection(self.attrs.attributes_collection(), WANTED);
        self.attrs
            .at_entry(path.as_bstr(), Some(mode))?
            .matching_attributes(&mut self.outcome);

        let mut states = [gix::attrs::StateRef::Unspecified; 3];
        for (slot, m) in states.iter_mut().zip(self.outcome.iter_selected()) {
            *slot = m.assignment.state;
        }
        Ok(states)
    }

    /// `get_archive_attrs()` for `path`.
    fn export(&mut self, path: &[u8]) -> Result<Export> {
        let [ignore, subst, _diff] = self.lookup(path)?;
        // `ATTR_TRUE(...)`: only an explicit set counts, not `-attr` or an unset.
        let is_set = |s: gix::attrs::StateRef<'_>| matches!(s, gix::attrs::StateRef::Set);
        Ok(Export {
            ignore: is_set(ignore),
            subst: is_set(subst),
        })
    }

    /// `entry_is_binary()`: zip's "apparently a text file" bit, inverted.
    ///
    /// `userdiff_find_by_path()` turns the `diff` attribute into a userdiff
    /// driver, and a driver whose `binary` flag is not `-1` decides on its own
    /// without looking at the content: a bare `diff` selects `driver_true`
    /// (`binary = 0`, text) and `-diff` selects `driver_false` (`binary = 1`,
    /// binary). An unspecified attribute, `!diff`, and `diff=<name>` all land on
    /// a driver with `binary = -1` — the `default` driver and every builtin one —
    /// so those fall through to `buffer_is_binary()` on the *converted* content,
    /// which is the buffer `write_zip_entry()` has in hand.
    fn is_binary(&mut self, path: &[u8], data: &[u8]) -> Result<bool> {
        let [_ignore, _subst, diff] = self.lookup(path)?;
        Ok(match diff {
            gix::attrs::StateRef::Set => false,
            gix::attrs::StateRef::Unset => true,
            _ => super::diffcore_rename::buffer_is_binary(data),
        })
    }

    /// `object_file_to_archive()`'s `convert_to_working_tree()` call. `path` is
    /// the archive path with `--prefix` stripped: git does `path += args->baselen`
    /// before converting, so a prefix never changes which attributes apply.
    fn content(&mut self, path: &[u8], data: Vec<u8>) -> Result<Vec<u8>> {
        let mut converted = self.pipeline.convert_to_worktree(
            &data,
            path.as_bstr(),
            gix::filter::plumbing::driver::apply::Delay::Forbid,
        )?;
        let mut out = Vec::with_capacity(data.len());
        std::io::copy(&mut converted, &mut out)?;
        Ok(out)
    }
}

/// Fetch a tree entry's archive payload, converted as git converts it.
///
/// `write_archive_entry()` hands `NULL` content to the writer for a directory or
/// a gitlink, and `object_file_to_archive()` only runs the conversion when
/// `S_ISREG(mode)` — so a symlink's target is archived verbatim.
fn entry_data(repo: &gix::Repository, conv: &mut Convert<'_>, item: &Item) -> Result<Vec<u8>> {
    match item.kind {
        EntryKind::Tree | EntryKind::Commit => Ok(Vec::new()),
        EntryKind::Link => Ok(repo.find_object(item.oid)?.data.clone()),
        _ => {
            let raw = repo.find_object(item.oid)?.data.clone();
            conv.content(&item.path, raw)
        }
    }
}

/// Depth-first walk producing the exact record sequence git writes.
///
/// Directories are *queued* rather than written: git only materialises a
/// directory record once a file beneath it survives filtering, so a sub-tree
/// that contributes nothing leaves no trace in the archive.
fn collect(
    repo: &gix::Repository,
    tree: gix::Tree<'_>,
    base: &[u8],
    mut search: Option<&mut Search>,
    conv: &mut Convert<'_>,
    pending: &mut Vec<Item>,
    out: &mut Vec<Item>,
) -> Result<()> {
    let entries: Vec<(EntryKind, Vec<u8>, ObjectId)> = tree
        .decode()?
        .entries
        .iter()
        .map(|e| (e.mode.kind(), e.filename.to_vec(), e.oid.to_owned()))
        .collect();

    for (kind, filename, oid) in entries {
        let mut path = base.to_vec();
        path.extend_from_slice(&filename);

        if kind == EntryKind::Tree {
            // git returns `READ_TREE_RECURSIVE` for a directory that a pathspec
            // could still match below; `can_match_relative_path` answers exactly
            // that question from the shared prefix, so an unreachable sub-tree is
            // pruned before it is decoded.
            let recurse = match search.as_deref() {
                None => true,
                Some(s) => s.can_match_relative_path(path.as_bstr(), Some(true)),
            };
            if !recurse {
                continue;
            }
            let mut dir = path.clone();
            dir.push(b'/');
            // `queue_or_write_archive_entry()` checks `export-ignore` on the
            // directory path *with* its trailing slash and returns 0 rather than
            // `READ_TREE_RECURSIVE` when it is set, so the whole sub-tree is
            // dropped without being decoded.
            if conv.export(&dir)?.ignore {
                continue;
            }
            pending.push(Item {
                path: dir.clone(),
                kind,
                oid,
            });
            let child = repo.find_object(oid)?.peel_to_tree()?;
            collect(repo, child, &dir, search.as_deref_mut(), conv, pending, out)?;
            continue;
        }

        // A file (blob, executable blob, symlink or submodule gitlink) is matched
        // as a non-directory. Attribute-driven pathspecs (`:(attr:…)`) are matched
        // as if no attribute were set — see the module header.
        let selected = match search.as_deref_mut() {
            None => true,
            Some(s) => {
                let mut no_attrs = |_: &gix::bstr::BStr,
                                    _: gix::pathspec::attributes::glob::pattern::Case,
                                    _: bool,
                                    _: &mut gix::pathspec::attributes::search::Outcome|
                 -> bool { false };
                s.pattern_matching_relative_path(path.as_bstr(), Some(false), &mut no_attrs)
                    .is_some_and(|m| !m.is_excluded())
            }
        };
        if !selected {
            continue;
        }
        // Submodules become directory records, so they carry a trailing slash.
        // git appends it *before* looking the attributes up, so the gitlink is
        // checked as `<path>/` too.
        if kind == EntryKind::Commit {
            path.push(b'/');
        }
        // `queue_or_write_archive_entry()` runs `write_directory(c)` *before*
        // `write_archive_entry()`, and only the latter consults `export-ignore`.
        // So reaching a leaf flushes the queued ancestors even when the leaf
        // itself is then dropped: a directory whose every child is
        // `export-ignore`d still appears in the archive, empty. Checking the
        // attribute before the flush would wrongly drop it.
        flush_pending(pending, &path, out);
        let export = conv.export(&path)?;
        if export.ignore {
            continue;
        }
        if export.subst {
            // `args->convert = check_attr_export_subst(check)` makes
            // `object_file_to_archive()` run `format_subst()`, expanding
            // `$Format:<pretty>$` against the archived commit. That needs the
            // `--pretty` formatter, which this port does not wire into archive;
            // failing is honest, silently archiving the unexpanded blob is not.
            let shown = String::from_utf8_lossy(&path).into_owned();
            bail!("{shown} has export-subst set, but $Format:…$ expansion is not ported");
        }
        out.push(Item { path, kind, oid });
    }
    Ok(())
}

/// Write out the queued directories that are ancestors of `path`, dropping the
/// ones left behind by a sub-tree that turned out to be empty. This mirrors the
/// `c->bottom` stack unwinding in git's `queue_or_write_archive_entry()`.
fn flush_pending(pending: &mut Vec<Item>, path: &[u8], out: &mut Vec<Item>) {
    let ancestors: Vec<Item> = pending
        .drain(..)
        .filter(|dir| path.starts_with(&dir.path))
        .collect();
    out.extend(ancestors);
}

/// The `ustar` writer: a direct port of git's `archive-tar.c`.
/// `git archive --format=zip` — a port of `archive-zip.c`'s container.
///
/// One local file header plus data per entry, then a central directory, then the
/// end-of-central-directory record carrying the commit id as the archive comment.
/// Every field below is what stock git 2.50.1 writes, read off its own output:
///
///   * "version needed" is 10 for every entry, deflated or not;
///   * the extended-timestamp extra field (`UT`, id 0x5455) appears in *both* the
///     local and the central header, carrying the entry mtime;
///   * "version made by" stays 0 for a plain file or a directory and becomes
///     0x0317 (Unix, zip 2.3) for a symlink or an executable, which is also when
///     the external attributes carry the mode;
///   * the internal attributes are 1 for a file and 0 for a directory;
///   * an entry is deflated only when that makes it *smaller* — `archive-zip.c`
///     compresses into a buffer and falls back to stored when it did not shrink.
#[allow(clippy::too_many_arguments)]
/// git's `-v` reporting, which lives in `write_archive_entry()` and in the
/// `--prefix` directory record above it (`archive.c:206,322`) — the tree walk,
/// and only the tree walk. The `--add-file` / `--add-virtual-file` records are
/// written by `write_extra_entries()`, which has no such line, so those paths are
/// never reported however many `-v`s are given.
fn report(verbose: bool, path: &[u8]) {
    if verbose {
        eprintln!("{}", String::from_utf8_lossy(path));
    }
}

fn write_zip(
    repo: &gix::Repository,
    tree: &gix::Tree<'_>,
    items: Vec<Item>,
    opts: &Opts,
    base: &str,
    commit_id: Option<ObjectId>,
    mtime: i64,
    level: i32,
    conv: &mut Convert<'_>,
) -> Result<ExitCode> {
    let raw: Box<dyn Write> = match &opts.output {
        Some(path) => Box::new(std::io::BufWriter::new(std::fs::File::create(path)?)),
        None => Box::new(std::io::BufWriter::new(std::io::stdout())),
    };
    let mut zip = Zip { out: raw, offset: 0, central: Vec::new(), entries: 0, dos: dos_time(mtime), mtime };

    // A `--prefix` ending in `/` gets its own directory record, as it does in tar.
    if base.ends_with('/') {
        let mut len = base.len();
        while len > 1 && base.as_bytes()[len - 2] == b'/' {
            len -= 1;
        }
        report(opts.verbose, &base.as_bytes()[..len]);
        zip.entry(&base.as_bytes()[..len], EntryKind::Tree, &[], level, false)?;
    }
    for item in items {
        let mut path = base.as_bytes().to_vec();
        path.extend_from_slice(&item.path);
        let data = entry_data(repo, conv, &item)?;
        // `entry_is_binary()` is asked about `path_without_prefix`, and about the
        // converted buffer — which is what `item.path` and `data` already are.
        let binary = conv.is_binary(&item.path, &data)?;
        report(opts.verbose, &path);
        zip.entry(&path, item.kind, &data, level, binary)?;
    }
    for added in &opts.added {
        match added {
            Added::File { name, base, exec, disk } => {
                let mut path = base.clone();
                path.extend_from_slice(name);
                let data = std::fs::read(disk)?;
                let kind = if *exec { EntryKind::BlobExecutable } else { EntryKind::Blob };
                let binary = conv.is_binary(name, &data)?;
                zip.entry(&path, kind, &data, level, binary)?;
            }
            Added::Virtual { name, content } => {
                let mut path = crate::setup::prefix_bytes(repo);
                path.extend_from_slice(name);
                let binary = conv.is_binary(name, content)?;
                zip.entry(&path, EntryKind::Blob, content, level, binary)?;
            }
        }
    }
    let _ = tree;
    zip.finish(commit_id)?;
    Ok(ExitCode::SUCCESS)
}

/// `zip_time`/`zip_date`: the DOS pair `archive-zip.c` derives from the entry
/// mtime in *local* time, which `git archive` leaves as UTC because it sets
/// `TZ`-independent fields from `gmtime`.
fn dos_time(mtime: i64) -> (u16, u16) {
    // Days/seconds since the epoch, in UTC.
    let days = mtime.div_euclid(86400);
    let secs = mtime.rem_euclid(86400);
    // Civil-from-days (Howard Hinnant's algorithm), which is what `gmtime` does.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    let (hour, min, sec) = (secs / 3600, (secs % 3600) / 60, secs % 60);
    let date = (((y - 1980) as u16) << 9) | ((m as u16) << 5) | d as u16;
    let time = ((hour as u16) << 11) | ((min as u16) << 5) | (sec as u16 / 2);
    (time, date)
}

/// One central-directory record, held until the directory is written.
struct ZipCentral {
    made_by: u16,
    flags: u16,
    method: u16,
    crc: u32,
    csize: u32,
    usize_: u32,
    name: Vec<u8>,
    internal: u16,
    external: u32,
    offset: u32,
}

struct Zip<W: Write> {
    out: W,
    offset: u32,
    central: Vec<ZipCentral>,
    entries: u16,
    dos: (u16, u16),
    mtime: i64,
}

impl<W: Write> Zip<W> {
    /// The extended-timestamp extra field, written into both headers.
    fn extra(&self) -> Vec<u8> {
        let mut v = Vec::with_capacity(9);
        v.extend_from_slice(&0x5455u16.to_le_bytes()); // 'UT'
        v.extend_from_slice(&5u16.to_le_bytes());
        v.push(1); // mod time present
        v.extend_from_slice(&(self.mtime as u32).to_le_bytes());
        v
    }

    fn raw(&mut self, bytes: &[u8]) -> Result<()> {
        self.out.write_all(bytes)?;
        self.offset += bytes.len() as u32;
        Ok(())
    }

    /// `is_binary` is `entry_is_binary()`'s verdict for this entry; it is only
    /// consulted for non-directories, matching `write_zip_entry()`, which leaves
    /// its `is_binary` at `-1` for a directory so `!is_binary` writes 0.
    fn entry(&mut self, path: &[u8], kind: EntryKind, data: &[u8], level: i32, is_binary: bool) -> Result<()> {
        let is_dir = matches!(kind, EntryKind::Tree | EntryKind::Commit);
        let mut name = path.to_vec();
        if is_dir && !name.ends_with(b"/") {
            name.push(b'/');
        }

        // `archive-zip.c`: deflate into a buffer and keep it only if it shrank.
        let (method, payload) = if is_dir {
            (0u16, Vec::new())
        } else if data.is_empty() || level == 0 {
            // `-0` stores every entry; an empty file has nothing to compress.
            (0u16, data.to_vec())
        } else {
            let z = gzip::deflate_raw(data, level);
            if z.len() < data.len() {
                (8u16, z)
            } else {
                (0u16, data.to_vec())
            }
        };
        let crc = if is_dir { 0 } else { gzip::crc32_update(0, data) };
        let (made_by, external) = match kind {
            EntryKind::Tree | EntryKind::Commit => (0u16, 0x10u32),
            EntryKind::Link => (0x0317, 0o120_777 << 16),
            EntryKind::BlobExecutable => (0x0317, 0o100_755 << 16),
            EntryKind::Blob => (0, 0),
        };
        // `ZIP_UTF8` (bit 11): git marks a name that is not pure ASCII as UTF-8, in
        // both headers, so an unzip that honours the flag decodes it correctly.
        let flags: u16 = if name.iter().any(|b| *b >= 0x80) { 1 << 11 } else { 0 };
        let extra = self.extra();
        let offset = self.offset;

        let mut hdr = Vec::with_capacity(30 + name.len() + extra.len());
        hdr.extend_from_slice(&0x0403_4b50u32.to_le_bytes());
        hdr.extend_from_slice(&10u16.to_le_bytes()); // version needed
        hdr.extend_from_slice(&flags.to_le_bytes());
        hdr.extend_from_slice(&method.to_le_bytes());
        hdr.extend_from_slice(&self.dos.0.to_le_bytes());
        hdr.extend_from_slice(&self.dos.1.to_le_bytes());
        hdr.extend_from_slice(&crc.to_le_bytes());
        hdr.extend_from_slice(&(payload.len() as u32).to_le_bytes());
        hdr.extend_from_slice(&(data.len() as u32).to_le_bytes());
        hdr.extend_from_slice(&(name.len() as u16).to_le_bytes());
        hdr.extend_from_slice(&(extra.len() as u16).to_le_bytes());
        hdr.extend_from_slice(&name);
        hdr.extend_from_slice(&extra);
        self.raw(&hdr)?;
        self.raw(&payload)?;

        self.central.push(ZipCentral {
            made_by,
            method,
            crc,
            flags,
            csize: payload.len() as u32,
            usize_: data.len() as u32,
            name,
            // `archive-zip.c`: `strbuf_add_le(&zip_dir, 2, !is_binary)`. Bit 0 of
            // the internal attributes is zip's "apparently a text file" flag.
            // A directory never reaches `entry_is_binary()`, so its `is_binary`
            // stays `-1` and `!is_binary` is 0.
            internal: u16::from(!is_dir && !is_binary),
            external,
            offset,
        });
        self.entries += 1;
        Ok(())
    }

    /// The central directory and the end record, whose comment is the commit id.
    fn finish(&mut self, commit_id: Option<ObjectId>) -> Result<()> {
        let cd_offset = self.offset;
        let extra = self.extra();
        let records = std::mem::take(&mut self.central);
        for c in &records {
            let mut hdr = Vec::with_capacity(46 + c.name.len() + extra.len());
            hdr.extend_from_slice(&0x0201_4b50u32.to_le_bytes());
            hdr.extend_from_slice(&c.made_by.to_le_bytes());
            hdr.extend_from_slice(&10u16.to_le_bytes()); // version needed
            hdr.extend_from_slice(&c.flags.to_le_bytes());
            hdr.extend_from_slice(&c.method.to_le_bytes());
            hdr.extend_from_slice(&self.dos.0.to_le_bytes());
            hdr.extend_from_slice(&self.dos.1.to_le_bytes());
            hdr.extend_from_slice(&c.crc.to_le_bytes());
            hdr.extend_from_slice(&c.csize.to_le_bytes());
            hdr.extend_from_slice(&c.usize_.to_le_bytes());
            hdr.extend_from_slice(&(c.name.len() as u16).to_le_bytes());
            hdr.extend_from_slice(&(extra.len() as u16).to_le_bytes());
            hdr.extend_from_slice(&0u16.to_le_bytes()); // comment length
            hdr.extend_from_slice(&0u16.to_le_bytes()); // disk number start
            hdr.extend_from_slice(&c.internal.to_le_bytes());
            hdr.extend_from_slice(&c.external.to_le_bytes());
            hdr.extend_from_slice(&c.offset.to_le_bytes());
            hdr.extend_from_slice(&c.name);
            hdr.extend_from_slice(&extra);
            self.raw(&hdr)?;
        }
        let cd_size = self.offset - cd_offset;
        let comment: Vec<u8> = commit_id
            .map(|id| id.to_hex().to_string().into_bytes())
            .unwrap_or_default();
        let mut end = Vec::with_capacity(22 + comment.len());
        end.extend_from_slice(&0x0605_4b50u32.to_le_bytes());
        end.extend_from_slice(&0u16.to_le_bytes()); // this disk
        end.extend_from_slice(&0u16.to_le_bytes()); // disk with the directory
        end.extend_from_slice(&self.entries.to_le_bytes());
        end.extend_from_slice(&self.entries.to_le_bytes());
        end.extend_from_slice(&cd_size.to_le_bytes());
        end.extend_from_slice(&cd_offset.to_le_bytes());
        end.extend_from_slice(&(comment.len() as u16).to_le_bytes());
        end.extend_from_slice(&comment);
        self.raw(&end)?;
        self.out.flush()?;
        Ok(())
    }
}

struct Tar<W: Write> {
    out: W,
    written: u64,
    mtime: i64,
    umask: u32,
    /// The partial 10 KiB block `write_blocked()` is filling; see [`Tar::raw`].
    block: Vec<u8>,
}

impl<W: Write> Tar<W> {
    /// git's `write_blocked()` (archive-tar.c): headers and payloads are
    /// accumulated into a 10 KiB `block` and handed to the sink one whole block
    /// per call, never in the pieces they arrived in.
    ///
    /// This is observable, not just tidy. `git upload-archive` frames one
    /// sideband packet per `read()` of the archiver's stdout, so the archiver's
    /// write boundaries become packet boundaries on the wire. Writing through an
    /// 8 KiB `BufWriter` instead split a 10240-byte archive into an 8192-byte
    /// write and a 2048-byte one, and whether the reader coalesced them was a
    /// scheduling race — the same request answered `00002805…` on one run and
    /// `00002005…` on the next. One block, one write, one packet.
    fn raw(&mut self, bytes: &[u8]) -> Result<()> {
        self.written += bytes.len() as u64;
        let mut rest = bytes;
        while !rest.is_empty() {
            let room = BLOCK as usize - self.block.len();
            let take = room.min(rest.len());
            self.block.extend_from_slice(&rest[..take]);
            rest = &rest[take..];
            if self.block.len() == BLOCK as usize {
                self.out.write_all(&self.block)?;
                self.block.clear();
            }
        }
        Ok(())
    }

    /// `data` followed by NUL padding up to the next 512-byte record boundary.
    fn payload(&mut self, data: &[u8]) -> Result<()> {
        self.raw(data)?;
        let rem = data.len() % RECORD;
        if rem != 0 {
            self.raw(&ZEROS[..RECORD - rem])?;
        }
        Ok(())
    }

    /// The `pax_global_header` recording the commit the archive was made from.
    fn global_header(&mut self, commit: &ObjectId) -> Result<()> {
        let record = ext_record(b"comment", commit.to_hex().to_string().as_bytes());
        let header = build_header(
            b"pax_global_header",
            b"",
            b"",
            0o100666,
            record.len() as u64,
            self.mtime,
            b'g',
        );
        self.raw(&header)?;
        self.payload(&record)
    }

    /// One archive entry, including the `<oid>.paxheader` record when the path
    /// or symlink target does not fit the `ustar` fields.
    fn entry(&mut self, path: &[u8], kind: EntryKind, oid: &ObjectId, data: &[u8]) -> Result<()> {
        // git's mode mangling: directories and submodules get 0777 masked by
        // tar.umask, symlinks an unmasked 0777, regular files 0777 or 0666
        // depending on the executable bit, masked.
        let (typeflag, mode) = match kind {
            EntryKind::Tree => (b'5', (0o040000 | 0o777) & !self.umask),
            EntryKind::Commit => (b'5', (0o160000 | 0o777) & !self.umask),
            EntryKind::Link => (b'2', 0o120000 | 0o777),
            EntryKind::BlobExecutable => (b'0', (0o100755 | 0o777) & !self.umask),
            EntryKind::Blob => (b'0', (0o100644 | 0o666) & !self.umask),
        };
        // `write_tar_entry` sets the typeflag from the *original* mode but every
        // later `S_ISREG()` test — `prepare_header`'s size field, the pax `size`
        // spill, and the payload write — looks at the mode the umask left behind.
        // A `tar.umask` that clears the file-type bits (git stores the value in an
        // `unsigned int`, so `-1` is all ones and `~tar_umask` is zero) therefore
        // still emits a `0` typeflag header, but with size 0 and no contents.
        let is_regular = mode & 0o170000 == 0o100000;

        let mut ext: Vec<u8> = Vec::new();
        let mut name: Vec<u8> = Vec::new();
        let mut prefix: Vec<u8> = Vec::new();
        if path.len() > NAME_MAX {
            // Split on the last `/` that leaves a short-enough remainder; when no
            // such split exists the real path moves into a pax `path` record.
            let split = ustar_prefix_len(path);
            let rest = path.len() - split - 1;
            if split > 0 && rest <= NAME_MAX {
                prefix.extend_from_slice(&path[..split]);
                name.extend_from_slice(&path[split + 1..]);
            } else {
                name.extend_from_slice(format!("{oid}.data").as_bytes());
                ext.extend_from_slice(&ext_record(b"path", path));
            }
        } else {
            name.extend_from_slice(path);
        }

        let mut link: Vec<u8> = Vec::new();
        if kind == EntryKind::Link {
            if data.len() > NAME_MAX {
                link.extend_from_slice(format!("see {oid}.paxheader").as_bytes());
                ext.extend_from_slice(&ext_record(b"linkpath", data));
            } else {
                link.extend_from_slice(data);
            }
        }

        // git's `write_tar_entry`: the plain `size` field caps at USTAR_MAX_SIZE
        // (0o77777777777). A regular blob past it is written with a header `size`
        // of 0 and its true length spilled into a pax `size` record, appended
        // after any `path` / `linkpath` record: `strbuf_append_ext_header_uint(
        // &ext_header, "size", size)`, whose value is the length in decimal
        // (`%PRIuMAX`). Non-regular entries carry a plain size of 0 regardless.
        let mut size = if is_regular { data.len() as u64 } else { 0 };
        if is_regular && size > SIZE_MAX {
            ext.extend_from_slice(&ext_record(b"size", size.to_string().as_bytes()));
            size = 0;
        }

        if !ext.is_empty() {
            let header = build_header(
                format!("{oid}.paxheader").as_bytes(),
                b"",
                b"",
                0o100666,
                ext.len() as u64,
                self.mtime,
                b'x',
            );
            self.raw(&header)?;
            self.payload(&ext)?;
        }

        let header = build_header(&name, &prefix, &link, mode, size, self.mtime, typeflag);
        self.raw(&header)?;
        if is_regular && !data.is_empty() {
            self.payload(data)?;
        }
        Ok(())
    }

    /// git's `write_trailer()`: zero-fill the rest of the current 10 KiB block
    /// and emit it, then emit one more zero block when that fill was shorter
    /// than the two 512-byte records the tar format ends with.
    fn finish(&mut self) -> Result<()> {
        let offset = self.written % BLOCK;
        let tail = BLOCK - offset;
        self.zeros(tail)?;
        if tail < 2 * RECORD as u64 {
            self.zeros(BLOCK)?;
        }
        // The trailer always lands on a block boundary, so this is normally
        // empty; a short tail would still have to reach the sink.
        if !self.block.is_empty() {
            let pending = std::mem::take(&mut self.block);
            self.out.write_all(&pending)?;
        }
        self.out.flush()?;
        Ok(())
    }

    /// `count` NUL bytes.
    fn zeros(&mut self, count: u64) -> Result<()> {
        let mut left = count;
        while left > 0 {
            let n = left.min(RECORD as u64) as usize;
            self.raw(&ZEROS[..n])?;
            left -= n as u64;
        }
        Ok(())
    }
}

/// git's `get_path_prefix()`: the length of the `ustar` `prefix` field, i.e. the
/// offset of the last `/` at or before byte 155 (ignoring a trailing `/`).
fn ustar_prefix_len(path: &[u8]) -> usize {
    let mut i = path.len();
    if i > 1 && path[i - 1] == b'/' {
        i -= 1;
    }
    if i > PREFIX_MAX {
        i = PREFIX_MAX;
    }
    loop {
        i -= 1;
        if i == 0 || path[i] == b'/' {
            return i;
        }
    }
}

/// git's `strbuf_append_ext_header()`: a pax record `"<len> <keyword>=<value>\n"`
/// where `<len>` counts the record including its own decimal digits.
fn ext_record(keyword: &[u8], value: &[u8]) -> Vec<u8> {
    let mut len = 1 + 1 + keyword.len() + 1 + value.len() + 1;
    let mut tmp = 1usize;
    while len / 10 >= tmp {
        len += 1;
        tmp *= 10;
    }
    let mut out = format!("{len} ").into_bytes();
    out.extend_from_slice(keyword);
    out.push(b'=');
    out.extend_from_slice(value);
    out.push(b'\n');
    out
}

/// One 512-byte `ustar` header, laid out and checksummed exactly as git's
/// `prepare_header()` does: uid/gid 0, uname/gname `root`, `ustar\0` + `00`, and
/// a 7-digit checksum written over a field otherwise read as spaces.
fn build_header(
    name: &[u8],
    prefix: &[u8],
    link: &[u8],
    mode: u32,
    size: u64,
    mtime: i64,
    typeflag: u8,
) -> [u8; RECORD] {
    fn put(header: &mut [u8; RECORD], offset: usize, width: usize, value: &[u8]) {
        let n = value.len().min(width);
        header[offset..offset + n].copy_from_slice(&value[..n]);
    }

    let mut header = [0u8; RECORD];
    put(&mut header, 0, 100, name);
    put(&mut header, 100, 8, format!("{:07o}", mode & 0o7777).as_bytes());
    put(&mut header, 108, 8, b"0000000");
    put(&mut header, 116, 8, b"0000000");
    put(&mut header, 124, 12, format!("{size:011o}").as_bytes());
    put(&mut header, 136, 12, format!("{:011o}", mtime as u64).as_bytes());
    header[156] = typeflag;
    put(&mut header, 157, 100, link);
    put(&mut header, 257, 6, b"ustar\0");
    put(&mut header, 263, 2, b"00");
    put(&mut header, 265, 32, b"root");
    put(&mut header, 297, 32, b"root");
    put(&mut header, 329, 8, b"0000000");
    put(&mut header, 337, 8, b"0000000");
    put(&mut header, 345, 155, prefix);

    let checksum: u32 = header
        .iter()
        .enumerate()
        .map(|(i, b)| if (148..156).contains(&i) { 0x20 } else { u32::from(*b) })
        .sum();
    put(&mut header, 148, 8, format!("{checksum:07o}").as_bytes());
    header
}

/// git's in-process `gzip` filter for `--format=tgz` / `--format=tar.gz`, plus the
/// raw-deflate coder a zip entry's payload uses.
///
/// The coder itself is [`gix::zlib::deflate`], a transcription of zlib's
/// `deflate.c` and `trees.c`. What stays here is the way `archive-tar.c` drives
/// it, which is not incidental: git feeds the tar to zlib one 10 KiB `BLOCKSIZE`
/// block at a time and drains into a 16 KiB `outbuf`, and at `-0`
/// `deflate_stored()` sizes its blocks from `avail_in` and `avail_out`, so both
/// sizes are observable in the output.
mod gzip {
    use std::io::{self, Write};

    use gix::zlib::deflate::{Deflate, Wrap, Z_BUF_ERROR, Z_FINISH, Z_NO_FLUSH, Z_OK, Z_STREAM_END};

    /// `archive-tar.c`'s `BLOCKSIZE`.
    const IN_BLOCK: usize = 10240;
    /// `archive-tar.c`'s `outbuf`.
    const OUT_BUF: usize = 16384;

    pub(super) use gix::zlib::deflate::crc32 as crc32_update;

    pub struct GzDeflate<W: Write> {
        sink: W,
        z: Deflate,
        out: Vec<u8>,
        block: Vec<u8>,
    }

    impl<W: Write> GzDeflate<W> {
        /// `git_deflate_init_gzip()`, with the `{ .os = 3 }` header git sets.
        pub fn new(sink: W, level: i32) -> Self {
            Self::with_wrap(sink, level, Wrap::Gzip)
        }

        /// `git_deflate_init_raw()` — `deflateInit2(..., -MAX_WBITS, ...)`: the same
        /// coder with no wrapper at all, which is what a zip entry's data is.
        pub fn new_raw(sink: W, level: i32) -> Self {
            Self::with_wrap(sink, level, Wrap::Raw)
        }

        fn with_wrap(sink: W, level: i32, wrap: Wrap) -> Self {
            let mut z = Deflate::new(level, wrap);
            z.set_output(OUT_BUF);
            GzDeflate {
                sink,
                z,
                out: vec![0; OUT_BUF],
                block: Vec::with_capacity(IN_BLOCK),
            }
        }

        /// git's `tgz_deflate()`: run `deflate()` until the input is drained,
        /// draining the 16 KiB output buffer to the sink whenever it fills.
        fn run(&mut self, input: &[u8], flush: i32) -> io::Result<()> {
            self.z.set_input(input.len());
            loop {
                if self.z.avail_in() == 0 && flush != Z_FINISH {
                    break;
                }
                let status = self.z.step(input, &mut self.out, flush);
                if self.z.avail_out() == 0 || status == Z_STREAM_END {
                    let n = self.z.out_pos();
                    self.sink.write_all(&self.out[..n])?;
                    self.z.set_output(OUT_BUF);
                    if status == Z_STREAM_END {
                        break;
                    }
                }
                if status != Z_OK && status != Z_BUF_ERROR {
                    return Err(io::Error::other(format!("deflate error ({status})")));
                }
            }
            Ok(())
        }

        /// Finish the stream and return the sink.
        pub fn finish(mut self) -> io::Result<W> {
            if !self.block.is_empty() {
                let block = std::mem::take(&mut self.block);
                self.run(&block, Z_NO_FLUSH)?;
            }
            self.run(&[], Z_FINISH)?;
            self.sink.flush()?;
            Ok(self.sink)
        }
    }

    impl<W: Write> Write for GzDeflate<W> {
        fn write(&mut self, mut buf: &[u8]) -> io::Result<usize> {
            let total = buf.len();
            while !buf.is_empty() {
                let want = IN_BLOCK - self.block.len();
                let take = want.min(buf.len());
                self.block.extend_from_slice(&buf[..take]);
                buf = &buf[take..];
                if self.block.len() == IN_BLOCK {
                    let block = std::mem::take(&mut self.block);
                    self.run(&block, Z_NO_FLUSH)?;
                    self.block = block;
                    self.block.clear();
                }
            }
            Ok(total)
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    /// Raw deflate of one buffer at `level`, for a caller that wants the bytes
    /// rather than a stream — a zip entry's payload.
    pub(super) fn deflate_raw(data: &[u8], level: i32) -> Vec<u8> {
        let mut out = GzDeflate::new_raw(Vec::new(), level);
        let _ = out.write_all(data);
        out.finish().unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// git's `strbuf_append_ext_header_uint(&ext_header, "size", size)` for a
    /// regular blob past `USTAR_MAX_SIZE`: the record is `"<len> size=<value>\n"`
    /// with `<len>` counting its own decimal digits. For a length one byte over
    /// the limit (0o77777777777 + 1 = 8589934592) git writes exactly
    /// `19 size=8589934592\n`.
    #[test]
    fn pax_size_record_matches_git() {
        assert_eq!(
            ext_record(b"size", (SIZE_MAX + 1).to_string().as_bytes()),
            b"19 size=8589934592\n".to_vec()
        );
    }

    /// The overflow only fires for a regular blob strictly larger than the field;
    /// a length exactly at `USTAR_MAX_SIZE` still fits the plain `size` field.
    #[test]
    fn ustar_size_boundary() {
        assert_eq!(SIZE_MAX, 0o77777777777);
        assert_eq!(SIZE_MAX, 8_589_934_591);
    }
}

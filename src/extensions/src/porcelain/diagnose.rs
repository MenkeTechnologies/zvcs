//! `git diagnose` — generate a zip archive of diagnostic information.
//!
//! Ported from `builtin/diagnose.c` + `diagnose.c`, together with the parts of
//! `archive.c` / `archive-zip.c` that `create_diagnostics_archive()` drives:
//! git builds a `git archive --format=zip` command line out of
//! `--add-virtual-file=` / `--add-file=` entries against the empty tree, so the
//! only archive members are the ones listed below, in that command line's order.
//!
//! Covered:
//!   * The option grammar (`-o`/`--output-directory`, `-s`/`--suffix`,
//!     `--mode=<mode>`, their `--no-` forms, sticky `-o<v>`/`-s<v>`, `=`-joined
//!     long values, unique long-option prefixes, `--`, `-h`), the `error: …`
//!     diagnostics and their exit status 129, and git's acceptance of ignored
//!     positional arguments.
//!   * The archive path: `<-o value>` + `strbuf_complete('/')` +
//!     `git-diagnostics-` + the `strftime`-formatted suffix + `.zip`, with the
//!     leading directories created and `die_errno`'s wording on failure.
//!   * `setup_git_directory_gently`'s chdir to the worktree root plus
//!     `prefix_filename()` on `-o`, so an invocation from a subdirectory writes
//!     and reports the same path stock does.
//!   * The stdout report — `Collecting diagnostic info`, the version block,
//!     `Repository root: <worktree>` and `Available space on '<realpath>': <n>
//!     <unit> (mount flags 0x…)` (a `statvfs(3)` call through the `libc` crate,
//!     with `humanise_bytes()`'s rounding reproduced) — and the
//!     `Diagnostics complete.` / `All of the gathered info is captured in …`
//!     lines on stderr.
//!   * The archive members: `diagnostics.log` (the stdout report verbatim),
//!     `packs-local.txt` (`Contents of <objdir>:` plus every entry of
//!     `<objdir>/pack` as `%-70s %16ju`, for the primary object database and
//!     each alternate), `objects-local.txt` (`loose_objs_stats()`: the per-fanout
//!     `%s : %7d files` lines in readdir order and the `Total: <n> loose
//!     objects` tail, read from the literal relative path `.git/objects` as
//!     stock does — so a bare repository yields an empty member), and for
//!     `--mode=all` the contents of `.git`, `.git/hooks`, `.git/info`,
//!     `.git/logs` (recursively) and `.git/objects/info`, including
//!     `add_directory_to_archiver()`'s `could not archive missing directory` and
//!     `skipping '…', which is neither file nor directory` warnings and its
//!     archive-prefix bookkeeping (which, as upstream, keeps the prefix a
//!     recursion left behind for the rest of the parent directory's files).
//!   * The zip container itself — see [`zip`]: `archive-zip.c`'s local file
//!     header, the `0x5455` extended-timestamp extra field, the DOS date/time
//!     from `localtime_r()`, the central directory (creator version `0x0317`
//!     and `mode << 16` external attributes for an executable, the
//!     `!buffer_is_binary()` internal-attribute text bit) and the end-of-central
//!     -directory record.
//!
//! Divergences, all of them forced and none of them papered over:
//!
//!   * **Entries are stored, not deflated.** `write_zip_entry()` deflates a
//!     regular file of non-zero size at `Z_DEFAULT_COMPRESSION` and falls back
//!     to `ZIP_METHOD_STORE` when that does not shrink it; this port always
//!     stores. The container, the member set, the member sizes and the CRCs are
//!     the same — stock produces exactly these bytes at `--format=zip -0` — but
//!     the archive is larger. The deflate coder that would close this lives in
//!     `porcelain::archive`'s private `gzip` module and is not reachable from
//!     here; no second deflate implementation was written to fake it.
//!   * **The version block describes this build, not stock's.** [`version_info`]
//!     is `cmd_diagnose()`'s `get_version_info(&buf, 1)` and is rendered by
//!     [`super::version::get_version_info`] — the same function
//!     `git version --build-options` uses, as in git. It reports every line that
//!     is true of a Rust binary on gitoxide and omits the ones naming C
//!     components this build does not link (`gettext`, `libcurl`, `OpenSSL`,
//!     `zlib`, `SHA-256`); nothing is copied from stock. That function's docs
//!     carry the line-by-line reasoning.
//!   * **No repository is not a crash.** Stock dereferences `r->objects` with no
//!     repository and dies of `SIGSEGV` (exit 139) after leaving a zero-byte
//!     zip behind; verified against git 2.55.0. This port writes the archive
//!     with an empty `packs-local.txt` instead.
//!   * The alternate object databases in `packs-local.txt` are named by the
//!     absolute paths `gix` resolves them to, where stock prints the
//!     `odb_source` path string; and the primary database is rendered as
//!     `.git/objects` / `./objects` when it sits directly under the current
//!     directory, as stock's `git_dir + "/objects"` does, but as an absolute
//!     path otherwise (e.g. under `GIT_OBJECT_DIRECTORY`).
//!   * `entry_is_binary()`'s userdiff lookup is not consulted, only its
//!     `buffer_is_binary()` fallback, so a `diff=<driver>` attribute claiming a
//!     file is text or binary does not move the central directory's text bit.
//!     Nothing any unzip implementation reports depends on it.
//!   * Stock's ambiguous-prefix diagnostic (`error: ambiguous option: …`) is not
//!     reproduced; an ambiguous prefix `bail!`s instead of guessing the message.

use anyhow::{bail, Result};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

/// The usage block, byte-identical to stock `git diagnose -h`.
const USAGE: &str = "\
usage: git diagnose [(-o | --output-directory) <path>] [(-s | --suffix) <format>]
                    [--mode=<mode>]

    -o, --[no-]output-directory <path>
                          specify a destination for the diagnostics archive
    -s, --[no-]suffix <format>
                          specify a strftime format suffix for the filename
    --mode (stats|all)    specify the content of the diagnostic archive
";

/// The long options stock's `parse_options` table exposes, in table order.
/// Prefix matching resolves against this list, as `parse-options` does.
const LONG_OPTS: [&str; 3] = ["output-directory", "suffix", "mode"];

/// git's `die()` exit status.
const EXIT_FATAL: u8 = 128;
/// git's usage/parse-error exit status (`PARSE_OPT_HELP`).
const EXIT_USAGE: u8 = 129;

/// `enum diagnose_mode` — how much of the repository the archive carries.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Mode {
    /// `DIAGNOSE_STATS`: the report plus the pack and loose-object statistics.
    Stats,
    /// `DIAGNOSE_ALL`: additionally the contents of the metadata directories.
    All,
}

/// `diagnose_options[]` — the values `option_parse_diagnose()` accepts.
pub(crate) fn parse_mode(value: &str) -> Option<Mode> {
    match value {
        "stats" => Some(Mode::Stats),
        "all" => Some(Mode::All),
        _ => None,
    }
}

/// `git diagnose` — collect diagnostic information into a zip archive.
pub fn diagnose(args: &[String]) -> Result<ExitCode> {
    // The dispatcher passes the argument tail, but tolerate the subcommand
    // being present at index 0 so both calling conventions behave the same.
    let args = match args.first() {
        Some(a) if a == "diagnose" => &args[1..],
        _ => args,
    };

    let opts = match parse_options(args)? {
        Parsed::Ok(o) => o,
        Parsed::Exit(code) => return Ok(ExitCode::from(code)),
    };

    // `setup_git_directory_gently()` leaves the process at the worktree root and
    // hands the command the subdirectory it was started in; `prefix_filename()`
    // then re-attaches that subdirectory to a relative `-o` value, so the file
    // lands where the user asked for it and is *reported* relative to the root.
    let start = Setup::enter();
    let now = LocalTime::now();

    let mut zip_path = start.prefix_filename(opts.output.as_deref().unwrap_or(""));
    if !zip_path.is_empty() && !zip_path.ends_with('/') {
        zip_path.push('/');
    }
    zip_path.push_str("git-diagnostics-");
    zip_path.push_str(&now.strftime(opts.suffix.as_deref().unwrap_or("")));
    zip_path.push_str(".zip");

    // `safe_create_leading_directories()` — a missing parent is created, an
    // existing one is fine, anything else is fatal.
    if let Some(parent) = Path::new(&zip_path).parent() {
        if !parent.as_os_str().is_empty() && std::fs::create_dir_all(parent).is_err() {
            eprintln!("fatal: could not create leading directories for '{zip_path}'");
            return Ok(ExitCode::from(EXIT_FATAL));
        }
    }

    if let Err(e) = create_diagnostics_archive(&zip_path, opts.mode, &now) {
        eprintln!("fatal: unable to create diagnostics archive {zip_path}: {e}");
        return Ok(ExitCode::from(EXIT_FATAL));
    }
    Ok(ExitCode::SUCCESS)
}

/// Everything the option parser produces.
struct Opts {
    /// `-o`/`--output-directory`; `None` means the current directory.
    output: Option<String>,
    /// `-s`/`--suffix`, defaulting to git's `%Y-%m-%d-%H%M`.
    suffix: Option<String>,
    /// `--mode`, defaulting to `DIAGNOSE_STATS`.
    mode: Mode,
}

/// Either parsed options, or an exit status git would leave with immediately.
enum Parsed {
    Ok(Opts),
    Exit(u8),
}

/// git's `parse_options()` for this command's option table.
///
/// Long options may be abbreviated to any unambiguous prefix and negated with
/// `no-`, as `parse-options` allows; `--` stops option parsing. Positional
/// arguments are accepted and ignored, as stock does.
fn parse_options(args: &[String]) -> Result<Parsed> {
    let mut opts = Opts {
        output: None,
        suffix: Some("%Y-%m-%d-%H%M".to_string()),
        mode: Mode::Stats,
    };
    let mut i = 0;
    let mut no_more_opts = false;

    while i < args.len() {
        let a = args[i].as_str();
        i += 1;

        if no_more_opts || a == "-" || !a.starts_with('-') {
            continue;
        }
        if a == "--" {
            no_more_opts = true;
            continue;
        }
        // `parse_options_step()` tests `--help-all` with a `strcmp()` of its own,
        // ahead of `parse_long_opt()`: the name never abbreviates and never takes
        // an `=<value>`. This table has no `PARSE_OPT_HIDDEN` entry, so
        // `USAGE_FULL` renders the same block `-h` prints.
        if a == "--help-all" {
            println!("{USAGE}");
            return Ok(Parsed::Exit(EXIT_USAGE));
        }

        if let Some(long) = a.strip_prefix("--") {
            // Split `--name=value` before resolving the name.
            let (name, inline) = match long.split_once('=') {
                Some((n, v)) => (n, Some(v)),
                None => (long, None),
            };
            let (name, negated) = match name.strip_prefix("no-") {
                Some(rest) => (rest, true),
                None => (name, false),
            };

            let matches: Vec<&str> = LONG_OPTS
                .iter()
                .copied()
                .filter(|o| o.starts_with(name))
                .collect();
            let resolved = match matches.as_slice() {
                [exact] => *exact,
                // An exact hit wins over the longer options it prefixes.
                _ if LONG_OPTS.contains(&name) => name,
                [] => {
                    eprint!("error: unknown option `{long}'\n{USAGE}\n");
                    return Ok(Parsed::Exit(EXIT_USAGE));
                }
                _ => bail!("ambiguous option `--{long}' (ambiguity diagnostics not ported)"),
            };

            if negated {
                // `--no-<opt>` clears the value and takes no argument.
                if inline.is_some() {
                    eprintln!("error: option `no-{resolved}' takes no value");
                    return Ok(Parsed::Exit(EXIT_USAGE));
                }
                match resolved {
                    "output-directory" => opts.output = None,
                    "suffix" => opts.suffix = None,
                    // `--mode` carries PARSE_OPT_NONEG, so `--no-mode` is not an
                    // option at all and `parse-options` reports it as unknown.
                    _ => {
                        eprint!("error: unknown option `{long}'\n{USAGE}\n");
                        return Ok(Parsed::Exit(EXIT_USAGE));
                    }
                }
                continue;
            }

            // Every option in this table takes a value.
            let value = match inline {
                Some(v) => v.to_string(),
                None => match args.get(i) {
                    Some(v) => {
                        i += 1;
                        v.clone()
                    }
                    None => {
                        eprintln!("error: option `{resolved}' requires a value");
                        return Ok(Parsed::Exit(EXIT_USAGE));
                    }
                },
            };
            match resolved {
                "output-directory" => opts.output = Some(value),
                "suffix" => opts.suffix = Some(value),
                _ => match parse_mode(&value) {
                    Some(m) => opts.mode = m,
                    None => {
                        eprintln!("error: invalid --mode value '{value}'");
                        return Ok(Parsed::Exit(EXIT_USAGE));
                    }
                },
            }
            continue;
        }

        // Short switches: grouped, with a sticky or following value.
        let mut chars = a[1..].chars();
        while let Some(c) = chars.next() {
            match c {
                'h' => {
                    println!("{USAGE}");
                    return Ok(Parsed::Exit(EXIT_USAGE));
                }
                'o' | 's' => {
                    let rest: String = chars.by_ref().collect();
                    let value = if rest.is_empty() {
                        match args.get(i) {
                            Some(v) => {
                                i += 1;
                                v.clone()
                            }
                            None => {
                                eprintln!("error: switch `{c}' requires a value");
                                return Ok(Parsed::Exit(EXIT_USAGE));
                            }
                        }
                    } else {
                        rest
                    };
                    if c == 'o' {
                        opts.output = Some(value);
                    } else {
                        opts.suffix = Some(value);
                    }
                }
                _ => {
                    eprint!("error: unknown switch `{c}'\n{USAGE}\n");
                    return Ok(Parsed::Exit(EXIT_USAGE));
                }
            }
        }
    }

    Ok(Parsed::Ok(opts))
}

/// What `setup_git_directory_gently()` leaves behind: the process sitting at the
/// worktree root, and the subdirectory it was started in as `prefix`.
///
/// A repository without a worktree (bare, or none at all) leaves the current
/// directory alone, which is what stock does and what makes a bare repository's
/// `Contents of ./objects:` and empty `objects-local.txt` come out right.
pub(crate) struct Setup {
    /// `startup_info->prefix`: the start directory relative to the worktree
    /// root, with a trailing slash, or empty when they are the same.
    prefix: String,
}

impl Setup {
    pub(crate) fn enter() -> Self {
        let mut prefix = String::new();
        if let Ok(repo) = gix::discover(".") {
            if let Some(workdir) = repo.workdir() {
                if let (Ok(root), Ok(cwd)) = (workdir.canonicalize(), std::env::current_dir()) {
                    if let Ok(rel) = cwd.strip_prefix(&root) {
                        if !rel.as_os_str().is_empty() && std::env::set_current_dir(&root).is_ok() {
                            prefix = format!("{}/", rel.display());
                        }
                    }
                }
            }
        }
        Self { prefix }
    }

    /// `startup_info->prefix`, for the callers that strip it back off a path
    /// before showing it to the user.
    pub(crate) fn prefix(&self) -> &str {
        &self.prefix
    }

    /// `prefix_filename()`: an absolute name is used as-is, a relative one is
    /// taken to be relative to the directory the command started in.
    pub(crate) fn prefix_filename(&self, name: &str) -> String {
        if name.starts_with('/') {
            name.to_string()
        } else {
            format!("{}{name}", self.prefix)
        }
    }
}

/// `cmd_diagnose()`'s `get_version_info(&buf, 1)`.
///
/// The one implementation lives in [`super::version`], which is also where
/// `git version --build-options` renders it — in git the two share
/// `get_version_info()`, and here they share it too, so a report can never
/// describe a different build than `git version` does.
/// `porcelain::bugreport` prints the same block under its `git version:` header.
pub(crate) fn version_info(out: &mut String) {
    super::version::get_version_info(out, true);
}

/// `create_diagnostics_archive()` — write the report to stdout and the archive
/// to `zip_path`.
///
/// `now` is the caller's single `time(NULL)`, so the archive's entry timestamps
/// and the suffix in its own file name come from the same instant, as they do
/// in `cmd_diagnose()` and `cmd_bugreport()`.
pub(crate) fn create_diagnostics_archive(zip_path: &str, mode: Mode, now: &LocalTime) -> Result<()> {
    let repo = gix::discover(".").ok();

    // git opens (and truncates) the archive before it collects anything, so a
    // path that cannot be created fails before any work is done.
    let file = std::fs::File::create(zip_path)?;

    let mut log = String::from("Collecting diagnostic info\n\n");
    version_info(&mut log);
    match repo.as_ref().and_then(|r| r.workdir()) {
        // `strbuf_addf("Repository root: %s\n", r->worktree)` — an absolute,
        // resolved path, which is what `setup_work_tree()` stores.
        Some(wt) => {
            let wt = wt.canonicalize().unwrap_or_else(|_| wt.to_path_buf());
            log.push_str(&format!("Repository root: {}\n", wt.display()));
        }
        // git hands `printf` a NULL here; both glibc and Apple libc print this.
        None => log.push_str("Repository root: (null)\n"),
    }
    disk_info(&mut log);

    print!("{log}");
    std::io::stdout().flush()?;

    let mut zip = zip::Writer::new(std::io::BufWriter::new(file), now);
    zip.add("diagnostics.log", 0o100644, log.as_bytes())?;
    zip.add("packs-local.txt", 0o100644, packs_local(repo.as_ref()).as_bytes())?;

    let mut objects = String::new();
    loose_objs_stats(&mut objects, Path::new(".git/objects"));
    zip.add("objects-local.txt", 0o100644, objects.as_bytes())?;

    if mode == Mode::All {
        // `archive_dirs[]`, in order, with only `.git/logs` walked recursively.
        const ARCHIVE_DIRS: [(&str, bool); 5] = [
            (".git", false),
            (".git/hooks", false),
            (".git/info", false),
            (".git/logs", true),
            (".git/objects/info", false),
        ];
        let mut entries: Vec<(String, PathBuf)> = Vec::new();
        let mut prefix = String::new();
        for (path, recursive) in ARCHIVE_DIRS {
            add_directory_to_archiver(&mut entries, &mut prefix, path, recursive)
                .map_err(|e| anyhow::anyhow!("could not add directory '{path}' to archiver: {e}"))?;
        }
        for (name, disk) in entries {
            let content = std::fs::read(&disk)
                .map_err(|e| anyhow::anyhow!("cannot read '{}': {e}", disk.display()))?;
            zip.add(&name, canon_mode(&disk), &content)?;
        }
    }

    zip.finish()?;
    eprint!("\nDiagnostics complete.\nAll of the gathered info is captured in '{zip_path}'\n");
    Ok(())
}

/// `canon_mode(st.st_mode)` for a regular file: the executable bit is the only
/// permission git keeps.
fn canon_mode(path: &Path) -> u32 {
    use std::os::unix::fs::PermissionsExt;
    let executable = std::fs::metadata(path)
        .is_ok_and(|m| m.permissions().mode() & 0o100 != 0);
    if executable {
        0o100755
    } else {
        0o100644
    }
}

/// `get_disk_info()` — free space and mount flags for the current directory,
/// via `statvfs(3)`.
fn disk_info(out: &mut String) {
    // `strbuf_realpath(&buf, ".", 1)`.
    let Ok(here) = std::fs::canonicalize(".") else {
        eprintln!("error: could not determine free disk size for '.'");
        return;
    };
    let shown = here.display().to_string();

    let Ok(c_path) = std::ffi::CString::new(shown.as_bytes()) else {
        eprintln!("error: could not determine free disk size for '{shown}'");
        return;
    };
    // SAFETY: `c_path` is a NUL-terminated string that outlives the call, and
    // `stat` is a fully-owned, correctly-sized `statvfs` record.
    let mut stat: libc::statvfs = unsafe { std::mem::zeroed() };
    if unsafe { libc::statvfs(c_path.as_ptr(), &mut stat) } < 0 {
        eprintln!(
            "error: could not determine free disk size for '{shown}': {}",
            std::io::Error::last_os_error()
        );
        return;
    }

    out.push_str(&format!("Available space on '{shown}': "));
    // `(off_t)stat.f_bsize * (off_t)stat.f_bavail`, in git's signed 64-bit
    // arithmetic.
    humanise_bytes(out, (stat.f_bsize as i64).wrapping_mul(stat.f_bavail as i64));
    out.push_str(&format!(" (mount flags 0x{:x})\n", stat.f_flag));
}

/// `humanise_bytes()` — the IEC unit and the two-decimal rounding git uses,
/// including its truncation to `unsigned` in the MiB and KiB branches.
fn humanise_bytes(out: &mut String, bytes: i64) {
    if bytes > 1 << 30 {
        let whole = (bytes >> 30) as u32;
        let frac = ((bytes & ((1 << 30) - 1)) as u32) / 10_737_419;
        out.push_str(&format!("{whole}.{frac:02} GiB"));
    } else if bytes > 1 << 20 {
        let x = (bytes as u32).wrapping_add(5243); /* for rounding */
        out.push_str(&format!("{}.{:02} MiB", x >> 20, ((x & ((1 << 20) - 1)) * 100) >> 20));
    } else if bytes > 1 << 10 {
        let x = (bytes as u32).wrapping_add(5); /* for rounding */
        out.push_str(&format!("{}.{:02} KiB", x >> 10, ((x & ((1 << 10) - 1)) * 100) >> 10));
    } else {
        let unit = if bytes == 1 { "byte" } else { "bytes" };
        out.push_str(&format!("{} {unit}", bytes as u32));
    }
}

/// `dir_file_stats()` over the primary object database and every alternate.
fn packs_local(repo: Option<&gix::Repository>) -> String {
    let mut out = String::new();
    let Some(repo) = repo else { return out };

    let store = repo.objects.store_ref();
    dir_file_stats(&mut out, &objdir_display(store.path()), store.path());
    for alternate in store.alternate_db_paths().unwrap_or_default() {
        dir_file_stats(&mut out, &alternate.display().to_string(), &alternate);
    }
    out
}

/// Stock names the primary object database by `git_dir + "/objects"`, which
/// after `setup_git_directory_gently()` is `.git/objects` in a worktree and
/// `./objects` in a bare repository entered at its git directory. Reproduce both
/// forms, and fall back to the absolute path when the database is somewhere else
/// entirely (`GIT_OBJECT_DIRECTORY`, a linked worktree, `--git-dir`).
fn objdir_display(objdir: &Path) -> String {
    let Ok(cwd) = std::env::current_dir() else {
        return objdir.display().to_string();
    };
    // `gix` hands back the path as it discovered it, which for a repository
    // found at the current directory still carries the leading `./`.
    let absolute = if objdir.is_absolute() {
        objdir.to_path_buf()
    } else {
        cwd.join(objdir)
    };
    let Ok(rel) = absolute.strip_prefix(&cwd) else {
        return objdir.display().to_string();
    };
    let rel: PathBuf = rel
        .components()
        .filter(|c| !matches!(c, std::path::Component::CurDir))
        .collect();
    match rel.to_str() {
        Some("objects") => "./objects".to_string(),
        Some(".git/objects") => ".git/objects".to_string(),
        _ => objdir.display().to_string(),
    }
}

/// `dir_file_stats()` — a header naming the object database, then every entry of
/// its `pack` directory that `stat(2)` succeeds on, as `%-70s %16ju`.
fn dir_file_stats(out: &mut String, shown: &str, objdir: &Path) {
    out.push_str(&format!("Contents of {shown}:\n"));
    let Ok(entries) = std::fs::read_dir(objdir.join("pack")) else {
        return;
    };
    for entry in entries.flatten() {
        let Ok(meta) = std::fs::metadata(entry.path()) else {
            continue;
        };
        let name = entry.file_name().to_string_lossy().into_owned();
        out.push_str(&format!("{name:<70} {:>16}\n", meta.len()));
    }
}

/// `loose_objs_stats()` — the per-fanout file counts and their total.
///
/// A directory that cannot be opened appends nothing at all, not even the
/// header, exactly as stock's early `return` does.
fn loose_objs_stats(out: &mut String, path: &Path) {
    let Ok(entries) = std::fs::read_dir(path) else {
        return;
    };

    out.push_str("Object directory stats for ");
    out.push_str(&absolute_path(path));
    out.push_str(":\n");

    let mut total = 0usize;
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        let is_fanout = name.len() == 2 && name.bytes().all(|b| b.is_ascii_hexdigit());
        if !is_fanout || !entry.file_type().is_ok_and(|t| t.is_dir()) {
            continue;
        }
        let count = count_files(&entry.path());
        total += count;
        out.push_str(&format!("{name} : {count:>7} files\n"));
    }
    // No trailing newline: git leaves the member ending on this line.
    out.push_str(&format!("Total: {total} loose objects"));
}

/// `count_files()` — the regular files directly inside `path`.
fn count_files(path: &Path) -> usize {
    let Ok(entries) = std::fs::read_dir(path) else {
        return 0;
    };
    entries
        .flatten()
        .filter(|e| e.file_type().is_ok_and(|t| t.is_file()))
        .count()
}

/// `add_directory_to_archiver()` — collect `(name in archive, path on disk)` for
/// every regular file at or below `path`.
///
/// `prefix` is the `--prefix=` value stock pushes onto the archiver command
/// line. It is threaded through the recursion by reference on purpose: upstream
/// never restores it after descending, so files listed after a subdirectory in
/// readdir order inherit that subdirectory's prefix. Restoring it here would be
/// a different program.
fn add_directory_to_archiver(
    out: &mut Vec<(String, PathBuf)>,
    prefix: &mut String,
    path: &str,
    recurse: bool,
) -> std::io::Result<()> {
    let at_root = path.is_empty();
    let entries = match std::fs::read_dir(if at_root { "." } else { path }) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            eprintln!("warning: could not archive missing directory '{path}'");
            return Ok(());
        }
        Err(e) => return Err(e),
    };

    *prefix = if at_root {
        String::new()
    } else {
        format!("{path}/")
    };

    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        // `strbuf_setlen(&buf, len); strbuf_addstr(&buf, e->d_name)`.
        let child = if at_root {
            name.clone()
        } else {
            format!("{path}/{name}")
        };
        // `get_dtype(e, &abspath, 0)` does not follow symlinks.
        let Ok(kind) = entry.file_type() else {
            continue;
        };
        if kind.is_file() {
            out.push((format!("{prefix}{name}"), PathBuf::from(child)));
        } else if !kind.is_dir() {
            eprintln!("warning: skipping '{child}', which is neither file nor directory");
        } else if recurse {
            add_directory_to_archiver(out, prefix, &child, recurse)?;
        }
    }
    Ok(())
}

/// `strbuf_add_absolute_path()`: prepend the current directory to a relative
/// path, without resolving symlinks.
fn absolute_path(path: &Path) -> String {
    if path.is_absolute() {
        return path.display().to_string();
    }
    match std::env::current_dir() {
        Ok(cwd) => format!("{}/{}", cwd.display(), path.display()),
        Err(_) => path.display().to_string(),
    }
}

/// A single `time(NULL)` and the `struct tm` `localtime_r()` derives from it.
///
/// git captures the timestamp once per command and formats it several times —
/// the report file's suffix, the archive's suffix, and every zip entry's DOS
/// date/time — so they can never straddle a second boundary.
pub(crate) struct LocalTime {
    epoch: libc::time_t,
    tm: libc::tm,
}

impl LocalTime {
    /// `time(NULL)` + `localtime_r(&now, &tm)`.
    pub(crate) fn now() -> Self {
        // SAFETY: `time(NULL)` takes no output pointer, and `localtime_r` fills
        // a `tm` this frame owns. A NULL return leaves the zeroed `tm` in place.
        let epoch = unsafe { libc::time(std::ptr::null_mut()) };
        let mut tm: libc::tm = unsafe { std::mem::zeroed() };
        unsafe { libc::localtime_r(&epoch, &mut tm) };
        Self { epoch, tm }
    }

    /// `strbuf_addftime(fmt, tm, 0, 0)`.
    ///
    /// git handles `%s` and `%z` itself because there is no portable way to pass
    /// a zone to `strftime`; with the zero offset these commands use, `%z`
    /// becomes `+0000` and `%s` the epoch `tm_to_time_t()` reconstructs. The
    /// rest goes to the same libc `strftime` git calls, so every other
    /// conversion specifier behaves identically.
    pub(crate) fn strftime(&self, fmt: &str) -> String {
        if fmt.is_empty() {
            return String::new();
        }

        let mut munged = String::new();
        let mut rest = fmt;
        while let Some(percent) = rest.find('%') {
            munged.push_str(&rest[..percent]);
            rest = &rest[percent + 1..];
            let mut chars = rest.chars();
            match chars.next() {
                Some('%') => {
                    munged.push_str("%%");
                    rest = chars.as_str();
                }
                Some('s') => {
                    munged.push_str(&tm_to_time_t(&self.tm).to_string());
                    rest = chars.as_str();
                }
                Some('z') => {
                    munged.push_str("+0000");
                    rest = chars.as_str();
                }
                // Any other specifier is left for strftime, one `%` at a time.
                _ => munged.push('%'),
            }
        }
        munged.push_str(rest);

        format_tm(&munged, &self.tm)
    }

    /// `dos_time()`'s date half: day, 1-based month, and years since 1980.
    fn dos_date(&self) -> u16 {
        (self.tm.tm_mday + (self.tm.tm_mon + 1) * 32 + (self.tm.tm_year + 1900 - 1980) * 512) as u16
    }

    /// `dos_time()`'s time half, in two-second units.
    fn dos_time(&self) -> u16 {
        (self.tm.tm_sec / 2 + self.tm.tm_min * 32 + self.tm.tm_hour * 2048) as u16
    }

    /// `args->time` — the Unix timestamp the `0x5455` extra field carries.
    fn epoch(&self) -> u32 {
        self.epoch as u32
    }
}

/// `strftime(3)` into a growing buffer, with git's disambiguation of the
/// "produced nothing" and "did not fit" cases: append a space to the format,
/// grow until something comes out, then drop that space again.
fn format_tm(fmt: &str, tm: &libc::tm) -> String {
    let Ok(c_fmt) = std::ffi::CString::new(fmt) else {
        return String::new();
    };
    let mut hint = 128usize;
    let mut buf = vec![0u8; hint];
    // SAFETY: `buf` is a live allocation of `hint` bytes and `c_fmt` is
    // NUL-terminated; `strftime` writes at most `hint` bytes including the NUL.
    let mut len = unsafe { libc::strftime(buf.as_mut_ptr().cast(), hint, c_fmt.as_ptr(), tm) };

    if len == 0 {
        let Ok(padded) = std::ffi::CString::new(format!("{fmt} ")) else {
            return String::new();
        };
        while len == 0 {
            hint *= 2;
            buf = vec![0u8; hint];
            // SAFETY: as above, with the grown buffer.
            len = unsafe { libc::strftime(buf.as_mut_ptr().cast(), hint, padded.as_ptr(), tm) };
        }
        len -= 1; /* drop munged space */
    }
    String::from_utf8_lossy(&buf[..len]).into_owned()
}

/// `tm_to_time_t()` from `date.c`, which reads the broken-down time as UTC.
/// Outside 1970..=2099 git returns -1 and so does this.
fn tm_to_time_t(tm: &libc::tm) -> i64 {
    const MDAYS: [i64; 12] = [0, 31, 59, 90, 120, 151, 181, 212, 243, 273, 304, 334];
    let year = i64::from(tm.tm_year) - 70;
    let month = i64::from(tm.tm_mon);
    let mut day = i64::from(tm.tm_mday);

    if !(0..=129).contains(&year) || !(0..=11).contains(&month) {
        return -1;
    }
    if month < 2 || (year + 2) % 4 != 0 {
        day -= 1;
    }
    if tm.tm_hour < 0 || tm.tm_min < 0 || tm.tm_sec < 0 {
        return -1;
    }
    (year * 365 + (year + 1) / 4 + MDAYS[month as usize] + day) * 24 * 60 * 60
        + i64::from(tm.tm_hour) * 60 * 60
        + i64::from(tm.tm_min) * 60
        + i64::from(tm.tm_sec)
}

/// The zip container `archive-zip.c` writes.
///
/// Only the shape `create_diagnostics_archive()` needs is here: whole buffers,
/// no streaming, no zip64 escapes (every member is a file this command produced
/// or a `.git` metadata file, none of which reach 4 GiB) and, per the module
/// docs, the `ZIP_METHOD_STORE` half of the method choice.
mod zip {
    use super::LocalTime;
    use std::io::{self, Write};

    /// `ZIP_EXTRA_MTIME_SIZE`: 2-byte magic, 2-byte size, a flag byte and a
    /// 4-byte Unix mtime.
    const EXTRA_MTIME_SIZE: u16 = 9;
    /// `ZIP_UTF8` — the general-purpose flag saying the name is UTF-8.
    const ZIP_UTF8: u16 = 1 << 11;
    /// `version_needed` for a non-zip64 entry.
    const VERSION_NEEDED: u16 = 10;
    /// The creator version git stamps on an entry carrying Unix permissions.
    const UNIX_CREATOR_VERSION: u16 = 0x0317;
    /// `buffer_is_binary()` only looks this far into the content.
    const FIRST_FEW_BYTES: usize = 8000;

    pub(super) struct Writer<W: Write> {
        out: W,
        /// The central directory, accumulated until `finish()`.
        dir: Vec<u8>,
        /// `zip_offset` — bytes written so far, i.e. the next entry's offset.
        offset: u32,
        entries: u16,
        dos_date: u16,
        dos_time: u16,
        mtime: u32,
    }

    impl<W: Write> Writer<W> {
        pub(super) fn new(out: W, now: &LocalTime) -> Self {
            Self {
                out,
                dir: Vec::new(),
                offset: 0,
                entries: 0,
                dos_date: now.dos_date(),
                dos_time: now.dos_time(),
                mtime: now.epoch(),
            }
        }

        /// `write_zip_entry()` for a regular file held entirely in memory.
        pub(super) fn add(&mut self, path: &str, mode: u32, data: &[u8]) -> io::Result<()> {
            let crc = gix::features::hash::crc32(data);
            let size = data.len() as u32;
            let name = path.as_bytes();
            let pathlen = name.len() as u16;

            // A Rust string is always valid UTF-8, so git's `is_utf8()` arm is
            // the only one reachable and its `path is not valid UTF-8` warning
            // is unreachable.
            let flags = if path.is_ascii() { 0 } else { ZIP_UTF8 };
            let executable = mode & 0o111 != 0;
            let creator_version = if executable { UNIX_CREATOR_VERSION } else { 0 };
            let attr2 = if executable { mode << 16 } else { 0 };
            let offset = self.offset;

            // The `0x5455` extended-timestamp extra field, identical in the
            // local header and the central directory.
            let mut extra = Vec::with_capacity(EXTRA_MTIME_SIZE as usize);
            le16(&mut extra, 0x5455);
            le16(&mut extra, EXTRA_MTIME_SIZE - 4);
            extra.push(1); /* just mtime */
            le32(&mut extra, self.mtime);

            let mut header = Vec::with_capacity(30);
            le32(&mut header, 0x0403_4b50);
            le16(&mut header, VERSION_NEEDED);
            le16(&mut header, flags);
            le16(&mut header, 0); /* ZIP_METHOD_STORE */
            le16(&mut header, self.dos_time);
            le16(&mut header, self.dos_date);
            le32(&mut header, crc);
            le32(&mut header, size); /* compressed size == size when stored */
            le32(&mut header, size);
            le16(&mut header, pathlen);
            le16(&mut header, EXTRA_MTIME_SIZE);

            self.out.write_all(&header)?;
            self.out.write_all(name)?;
            self.out.write_all(&extra)?;
            self.out.write_all(data)?;
            self.offset += header.len() as u32 + u32::from(pathlen) + u32::from(EXTRA_MTIME_SIZE) + size;

            let dir = &mut self.dir;
            le32(dir, 0x0201_4b50);
            le16(dir, creator_version);
            le16(dir, VERSION_NEEDED);
            le16(dir, flags);
            le16(dir, 0); /* ZIP_METHOD_STORE */
            le16(dir, self.dos_time);
            le16(dir, self.dos_date);
            le32(dir, crc);
            le32(dir, size);
            le32(dir, size);
            le16(dir, pathlen);
            le16(dir, EXTRA_MTIME_SIZE);
            le16(dir, 0); /* comment length */
            le16(dir, 0); /* disk */
            le16(dir, u16::from(!is_binary(data))); /* internal attributes */
            le32(dir, attr2);
            le32(dir, offset);
            dir.extend_from_slice(name);
            dir.extend_from_slice(&extra);
            self.entries += 1;
            Ok(())
        }

        /// `write_zip_trailer(NULL)`: the archive is built against the empty
        /// tree rather than a commit, so there is no object-id comment.
        pub(super) fn finish(mut self) -> io::Result<()> {
            let mut trailer = Vec::with_capacity(22);
            le32(&mut trailer, 0x0605_4b50);
            le16(&mut trailer, 0); /* disk */
            le16(&mut trailer, 0); /* directory start disk */
            le16(&mut trailer, self.entries);
            le16(&mut trailer, self.entries);
            le32(&mut trailer, self.dir.len() as u32);
            le32(&mut trailer, self.offset);
            le16(&mut trailer, 0); /* comment length */

            self.out.write_all(&self.dir)?;
            self.out.write_all(&trailer)?;
            self.out.flush()
        }
    }

    /// `buffer_is_binary()` — a NUL byte in the first 8000 bytes.
    fn is_binary(data: &[u8]) -> bool {
        let window = &data[..data.len().min(FIRST_FEW_BYTES)];
        memchr::memchr(0, window).is_some()
    }

    fn le16(out: &mut Vec<u8>, n: u16) {
        out.extend_from_slice(&n.to_le_bytes());
    }

    fn le32(out: &mut Vec<u8>, n: u32) {
        out.extend_from_slice(&n.to_le_bytes());
    }
}

use anyhow::Result;
use std::io::Write;
use std::process::ExitCode;

use gix::bstr::{BString, ByteSlice};
use gix::hash::ObjectId;

/// `git ls-files` — list entries of the index.
///
/// Supported invocations (all index-only, no worktree scan required):
///   * `git ls-files`                    → one path per entry (cwd-relative)
///   * `git ls-files -c` / `--cached`    → same as the default
///   * `git ls-files -s` / `--stage`     → `<mode> <sha> <stage>\t<path>` per entry
///   * `git ls-files -u` / `--unmerged`  → stage lines, restricted to conflicted (stage > 0) entries
///   * `--full-name`                     → paths relative to the repository root, not cwd
///   * `-z`                              → NUL line terminators instead of newlines
///   * trailing pathspecs (after `--`)   → restrict the listing to matching entries
///
/// Flags that require inspecting the working tree (`-o/--others`, `-m/--modified`,
/// `-d/--deleted`, `-i/--ignored`, `-k`, `--directory`, `--eol`, `--error-unmatch`,
/// `--with-tree`, `--abbrev`, `--debug`, `-t`, `-v`, `-f`, exclude options, …) are
/// not backed by the index alone and are rejected with a precise message rather
/// than silently ignored.
pub fn ls_files(args: &[String]) -> Result<ExitCode> {
    let mut stage = false; // -s / --stage
    let mut unmerged = false; // -u / --unmerged
    let mut cached = false; // -c / --cached (default anyway)
    let mut zero = false; // -z
    let mut full_name = false; // --full-name
    let mut no_more_flags = false;
    let mut patterns: Vec<BString> = Vec::new();

    for a in args {
        if no_more_flags {
            patterns.push(BString::from(a.as_str()));
            continue;
        }
        match a.as_str() {
            "--" => no_more_flags = true,
            "-s" | "--stage" => stage = true,
            "-u" | "--unmerged" => unmerged = true,
            "-c" | "--cached" => cached = true,
            "-z" => zero = true,
            "--full-name" => full_name = true,
            other if other.starts_with('-') => {
                anyhow::bail!("unsupported flag {other:?} (only -c/--cached, -s/--stage, -u/--unmerged, --full-name, -z and pathspecs are ported)");
            }
            _ => patterns.push(BString::from(a.as_str())),
        }
    }
    let _ = cached; // accepted for compatibility; listing the cache is the default

    let repo = gix::discover(".")?;
    let index = repo.open_index()?;

    // Paths kept in the index are relative to the repository root. Unless
    // `--full-name` was requested, git prints them relative to the current
    // directory, i.e. with the repository-to-cwd prefix stripped off.
    let prefix: Option<BString> = if full_name {
        None
    } else {
        match repo.prefix()? {
            Some(p) if !p.as_os_str().is_empty() => {
                let mut b = gix::path::into_bstr(p).into_owned();
                b.push(b'/');
                Some(b)
            }
            _ => None,
        }
    };

    // Collect the matching entries. `empty_patterns_match_prefix = true` makes an
    // empty pathspec list resolve to "everything under the current prefix", which
    // is exactly git's default behaviour when run from a subdirectory.
    let mut rows: Vec<(BString, u32, ObjectId, u32)> = Vec::new();
    let mut ps = repo.pathspec(
        true,
        &patterns,
        false,
        &index,
        gix::worktree::stack::state::attributes::Source::IdMapping,
    )?;
    if let Some(iter) = ps.index_entries_with_paths(&index) {
        for (path, entry) in iter {
            rows.push((
                path.to_owned(),
                entry.mode.bits(),
                entry.id,
                entry.stage_raw(),
            ));
        }
    }

    let stage_format = stage || unmerged;
    let terminator = if zero { b'\0' } else { b'\n' };

    let stdout = std::io::stdout();
    let mut out = std::io::BufWriter::new(stdout.lock());

    for (path, mode, id, entry_stage) in &rows {
        // `-u`/`--unmerged` restricts output to conflicted (higher-stage) entries.
        if unmerged && *entry_stage == 0 {
            continue;
        }

        let display: &[u8] = match &prefix {
            Some(pref) => path
                .as_bytes()
                .strip_prefix(pref.as_bytes())
                .unwrap_or_else(|| path.as_bytes()),
            None => path.as_bytes(),
        };

        if stage_format {
            // `<mode-octal> <sha> <stage>\t<path>`, matching `git ls-files -s`.
            write!(out, "{mode:06o} {id} {entry_stage}\t")?;
        }
        out.write_all(display)?;
        out.write_all(&[terminator])?;
    }

    out.flush()?;
    Ok(ExitCode::SUCCESS)
}

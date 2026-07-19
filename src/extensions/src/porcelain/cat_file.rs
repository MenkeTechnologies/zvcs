use anyhow::{anyhow, bail, Result};
use std::io::Write;
use std::process::ExitCode;

use gix::objs::tree::EntryKind;
use gix::objs::{Kind, TreeRefIter};

/// `git cat-file` — inspect a single object in the database.
///
/// Supports the three canonical query modes:
///   * `git cat-file -t <object>` → object type (`blob`/`tree`/`commit`/`tag`)
///   * `git cat-file -s <object>` → object size in bytes
///   * `git cat-file -p <object>` → pretty-printed content
///
/// `<object>` is any revision spec gix can resolve (a full/abbreviated hash,
/// `HEAD`, `HEAD:path`, `<rev>^{tree}`, …). `-t`/`-s` read only the object
/// header, avoiding a full decode. `-p` mirrors git: raw bytes for
/// blob/commit/tag, and a `ls-tree`-style listing for a tree.
///
/// Batch modes (`--batch`, `--batch-check`), the existence probe (`-e`),
/// the bare `<type> <object>` form, and content filters (`--textconv`,
/// `--filters`) are not ported and fail with a precise message.
pub fn cat_file(args: &[String]) -> Result<ExitCode> {
    // Exactly one of -t/-s/-p selects the mode; the object spec is the single
    // positional argument.
    let mut mode: Option<char> = None;
    let mut spec: Option<&str> = None;

    for arg in args {
        match arg.as_str() {
            "-t" | "-s" | "-p" => {
                let next = arg.as_bytes()[1] as char;
                if let Some(prev) = mode {
                    if prev != next {
                        bail!("only one of -t, -s, -p may be given");
                    }
                }
                mode = Some(next);
            }
            "-e" => bail!("existence check (-e) is not ported"),
            "--batch" | "--batch-check" | "--batch-all-objects" => {
                bail!("batch mode ({arg}) is not ported")
            }
            "--textconv" | "--filters" => bail!("content filters ({arg}) are not ported"),
            other if other.starts_with('-') => bail!("unsupported flag {other}"),
            other => {
                if spec.is_some() {
                    bail!("too many arguments, expected a single object");
                }
                spec = Some(other);
            }
        }
    }

    let mode = mode.ok_or_else(|| anyhow!("one of -t, -s, -p is required"))?;
    let spec = spec.ok_or_else(|| anyhow!("an object name is required"))?;

    let repo = gix::discover(".")?;

    let id = repo
        .rev_parse_single(spec)
        .map_err(|_| anyhow!("Not a valid object name {spec}"))?;
    let oid = id.detach();

    match mode {
        't' => {
            let header = repo.find_header(oid)?;
            println!("{}", header.kind());
        }
        's' => {
            let header = repo.find_header(oid)?;
            println!("{}", header.size());
        }
        'p' => {
            let object = repo.find_object(oid)?;
            let stdout = std::io::stdout();
            let mut out = stdout.lock();
            if object.kind == Kind::Tree {
                // ls-tree-style: `<mode6> <type> <hash>\t<name>` per entry.
                for entry in TreeRefIter::from_bytes(&object.data, oid.kind()) {
                    let entry = entry.map_err(|e| anyhow!("failed to decode tree {oid}: {e}"))?;
                    let typ = match entry.mode.kind() {
                        EntryKind::Tree => "tree",
                        EntryKind::Commit => "commit",
                        _ => "blob",
                    };
                    write!(out, "{:06o} {} {}\t", entry.mode.value(), typ, entry.oid)?;
                    let name: &[u8] = entry.filename.as_ref();
                    out.write_all(name)?;
                    out.write_all(b"\n")?;
                }
            } else {
                // blob / commit / tag: raw content, no added newline.
                out.write_all(&object.data)?;
            }
            out.flush()?;
        }
        _ => unreachable!("mode is validated to be one of t/s/p"),
    }

    Ok(ExitCode::SUCCESS)
}

use anyhow::Result;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use gix::refs::transaction::{Change, LogChange, PreviousValue, RefEdit, RefLog};
use gix::refs::{FullName, Target};

use crate::lock::RepoLock;

/// `git init` — create an empty repository (worktree or `--bare`).
///
/// Ported onto gitoxide's `gix::init` / `gix::init_bare`, which lay down the
/// same on-disk layout git does (`.git/{HEAD,config,objects,refs,hooks,info}`)
/// with an unborn `HEAD` pointing at the initial branch. The initial branch is
/// resolved with git's exact precedence: `-b`/`--initial-branch` on the command
/// line, else the `init.defaultBranch` config value, else the compiled-in
/// default `master`. (gix's own fallback is `main`; this port overrides it to
/// git's `master` so the no-config case matches stock git byte-for-byte.)
/// Output mirrors stock git:
///   * fresh repo:    `Initialized empty Git repository in <gitdir>/`
///   * existing repo: `Reinitialized existing Git repository in <gitdir>/`
///   * with `--shared`: the word `shared ` is inserted before `Git repository`.
///
/// Supported invocation forms:
///   * `git init [<directory>]`
///   * `git init --bare [<directory>]`                    (also into a non-empty dir)
///   * `git init -b <name>` / `--initial-branch=<name>`   (sets `HEAD` symref)
///   * `git init -q` / `--quiet`                          (suppresses the line)
///   * `git init --template=<dir>` / `--template <dir>`   (seed from a template)
///   * `git init --separate-git-dir=<gitdir>`             (real git dir elsewhere + `.git` link file)
///   * `git init --shared[=<permissions>]`                (group/world/octal sharing)
///   * `--` to terminate option parsing
///
/// Ported from git's `builtin/init-db.c` + `setup.c` (`create_default_files`,
/// `copy_templates_1`, `separate_git_dir`) and `path.c`
/// (`calc_shared_perm`/`adjust_shared_perm`). The `--shared` permission math,
/// the `core.sharedrepository`/`receive.denyNonFastforwards` config values, the
/// template merge semantics, and the `gitdir:` link file all match stock git.
///
/// # Deviations (surfaced honestly, never faked)
///   * Reinitialization prints the git message and succeeds but applies no new
///     options: `--template`, `--shared`, and `--separate-git-dir` migration are
///     only honored on a *fresh* init. gix exposes no reinit path, so an
///     already-initialized repo is not re-templated, re-shared, or migrated to a
///     separate git dir. The overwhelmingly common `git init` into a fresh dir
///     is unaffected.
///   * `--object-format=sha256` and `--ref-format=reftable` are rejected (not
///     "silently accepted"): the sha256 write path is unverified against gix and
///     there is no vendored reftable backend. `--object-format=sha1` /
///     `--ref-format=files` (the defaults) are likewise not special-cased.
pub fn init(args: &[String]) -> Result<ExitCode> {
    let mut bare = false;
    let mut quiet = false;
    let mut initial_branch: Option<String> = None;
    let mut directory: Option<String> = None;
    let mut template: Option<String> = None;
    let mut separate_git_dir: Option<String> = None;
    // The `git_config_perm` value: `None` = `--shared` not given; `Some(0)` =
    // umask/false (no sharing); `Some(0o660)` = group; `Some(0o664)` = everybody;
    // `Some(neg)` = an explicit `0xxx` file mode (stored negated, as git does).
    let mut shared: Option<i32> = None;
    let mut positional_only = false;

    let mut i = 0;
    while i < args.len() {
        let arg = &args[i];
        if positional_only || !arg.starts_with('-') || arg == "-" {
            if directory.is_some() {
                anyhow::bail!("too many arguments, expected at most one directory");
            }
            directory = Some(arg.clone());
            i += 1;
            continue;
        }
        match arg.as_str() {
            "--" => positional_only = true,
            "--bare" => bare = true,
            "-q" | "--quiet" => quiet = true,
            "-b" | "--initial-branch" => {
                i += 1;
                let name = args
                    .get(i)
                    .ok_or_else(|| anyhow::anyhow!("option `{arg}' requires a value"))?;
                initial_branch = Some(name.clone());
            }
            "--template" => {
                i += 1;
                let dir = args
                    .get(i)
                    .ok_or_else(|| anyhow::anyhow!("option `{arg}' requires a value"))?;
                template = Some(dir.clone());
            }
            "--separate-git-dir" => {
                i += 1;
                let dir = args
                    .get(i)
                    .ok_or_else(|| anyhow::anyhow!("option `{arg}' requires a value"))?;
                separate_git_dir = Some(dir.clone());
            }
            // `--shared` takes an OPTIONAL argument, attached with `=` only (git's
            // PARSE_OPT_OPTARG). `--shared group` treats `group` as a positional.
            "--shared" => shared = Some(0o660),
            _ if arg.starts_with("--initial-branch=") => {
                initial_branch = Some(arg["--initial-branch=".len()..].to_string());
            }
            _ if arg.starts_with("--template=") => {
                template = Some(arg["--template=".len()..].to_string());
            }
            _ if arg.starts_with("--separate-git-dir=") => {
                separate_git_dir = Some(arg["--separate-git-dir=".len()..].to_string());
            }
            _ if arg.starts_with("--shared=") => {
                shared = Some(parse_shared_value(&arg["--shared=".len()..])?);
            }
            _ if arg.starts_with("-b") => {
                initial_branch = Some(arg[2..].to_string());
            }
            _ => anyhow::bail!("unknown option `{arg}'"),
        }
        i += 1;
    }

    // umask/false/0 leaves the repository unshared, exactly like `--shared` never
    // being passed (git's init_shared_repository == 0 is falsy).
    let shared = shared.filter(|&s| s != 0);

    // git refuses to combine these (builtin/init-db.c: "cannot be used together").
    if separate_git_dir.is_some() && bare {
        anyhow::bail!(
            "options '--separate-git-dir' and '--bare' cannot be used together"
        );
    }

    let target = PathBuf::from(directory.as_deref().unwrap_or("."));

    // Detect an already-initialized repository at the target so we can emit the
    // `Reinitialized existing ...` line instead of failing. For a worktree repo
    // the git dir is `<target>/.git`; for a bare repo it is `<target>` itself,
    // recognized by its `HEAD` file at the root.
    let existing_git_dir: Option<PathBuf> = {
        let dot_git = target.join(".git");
        if dot_git.exists() {
            Some(dot_git)
        } else if target.join("HEAD").is_file() && target.join("objects").is_dir() {
            Some(target.clone())
        } else {
            None
        }
    };

    if let Some(git_dir) = existing_git_dir {
        if !quiet {
            println!(
                "Reinitialized existing Git repository in {}",
                display_git_dir(&git_dir)
            );
        }
        return Ok(ExitCode::SUCCESS);
    }

    // Create the repository. gix lays down the full template + config and returns
    // an opened handle with an unborn HEAD on the default branch. gix refuses
    // `--bare` into a non-empty directory where stock git permits it, so fall
    // back to a scratch-dir build in that one case.
    let repo = if bare {
        match gix::init_bare(&target) {
            Ok(r) => r,
            Err(gix::init::Error::Init(gix::create::Error::DirectoryNotEmpty { .. })) => {
                init_bare_into_nonempty(&target)?
            }
            Err(e) => return Err(anyhow::anyhow!("{e}")),
        }
    } else {
        gix::init(&target).map_err(|e| anyhow::anyhow!("{e}"))?
    };

    // Resolve the initial branch name, matching git's precedence exactly:
    //   1. `-b <name>` / `--initial-branch=<name>` on the command line, else
    //   2. the `init.defaultBranch` config value (any scope), else
    //   3. the compiled-in default `master`.
    // gix::init already points the unborn HEAD at `init.defaultBranch` (or its
    // own `main` fallback when that is unset), so recomputing here is what lets
    // the no-config case land on git's `master` rather than gix's `main`. When
    // the name gix already chose matches, the HEAD repoint below is a no-op.
    let branch_name = match initial_branch {
        Some(name) => name,
        None => repo
            .config_snapshot()
            .string("init.defaultBranch")
            .map(|v| v.to_string())
            .filter(|v| !v.trim().is_empty())
            .unwrap_or_else(|| "master".to_string()),
    };

    // Repoint the unborn HEAD symref to the resolved branch. This is a ref
    // mutation, so serialize it through the repo coordinator like every other
    // write command. gix writes no reflog for a symbolic update to an unborn
    // branch, matching stock git init (which creates no `logs/HEAD`). For a
    // separate git dir this happens before the git dir is moved, so the lock and
    // ref edit still target the freshly-created `<target>/.git`.
    let branch: FullName = format!("refs/heads/{branch_name}")
        .try_into()
        .map_err(|e| anyhow::anyhow!("invalid initial branch name {branch_name:?}: {e}"))?;
    let src_git_dir = repo.git_dir().to_path_buf();
    {
        let _lock = RepoLock::acquire(&src_git_dir);
        repo.edit_reference(RefEdit {
            change: Change::Update {
                log: LogChange {
                    mode: RefLog::AndReference,
                    force_create_reflog: false,
                    message: "init: set initial branch".into(),
                },
                expected: PreviousValue::Any,
                new: Target::Symbolic(branch),
            },
            name: "HEAD"
                .try_into()
                .map_err(|e| anyhow::anyhow!("invalid ref name HEAD: {e}"))?,
            deref: false,
        })
        .map_err(|e| anyhow::anyhow!("{e}"))?;
    }

    // `--separate-git-dir`: relocate the git dir to the requested path and drop a
    // `gitdir: <abs>` link file in its place, exactly like git's
    // `separate_git_dir()` (`setup.c`). The message then names the real git dir.
    let git_dir: PathBuf = match &separate_git_dir {
        Some(real) => relocate_git_dir(&src_git_dir, &target, real)?,
        None => src_git_dir.clone(),
    };

    // `--template=<dir>`: seed the git dir from the template, replacing gix's
    // built-in default template so ONLY the requested template's files remain
    // (matching git, which uses the given template dir instead of the default,
    // not in addition to it). Structural files stay in place.
    if let Some(tpl) = template.as_deref().filter(|t| !t.is_empty()) {
        apply_template(tpl, &git_dir)?;
    }

    // `--shared[=...]`: record the sharing config and widen permissions across the
    // whole git dir, porting git's `create_default_files` config write and
    // `adjust_shared_perm` (which git applies per-file during creation; a single
    // recursive walk here produces the identical on-disk result).
    if let Some(shared) = shared {
        write_shared_config(&git_dir, shared)?;
        #[cfg(unix)]
        adjust_shared_perm_recursive(&git_dir, shared)?;
    }

    if !quiet {
        let shared_word = if shared.is_some() { "shared " } else { "" };
        println!(
            "Initialized empty {shared_word}Git repository in {}",
            display_git_dir(&git_dir)
        );
    }
    Ok(ExitCode::SUCCESS)
}

/// Build a bare repository inside a non-empty `target`. gix hard-refuses this
/// (`create::into` checks emptiness unconditionally for bare), while stock git
/// permits it. Lay the layout down in an empty scratch subdirectory, then move
/// each entry up into `target`, yielding the same on-disk result git produces.
fn init_bare_into_nonempty(target: &Path) -> Result<gix::Repository> {
    std::fs::create_dir_all(target)?;
    let scratch = target.join(format!(".git-init-scratch-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&scratch);
    gix::init_bare(&scratch).map_err(|e| anyhow::anyhow!("{e}"))?;
    for entry in std::fs::read_dir(&scratch)? {
        let entry = entry?;
        std::fs::rename(entry.path(), target.join(entry.file_name()))?;
    }
    std::fs::remove_dir(&scratch)?;
    gix::open(target).map_err(|e| anyhow::anyhow!("{e}"))
}

/// Move the freshly-created git dir (`src`, i.e. `<target>/.git`) to the
/// requested `real` location and write a `gitdir: <abs>` link file at
/// `<target>/.git`. Returns the absolute real git dir. Ports `separate_git_dir()`
/// from git's `setup.c` for the fresh-init case.
fn relocate_git_dir(src: &Path, target: &Path, real: &str) -> Result<PathBuf> {
    let real_pb = {
        let p = PathBuf::from(real);
        if p.is_absolute() {
            p
        } else {
            std::env::current_dir()?.join(p)
        }
    };
    if let Some(parent) = real_pb.parent().filter(|p| !p.as_os_str().is_empty()) {
        std::fs::create_dir_all(parent)?;
    }
    // Resolve symlinks in the (now-existing) parent, keeping the leaf, so the link
    // file records the same absolute path git would (git real-paths the git dir).
    let real_abs = match (real_pb.parent(), real_pb.file_name()) {
        (Some(par), Some(leaf)) => std::fs::canonicalize(par)
            .map(|c| c.join(leaf))
            .unwrap_or_else(|_| real_pb.clone()),
        _ => real_pb.clone(),
    };

    std::fs::rename(src, &real_abs).map_err(|e| {
        anyhow::anyhow!(
            "unable to move {} to {}: {e}",
            src.display(),
            real_abs.display()
        )
    })?;
    std::fs::write(
        target.join(".git"),
        format!("gitdir: {}\n", real_abs.display()),
    )?;
    Ok(real_abs)
}

/// Seed `git_dir` from the template at `template`. Ports git's `copy_templates`
/// + `copy_templates_1`: entries whose name starts with `.` are skipped,
/// directories are merged, existing files are left untouched, symlinks are
/// recreated, and regular files are copied preserving their source mode.
///
/// gix already populated its built-in default template (sample hooks,
/// `info/exclude`, `description`) inside `git_dir`; git, given `--template`, uses
/// that dir *instead of* the default. So the default-template artifacts are
/// stripped first, letting the requested template fully define the
/// template-provided files while structural files (`HEAD`, `config`, `objects/`,
/// `refs/`) remain.
fn apply_template(template: &str, git_dir: &Path) -> Result<()> {
    let src = {
        let p = PathBuf::from(template);
        if p.is_absolute() {
            p
        } else {
            std::env::current_dir()?.join(p)
        }
    };
    // git warns and skips when the template dir cannot be opened.
    if std::fs::read_dir(&src).is_err() {
        eprintln!("warning: templates not found in {template}");
        return Ok(());
    }
    strip_default_template(git_dir)?;
    copy_template_dir(&src, git_dir)?;
    Ok(())
}

/// Remove gix's built-in default-template files so a `--template` dir can fully
/// replace them. Only the template-provided paths are touched
/// (`description`, `info/exclude` + the now-empty `info/`, and everything under
/// `hooks/` + the now-empty `hooks/`); structural files are left in place. Empty
/// directories are removed only when they end up empty, so a template that omits
/// them leaves them absent, matching git.
fn strip_default_template(git_dir: &Path) -> Result<()> {
    let _ = std::fs::remove_file(git_dir.join("description"));
    let _ = std::fs::remove_file(git_dir.join("info").join("exclude"));
    let _ = std::fs::remove_dir(git_dir.join("info"));

    let hooks = git_dir.join("hooks");
    if let Ok(entries) = std::fs::read_dir(&hooks) {
        for entry in entries {
            let _ = std::fs::remove_file(entry?.path());
        }
    }
    let _ = std::fs::remove_dir(&hooks);
    Ok(())
}

/// Recursively copy `src` into `dst` with git's `copy_templates_1` semantics.
fn copy_template_dir(src: &Path, dst: &Path) -> Result<()> {
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let name = entry.file_name();
        if name.to_string_lossy().starts_with('.') {
            continue;
        }
        let s = entry.path();
        let d = dst.join(&name);
        let meta = std::fs::symlink_metadata(&s)?;
        let ft = meta.file_type();
        if ft.is_dir() {
            copy_template_dir(&s, &d)?;
        } else if d.exists() {
            // git's copy_templates_1 never overwrites an existing file.
            continue;
        } else if ft.is_symlink() {
            #[cfg(unix)]
            std::os::unix::fs::symlink(std::fs::read_link(&s)?, &d)?;
            #[cfg(not(unix))]
            {
                std::fs::copy(&s, &d)?;
            }
        } else if ft.is_file() {
            std::fs::copy(&s, &d)?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(
                    &d,
                    std::fs::Permissions::from_mode(meta.permissions().mode()),
                )?;
            }
        }
    }
    Ok(())
}

/// Parse a `--shared=<value>` argument into git's `git_config_perm` result:
/// `umask`/`false` → 0; `group`/`true` → `0o660`; `all`/`world`/`everybody` →
/// `0o664`; a `0`/`1`/`2` compatibility number → 0/`0o660`/`0o664`; any other
/// octal `0xxx` file mode → `-(mode & 0o666)` (stored negated). An octal mode
/// that would deny the owner read+write is rejected, exactly like git.
fn parse_shared_value(value: &str) -> Result<i32> {
    match value {
        "umask" => return Ok(0),
        "group" => return Ok(0o660),
        "all" | "world" | "everybody" => return Ok(0o664),
        _ => {}
    }
    match parse_octal_full(value) {
        Some(0) => Ok(0),
        Some(1) => Ok(0o660),
        Some(2) => Ok(0o664),
        Some(mode) => {
            if (mode & 0o600) != 0o600 {
                anyhow::bail!(
                    "problem with core.sharedRepository filemode value (0{mode:03o}).\n\
                     The owner of files must always have read and write permissions."
                );
            }
            Ok(-(mode & 0o666))
        }
        // Not an octal number: fall back to boolean, like git_config_bool.
        None => Ok(if parse_bool(value) { 0o660 } else { 0 }),
    }
}

/// Whole-string octal parse mirroring C's `strtol(value, &endptr, 8)` with
/// `*endptr == 0`: an empty string is 0 (as `strtol` reports), a fully-octal
/// string is its value, anything else is `None` (falls through to boolean).
fn parse_octal_full(s: &str) -> Option<i32> {
    if s.is_empty() {
        return Some(0);
    }
    if s.bytes().all(|b| b.is_ascii_digit() && b <= b'7') {
        i32::from_str_radix(s, 8).ok()
    } else {
        None
    }
}

/// git's boolean truthy spellings for the `--shared` fallback.
fn parse_bool(s: &str) -> bool {
    matches!(s.to_ascii_lowercase().as_str(), "true" | "yes" | "on")
}

/// Write `core.sharedrepository` and `receive.denyNonFastforwards` into the git
/// dir's config, porting the config write in git's `create_default_files`.
/// The stored value uses git's compatibility encoding: `1` for group, `2` for
/// everybody, `0xxx` for an explicit file mode.
fn write_shared_config(git_dir: &Path, shared: i32) -> Result<()> {
    let value = if shared < 0 {
        format!("0{:o}", -shared)
    } else if shared == 0o660 {
        "1".to_string()
    } else if shared == 0o664 {
        "2".to_string()
    } else {
        anyhow::bail!("invalid value for shared repository");
    };

    let path = git_dir.join("config");
    let mut file =
        gix::config::File::from_path_no_includes(path.clone(), gix::config::Source::Local)
            .map_err(|e| anyhow::anyhow!("{e}"))?;
    file.set_raw_value_by("core", None, "sharedrepository", value.as_str())?;
    file.set_raw_value_by("receive", None, "denyNonFastforwards", "true")?;
    std::fs::write(&path, file.to_bstring())?;
    Ok(())
}

/// git's `FORCE_DIR_SET_GID`: `git-compat-util.h` defaults it to `S_ISGID`
/// (`#ifndef FORCE_DIR_SET_GID #define FORCE_DIR_SET_GID S_ISGID`), so a shared
/// directory that grants any group access is forced set-gid. No config.mak.uname
/// entry for the platforms zvcs targets (Darwin, Linux) undefines it — verified
/// against stock git, which stamps `.git/` `2775` under `--shared=group`.
#[cfg(unix)]
const FORCE_DIR_SET_GID: bool = true;

/// Port of git's `calc_shared_perm` (`path.c`): widen `mode` according to the
/// stored shared value. Positive values OR in extra bits; a negative value forces
/// the low 9 bits to the requested file mode.
#[cfg(unix)]
fn calc_shared_perm(shared: i32, mode: u32) -> u32 {
    const S_IWUSR: u32 = 0o200;
    const S_IXUSR: u32 = 0o100;

    let mut tweak: i32 = if shared < 0 { -shared } else { shared };
    if mode & S_IWUSR == 0 {
        tweak &= !0o222;
    }
    if mode & S_IXUSR != 0 {
        // Copy read bits to execute bits.
        tweak |= (tweak & 0o444) >> 2;
    }
    let mode = mode as i32;
    let new = if shared < 0 {
        (mode & !0o777) | tweak
    } else {
        mode | tweak
    };
    new as u32
}

/// Port of git's `adjust_shared_perm` (`path.c`), applied recursively so the
/// whole git dir git built up file-by-file ends up with the same modes. For
/// directories, read bits are copied to execute bits and — where git does — the
/// set-gid bit is forced when any group access is granted. Symlinks are left
/// untouched (git init creates none, and chmod through them is undesirable).
#[cfg(unix)]
fn adjust_shared_perm_recursive(path: &Path, shared: i32) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;

    const S_ISGID: u32 = 0o2000;

    let meta = std::fs::symlink_metadata(path)?;
    if meta.file_type().is_symlink() {
        return Ok(());
    }
    let old = meta.permissions().mode();
    let is_dir = meta.is_dir();

    let mut new = calc_shared_perm(shared, old);
    if is_dir {
        new |= (new & 0o444) >> 2;
        if FORCE_DIR_SET_GID && (new & 0o60) != 0 {
            new |= S_ISGID;
        }
    }

    if (old & 0o7777) != (new & 0o7777) {
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(new & 0o7777))?;
    }

    if is_dir {
        for entry in std::fs::read_dir(path)? {
            adjust_shared_perm_recursive(&entry?.path(), shared)?;
        }
    }
    Ok(())
}

/// Render a git-dir path the way stock git does in the init message: an absolute,
/// symlink-resolved path with a trailing slash. Falls back to the given path when
/// canonicalization is unavailable (should not happen for a just-created dir).
fn display_git_dir(git_dir: &Path) -> String {
    let abs = std::fs::canonicalize(git_dir).unwrap_or_else(|_| git_dir.to_path_buf());
    format!("{}/", abs.display())
}

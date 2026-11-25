//! `git var` — print a Git logical variable.
//!
//! Backed entirely by the vendored gitoxide (`src/ported`). The logical variables
//! are recomputed here from the same inputs stock git uses (environment, then the
//! merged configuration, then a compiled-in default), because upstream's
//! `builtin/var.c` reads them from git-internal globals (`editor_program`,
//! `pager_program`, `ident_default_*`) that have no single gitoxide equivalent.
//! The path-valued variables resolve through `gix_config::Source::storage_location`
//! and `gix_path::env`, which are the vendored crates' own answer to git's
//! `system_path()` / `xdg_config()`.
//!
//! Unlike most porcelain here, `git var` works outside a repository; when
//! `gix::discover` fails the configuration falls back to
//! `gix::config::File::from_globals()` plus the `GIT_CONFIG_*` environment
//! overrides, which is the same set git reads without a repo.
//!
//! ### Covered (byte-identical stdout and exit code against stock git)
//!
//! * `git var <variable>` for every variable stock git 2.55 knows:
//!   `GIT_AUTHOR_IDENT`, `GIT_COMMITTER_IDENT`, `GIT_EDITOR`,
//!   `GIT_SEQUENCE_EDITOR`, `GIT_PAGER`, `GIT_DEFAULT_BRANCH`, `GIT_SHELL_PATH`,
//!   `GIT_ATTR_SYSTEM`, `GIT_ATTR_GLOBAL`, `GIT_CONFIG_SYSTEM`,
//!   `GIT_CONFIG_GLOBAL`
//! * exit 1 with no output when the variable has no value (`GIT_ATTR_NOSYSTEM=1`
//!   with `GIT_ATTR_SYSTEM`, `GIT_CONFIG_NOSYSTEM=1` with `GIT_CONFIG_SYSTEM`,
//!   a dumb terminal and no editor set with `GIT_EDITOR`, …)
//! * `git var -l` — the merged configuration as `key=value` (git's `config list`
//!   order and normalisation, multivars in file order rather than grouped by
//!   key), followed by the logical variables in git's own table order, skipping
//!   any that have no value. The `gitoxide.*` layers `gix` synthesizes from the
//!   ambient environment and from zvcs' API-scope defaults are not git
//!   configuration and are excluded, exactly as `git config --list` excludes them
//! * `-h` — git's usage line on stdout, exit 0; no argument, an unknown
//!   variable, or more than one argument — the same line on stderr, exit 129
//! * `GIT_CONFIG_GLOBAL` correctly collapses to a single line when the
//!   environment variable of that name is set, and expands to the XDG path plus
//!   `$HOME/.gitconfig` otherwise
//!
//! ### Honest limitations
//!
//! * The ident variables are `fmt_ident()`'s, auto-detection from the passwd
//!   entry and the host name included (see [`ident`]); only a uid with no passwd
//!   entry, which git reports as a bogus name, is not modelled.
//! * `GIT_ATTR_SYSTEM` and `GIT_CONFIG_SYSTEM` hang off git's *installation
//!   prefix*, which git resolves in `exec-cmd.c`'s `system_prefix()` — the
//!   compiled-in `prefix` for an ordinary build, or, under `RUNTIME_PREFIX`, the
//!   directory of the running binary with `libexec/git-core`, `bin` or `git`
//!   stripped off it. A compiled-in constant of *another* program is not readable,
//!   so [`installation_prefix`] recovers it the two ways it is observable at
//!   runtime, in order: ask the git installation for its own `GIT_ATTR_SYSTEM`,
//!   or where its `etc/gitconfig` lives (see [`installation_etc`]), then fall
//!   back to `system_prefix()`'s suffix-stripping applied to this binary's own
//!   location, which is the right answer once zvcs is itself installed under a prefix. `gix_path::env::system_prefix()`
//!   — a flat `/` off Windows — is the last resort, correct only for a git
//!   installed under `/`. The probe deliberately ignores `GIT_CONFIG_SYSTEM` and
//!   `GIT_CONFIG_NOSYSTEM`, which relocate or switch off the system config *scope*
//!   without moving the installation — matching git, which keeps printing
//!   `GIT_ATTR_SYSTEM` under `GIT_CONFIG_NOSYSTEM=1`.
//! * `init.defaultBranch` is run through the same `refs/heads/<name>` validation
//!   `git_default_branch_name()` applies, so a name `check_refname_format()`
//!   rejects is `fatal: invalid branch name: init.defaultBranch = <value>` at
//!   128 rather than being echoed.

use anyhow::Result;
use std::io::Write;
use std::path::Path;
use std::process::ExitCode;

use gix::bstr::BString;
use gix::config::{File as ConfigFile, Source};

/// Stock git's `var` usage line, byte-for-byte. Printed on `-h` (stdout) and on
/// any argument error (stderr); both exit 129.
const USAGE: &str = "usage: git var (-l | <variable>)\n";

/// The logical variables in the order `builtin/var.c` declares them, which is the
/// order `git var -l` emits them in.
const VARS: &[&str] = &[
    "GIT_COMMITTER_IDENT",
    "GIT_AUTHOR_IDENT",
    "GIT_EDITOR",
    "GIT_SEQUENCE_EDITOR",
    "GIT_PAGER",
    "GIT_DEFAULT_BRANCH",
    "GIT_SHELL_PATH",
    "GIT_ATTR_SYSTEM",
    "GIT_ATTR_GLOBAL",
    "GIT_CONFIG_SYSTEM",
    "GIT_CONFIG_GLOBAL",
];

/// `git var` — show a Git logical variable.
///
/// See the module docs for the covered surface and the remaining limitations
/// (an absent passwd entry and the system prefix).
pub fn var(args: &[String]) -> Result<ExitCode> {
    // Dispatch passes the argument list without the subcommand; tolerate a
    // leading `var` so the function is correct under either calling convention.
    let argv: &[String] = match args.first() {
        Some(first) if first == "var" => &args[1..],
        _ => args,
    };

    // `show_usage_if_asked(argc, argv, var_usage)` (builtin/var.c:225): a LONE
    // `-h` only. `git var -h <anything>` is an ordinary command line, and the
    // wrong-arity `usage()` below answers it on stderr.
    if let Some(code) = super::show_usage_if_asked(argv, USAGE) {
        return Ok(code);
    }

    let [request] = argv else {
        return Ok(usage_error());
    };

    let cfg = load_config()?;

    let stdout = std::io::stdout();
    let mut out = stdout.lock();

    if request == "-l" {
        // `show_config()` prints each value and then chains to
        // `git_default_config()` (builtin/var.c:207-215), so a value that
        // callback refuses dies right after its own line is printed. The
        // dispatch gate skips `var -l` for this reason.
        let refused = match crate::setup::discover() {
            Ok(repo) => crate::default_config::validate_counted(&crate::config::walk_config(&repo)).err(),
            Err(_) => None,
        };
        if let Some((at, rejection)) = refused {
            // The listing sits in stdio's buffer when `die()` writes to stderr,
            // and reaches a non-terminal stdout only at `exit()`.
            let mut listed = Vec::new();
            list_config(&cfg, &mut listed, Some(at + 1))?;
            crate::cstdio::defer();
            crate::cstdio::write_bytes(&listed);
            return Err(rejection.into_error());
        }
        list_config(&cfg, &mut out, None)?;
        for name in VARS {
            let Some(values) = resolve(name, &cfg, false)? else {
                continue;
            };
            for value in values {
                write!(out, "{name}=")?;
                out.write_all(&value)?;
                out.write_all(b"\n")?;
            }
        }
        return Ok(ExitCode::SUCCESS);
    }

    if !VARS.contains(&request.as_str()) {
        return Ok(usage_error());
    }

    match resolve(request, &cfg, true)? {
        // A variable with no value prints nothing and exits 1.
        None => Ok(ExitCode::from(1)),
        Some(values) => {
            for value in values {
                out.write_all(&value)?;
                out.write_all(b"\n")?;
            }
            Ok(ExitCode::SUCCESS)
        }
    }
}

/// Git's argument-error path: the usage line on stderr, exit 129.
fn usage_error() -> ExitCode {
    eprint!("{USAGE}");
    ExitCode::from(129)
}

/// The configuration `git var` reads.
///
/// Inside a repository this is the repository's fully-merged snapshot (system +
/// global + local + environment). Outside one — `git var` is legal there — it is
/// the global set plus the `GIT_CONFIG_*` overrides, which is what git falls back
/// to when there is no repository.
fn load_config() -> Result<ConfigFile> {
    match crate::setup::discover() {
        Ok(repo) => Ok(repo.config_snapshot().plumbing().clone()),
        Err(_) => {
            let mut file = ConfigFile::from_globals()?;
            file.append(ConfigFile::from_environment_overrides()?)?;
            Ok(file)
        }
    }
}

/// Compute a logical variable, or `None` when it has no value (exit 1 territory).
///
/// The return is a list because `GIT_CONFIG_GLOBAL` legitimately has two values
/// (XDG then `$HOME/.gitconfig`), which git prints highest-priority first.
fn resolve(name: &str, cfg: &ConfigFile, strict: bool) -> Result<Option<Vec<BString>>> {
    let single = |s: Option<String>| -> Result<Option<Vec<BString>>> {
        Ok(s.map(|s| vec![BString::from(s)]))
    };

    match name {
        "GIT_AUTHOR_IDENT" => single(Some(ident(cfg, "Author", strict)?)),
        "GIT_COMMITTER_IDENT" => single(Some(ident(cfg, "Committer", strict)?)),
        "GIT_EDITOR" => single(crate::editor::select_editor(cfg_str(cfg, "core.editor"))),
        "GIT_SEQUENCE_EDITOR" => single(crate::editor::select_sequence_editor(
            cfg_str(cfg, "sequence.editor"),
            cfg_str(cfg, "core.editor"),
        )),
        "GIT_PAGER" => single(Some(pager(cfg))),
        "GIT_DEFAULT_BRANCH" => {
            // `git_default_branch_name()` (`refs.c`) composes `refs/heads/<name>`
            // out of the configured value and dies when `check_refname_format()`
            // rejects it — naming the config key in its display spelling and the
            // value it read, so an empty `init.defaultBranch` reports an empty
            // value rather than falling back to `master`.
            // `GIT_TEST_DEFAULT_INITIAL_BRANCH_NAME` is consulted ahead of the config
            // key and suppresses reading it entirely (refs.c:696-700).
            let name = crate::refname::default_branch_name_override()
                .or_else(|| cfg_str(cfg, "init.defaultBranch"))
                .unwrap_or_else(|| "master".into());
            let composed: std::result::Result<gix::refs::FullName, _> =
                format!("refs/heads/{name}").try_into();
            if composed.is_err() {
                crate::git_fatal!("invalid branch name: init.defaultBranch = {name}");
            }
            single(Some(name))
        }
        "GIT_SHELL_PATH" => Ok(Some(vec![os_bytes(gix::path::env::shell())])),
        "GIT_ATTR_SYSTEM" => Ok(attr_system().map(|v| vec![v])),
        "GIT_ATTR_GLOBAL" => Ok(attr_global(cfg)?.map(|v| vec![v])),
        "GIT_CONFIG_SYSTEM" => Ok(config_system().map(|v| vec![v])),
        "GIT_CONFIG_GLOBAL" => Ok(config_paths(&[Source::Git, Source::User])),
        _ => unreachable!("variable names are validated against VARS before resolve"),
    }
}

/// `GIT_AUTHOR_IDENT` / `GIT_COMMITTER_IDENT` — `Name <email> <seconds> <tz>`.
///
/// Port of `git_author_info()`/`git_committer_info()` (ident.c:565-589) over
/// `fmt_ident()` (ident.c:456-542). `role` is `Author` or `Committer`; `strict`
/// is `IDENT_STRICT`, which `cmd_var()` passes for a single variable and
/// `list_vars()` does not (builtin/var.c:240, :181).
///
/// * The email: `GIT_<ROLE>_EMAIL` when set at all, even empty; else a non-empty
///   `<role>.email`; else, unless a strict `user.useConfigOnly` refuses an
///   identity no `*.email` key gave, `ident_default_email()` — `user.email`, a
///   non-empty `EMAIL`, or the address built from the account and the host,
///   which a strict caller refuses when the host has no domain.
/// * The name the same way through `GIT_<ROLE>_NAME`, `<role>.name` and
///   `ident_default_name()`; an empty one is refused when strict and replaced by
///   the login name otherwise, and a strict name made only of `crud()` characters
///   is refused.
/// * Both are copied through `strbuf_addstr_without_crud()`, and the date is
///   `parse_date()` of a non-empty `GIT_<ROLE>_DATE`, else the current time.
///
/// `default_name_is_bogus` — a uid with no passwd entry — is not modelled:
/// [`crate::passwd_self`] substitutes `unknown`/`Unknown` without saying it did.
fn ident(cfg: &ConfigFile, role: &str, strict: bool) -> Result<String> {
    let upper = role.to_uppercase();
    let lower = role.to_lowercase();
    // `set_ident()` (ident.c:610-675): any of the three `*.email`, or of the three
    // `*.name`, keys marks `ident_config_given`, even with an empty value.
    let given = |field: &str| {
        ["user", "author", "committer"]
            .iter()
            .any(|section| cfg.string(&format!("{section}.{field}")).is_some())
    };
    let use_config_only = cfg.boolean("user.useConfigOnly").ok().flatten().unwrap_or(false);
    let refuse = |msg: String| -> Result<String> {
        crate::identity_advice(role);
        crate::git_fatal!("{msg}")
    };

    let email = match env(&format!("GIT_{upper}_EMAIL")).or_else(|| cfg_str(cfg, &format!("{lower}.email")).filter(|e| !e.is_empty())) {
        Some(email) => email,
        None => {
            if strict && use_config_only && !given("email") {
                return refuse("no email was given and auto-detection is disabled".into());
            }
            let (email, bogus) =
                crate::ident_default_email_bogus(cfg_str(cfg, "user.email").unwrap_or_default(), given("email"));
            if strict && bogus {
                return refuse(format!("unable to auto-detect email address (got '{email}')"));
            }
            email
        }
    };

    let (mut name, using_default) =
        match env(&format!("GIT_{upper}_NAME")).or_else(|| cfg_str(cfg, &format!("{lower}.name")).filter(|n| !n.is_empty())) {
            Some(name) => (name, false),
            None => {
                if strict && use_config_only && !given("name") {
                    return refuse("no name was given and auto-detection is disabled".into());
                }
                (crate::ident_default_name(cfg_str(cfg, "user.name").unwrap_or_default(), given("name")), true)
            }
        };
    if name.is_empty() {
        if strict {
            let msg = format!("empty ident name (for <{email}>) not allowed");
            return match using_default {
                true => refuse(msg),
                false => crate::git_fatal!("{msg}"),
            };
        }
        name = crate::passwd_self().0;
    }
    // `has_non_crud()` (ident.c:216-223) over `crud()` (ident.c:203-214).
    let crud = |c: u8| c <= 32 || b",:;<>\"\\'".contains(&c);
    if strict && name.bytes().all(crud) {
        crate::git_fatal!("name consists only of disallowed characters: {name}");
    }

    // `if (date_str && date_str[0])` (ident.c:533-538): an empty
    // `GIT_<ROLE>_DATE` is the current time, like an unset one. `parse_date()`
    // is `parse_date_basic()` — approxidate is not in this path, so a relative
    // date is an error here even though `commit --date=` would accept it.
    let time = match env(&format!("GIT_{upper}_DATE")).filter(|d| !d.is_empty()) {
        Some(raw) => match crate::date::parse_date_basic(&raw) {
            Some(time) => time,
            None => crate::git_fatal!("invalid date format: {raw}"),
        },
        None => gix::date::Time::now_local_or_utc(),
    };

    Ok(format!("{} <{}> {time}", without_crud(&name), without_crud(&email)))
}

/// `GIT_PAGER` — git's `git_pager(1)` chain, with `builtin/var.c`'s `cat` fallback.
///
/// `$GIT_PAGER`, then `core.pager`, then `$PAGER`; nothing found means the
/// compiled-in default `less`, while an empty value or a literal `cat` means "no
/// pager", which `var.c` renders as `cat`.
fn pager(cfg: &ConfigFile) -> String {
    match env("GIT_PAGER")
        .or_else(|| cfg_str(cfg, "core.pager"))
        .or_else(|| env("PAGER"))
    {
        None => "less".into(),
        Some(p) if p.is_empty() || p == "cat" => "cat".into(),
        Some(p) => p,
    }
}

/// `GIT_ATTR_SYSTEM` — `<installation prefix>/etc/gitattributes`, unless disabled.
///
/// Suppressed entirely by a true `GIT_ATTR_NOSYSTEM`, matching git's
/// `git_attr_system_is_enabled()`. Note that this is the *only* switch that
/// suppresses it: git derives the path from its installation prefix, so
/// `GIT_CONFIG_NOSYSTEM=1` and a redirected `GIT_CONFIG_SYSTEM` leave it intact.
///
/// git spells this `system_path(ETC_GITATTRIBUTES)`, and `ETC_GITATTRIBUTES` is
/// `<sysconfdir>/gitattributes` — absolute for a distribution build that sets an
/// absolute `sysconfdir` (Debian's `/etc/gitattributes`), and prefix-relative
/// otherwise (Homebrew's `<prefix>/etc/gitattributes`). Both cases fall out of
/// [`installation_prefix`]: its first source is the *directory* of the
/// installation's own `gitconfig`, which is that same `sysconfdir` whether or not
/// it sits under the prefix.
fn attr_system() -> Option<BString> {
    if env_bool("GIT_ATTR_NOSYSTEM") {
        return None;
    }
    Some(path_bytes(installation_prefix().etc().join("gitattributes")))
}

/// Where git's installation keeps `gitconfig` and `gitattributes`.
///
/// The two sources answer at different granularities and must not be conflated:
/// a probe of the installation yields the `etc` directory directly (`/etc` on
/// Debian, `<prefix>/etc` on Homebrew), while `exec-cmd.c`'s suffix stripping
/// yields the prefix, whose `etc` subdirectory is only a convention.
enum Installation {
    /// git's `sysconfdir`, as the installation itself reported it.
    Etc(std::path::PathBuf),
    /// git's `prefix`, from which `etc` is derived the way an ordinary build does.
    Prefix(std::path::PathBuf),
}

impl Installation {
    /// The directory `gitconfig` and `gitattributes` live in.
    fn etc(self) -> std::path::PathBuf {
        match self {
            Installation::Etc(dir) => dir,
            Installation::Prefix(prefix) => prefix.join("etc"),
        }
    }
}

/// git's `system_prefix()` (`exec-cmd.c`), recovered by a program that *shadows*
/// git rather than being the build that baked the prefix in.
///
/// Ordered by how directly each source observes the installation:
///
/// 1. **The installation's own answer** ([`installation_etc`]). This is the only
///    source that survives an absolute `sysconfdir`, where the config and
///    attributes files sit outside the prefix entirely.
/// 2. **This binary's location**, put through `system_prefix()`'s suffix chain —
///    `libexec/git-core`, then `bin`, then `git` (see [`strip_path_suffix`]).
///    That is exactly what a `RUNTIME_PREFIX` build of git computes, and it is
///    the right answer once zvcs is installed under a prefix of its own
///    (`<prefix>/bin/zvcs` → `<prefix>`). A build tree (`target/debug/git`) matches
///    no suffix, which is why it is not the first source.
/// 3. **`gix_path::env::system_prefix()`**, a flat `/` off Windows — git's
///    `FALLBACK_RUNTIME_PREFIX` role, and correct only for a git installed
///    under `/`.
fn installation_prefix() -> Installation {
    if let Some(etc) = installation_etc() {
        return Installation::Etc(etc.to_owned());
    }
    if let Some(prefix) = prefix_from_executable() {
        return Installation::Prefix(prefix);
    }
    Installation::Prefix(
        gix::path::env::system_prefix().map_or_else(|| std::path::PathBuf::from("/"), Path::to_owned),
    )
}

/// git's `strip_path_suffix()` (`path.c`), component-wise.
///
/// Returns `path` with a trailing `suffix` removed, or `None` when `path` does not
/// end in it. Matching whole components is what makes the removal a directory
/// boundary rather than a string one, so `/opt/homebrewbin` does not strip `bin`.
fn strip_path_suffix(path: &Path, suffix: &str) -> Option<std::path::PathBuf> {
    let mut left = path.components();
    let mut right = Path::new(suffix).components().rev().peekable();
    while right.peek().is_some() {
        if left.next_back()? != right.next()? {
            return None;
        }
    }
    let stripped = left.as_path();
    (!stripped.as_os_str().is_empty()).then(|| stripped.to_owned())
}

/// `system_prefix()`'s `RUNTIME_PREFIX` branch applied to the running binary.
///
/// `executable_dirname` is the directory of this executable, and the suffix chain
/// is git's own — `GIT_EXEC_PATH` (`libexec/git-core`), `BINDIR` (`bin`), then the
/// bare `git` a Git-for-Windows layout uses.
fn prefix_from_executable() -> Option<std::path::PathBuf> {
    let exe = crate::hosted::git_exe().ok()?;
    let dir = exe.parent()?;
    ["libexec/git-core", "bin", "git"]
        .iter()
        .find_map(|suffix| strip_path_suffix(dir, suffix))
}

/// The `sysconfdir` of the git installation, as the installation reports it.
///
/// Each `git` is asked first for `git var GIT_ATTR_SYSTEM`, which prints
/// `git_attr_system_file()`, that is `system_path(ETC_GITATTRIBUTES)` (attr.c:874-880):
/// the compiled-in path itself, whatever the prefix, `sysconfdir` or `RUNTIME_PREFIX`
/// of that build. Its directory is the `sysconfdir` `ETC_GITCONFIG` shares
/// (Makefile:651-652). A git whose `var` does not know the variable is asked where
/// its `gitconfig` lives instead. Asking for the variable first matters twice: git's
/// own `make install` writes no `etc/gitconfig`, so the `gitconfig` question alone
/// passed such an installation over for the next `git` on `PATH`; and an
/// installation whose highest-scoped file is not its system config (Xcode's git
/// reads `share/git-core/gitconfig` ahead of `/etc/gitconfig`) answered it with the
/// wrong directory.
///
/// The `gitconfig` probe is `gix_path::env::installation_config_unsuppressed()`'s,
/// argv for argv: `git config -lz --show-origin --name-only` run from `/` with the
/// repository- and scope-selecting variables neutralised, keeping the first
/// `file:` origin — the highest-scoped configuration file, which is the
/// installation's own. `GIT_CONFIG_SYSTEM` and `GIT_CONFIG_NOSYSTEM` are removed
/// because they relocate or switch off the system *scope* without moving the
/// installation, and this asks where git is installed; `GIT_ATTR_NOSYSTEM` is
/// removed for the same reason.
///
/// It differs in the one respect that matters for a binary that *is* `git` on this
/// machine's `PATH`: rather than probing whichever `git` `PATH` resolves first and
/// giving up if it does not answer, it walks every `git` on `PATH` and keeps the
/// first answer. A shadow that asks itself where git is installed learns nothing,
/// and gitoxide's probe — written for a library linked *into* a program that is
/// not git — has no reason to expect that. Candidates resolving to this very
/// executable are skipped outright, which is both the correct answer (we are not a
/// second installation) and what stops the probe from spawning itself.
///
/// Probed once per process; `git var` is the only caller and asks at most twice.
fn installation_etc() -> Option<&'static Path> {
    static ETC: std::sync::OnceLock<Option<std::path::PathBuf>> = std::sync::OnceLock::new();
    ETC.get_or_init(|| {
        let me = crate::hosted::git_exe().and_then(|p| p.canonicalize()).ok();
        let path = std::env::var_os("PATH")?;
        std::env::split_paths(&path)
            .filter(|dir| !dir.as_os_str().is_empty())
            .map(|dir| dir.join(gix::path::env::exe_invocation()))
            .filter(|git| git.is_file())
            .filter(|git| git.canonicalize().ok() != me)
            .find_map(|git| {
                ask_attr_system(&git)
                    .or_else(|| ask_installation_config(&git))
                    .and_then(|file| file.parent().map(Path::to_owned))
            })
    })
    .as_deref()
}

/// Ask one `git` binary for its `GIT_ATTR_SYSTEM`: one absolute path and a newline
/// (builtin/var.c:63-71, :244), or nothing from a git that does not know it.
fn ask_attr_system(git: &Path) -> Option<std::path::PathBuf> {
    let out = probe(git, &["var", "GIT_ATTR_SYSTEM"]).output().ok()?;
    if !out.status.success() {
        return None;
    }
    let path = gix::path::try_from_byte_slice(out.stdout.strip_suffix(b"\n")?).ok()?;
    path.is_absolute().then(|| path.to_owned())
}

/// Ask one `git` binary for the highest-scoped configuration file it can see.
fn ask_installation_config(git: &Path) -> Option<std::path::PathBuf> {
    let out = probe(git, &["config", "-lz", "--show-origin", "--name-only"]).output().ok()?;
    let first = out.stdout.strip_prefix(b"file:")?;
    let end = first.iter().position(|&b| b == 0)?;
    gix::path::try_from_byte_slice(&first[..end]).ok().map(Path::to_owned)
}

/// `git <args>` in the neutral environment both installation probes run in.
fn probe(git: &Path, args: &[&str]) -> std::process::Command {
    let mut cmd = std::process::Command::new(git);
    cmd.args(args)
        // Run from the root, as gitoxide does: a deleted or deeply nested cwd
        // costs the child a discovery walk for an answer that does not depend on it.
        .current_dir(Path::new("/"))
        .env_remove("GIT_CONFIG")
        .env_remove("GIT_CONFIG_SYSTEM")
        .env_remove("GIT_CONFIG_NOSYSTEM")
        .env_remove("GIT_ATTR_NOSYSTEM")
        .env_remove("GIT_DISCOVERY_ACROSS_FILESYSTEM")
        .env_remove("GIT_OBJECT_DIRECTORY")
        .env_remove("GIT_ALTERNATE_OBJECT_DIRECTORIES")
        .env_remove("GIT_COMMON_DIR")
        .env("GIT_DIR", "/dev/null")
        .env("GIT_WORK_TREE", "/dev/null")
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    cmd
}

/// `GIT_ATTR_GLOBAL` — `core.attributesFile` if set, else the XDG attributes path.
///
/// The configured value is interpolated the way git expands config paths (`~/`,
/// `~user/`, `%(prefix)/`).
fn attr_global(cfg: &ConfigFile) -> Result<Option<BString>> {
    if let Some(configured) = cfg.path("core.attributesFile") {
        let home = gix::path::env::home_dir();
        let expanded = configured.interpolate(gix::config::path::interpolate::Context {
            home_dir: home.as_deref(),
            ..Default::default()
        })?;
        return Ok(Some(path_bytes(expanded)));
    }
    Ok(gix::path::env::xdg_config("attributes", &mut gix::path::env::var).map(path_bytes))
}

/// `GIT_CONFIG_SYSTEM` — git's `git_system_config()`.
///
/// `GIT_CONFIG_NOSYSTEM` suppresses it outright; failing that `GIT_CONFIG_SYSTEM`
/// names the file directly; failing that it is `gitconfig` in the same directory
/// `GIT_ATTR_SYSTEM` hangs off, resolved by [`installation_prefix`] — git derives
/// both from one `system_path()` call, so they cannot disagree.
fn config_system() -> Option<BString> {
    if env_bool("GIT_CONFIG_NOSYSTEM") {
        return None;
    }
    if let Some(configured) = std::env::var_os("GIT_CONFIG_SYSTEM") {
        return Some(path_bytes(std::path::PathBuf::from(configured)));
    }
    Some(path_bytes(installation_prefix().etc().join("gitconfig")))
}

/// The storage locations of the given config `sources`, highest priority first.
///
/// `None` when none of them resolves (e.g. `GIT_CONFIG_NOSYSTEM=1`, or no `HOME`
/// and no `XDG_CONFIG_HOME`). Consecutive duplicates are collapsed: with
/// `GIT_CONFIG_GLOBAL` set, both global sources point at that one file and git
/// prints it once.
fn config_paths(sources: &[Source]) -> Option<Vec<BString>> {
    let mut paths: Vec<BString> = Vec::new();
    for source in sources {
        if let Some(path) = source.storage_location(&mut gix::path::env::var) {
            let bytes = path_bytes(path);
            if paths.last() != Some(&bytes) {
                paths.push(bytes);
            }
        }
    }
    (!paths.is_empty()).then_some(paths)
}

/// `git var -l`'s configuration half: `git_config(show_config, NULL)`
/// (builtin/var.c:89), whose callback prints `var=value`, or the bare `var`
/// when the value is `NULL` (builtin/var.c:73-80) — a name written with no `=`.
/// `name =` is an empty value, not a missing one, and keeps its `=`.
///
/// The entries come from the same walker `git config --list` uses, so the two
/// listings agree on order, multivars and synthetic gitoxide layers.
///
/// `limit` stops the listing after that many entries.
fn list_config(cfg: &ConfigFile, out: &mut impl Write, limit: Option<usize>) -> Result<()> {
    let mut left = limit.unwrap_or(usize::MAX);
    super::config::for_each_entry(cfg, |key, value, implicit, _meta| {
        // `git_config()` reads through the configset, whose `configset_add_value()` refuses a
        // key with no section (config.c:1743-1745, 1764-1767), so an entry above the first
        // `[...]` line never reaches `show_config()`.
        if !key.contains('.') {
            return Ok(());
        }
        if left == 0 {
            return Ok(());
        }
        left -= 1;
        out.write_all(key.as_bytes())?;
        if !implicit {
            out.write_all(b"=")?;
            out.write_all(value)?;
        }
        out.write_all(b"\n")?;
        Ok(())
    })
}

/// `getenv` semantics: present-but-empty is a value, not an absence.
fn env(name: &str) -> Option<String> {
    std::env::var(name).ok()
}

/// `git_env_bool()` (parse.c:197-208) for the `*_NOSYSTEM` switches, which is
/// what both call sites stand in for: `attr.c:893` reads `GIT_ATTR_NOSYSTEM`
/// through it and `config.c:1541` reads `GIT_CONFIG_NOSYSTEM`.
///
/// gitoxide's own boolean parser was the wrong grammar in both directions: it
/// has no base-0 integer fallback, so `GIT_ATTR_NOSYSTEM=0x10` read as false
/// where git reads sixteen and suppresses the system file, and it coerces a
/// value that is not a boolean to false where git dies with
/// `bad boolean environment value`.
fn env_bool(name: &str) -> bool {
    crate::setup::git_env_bool(name, false)
}

/// A single configuration value as a `String`, or `None` if the key is unset.
fn cfg_str(cfg: &ConfigFile, key: &str) -> Option<String> {
    cfg.string(key).map(|v| v.to_string())
}

/// A path rendered as the raw bytes git would print, without a lossy conversion.
fn path_bytes(path: impl Into<std::path::PathBuf>) -> BString {
    gix::path::into_bstr(path.into()).into_owned()
}

/// The same, for the borrowed `OsStr` that `gix::path::env::shell()` returns.
fn os_bytes(s: &std::ffi::OsStr) -> BString {
    gix::path::os_str_into_bstr(s)
        .map(|b| b.to_owned())
        .unwrap_or_else(|_| BString::from(s.to_string_lossy().into_owned()))
}

/// `strbuf_addstr_without_crud()` (ident.c:229-266) for a `str`: `crud()` trimmed
/// from both ends — bytes up to the space, and `,:;<>"\'` — and `\n`, `<` and `>`
/// dropped from what is left. A `.` is not crud. Only ASCII bytes are removed, so
/// the result is still UTF-8.
pub(super) fn without_crud(s: &str) -> String {
    String::from_utf8(super::refs_migrate::without_crud(s.as_bytes())).expect("only ASCII bytes were removed")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `strip_path_suffix()` removes whole components, which is the difference
    /// between deriving a prefix and truncating a string: `/opt/homebrewbin`
    /// ends in the *characters* `bin` and is not a `bin` directory.
    #[test]
    fn suffix_stripping_respects_component_boundaries() {
        for (dir, suffix, want) in [
            ("/opt/homebrew/bin", "bin", Some("/opt/homebrew")),
            ("/usr/local/libexec/git-core", "libexec/git-core", Some("/usr/local")),
            // git installed at the root: the prefix is `/`, not "nothing".
            ("/bin", "bin", Some("/")),
            ("/opt/homebrewbin", "bin", None),
            ("/opt/homebrew/bin/x", "bin", None),
            ("/opt/homebrew/git-core", "libexec/git-core", None),
            // Nothing is left to be a prefix, so there is no prefix.
            ("bin", "bin", None),
        ] {
            assert_eq!(
                strip_path_suffix(Path::new(dir), suffix).as_deref(),
                want.map(Path::new),
                "strip_path_suffix({dir:?}, {suffix:?})"
            );
        }
    }

    /// The chain git tries in `system_prefix()`, in git's order. A `libexec/git-core`
    /// layout must not be answered by the `bin` rule, and a build tree must match
    /// nothing at all — that last case is what sends `installation_prefix()` on to
    /// its next source instead of inventing a prefix from `target/debug`.
    #[test]
    fn executable_suffix_chain_matches_gits_order() {
        let chain = |dir: &str| {
            ["libexec/git-core", "bin", "git"]
                .iter()
                .find_map(|s| strip_path_suffix(Path::new(dir), s))
        };
        assert_eq!(chain("/usr/local/libexec/git-core").as_deref(), Some(Path::new("/usr/local")));
        assert_eq!(chain("/opt/homebrew/bin").as_deref(), Some(Path::new("/opt/homebrew")));
        assert_eq!(chain("/c/Program Files/Git/git").as_deref(), Some(Path::new("/c/Program Files/Git")));
        assert_eq!(chain("/home/u/zvcs/target/debug"), None);
    }

    /// The two granularities must stay distinct. A probed `sysconfdir` is already
    /// the directory the files live in — appending `etc` to Debian's `/etc` would
    /// name `/etc/etc/gitattributes` — while a prefix still needs it.
    #[test]
    fn etc_is_appended_to_a_prefix_and_only_to_a_prefix() {
        assert_eq!(Installation::Etc("/etc".into()).etc(), Path::new("/etc"));
        assert_eq!(
            Installation::Prefix("/opt/homebrew".into()).etc(),
            Path::new("/opt/homebrew/etc")
        );
    }
}

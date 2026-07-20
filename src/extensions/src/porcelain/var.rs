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
//! * `-h` — git's usage line on stdout, exit 129; no argument, an unknown
//!   variable, or more than one argument — the same line on stderr, exit 129
//! * `GIT_CONFIG_GLOBAL` correctly collapses to a single line when the
//!   environment variable of that name is set, and expands to the XDG path plus
//!   `$HOME/.gitconfig` otherwise
//!
//! ### Honest limitations
//!
//! * The ident variables require `user.name`/`user.email` (or the
//!   `GIT_{AUTHOR,COMMITTER}_{NAME,EMAIL}` environment variables). Git falls back
//!   to the gecos field and `user@hostname`; the vendored crates deliberately have
//!   no such fallback (see `gix/src/repository/identity.rs`, "Deviation"), and
//!   there is no passwd/gethostname substrate to port it onto. Rather than invent
//!   a different ident, this errors out (exit 128) with a precise message.
//! * `GIT_ATTR_SYSTEM` and `GIT_CONFIG_SYSTEM` derive from
//!   `gix_path::env::system_prefix()`, which is `/` on every non-Windows target.
//!   Stock git uses its *compile-time* prefix, so a git installed under a prefix
//!   other than `/` (e.g. Homebrew's `/opt/homebrew`) reports a different path.
//!   This is a property of the vendored crate, not something this module can fix
//!   without hardcoding a foreign installation layout.
//! * `init.defaultBranch` is not run through git's `check_refname_format`, so an
//!   invalid branch name is echoed rather than rejected.

use anyhow::Result;
use std::io::Write;
use std::process::ExitCode;

use gix::bstr::BString;
use gix::config::{Boolean, File as ConfigFile, Source};

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
/// See the module docs for the covered surface and the two honest deviations
/// (ident auto-detection and the system prefix).
pub fn var(args: &[String]) -> Result<ExitCode> {
    // Dispatch passes the argument list without the subcommand; tolerate a
    // leading `var` so the function is correct under either calling convention.
    let argv: &[String] = match args.first() {
        Some(first) if first == "var" => &args[1..],
        _ => args,
    };

    if argv.iter().any(|a| a == "-h") {
        print!("{USAGE}");
        return Ok(ExitCode::from(129));
    }

    let [request] = argv else {
        return Ok(usage_error());
    };

    let cfg = load_config()?;

    let stdout = std::io::stdout();
    let mut out = stdout.lock();

    if request == "-l" {
        list_config(&cfg, &mut out)?;
        for name in VARS {
            let Some(values) = resolve(name, &cfg)? else {
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

    match resolve(request, &cfg)? {
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
    match gix::discover(".") {
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
fn resolve(name: &str, cfg: &ConfigFile) -> Result<Option<Vec<BString>>> {
    let single = |s: Option<String>| -> Result<Option<Vec<BString>>> {
        Ok(s.map(|s| vec![BString::from(s)]))
    };

    match name {
        "GIT_AUTHOR_IDENT" => single(Some(ident(cfg, "AUTHOR")?)),
        "GIT_COMMITTER_IDENT" => single(Some(ident(cfg, "COMMITTER")?)),
        "GIT_EDITOR" => single(editor(cfg)),
        "GIT_SEQUENCE_EDITOR" => single(
            env("GIT_SEQUENCE_EDITOR")
                .or_else(|| cfg_str(cfg, "sequence.editor"))
                .or_else(|| editor(cfg)),
        ),
        "GIT_PAGER" => single(Some(pager(cfg))),
        "GIT_DEFAULT_BRANCH" => single(Some(
            cfg_str(cfg, "init.defaultBranch").unwrap_or_else(|| "master".into()),
        )),
        "GIT_SHELL_PATH" => Ok(Some(vec![os_bytes(gix::path::env::shell())])),
        "GIT_ATTR_SYSTEM" => Ok(attr_system().map(|v| vec![v])),
        "GIT_ATTR_GLOBAL" => Ok(attr_global(cfg)?.map(|v| vec![v])),
        "GIT_CONFIG_SYSTEM" => Ok(config_paths(&[Source::System])),
        "GIT_CONFIG_GLOBAL" => Ok(config_paths(&[Source::Git, Source::User])),
        _ => unreachable!("variable names are validated against VARS before resolve"),
    }
}

/// `GIT_AUTHOR_IDENT` / `GIT_COMMITTER_IDENT` — `Name <email> <seconds> <tz>`.
///
/// Precedence follows git's `ident.c`: the role's environment variables, then
/// `user.name`/`user.email`, with `EMAIL` as a last resort for the address. The
/// timestamp comes from `GIT_<ROLE>_DATE` when set (parsed with git's date
/// grammar via `gix_date`), otherwise from the current local time.
///
/// Name and email are trimmed of git's "crud" characters exactly as
/// `strbuf_addstr_without_crud` does before they are formatted.
fn ident(cfg: &ConfigFile, role: &str) -> Result<String> {
    let name = env(&format!("GIT_{role}_NAME")).or_else(|| cfg_str(cfg, "user.name"));
    let email = env(&format!("GIT_{role}_EMAIL"))
        .or_else(|| cfg_str(cfg, "user.email"))
        .or_else(|| env("EMAIL"));

    let (Some(name), Some(email)) = (name, email) else {
        anyhow::bail!(
            "unable to determine the {} identity: set user.name and user.email \
             (git's gecos/hostname auto-detection has no substrate in the vendored crates)",
            role.to_lowercase()
        );
    };

    let name = without_crud(&name);
    let email = without_crud(&email);
    if name.is_empty() {
        anyhow::bail!("empty ident name (for <{email}>) not allowed");
    }

    let date_var = format!("GIT_{role}_DATE");
    let time = match env(&date_var) {
        Some(raw) => gix::date::parse(&raw, Some(std::time::SystemTime::now()))
            .map_err(|e| anyhow::anyhow!("invalid date in {date_var}: {e}"))?,
        None => gix::date::Time::now_local_or_utc(),
    };

    Ok(format!("{name} <{email}> {time}"))
}

/// `GIT_EDITOR` — git's `git_editor()` chain.
///
/// `$GIT_EDITOR`, then `core.editor`, then `$VISUAL` (skipped on a dumb
/// terminal), then `$EDITOR`, then the compiled-in default `vi` — except on a
/// dumb terminal, where an unset editor stays unset so the caller exits 1.
fn editor(cfg: &ConfigFile) -> Option<String> {
    let dumb = match std::env::var("TERM") {
        Ok(term) => term == "dumb",
        Err(_) => true,
    };

    let found = env("GIT_EDITOR")
        .or_else(|| cfg_str(cfg, "core.editor"))
        .or_else(|| if dumb { None } else { env("VISUAL") })
        .or_else(|| env("EDITOR"));

    match found {
        Some(e) => Some(e),
        None if dumb => None,
        None => Some("vi".into()),
    }
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

/// `GIT_ATTR_SYSTEM` — `<system prefix>/etc/gitattributes`, unless disabled.
///
/// Suppressed entirely by a true `GIT_ATTR_NOSYSTEM`, matching git's
/// `git_attr_system_is_enabled()`.
fn attr_system() -> Option<BString> {
    if env_bool("GIT_ATTR_NOSYSTEM") {
        return None;
    }
    let prefix = gix::path::env::system_prefix()?;
    Some(path_bytes(prefix.join("etc/gitattributes")))
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

/// Config layers that exist only inside gitoxide and have no counterpart in
/// stock git, so `git var -l` must not print them.
///
/// `gix` synthesizes a `Source::EnvOverride` layer from the ambient environment
/// (`GIT_COMMITTER_NAME` → `gitoxide.committer.nameFallback`,
/// `GIT_TERMINAL_PROMPT` → `gitoxide.credentials.terminalPrompt`, …), and zvcs
/// injects its own defaults through `Source::Api`. Neither is git configuration.
/// `Source::Env` (`GIT_CONFIG_COUNT`) and `Source::Cli` (`git -c`) are real and
/// stay visible. Same rule as `git config --list` applies in `config.rs`.
fn is_synthetic(source: Source) -> bool {
    matches!(source, Source::EnvOverride | Source::Api)
}

/// `git var -l`'s configuration half — every `key=value` from the merged config
/// in file order, with section and value names lower-cased (git-normalized) and
/// subsection case preserved. Verified byte-identical to `git config --list`,
/// which is what `builtin/var.c` delegates this half to.
fn list_config(cfg: &ConfigFile, out: &mut impl Write) -> Result<()> {
    for section in cfg.sections() {
        if is_synthetic(section.meta().source) {
            continue;
        }
        let header = section.header();
        let section_name = header.name().to_string().to_lowercase();
        let subsection = header.subsection_name().map(ToString::to_string);

        // Multivars keep their *file* order, not a per-key grouping: git prints
        // `foo.a=1 / foo.b=2 / foo.a=3` for a body in that order. `value_names()`
        // walks the body and repeats a name once per occurrence, so the nth
        // occurrence of a name pairs with `values(name)[n]`.
        let mut occurrence: Vec<(String, usize)> = Vec::new();
        for raw_name in section.value_names() {
            let lname = raw_name.to_lowercase();
            let nth = occurrence.iter().filter(|(n, _)| *n == lname).count();
            occurrence.push((lname, nth));
        }

        for (value_name, nth) in &occurrence {
            let Some(value) = section.values(value_name).into_iter().nth(*nth) else {
                continue;
            };
            match &subsection {
                Some(sub) => write!(out, "{section_name}.{sub}.{value_name}=")?,
                None => write!(out, "{section_name}.{value_name}=")?,
            }
            out.write_all(&value)?;
            out.write_all(b"\n")?;
        }
    }
    Ok(())
}

/// `getenv` semantics: present-but-empty is a value, not an absence.
fn env(name: &str) -> Option<String> {
    std::env::var(name).ok()
}

/// git's `git_env_bool` for the `*_NOSYSTEM` switches, using gitoxide's own
/// boolean parser so `0`/`false`/`no`/`off`/empty read as false.
fn env_bool(name: &str) -> bool {
    std::env::var_os(name)
        .and_then(|v| Boolean::try_from(v).ok())
        .is_some_and(|b| b.0)
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

/// git's `strbuf_addstr_without_crud`: strip leading and trailing whitespace and
/// the punctuation git refuses to carry into an ident (`.,:;<>"` and backslash/
/// apostrophe), then drop any remaining control characters.
fn without_crud(s: &str) -> String {
    const CRUD: &[char] = &['.', ',', ':', ';', '<', '>', '"', '\\', '\''];
    s.trim_matches(|c: char| c.is_whitespace() || CRUD.contains(&c))
        .chars()
        .filter(|c| !c.is_control())
        .collect()
}

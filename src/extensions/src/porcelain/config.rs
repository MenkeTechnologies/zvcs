use anyhow::{bail, Result};
use std::io::Write;
use std::process::ExitCode;

use gix::config::{File as ConfigFile, KeyRef, Source};

#[derive(PartialEq, Clone, Copy)]
enum Mode {
    Auto,
    Get,
    GetAll,
    GetRegexp,
    GetUrlMatch,
    GetColorBool,
    List,
    Add,
    ReplaceAll,
    Unset,
    UnsetAll,
    RenameSection,
    RemoveSection,
    Edit,
}

impl Mode {
    /// The long option that selects this mode, used verbatim in git's
    /// "cannot be used together" diagnostic.
    fn flag(self) -> &'static str {
        match self {
            Mode::Auto => "",
            Mode::Get => "--get",
            Mode::GetAll => "--get-all",
            Mode::GetRegexp => "--get-regexp",
            Mode::GetUrlMatch => "--get-urlmatch",
            Mode::GetColorBool => "--get-colorbool",
            Mode::ReplaceAll => "--replace-all",
            Mode::RenameSection => "--rename-section",
            Mode::RemoveSection => "--remove-section",
            Mode::Edit => "--edit",
            Mode::List => "--list",
            Mode::Add => "--add",
            Mode::Unset => "--unset",
            Mode::UnsetAll => "--unset-all",
        }
    }
}

/// Which config file a scoped read/write targets. `Default` keeps git's
/// implicit behavior (merged read, repository-local write); the rest pin a
/// single file — selected by a `--local`/`--global`/`--system` flag, or named
/// outright by `-f`/`--file`.
#[derive(PartialEq, Clone)]
enum Scope {
    Default,
    Local,
    Global,
    System,
    File(std::path::PathBuf),
}

/// A resolved write destination: the file to rewrite, its config `Source` (so a
/// freshly-created file carries the right provenance), the coordinator lane key
/// to serialize concurrent zvcs writers on, and whether a missing parent
/// directory may be created (git creates one for `--global`, never for
/// `--file`).
struct WriteTarget {
    path: std::path::PathBuf,
    source: Source,
    lock_key: std::path::PathBuf,
    create_parent: bool,
}

/// Exit 129 — git's usage-error code — after emitting `error: <msg>` on stderr.
///
/// `anyhow::bail!` would collapse to exit 1, so every usage diagnostic has to
/// report itself and return the code explicitly.
fn usage_error(msg: &str) -> Result<ExitCode> {
    eprintln!("error: {msg}");
    Ok(ExitCode::from(129))
}

/// How a read prints each `(key, value)` pair: git's `--show-origin` /
/// `--show-scope` prefixes, the `--null` record separator, and the `--type`
/// canonicalization applied to the value.
#[derive(Default, Clone, Copy)]
struct Display {
    show_origin: bool,
    show_scope: bool,
    null: bool,
    name_only: bool,
    ty: Option<ValueType>,
}

/// `--type=<t>` and its legacy spellings (`--bool`, `--int`, `--bool-or-int`,
/// `--path`). git canonicalizes the value on the way out; an unparsable value is
/// `fatal: bad <t> config value '<v>' for '<key>'` at exit 128.
#[derive(Clone, Copy, PartialEq)]
enum ValueType {
    Bool,
    Int,
    BoolOrInt,
    Path,
}

impl ValueType {
    fn parse(name: &str) -> Option<ValueType> {
        match name {
            "bool" => Some(ValueType::Bool),
            "int" => Some(ValueType::Int),
            "bool-or-int" => Some(ValueType::BoolOrInt),
            "path" => Some(ValueType::Path),
            _ => None,
        }
    }

    fn label(self) -> &'static str {
        match self {
            ValueType::Bool => "boolean",
            ValueType::Int => "numerical",
            ValueType::BoolOrInt => "numerical",
            ValueType::Path => "path",
        }
    }

    /// Canonicalize `value` the way git prints it under this type, or `None`
    /// when the value does not parse as the type.
    fn canonicalize(self, value: &[u8]) -> Option<Vec<u8>> {
        let text = String::from_utf8_lossy(value).trim().to_string();
        match self {
            ValueType::Bool => canonical_bool(&text).map(|b| b.to_string().into_bytes()),
            ValueType::Int => canonical_int(&text).map(|n| n.to_string().into_bytes()),
            // git tries integer first, then boolean, and prints 1/0 for a bool.
            ValueType::BoolOrInt => canonical_int(&text)
                .map(|n| n.to_string().into_bytes())
                .or_else(|| canonical_bool(&text).map(|b| u8::from(b).to_string().into_bytes())),
            ValueType::Path => Some(expand_config_path(&text).into_bytes()),
        }
    }
}

/// git's boolean grammar: the empty value is true, and the words are matched
/// case-insensitively (`git_parse_maybe_bool`).
fn canonical_bool(text: &str) -> Option<bool> {
    match text.to_ascii_lowercase().as_str() {
        "" | "true" | "yes" | "on" | "1" => Some(true),
        "false" | "no" | "off" | "0" => Some(false),
        _ => None,
    }
}

/// git's integer grammar: an optional `k`/`m`/`g` suffix scales the number
/// (`git_parse_int`), so `1k` reads as 1024.
fn canonical_int(text: &str) -> Option<i64> {
    let (digits, scale) = match text.chars().last() {
        Some('k') | Some('K') => (&text[..text.len() - 1], 1024i64),
        Some('m') | Some('M') => (&text[..text.len() - 1], 1024 * 1024),
        Some('g') | Some('G') => (&text[..text.len() - 1], 1024 * 1024 * 1024),
        _ => (text, 1),
    };
    digits.trim().parse::<i64>().ok().and_then(|n| n.checked_mul(scale))
}

/// `--type=path`: expand a leading `~` / `~user` the way git's
/// `expand_user_path` does. `~user` needs a passwd lookup that is not vendored,
/// so it is left verbatim rather than guessed at.
fn expand_config_path(text: &str) -> String {
    match text.strip_prefix("~/") {
        Some(rest) => match std::env::var_os("HOME") {
            Some(home) => format!("{}/{}", home.to_string_lossy().trim_end_matches('/'), rest),
            None => text.to_string(),
        },
        None => text.to_string(),
    }
}

/// The `--show-scope` word for a config source, matching git's scope names.
fn scope_word(source: Source) -> &'static str {
    match source {
        Source::System => "system",
        Source::Git | Source::User => "global",
        Source::Local | Source::Worktree => "local",
        Source::Cli => "command",
        Source::Env | Source::EnvOverride => "command",
        _ => "unknown",
    }
}

/// Config that stock git would never surface.
///
/// `gix` synthesizes a config layer from the ambient environment
/// (`GIT_COMMITTER_NAME` → `gitoxide.committer.nameFallback`,
/// `GIT_TERMINAL_PROMPT` → `gitoxide.credentials.terminalPrompt`, …) and zvcs
/// injects its own defaults through the API scope. Neither exists as far as
/// `git config` is concerned, so both are hidden from reads and from `--list`.
/// `Source::Env` (`GIT_CONFIG_COUNT`) and `Source::Cli` (`git -c`) are real
/// git config and stay visible.
fn is_synthetic(source: Source) -> bool {
    matches!(source, Source::EnvOverride | Source::Api)
}

/// The bare `strerror` text of an OS error, as git prints it.
///
/// Rust's `io::Error` Display appends ` (os error N)`; git's `strerror` does
/// not, so `No such file or directory (os error 2)` has to become
/// `No such file or directory` to match messages like
/// `fatal: unable to read config file 'x': No such file or directory`.
fn errno_text(err: &std::io::Error) -> String {
    let text = err.to_string();
    match text.find(" (os error ") {
        Some(cut) => text[..cut].to_owned(),
        None => text,
    }
}

/// `git config` — get/set/list configuration values, backed by gitoxide.
///
/// Reads resolve through the fully-merged config snapshot (system + global +
/// local, last-one-wins), matching stock `git config`'s default scope. Outside a
/// repository a read still works, falling back to the global+system+env cascade
/// exactly as stock `git config` does (so `config --list` / `config user.name`
/// never require a repo).
///
/// A scope flag narrows both reads and writes to a single file, like git:
/// ```text
///   * `--local`  → the repository-local config (`<common_dir>/config`); requires
///     a repo, else `--local can only be used inside a git repository`.
///   * `--global` → the per-user config: `$XDG_CONFIG_HOME/git/config` and
///     `~/.gitconfig` merged for reads; for writes, `~/.gitconfig` (git's target)
///     unless only the XDG file exists, created if absent. Never needs a repo.
///   * `--system` → `$(prefix)/etc/gitconfig` (honoring `GIT_CONFIG_SYSTEM` /
///     `GIT_CONFIG_NOSYSTEM`). Never needs a repo.
///   * `-f <path>` / `--file <path>` → exactly that file, `include.path`
///     directives NOT followed (git only honors them here under `--includes`,
///     which is not implemented). Never needs a repo; created on write, but its
///     parent directory is not — a missing one is git's
///     `could not lock config file <path>: <errno>` at exit 255. Reading a
///     missing file is exit 1 for the get forms and
///     `fatal: unable to read config file '<path>': <errno>` at exit 128 for
///     `--list`, exactly as git splits those two paths.
/// ```
/// The default (no scope) write still targets the repository-local file and so
/// still needs a repo — attempting one without one fails with `not in a git
/// directory`. `--worktree` is rejected with a precise error rather than
/// silently mistargeted. Two *different* scope flags at once → `only one config
/// file at a time`; a repeated `--file` just replaces the path, as git's
/// `given_config_source.file` does.
///
/// Supported forms:
/// ```text
///   * `git config <name>` / `--get <name>`   → last value, exit 1 if absent
///   * `git config --get-all <name>`          → every value, one per line
///   * `git config --get-regexp <regex>`      → `key value` for every key the
///                                              ERE matches, exit 1 if none
///   * `--get`/`--get-all`/`--get-regexp` also take an optional trailing
///     `<value-pattern>` — an ERE (`!` inverts) that filters which VALUES count;
///     `--get` then reports the last survivor, exit 1 when none survive
///   * `git config -l` / `--list`             → all `key=value`, merged scopes
///   * `git config <name> <value>`            → set (overwrite last), local
///   * `git config <name> <value> <pattern>`  → rewrite values matching the ERE,
///                                              or append when none match, local
///   * `git config --add <name> <value>`      → append a multivar entry, local
///   * `git config --unset <name>`            → drop the value, exit 5 if absent
///   * `git config --unset-all <name>`        → drop every value of the key
///   * `--name-only`                          → with `--list` or `--get-regexp`,
///                                              keys without values
/// ```
///
/// Usage errors (conflicting action flags, a misplaced `--name-only`, a wrong
/// argument count) report `error: …` on stderr and exit 129, as git's
/// parse-options layer does; they never travel as `anyhow` errors, which would
/// collapse to exit 1.
pub fn config(args: &[String]) -> Result<ExitCode> {
    let mut mode = Mode::Auto;
    let mut scope = Scope::Default;
    let mut name_only = false;
    let mut d = Display::default();
    // `include.path` / `includeIf` following. git resolves includes for the
    // implicit scopes and, for an explicitly named file, only when asked.
    let mut includes = false;
    let mut positional: Vec<&str> = Vec::new();
    // git config parses options with `PARSE_OPT_STOP_AT_NON_OPTION`: option
    // scanning ends at the FIRST argument that is not an option, and that token
    // plus every one after it are operands — even the ones that look like
    // `--flags`. Two independent terminators reach this state:
    //   * a bare `--`, which is consumed (never a positional itself), or
    //   * the first non-option token (anything not starting with `-`, plus a
    //     lone `-`), which is itself the first operand.
    // Consequences that must match stock git:
    //   * `git config user.name value --get` is a 3-operand value-pattern set —
    //     the trailing `--get` is the pattern, not an action flag.
    //   * `git config key --local a b` is 4 operands (`--local` is data here),
    //     so it is rejected as "no action specified", not a scoped write.
    //   * `git config --get -- --list` still reads the key literally `--list`.
    let mut end_of_options = false;

    // Indexed rather than iterated because `-f`/`--file` consumes the argument
    // after it as its value.
    let mut i = 0;
    while i < args.len() {
        let a = &args[i];
        i += 1;

        if end_of_options {
            positional.push(a.as_str());
            continue;
        }
        if a.as_str() == "--" {
            end_of_options = true;
            continue;
        }
        // First non-option token: it ends option parsing AND is the first
        // operand. A lone `-` is a non-option (git treats it as data), so it
        // stops here too.
        if a.as_str() == "-" || !a.starts_with('-') {
            end_of_options = true;
            positional.push(a.as_str());
            continue;
        }

        let action = match a.as_str() {
            "-l" | "--list" => Some(Mode::List),
            "--get" => Some(Mode::Get),
            "--get-all" => Some(Mode::GetAll),
            "--get-regexp" => Some(Mode::GetRegexp),
            "--get-urlmatch" => Some(Mode::GetUrlMatch),
            "--get-colorbool" => Some(Mode::GetColorBool),
            "--replace-all" => Some(Mode::ReplaceAll),
            "--rename-section" => Some(Mode::RenameSection),
            "--remove-section" => Some(Mode::RemoveSection),
            "-e" | "--edit" => Some(Mode::Edit),
            "--add" => Some(Mode::Add),
            "--unset" => Some(Mode::Unset),
            "--unset-all" => Some(Mode::UnsetAll),
            _ => None,
        };

        // Action flags are git's `OPT_CMDMODE`: they all write one slot, and a
        // second *different* one is rejected the moment it is parsed — before
        // any post-parse validation gets a chance to complain.
        if let Some(new) = action {
            if mode != Mode::Auto && mode != new {
                return usage_error(&format!(
                    "options '{}' and '{}' cannot be used together",
                    new.flag(),
                    mode.flag()
                ));
            }
            mode = new;
            continue;
        }

        // Scope flags (`OPT_CMDMODE` on `given_config_source.scope` in git): each
        // pins the target file, and a second, different one is a usage error the
        // moment it is parsed.
        let new_scope = match a.as_str() {
            "--local" => Some(Scope::Local),
            "--global" => Some(Scope::Global),
            "--system" => Some(Scope::System),
            _ => None,
        };
        if let Some(new) = new_scope {
            if scope != Scope::Default && scope != new {
                return usage_error("only one config file at a time");
            }
            scope = new;
            continue;
        }

        // `-f`/`--file` takes a path, in all four of git's parse-options
        // spellings: separate (`-f p`, `--file p`), sticky (`-fp`), and
        // `--file=p`. An empty value is a legal (unreadable) path, not an error.
        let file_value = match a.as_str() {
            "-f" | "--file" => match args.get(i) {
                Some(v) => {
                    i += 1;
                    Some(v.clone())
                }
                // git's parse-options phrasing differs for the short and long
                // spellings of the same option.
                None if a == "-f" => return usage_error("switch `f' requires a value"),
                None => return usage_error("option `file' requires a value"),
            },
            other => other
                .strip_prefix("--file=")
                .or_else(|| other.strip_prefix("-f"))
                .map(ToOwned::to_owned),
        };
        if let Some(path) = file_value {
            // git counts `--file` once no matter how often it is given, so only
            // a *different* kind of scope flag collides with it.
            if !matches!(scope, Scope::Default | Scope::File(_)) {
                return usage_error("only one config file at a time");
            }
            scope = Scope::File(path.into());
            continue;
        }

        // `--type=<t>` and its legacy spellings canonicalize the value on the way
        // out; an unknown type is git's usage error.
        if let Some(t) = a.strip_prefix("--type=") {
            match ValueType::parse(t) {
                Some(ty) => {
                    d.ty = Some(ty);
                    continue;
                }
                None => return usage_error(&format!("unrecognized --type argument, {t}")),
            }
        }

        match a.as_str() {
            "--includes" => includes = true,
            "--no-includes" => includes = false,
            "--show-origin" => d.show_origin = true,
            "--show-scope" => d.show_scope = true,
            "-z" | "--null" => d.null = true,
            "--bool" => d.ty = Some(ValueType::Bool),
            "--int" => d.ty = Some(ValueType::Int),
            "--bool-or-int" => d.ty = Some(ValueType::BoolOrInt),
            "--path" => d.ty = Some(ValueType::Path),
            "--type" => {
                let v = args.get(i).cloned().unwrap_or_default();
                i += 1;
                match ValueType::parse(&v) {
                    Some(ty) => d.ty = Some(ty),
                    None => return usage_error(&format!("unrecognized --type argument, {v}")),
                }
            }
            "--name-only" => name_only = true,
            // Per-worktree config needs `extensions.worktreeConfig`; not ported.
            "--worktree" => bail!("--worktree scope is not supported"),
            other if other.starts_with('-') => bail!("unknown option {other}"),
            other => positional.push(other),
        }
    }

    // Post-parse validation, in git's own order and — like git — ahead of any
    // repository lookup, so a usage error reports the same way outside a repo.
    //
    // An entirely actionless invocation is reported first. Without an action
    // flag the form is `<name> [value [value-pattern]]`, and git recognizes no
    // action at all outside that 1..=3 window, the zero-argument case included.
    if mode == Mode::Auto && !(1..=3).contains(&positional.len()) {
        return usage_error("no action specified");
    }
    d.name_only = name_only;
    if name_only && !matches!(mode, Mode::List | Mode::GetRegexp) {
        return usage_error("--name-only is only applicable to --list or --get-regexp");
    }
    match mode {
        Mode::List if !positional.is_empty() => {
            return usage_error("wrong number of arguments, should be 0");
        }
        Mode::Get | Mode::GetAll | Mode::GetRegexp if !(1..=2).contains(&positional.len()) => {
            return usage_error("wrong number of arguments, should be from 1 to 2");
        }
        _ => {}
    }

    // A repository is optional: reads resolve fine outside one (git reads global
    // and system config with no repo present), while writes target the local
    // scope and still require a repo. Discovery failure is therefore not fatal
    // here — only an attempted write without a repo is.
    let repo = gix::discover(".").ok();

    // The config to READ from, by scope. Owned holders live to the end of the
    // function so `file` can borrow whichever one this scope selects:
    //   * Default → the repo's fully-merged snapshot inside one, else the
    //     global+system+env cascade git falls back to.
    //   * Local   → the repository-local file alone (requires a repo).
    //   * Global  → the XDG + `~/.gitconfig` pair, merged (last wins).
    //   * System  → `$(prefix)/etc/gitconfig` alone.
    //   * File    → the named file alone, includes not followed.
    let snapshot = repo.as_ref().map(gix::Repository::config_snapshot);
    let default_global;
    let scoped;
    // Set when a `--file` target could not be read. git only makes that fatal
    // for `--list`; the get forms treat it as "key not found" (exit 1), so the
    // error is carried to the dispatch below rather than raised here.
    let mut unreadable: Option<std::io::Error> = None;
    // A pure write skips the read side entirely: the write path re-reads its
    // target under the lock, and reading here as well would repeat git's
    // `warning: unable to access …` diagnostic for an unreadable file.
    let reads_config = match mode {
        Mode::List
        | Mode::Get
        | Mode::GetAll
        | Mode::GetRegexp
        | Mode::GetUrlMatch
        | Mode::GetColorBool => true,
        Mode::Auto => positional.len() == 1,
        Mode::Add
        | Mode::ReplaceAll
        | Mode::Unset
        | Mode::UnsetAll
        | Mode::RenameSection
        | Mode::RemoveSection
        | Mode::Edit => false,
    };
    let file: &gix::config::File = match &scope {
        Scope::Default => match snapshot.as_ref() {
            Some(s) => s.plumbing(),
            None => {
                default_global = crate::config::global_config();
                &default_global
            }
        },
        Scope::Local => {
            let repo = repo
                .as_ref()
                .ok_or_else(|| anyhow::anyhow!("--local can only be used inside a git repository"))?;
            let path = repo.common_dir().join("config");
            scoped = load_or_empty(&path, Source::Local)?;
            &scoped
        }
        Scope::Global => {
            scoped = read_scope(&[Source::Git, Source::User]);
            &scoped
        }
        Scope::System => {
            scoped = read_scope(&[Source::System]);
            &scoped
        }
        // `--file` is git's `CONFIG_SCOPE_COMMAND`, hence `Source::Cli`. Read
        // through `fs::read` so a missing or unreadable path surfaces as a
        // plain `io::Error` whose errno git reports verbatim.
        Scope::File(path) if !reads_config => {
            scoped = empty_config(path, Source::Cli);
            &scoped
        }
        Scope::File(path) => {
            scoped = match read_config_bytes(path) {
                Ok(bytes) => {
                    let mut f = parse_config(&bytes, path, Source::Cli)?;
                    if includes {
                        // Follow `include.path` / `includeIf` from the named file,
                        // which git does here only under `--includes`. The
                        // DEFAULT include options are "do not follow", so the
                        // follow-mode options have to be passed explicitly, with
                        // the gitdir context the conditional forms need.
                        let git_dir = repo.as_ref().map(|r| r.git_dir().to_owned());
                        let conditional = gix::config::file::includes::conditional::Context {
                            git_dir: git_dir.as_deref(),
                            branch_name: None,
                        };
                        let opts = gix::config::file::init::Options {
                            includes: gix::config::file::includes::Options::follow(
                                Default::default(),
                                conditional,
                            ),
                            ..Default::default()
                        };
                        f.resolve_includes(opts)?;
                    }
                    f
                }
                Err(err) => {
                    unreadable = Some(err);
                    empty_config(path, Source::Cli)
                }
            };
            &scoped
        }
    };

    // Resolve the write destination for this scope, erroring like git when a
    // repository is required but absent.
    let write_target = || resolve_write_target(&scope, repo.as_ref());

    match mode {
        // Unlike the get forms, `--list` reports an unreadable `--file` as a
        // fatal error rather than as an empty result.
        Mode::List => match (&scope, &unreadable) {
            (Scope::File(path), Some(err)) => {
                eprintln!(
                    "fatal: unable to read config file '{}': {}",
                    path.display(),
                    errno_text(err)
                );
                Ok(ExitCode::from(128))
            }
            _ => list(file, &d),
        },
        // `--get`/`--get-all`/`--get-regexp <name> <value-pattern>`: the optional
        // second operand filters the returned values by an ERE (`!` inverts).
        Mode::Get => get(file, positional[0], false, positional.get(1).copied(), &d),
        Mode::GetAll => get(file, positional[0], true, positional.get(1).copied(), &d),
        Mode::GetRegexp => {
            get_regexp(file, positional[0], positional.get(1).copied(), &d)
        }
        Mode::GetUrlMatch => get_urlmatch(file, &positional, &d),
        Mode::GetColorBool => get_colorbool(file, &positional),
        Mode::Edit => edit_config(&write_target()?),
        Mode::RenameSection => rename_section(&write_target()?, &positional),
        Mode::RemoveSection => remove_section(&write_target()?, &positional),
        Mode::ReplaceAll => {
            let name = positional.first().copied().unwrap_or_default();
            let value = positional.get(1).copied().unwrap_or_default();
            replace_all(&write_target()?, name, value, positional.get(2).copied())
        }
        // No action flag: one positional reads, two set the value.
        Mode::Auto if positional.len() == 1 => get(file, positional[0], false, None, &d),
        Mode::Auto if positional.len() == 2 => {
            write_scoped(&write_target()?, positional[0], positional[1], WriteOp::Set)
        }
        // `<name> <value> <value-pattern>` rewrites the values whose text matches
        // the POSIX ERE, or adds a new value when none match.
        Mode::Auto => {
            set_with_value_pattern(&write_target()?, positional[0], positional[1], positional[2])
        }
        Mode::Add => {
            let (name, value) = name_and_value(&positional)?;
            write_scoped(&write_target()?, name, value, WriteOp::Add)
        }
        Mode::Unset => {
            let name = one_name(&positional)?;
            write_scoped(&write_target()?, name, "", WriteOp::Unset)
        }
        Mode::UnsetAll => {
            let name = one_name(&positional)?;
            write_scoped(&write_target()?, name, "", WriteOp::UnsetAll)
        }
    }
}


fn one_name<'a>(positional: &[&'a str]) -> Result<&'a str> {
    match positional {
        [name] => Ok(*name),
        [] => crate::git_fatal!("no config key given"),
        _ => crate::git_fatal!("too many arguments, expected a single `<name>`"),
    }
}

fn name_and_value<'a>(positional: &[&'a str]) -> Result<(&'a str, &'a str)> {
    match positional {
        [name, value] => Ok((*name, *value)),
        _ => crate::git_fatal!("expected `<name> <value>`"),
    }
}

/// Parse `section[.subsection].name`, erroring the way stock git does when the
/// key has no section component.
fn parse_key(name: &str) -> Result<KeyRef<'_>> {
    KeyRef::parse_unvalidated(name.into())
        .ok_or_else(|| anyhow::anyhow!("key does not contain a section: {name}"))
}

/// A compiled `<value-pattern>`: the optional second operand of a read, an
/// unanchored POSIX ERE matched against the value bytes, inverted by a leading
/// `!` — the same grammar the value-pattern *set* form uses.
struct ValueFilter {
    re: regex::bytes::Regex,
    invert: bool,
}

impl ValueFilter {
    /// Compile `pattern`, or report git's `error: invalid pattern: <p>` at exit 6.
    fn parse(pattern: &str) -> Result<Self, ExitCode> {
        let (invert, pat) = match pattern.strip_prefix('!') {
            Some(rest) => (true, rest),
            None => (false, pattern),
        };
        match regex::bytes::Regex::new(pat) {
            Ok(re) => Ok(Self { re, invert }),
            Err(_) => {
                eprintln!("error: invalid pattern: {pat}");
                Err(ExitCode::from(6))
            }
        }
    }

    fn matches(&self, value: &[u8]) -> bool {
        self.re.is_match(value) != self.invert
    }
}

/// `git config <name>` / `--get` / `--get-all` — read from the merged snapshot.
///
/// With a `<value-pattern>` only the values it selects are considered, so
/// `--get` reports the LAST matching value and `--get-all` every matching one,
/// in file order. A pattern that selects nothing is exit 1 with no output, the
/// same as an absent key — git does not distinguish the two.
///
/// Exit code 1 (no output) when the key is absent, matching stock git.
fn get(
    file: &gix::config::File,
    name: &str,
    all: bool,
    value_pattern: Option<&str>,
    d: &Display,
) -> Result<ExitCode> {
    let key = parse_key(name)?;
    let filter = match value_pattern.map(ValueFilter::parse) {
        Some(Err(code)) => return Ok(code),
        Some(Ok(f)) => Some(f),
        None => None,
    };

    let stdout = std::io::stdout();
    let mut out = stdout.lock();

    // Walked through `for_each_entry` rather than `raw_values_*` so each value
    // arrives with the metadata `--show-origin`/`--show-scope` need; the walk is
    // in file order, and git's `--get` is the LAST value that survives the
    // filter.
    let wanted = key_of(&key);
    let mut selected: Vec<(Vec<u8>, gix::config::file::Metadata)> = Vec::new();
    for_each_entry(file, |k, value, meta| {
        if k == wanted && filter.as_ref().is_none_or(|f| f.matches(value)) {
            selected.push((value.to_vec(), meta.clone()));
        }
        Ok(())
    })?;
    if selected.is_empty() {
        return Ok(ExitCode::from(1));
    }

    // git canonicalizes in file order and dies on the first value that does not
    // parse as the requested type — even when `--get` would have returned a
    // later one, so the error names the same value stock git names.
    let mut canonical: Vec<(Vec<u8>, gix::config::file::Metadata)> = Vec::new();
    for (v, meta) in &selected {
        match typed(d, name, v) {
            Ok(v) => canonical.push((v, meta.clone())),
            Err(code) => return Ok(code),
        }
    }

    let emit: &[_] = if all { &canonical } else { &canonical[canonical.len() - 1..] };
    for (v, meta) in emit {
        emit_kv(&mut out, d, name, v, meta, b'\n', false)?;
    }
    Ok(ExitCode::SUCCESS)
}

/// The git-normalized `section[.subsection].name` spelling of a parsed key, so a
/// lookup can be compared against what [`for_each_entry`] yields.
fn key_of(key: &KeyRef<'_>) -> String {
    let section = key.section_name.to_lowercase();
    let value = key.value_name.to_lowercase();
    match key.subsection_name {
        Some(sub) => format!("{section}.{sub}.{value}"),
        None => format!("{section}.{value}"),
    }
}

/// Print one `key`/`value` pair the way the active display flags ask for it:
/// the `--show-scope` word and `--show-origin` `file:<path>` prefix (each
/// TAB-separated, scope first, exactly as git orders them), then the key, then
/// the value under `sep` unless `--name-only` dropped it. `--null` ends the
/// record with a NUL and separates key from value with a newline, which is what
/// makes the output unambiguous for values containing newlines.
///
/// Returns `Err` only on I/O; a value that fails `--type` conversion is git's
/// fatal, reported by the caller.
fn emit_kv(
    out: &mut impl Write,
    d: &Display,
    key: &str,
    value: &[u8],
    meta: &gix::config::file::Metadata,
    sep: u8,
    with_key: bool,
) -> Result<()> {
    if d.show_scope {
        out.write_all(scope_word(meta.source).as_bytes())?;
        out.write_all(b"\t")?;
    }
    if d.show_origin {
        match &meta.path {
            Some(path) => {
                // git prints the path as it resolved it, without the `./` a
                // relative discovery leaves on the front.
                let text = path.to_string_lossy();
                let text = text.strip_prefix("./").unwrap_or(&text);
                out.write_all(b"file:")?;
                out.write_all(text.as_bytes())?;
            }
            None => out.write_all(origin_word(meta.source).as_bytes())?,
        }
        out.write_all(b"\t")?;
    }
    if with_key {
        out.write_all(key.as_bytes())?;
    }
    if !d.name_only && !(with_key && d.name_only) {
        if with_key {
            let sep_buf = [sep];
            out.write_all(if d.null { b"\n".as_slice() } else { sep_buf.as_slice() })?;
        }
        out.write_all(value)?;
    }
    out.write_all(if d.null { b"\0" } else { b"\n" })?;
    Ok(())
}

/// git's `--show-origin` word for a source with no file behind it.
fn origin_word(source: Source) -> &'static str {
    match source {
        Source::Cli => "command line:",
        Source::Env | Source::EnvOverride => "environment:",
        _ => "blob:",
    }
}

/// Apply `--type` to a value, reporting git's fatal and exit 128 when the value
/// does not parse as that type.
fn typed(d: &Display, key: &str, value: &[u8]) -> std::result::Result<Vec<u8>, ExitCode> {
    match d.ty {
        None => Ok(value.to_vec()),
        Some(t) => t.canonicalize(value).ok_or_else(|| {
            eprintln!(
                "fatal: bad {} config value '{}' for '{}'",
                t.label(),
                String::from_utf8_lossy(value),
                key
            );
            ExitCode::from(128)
        }),
    }
}

/// `git config -l` — emit every `key=value` from the merged snapshot, in file
/// order. Section and value names are lower-cased (git-normalized); subsection
/// case is preserved. With `name_only`, the `=value` half is dropped, one line
/// per value occurrence.
///
/// Entries are emitted in the order they appear in their file, multivars
/// included: an `a=1 / b=2 / a=3` section lists as `a=1`, `b=2`, `a=3`, not with
/// the two `a`s collapsed together.
fn list(file: &gix::config::File, d: &Display) -> Result<ExitCode> {
    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    let mut failed: Option<ExitCode> = None;

    for_each_entry(file, |key, value, meta| {
        if failed.is_some() {
            return Ok(());
        }
        match typed(d, key, value) {
            Ok(v) => emit_kv(&mut out, d, key, &v, meta, b'=', true)?,
            Err(code) => failed = Some(code),
        }
        Ok(())
    })?;

    Ok(failed.unwrap_or(ExitCode::SUCCESS))
}

/// `git config --get-regexp <name-regex>` — every entry whose canonical key
/// matches the POSIX ERE, one per line.
///
/// git separates key and value with a **space** here (not the `=` of `--list`),
/// and `--name-only` drops the value half. Matching is unanchored against the
/// git-normalized key (`section[.subsection].name`, section and value names
/// lower-cased), so `^remote\..*\.url$` behaves as it does under stock git.
/// Exit 1 with no output when nothing matched. An invalid ERE is git's
/// `error: invalid key pattern: <pattern>` at exit 6 — note "key pattern" here,
/// against the plain "pattern" the value-pattern form reports.
fn get_regexp(
    file: &gix::config::File,
    pattern: &str,
    value_pattern: Option<&str>,
    d: &Display,
) -> Result<ExitCode> {
    let re = match regex::bytes::Regex::new(pattern) {
        Ok(re) => re,
        Err(_) => {
            eprintln!("error: invalid key pattern: {pattern}");
            return Ok(ExitCode::from(6));
        }
    };
    // The optional second operand narrows by VALUE, on top of the key match.
    let filter = match value_pattern.map(ValueFilter::parse) {
        Some(Err(code)) => return Ok(code),
        Some(Ok(f)) => Some(f),
        None => None,
    };

    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    let mut matched = false;

    let mut failed: Option<ExitCode> = None;
    for_each_entry(file, |key, value, meta| {
        if failed.is_some() || !re.is_match(key.as_bytes()) {
            return Ok(());
        }
        if filter.as_ref().is_some_and(|f| !f.matches(value)) {
            return Ok(());
        }
        matched = true;
        match typed(d, key, value) {
            Ok(v) => emit_kv(&mut out, d, key, &v, meta, b' ', true)?,
            Err(code) => failed = Some(code),
        }
        Ok(())
    })?;
    if let Some(code) = failed {
        return Ok(code);
    }

    Ok(if matched { ExitCode::SUCCESS } else { ExitCode::from(1) })
}

/// Walk every visible entry of `file` in file order, handing `emit` the
/// git-normalized key and the raw value bytes.
///
/// Multivars repeat rather than collapse: `value_names()` walks the section body
/// in order, naming a multivar once per occurrence, and the nth occurrence pairs
/// with `values(name)[n]` — so an `a=1 / b=2 / a=3` section yields `a=1`, `b=2`,
/// `a=3`. Section and value names are lower-cased (git-normalized); subsection
/// case is preserved.
fn for_each_entry(
    file: &gix::config::File,
    mut emit: impl FnMut(&str, &[u8], &gix::config::file::Metadata) -> Result<()>,
) -> Result<()> {
    for section in file.sections() {
        if is_synthetic(section.meta().source) {
            continue;
        }
        let header = section.header();
        let section_name = header.name().to_string().to_lowercase();
        let subsection = header.subsection_name().map(ToString::to_string);

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
            let key = match &subsection {
                Some(sub) => format!("{section_name}.{sub}.{value_name}"),
                None => format!("{section_name}.{value_name}"),
            };
            emit(&key, &value, section.meta())?;
        }
    }
    Ok(())
}


/// `git config --get-urlmatch <section[.key]> <url>` — the URL-specific lookup
/// that `http.<url>.*` config is built on.
///
/// Ported from git's `urlmatch.c`: every `<section> "<pattern-url>"` subsection
/// whose URL is a prefix-match for `<url>` is a candidate, and the most specific
/// candidate wins per key. Specificity is git's tuple, compared in this order:
///
///   1. a pattern that names a user beats one that does not,
///   2. a longer matched host wins,
///   3. a longer matched path wins.
///
/// A bare `<section>` prints `key value` for every key the winning candidates
/// define (git lower-cases the key here); `<section>.<key>` prints just that
/// key's value. The plain, subsection-less section is the fallback candidate, so
/// a URL that matches no pattern still gets the generic setting.
fn get_urlmatch(file: &gix::config::File, positional: &[&str], d: &Display) -> Result<ExitCode> {
    let (spec, url) = match positional {
        [spec, url] => (*spec, *url),
        _ => return usage_error("wrong number of arguments, should be 2"),
    };
    let (section, key) = match spec.split_once('.') {
        Some((s, k)) => (s.to_lowercase(), Some(k.to_lowercase())),
        None => (spec.to_lowercase(), None),
    };
    let Some(want) = UrlParts::parse(url) else {
        eprintln!("fatal: bad URL: {url}");
        return Ok(ExitCode::from(128));
    };

    // key -> (score, value): the winner for each key seen so far.
    let mut best: std::collections::BTreeMap<String, (UrlScore, Vec<u8>)> =
        std::collections::BTreeMap::new();

    for sec in file.sections() {
        if is_synthetic(sec.meta().source) || sec.header().name() != section.as_str() {
            continue;
        }
        let score = match sec.header().subsection_name() {
            // The generic section matches every URL, at the lowest specificity.
            None => UrlScore::default(),
            Some(pattern) => {
                let text = pattern.to_string();
                match UrlParts::parse(&text).and_then(|p| p.score_against(&want)) {
                    Some(score) => score,
                    None => continue,
                }
            }
        };
        for name in sec.value_names() {
            let lname = name.to_lowercase();
            if key.as_ref().is_some_and(|k| *k != lname) {
                continue;
            }
            let Some(value) = sec.value(&lname) else { continue };
            let entry = best.entry(lname).or_insert_with(|| (UrlScore::default(), Vec::new()));
            if entry.1.is_empty() || score >= entry.0 {
                *entry = (score, value.to_vec());
            }
        }
    }

    if best.is_empty() {
        return Ok(ExitCode::from(1));
    }
    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    let meta = gix::config::file::Metadata::from(Source::Cli);
    for (name, (_, value)) in &best {
        // A single-key query prints the value alone; a whole-section query
        // prints `section.key value`, as git does.
        match &key {
            Some(_) => emit_kv(&mut out, d, name, value, &meta, b' ', false)?,
            None => emit_kv(&mut out, d, &format!("{section}.{name}"), value, &meta, b' ', true)?,
        }
    }
    Ok(ExitCode::SUCCESS)
}

/// How specifically a config URL pattern matched the queried URL. Ordered the
/// way git ranks candidates: user first, then host length, then path length.
#[derive(Default, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct UrlScore {
    user: bool,
    host_len: usize,
    path_len: usize,
}

/// The pieces of a URL git compares: scheme, optional user, host (with port),
/// and path. Deliberately hand-split rather than parsed by a URL crate, because
/// the comparison is textual and git's is too.
struct UrlParts {
    scheme: String,
    user: Option<String>,
    host: String,
    path: String,
}

impl UrlParts {
    fn parse(url: &str) -> Option<UrlParts> {
        let (scheme, rest) = url.split_once("://")?;
        let (authority, path) = match rest.find('/') {
            Some(i) => (&rest[..i], &rest[i..]),
            None => (rest, ""),
        };
        let (user, host) = match authority.rsplit_once('@') {
            Some((u, h)) => (Some(u.to_string()), h),
            None => (None, authority),
        };
        Some(UrlParts {
            scheme: scheme.to_lowercase(),
            user,
            host: host.to_lowercase(),
            path: path.trim_end_matches('/').to_string(),
        })
    }

    /// Score this PATTERN against `url`, or `None` when it does not match.
    fn score_against(&self, url: &UrlParts) -> Option<UrlScore> {
        if self.scheme != url.scheme || self.host != url.host {
            return None;
        }
        // A pattern that names a user matches only that user's URLs.
        if self.user.is_some() && self.user != url.user {
            return None;
        }
        // The pattern's path must be a prefix of the URL's, at a `/` boundary.
        let path = self.path.trim_end_matches('/');
        if !path.is_empty() {
            let rest = url.path.strip_prefix(path)?;
            if !(rest.is_empty() || rest.starts_with('/')) {
                return None;
            }
        }
        Some(UrlScore {
            user: self.user.is_some(),
            host_len: self.host.len(),
            path_len: path.len(),
        })
    }
}

/// `git config -e|--edit` — open the target config in the user's editor.
///
/// Editor precedence is git's `GIT_EDITOR` → `core.editor` → `VISUAL` →
/// `EDITOR` → `vi`, and the command is run through the shell exactly as git
/// does, so a configured editor with arguments (`code --wait`) works.
fn edit_config(target: &WriteTarget) -> Result<ExitCode> {
    let editor = std::env::var("GIT_EDITOR")
        .ok()
        .or_else(|| {
            gix::discover(".")
                .ok()
                .and_then(|r| r.config_snapshot().string("core.editor").map(|v| v.to_string()))
        })
        .or_else(|| std::env::var("VISUAL").ok())
        .or_else(|| std::env::var("EDITOR").ok())
        .unwrap_or_else(|| "vi".to_string());

    if target.create_parent {
        if let Some(parent) = target.path.parent() {
            std::fs::create_dir_all(parent).ok();
        }
    }
    // git creates the file first so the editor always opens something.
    if !target.path.exists() {
        std::fs::File::create(&target.path)?;
    }

    let status = std::process::Command::new("sh")
        .arg("-c")
        .arg(format!("{editor} \"$@\"", editor = editor))
        .arg(editor)
        .arg(&target.path)
        .status()?;
    Ok(match status.code() {
        Some(0) | None => ExitCode::SUCCESS,
        Some(code) => ExitCode::from(code as u8),
    })
}

/// `git config --get-colorbool <name> [<stdout-is-tty>]` — resolve a color
/// setting to `true`/`false`, printing it and exiting 0 when color is on, 1 when
/// off (git inverts the usual convention here so shell `if` reads naturally).
///
/// `auto` (and an unset key) depend on whether stdout is a terminal: the caller
/// may state that as the optional second operand, otherwise it is probed.
fn get_colorbool(file: &gix::config::File, positional: &[&str]) -> Result<ExitCode> {
    let Some(name) = positional.first() else {
        return usage_error("wrong number of arguments, should be from 1 to 2");
    };
    // git prints the resolved value only when the caller SAYS whether stdout is
    // a tty; with the argument omitted it answers through the exit code alone.
    let stated = positional.get(1);
    let tty = match stated {
        Some(v) => canonical_bool(v).unwrap_or(false),
        None => std::io::IsTerminal::is_terminal(&std::io::stdout()),
    };

    let key = parse_key(name)?;
    let raw = file
        .raw_value_filter_by(key.section_name, key.subsection_name, key.value_name, |m| {
            !is_synthetic(m.source)
        })
        .ok()
        .map(|v| String::from_utf8_lossy(&v).trim().to_string());

    // git: an explicit boolean wins; `auto` and "unset" both defer to the tty.
    let on = match raw.as_deref() {
        Some("auto") | None => tty,
        Some(v) => canonical_bool(v).unwrap_or(true), // a color NAME means "on"
    };
    if stated.is_some() {
        println!("{on}");
        return Ok(ExitCode::SUCCESS);
    }
    Ok(if on { ExitCode::SUCCESS } else { ExitCode::from(1) })
}

/// `git config --rename-section <old> <new>` — rewrite the section header in
/// place, keeping every value. Missing section is git's
/// `fatal: no such section: <old>` at exit 128.
fn rename_section(target: &WriteTarget, positional: &[&str]) -> Result<ExitCode> {
    let (old, new) = match positional {
        [old, new] => (*old, *new),
        _ => return usage_error("wrong number of arguments, should be 2"),
    };
    let (old_name, old_sub) = split_section(old);
    let (new_name, new_sub) = split_section(new);

    let _lock = crate::lock::RepoLock::acquire(&target.lock_key);
    let mut file = load_or_empty(&target.path, target.source)?;

    // gix exposes no public "rewrite this header", so the rename is a move: read
    // the old section's entries in order, drop it, and push them into a section
    // with the new name. Values keep their order and their multivar repeats.
    let Ok(section) = file.section(&old_name, old_sub.as_deref().map(gix::bstr::BStr::new)) else {
        eprintln!("fatal: no such section: {old}");
        return Ok(ExitCode::from(128));
    };
    let mut entries: Vec<(String, Vec<u8>)> = Vec::new();
    let mut seen: Vec<(String, usize)> = Vec::new();
    for raw in section.value_names() {
        let name = raw.to_lowercase();
        let nth = seen.iter().filter(|(n, _)| *n == name).count();
        seen.push((name.clone(), nth));
        if let Some(v) = section.values(&name).into_iter().nth(nth) {
            entries.push((name, v.to_vec()));
        }
    }
    file.remove_section(&old_name, old_sub.as_deref().map(gix::bstr::BStr::new));

    let mut dest = file.section_mut_or_create_new(
        &new_name,
        new_sub.as_deref().map(gix::bstr::BStr::new),
    )?;
    for (name, value) in &entries {
        dest.push(name.as_str(), Some(value.as_slice().into()))?;
    }
    persist(&target.path, &file)?;
    Ok(ExitCode::SUCCESS)
}

/// `git config --remove-section <name>` — drop the section and everything in it.
fn remove_section(target: &WriteTarget, positional: &[&str]) -> Result<ExitCode> {
    let Some(name) = positional.first() else {
        return usage_error("wrong number of arguments, should be 1");
    };
    let (section, sub) = split_section(name);

    let _lock = crate::lock::RepoLock::acquire(&target.lock_key);
    let mut file = load_or_empty(&target.path, target.source)?;
    let mut removed = false;
    while file
        .remove_section(&section, sub.as_deref().map(gix::bstr::BStr::new))
        .is_some()
    {
        removed = true;
    }
    if !removed {
        eprintln!("fatal: no such section: {name}");
        return Ok(ExitCode::from(128));
    }
    persist(&target.path, &file)?;
    Ok(ExitCode::SUCCESS)
}

/// `git config --replace-all <name> <value> [<value-pattern>]` — collapse every
/// matching value of the key to a single `<value>`. Without a pattern that is
/// every value of the key; with one, only the values the ERE selects (and a key
/// whose values none match gains `<value>` as a new entry, as git's
/// `--replace-all` does).
fn replace_all(
    target: &WriteTarget,
    name: &str,
    value: &str,
    value_pattern: Option<&str>,
) -> Result<ExitCode> {
    let key = parse_key(name)?;
    let section_lc = key.section_name.to_lowercase();
    let value_lc = key.value_name.to_lowercase();
    let filter = match value_pattern.map(ValueFilter::parse) {
        Some(Err(code)) => return Ok(code),
        Some(Ok(f)) => Some(f),
        None => None,
    };

    let _lock = crate::lock::RepoLock::acquire(&target.lock_key);
    let mut file = load_or_empty(&target.path, target.source)?;

    // Drop every value the filter selects, then push the replacement once — the
    // "collapse to one" half of git's semantics.
    let keep: Vec<Vec<u8>> = file
        .raw_values_by(&section_lc, key.subsection_name, &value_lc)
        .unwrap_or_default()
        .into_iter()
        .filter(|v| filter.as_ref().is_some_and(|f| !f.matches(v)))
        .map(|v| v.to_vec())
        .collect();

    if let Ok(mut section) = file.section_mut(&section_lc, key.subsection_name) {
        while section.remove(&value_lc).is_some() {}
        for v in &keep {
            section.push(value_lc.as_str(), Some(v.as_slice().into()))?;
        }
        section.push(value_lc.as_str(), Some(value.into()))?;
    } else {
        file.section_mut_or_create_new(&section_lc, key.subsection_name)?
            .push(value_lc.as_str(), Some(value.into()))?;
    }
    persist(&target.path, &file)?;
    Ok(ExitCode::SUCCESS)
}

/// Split a `--rename-section`/`--remove-section` operand into its section and
/// optional subsection halves: `remote.origin` is the subsection `origin` of
/// `remote`, while `core` has none.
fn split_section(spec: &str) -> (String, Option<String>) {
    match spec.split_once('.') {
        Some((name, sub)) => (name.to_lowercase(), Some(sub.to_string())),
        None => (spec.to_lowercase(), None),
    }
}

enum WriteOp {
    Set,
    Add,
    Unset,
    UnsetAll,
}

/// Read a config file, mirroring git's `fopen_or_warn`: every errno except
/// `ENOENT`/`ENOTDIR` — a directory in the way, a file the user cannot read —
/// first gets a `warning: unable to access '<path>': <errno>` line, then the
/// caller decides how fatal the failure is.
fn read_config_bytes(path: &std::path::Path) -> std::io::Result<Vec<u8>> {
    std::fs::read(path).inspect_err(|err| {
        if !matches!(err.kind(), std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory)
        {
            eprintln!("warning: unable to access '{}': {}", path.display(), errno_text(err));
        }
    })
}

/// An empty config file carrying `path`/`source` metadata, so entries written
/// into it later report the right provenance.
fn empty_config(path: &std::path::Path, source: Source) -> ConfigFile {
    ConfigFile::new(gix::config::file::Metadata::from(source).at(path))
}

fn parse_config(bytes: &[u8], path: &std::path::Path, source: Source) -> Result<ConfigFile> {
    Ok(ConfigFile::from_bytes_no_includes(
        bytes,
        gix::config::file::Metadata::from(source).at(path),
        Default::default(),
    )?)
}

/// Load the config file at `path` for `source`, or an empty file carrying that
/// source/path metadata when it does not exist yet (git creates a fresh
/// `~/.gitconfig` on first `--global` write). Only a missing file is treated as
/// empty; a malformed existing file still errors, as git's parser does.
fn load_or_empty(path: &std::path::Path, source: Source) -> Result<ConfigFile> {
    match read_config_bytes(path) {
        Ok(bytes) => parse_config(&bytes, path, source),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(empty_config(path, source)),
        Err(err) => Err(err.into()),
    }
}

/// Load a write target's current contents, or an empty file when it does not
/// exist yet.
///
/// `None` means git would refuse the target outright — it exists but cannot be
/// read at all, e.g. a directory named by `--file` — which is
/// `error: invalid config file <path>` at exit 3. The diagnostic is emitted
/// here so both write paths report it identically.
fn load_for_write(path: &std::path::Path, source: Source) -> Result<Option<ConfigFile>> {
    match read_config_bytes(path) {
        Ok(bytes) => parse_config(&bytes, path, source).map(Some),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            Ok(Some(empty_config(path, source)))
        }
        Err(_) => {
            eprintln!("error: invalid config file {}", path.display());
            Ok(None)
        }
    }
}

/// The read file for a whole scope: every existing file among `sources`, merged
/// in order so the last (highest-precedence) one wins — `~/.gitconfig` over the
/// XDG file for `--global`, for instance. Missing files are skipped.
fn read_scope(sources: &[Source]) -> ConfigFile {
    let mut env = |k: &str| std::env::var_os(k);
    let mut acc = gix::config::File::new(gix::config::file::Metadata::default());
    for &source in sources {
        if let Some(path) = source.storage_location(&mut env) {
            if path.exists() {
                if let Ok(f) = ConfigFile::from_path_no_includes(path, source) {
                    let _ = acc.append(f);
                }
            }
        }
    }
    acc
}

/// Resolve the single file a scoped write targets, mirroring git's
/// `given_config_source` resolution.
fn resolve_write_target(scope: &Scope, repo: Option<&gix::Repository>) -> Result<WriteTarget> {
    match scope {
        Scope::Default | Scope::Local => {
            let repo = repo.ok_or_else(|| match scope {
                Scope::Local => {
                    anyhow::anyhow!("--local can only be used inside a git repository")
                }
                _ => anyhow::anyhow!("not in a git directory"),
            })?;
            Ok(WriteTarget {
                path: repo.common_dir().join("config"),
                source: Source::Local,
                lock_key: repo.git_dir().to_path_buf(),
                create_parent: true,
            })
        }
        Scope::Global => {
            let mut env = |k: &str| std::env::var_os(k);
            let user = Source::User.storage_location(&mut env); // ~/.gitconfig
            let xdg = Source::Git.storage_location(&mut env); // $XDG_CONFIG_HOME/git/config
            // git writes `~/.gitconfig` unless it is absent while the XDG file exists.
            let (path, source) = match (user, xdg) {
                (Some(u), Some(x)) if !u.exists() && x.exists() => (x, Source::Git),
                (Some(u), _) => (u, Source::User),
                (None, Some(x)) => (x, Source::Git),
                (None, None) => crate::git_fatal!("could not determine the global config path (HOME unset)"),
            };
            Ok(WriteTarget {
                lock_key: path.clone(),
                path,
                source,
                create_parent: true,
            })
        }
        Scope::System => {
            let mut env = |k: &str| std::env::var_os(k);
            let path = Source::System
                .storage_location(&mut env)
                .ok_or_else(|| anyhow::anyhow!("the system config is unavailable"))?;
            Ok(WriteTarget {
                lock_key: path.clone(),
                path,
                source: Source::System,
                create_parent: true,
            })
        }
        // `--file` writes exactly the named path, creating the file but never
        // its directory — git fails to take its lock there instead.
        Scope::File(path) => Ok(WriteTarget {
            path: path.clone(),
            source: Source::Cli,
            lock_key: path.clone(),
            create_parent: false,
        }),
    }
}

/// Mutate the scoped config file (`<common_dir>/config` for `--local`,
/// `~/.gitconfig` for `--global`, …) and persist it atomically. Serialized
/// through the coordinator (keyed on the target file) so a concurrent zvcs writer
/// can't interleave a partial rewrite; the parent directory is created for a
/// first-time global/system write.
fn write_scoped(target: &WriteTarget, name: &str, value: &str, op: WriteOp) -> Result<ExitCode> {
    let key = parse_key(name)?;
    let section_lc = key.section_name.to_lowercase();
    let value_lc = key.value_name.to_lowercase();

    let _lock = crate::lock::RepoLock::acquire(&target.lock_key);

    prepare_parent(target);
    let path = &target.path;
    let Some(mut file) = load_for_write(path, target.source)? else {
        return Ok(ExitCode::from(3));
    };

    match op {
        WriteOp::Set => {
            file.set_raw_value_by(&section_lc, key.subsection_name, &value_lc, value)?;
        }
        WriteOp::Add => {
            file.section_mut_or_create_new(&section_lc, key.subsection_name)?
                .push(&value_lc, value)?;
        }
        WriteOp::Unset | WriteOp::UnsetAll => {
            let mut section = match file.section_mut(key.section_name, key.subsection_name) {
                Ok(s) => s,
                // Unsetting an absent key is exit 5 in stock git.
                Err(_) => return Ok(ExitCode::from(5)),
            };
            let count = section.values(key.value_name).len();
            if count == 0 {
                return Ok(ExitCode::from(5));
            }
            if matches!(op, WriteOp::Unset) && count > 1 {
                crate::git_fatal!("key contains multiple values: {name}");
            }
            if matches!(op, WriteOp::UnsetAll) {
                while section.remove(key.value_name).is_some() {}
            } else {
                section.remove(key.value_name);
            }
        }
    }

    persist_or_lock_error(path, &file)
}

/// `git config <name> <value> <value-pattern>` — the value-pattern set form.
///
/// Among the existing values of `<name>` in the scoped file, the POSIX ERE
/// `<value-pattern>` selects which are rewritten to `<value>` (a leading `!`
/// inverts the match, matching against the value text as bytes, unanchored —
/// git's `regexec`). The outcomes mirror stock git exactly:
///
/// ```text
///   * no value matches   → append `<value>` as a new line (exit 0)
///   * exactly one matches → rewrite that value in place (exit 0)
///   * more than one       → without `--replace-all` git refuses: it prints
///                           `warning: <key> has multiple values` on stderr,
///                           leaves the file untouched, and exits 5
///   * invalid ERE         → `error: invalid pattern: <pattern>`, exit 6
/// ```
fn set_with_value_pattern(
    target: &WriteTarget,
    name: &str,
    value: &str,
    value_pattern: &str,
) -> Result<ExitCode> {
    let key = parse_key(name)?;
    let section_lc = key.section_name.to_lowercase();
    let value_lc = key.value_name.to_lowercase();

    // A leading `!` inverts the match; the remainder is the ERE. Compile it the
    // way git does before touching the file, so a bad pattern is exit 6 whether
    // or not any value would have matched.
    let (invert, pat) = match value_pattern.strip_prefix('!') {
        Some(rest) => (true, rest),
        None => (false, value_pattern),
    };
    let re = match regex::bytes::Regex::new(pat) {
        Ok(re) => re,
        Err(_) => {
            eprintln!("error: invalid pattern: {pat}");
            return Ok(ExitCode::from(6));
        }
    };

    let _lock = crate::lock::RepoLock::acquire(&target.lock_key);

    prepare_parent(target);
    let path = &target.path;
    let Some(mut file) = load_for_write(path, target.source)? else {
        return Ok(ExitCode::from(3));
    };

    // Existing values of the key in this file, in order of occurrence. An absent
    // key yields an empty list, which routes to the append branch below.
    let existing = file
        .raw_values_by(&section_lc, key.subsection_name, &value_lc)
        .unwrap_or_default();

    let mut matching: Vec<usize> = Vec::new();
    for (i, v) in existing.iter().enumerate() {
        if re.is_match(v.as_ref()) != invert {
            matching.push(i);
        }
    }

    match matching.as_slice() {
        // No value matches: append a new one (git's add-on-no-match).
        [] => {
            file.section_mut_or_create_new(&section_lc, key.subsection_name)?
                .push(&value_lc, value)?;
        }
        // Exactly one match: rewrite that value in place. The index is shared
        // with `raw_values_by` above — both walk values in occurrence order.
        [idx] => {
            file.raw_values_mut_by(&section_lc, key.subsection_name, &value_lc)?
                .set_string_at(*idx, value)?;
        }
        // Multiple matches without `--replace-all`: git warns and exits 5,
        // leaving the file untouched.
        _ => {
            let key_disp = match key.subsection_name {
                Some(sub) => format!("{section_lc}.{sub}.{value_lc}"),
                None => format!("{section_lc}.{value_lc}"),
            };
            eprintln!("warning: {key_disp} has multiple values");
            return Ok(ExitCode::from(5));
        }
    }

    persist_or_lock_error(path, &file)
}

/// Create the directory holding a scoped target when git would — a first
/// `--global` write into a fresh `~/.config/git/`, say. A `--file` target is
/// left alone: git does not create that directory either, and a missing one has
/// to fail at the write below with git's lock diagnostic.
fn prepare_parent(target: &WriteTarget) {
    if !target.create_parent {
        return;
    }
    if let Some(parent) = target.path.parent() {
        std::fs::create_dir_all(parent).ok();
    }
}

/// Persist `file`, reporting a failure the way git reports being unable to take
/// the config lock: `error: could not lock config file <path>: <errno>`, exit
/// 255. That is git's outcome for an unwritable target of any scope — a missing
/// `--file` directory, a read-only `~/.gitconfig`, a `--system` file owned by
/// root.
fn persist_or_lock_error(path: &std::path::Path, file: &ConfigFile) -> Result<ExitCode> {
    match persist(path, file) {
        Ok(()) => Ok(ExitCode::SUCCESS),
        Err(err) => {
            eprintln!(
                "error: could not lock config file {}: {}",
                path.display(),
                errno_text(&err)
            );
            Ok(ExitCode::from(255))
        }
    }
}

/// Write `file` to `path` atomically: serialize to a sibling temp file, then
/// rename over the target so a crash never leaves a half-written config.
/// Set `branch.<branch>.remote` and `branch.<branch>.merge` in the local config,
/// as `git push --set-upstream` / `git branch --set-upstream-to` do. Reuses the
/// same lock + atomic-write path as `git config`.
pub(crate) fn set_branch_upstream(
    repo: &gix::Repository,
    branch: &str,
    remote: &str,
    merge_ref: &str,
) -> Result<()> {
    let _lock = crate::lock::RepoLock::acquire(repo.git_dir());
    let path = repo.common_dir().join("config");
    let mut file = ConfigFile::from_path_no_includes(path.clone(), Source::Local)?;
    let sub = gix::bstr::BStr::new(branch);
    file.set_raw_value_by("branch", Some(sub), "remote", remote)?;
    file.set_raw_value_by("branch", Some(sub), "merge", merge_ref)?;
    persist(&path, &file)?;
    Ok(())
}

fn persist(path: &std::path::Path, file: &ConfigFile) -> std::io::Result<()> {
    let bytes = file.to_bstring();
    let tmp = path.with_extension("zvcs-tmp");
    {
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(&bytes)?;
        f.sync_all()?;
    }
    std::fs::rename(&tmp, path)?;
    Ok(())
}

use crate::optint;
use anyhow::{bail, Result};
use std::io::Write;
use std::process::ExitCode;

use gix::config::{File as ConfigFile, KeyRef, Source};

/// `usage_with_options()` over `builtin/config.c`'s subcommand table.
const USAGE: &str = r"usage: git config list [<file-option>] [<display-option>] [--includes]
   or: git config get [<file-option>] [<display-option>] [--includes] [--all] [--regexp] [--value=<pattern>] [--fixed-value] [--default=<default>] [--url=<url>] <name>
   or: git config set [<file-option>] [--type=<type>] [--all] [--value=<pattern>] [--fixed-value] <name> <value>
   or: git config unset [<file-option>] [--all] [--value=<pattern>] [--fixed-value] <name>
   or: git config rename-section [<file-option>] <old-name> <new-name>
   or: git config remove-section [<file-option>] <name>
   or: git config edit [<file-option>]
   or: git config [<file-option>] --get-colorbool <name> [<stdout-is-tty>]

";

/// The block `cmd_config_actions()` renders (3100 bytes, git 2.55.0): the same
/// usage lines as [`USAGE`] followed by the whole legacy option table.
///
/// git has two usage renderings for `config` and they are not interchangeable.
/// `-h` and `--help-all` are answered by the *outer* `parse_options()` call,
/// whose table is nothing but `OPT_SUBCOMMAND`s, so the block is the usage lines
/// alone. Every failure inside `cmd_config_actions()` — an unknown option, an
/// ambiguous abbreviation — is rendered from *its* table instead, and that one
/// lists all thirty-six entries.
const USAGE_ACTIONS: &str = r#"usage: git config list [<file-option>] [<display-option>] [--includes]
   or: git config get [<file-option>] [<display-option>] [--includes] [--all] [--regexp] [--value=<pattern>] [--fixed-value] [--default=<default>] [--url=<url>] <name>
   or: git config set [<file-option>] [--type=<type>] [--all] [--value=<pattern>] [--fixed-value] <name> <value>
   or: git config unset [<file-option>] [--all] [--value=<pattern>] [--fixed-value] <name>
   or: git config rename-section [<file-option>] <old-name> <new-name>
   or: git config remove-section [<file-option>] <name>
   or: git config edit [<file-option>]
   or: git config [<file-option>] --get-colorbool <name> [<stdout-is-tty>]

Config file location
    --[no-]global         use global config file
    --[no-]system         use system config file
    --[no-]local          use repository config file
    --[no-]worktree       use per-worktree config file
    -f, --[no-]file <file>
                          use given config file
    --[no-]blob <blob-id> read config from given blob object

Action
    --get                 get value: name [<value-pattern>]
    --get-all             get all values: key [<value-pattern>]
    --get-regexp          get values for regexp: name-regex [<value-pattern>]
    --get-urlmatch        get value specific for the URL: section[.var] URL
    --replace-all         replace all matching variables: name value [<value-pattern>]
    --add                 add a new variable: name value
    --unset               remove a variable: name [<value-pattern>]
    --unset-all           remove all matches: name [<value-pattern>]
    --rename-section      rename section: old-name new-name
    --remove-section      remove a section: name
    -l, --list            list all
    -e, --edit            open an editor
    --get-color           find the color configured: slot [<default>]
    --get-colorbool       find the color setting: slot [<stdout-is-tty>]

Display options
    -z, --[no-]null       terminate values with NUL byte
    --[no-]name-only      show variable names only
    --[no-]show-origin    show origin of config (file, standard input, blob, command line)
    --[no-]show-scope     show scope of config (worktree, local, global, system, command)
    --[no-]show-names     show config keys in addition to their values

Type
    -t, --[no-]type <type>
                          value is given this type
    --bool                value is "true" or "false"
    --int                 value is decimal number
    --bool-or-int         value is --bool or --int
    --bool-or-str         value is --bool or string
    --path                value is a path (file or directory name)
    --expiry-date         value is an expiry date

Other
    --[no-]default <value>
                          with --get, use default value when missing entry
    --[no-]comment <value>
                          human-readable comment string (# will be prepended as needed)
    --[no-]fixed-value    use string equality when comparing values to value pattern
    --[no-]includes       respect include directives on lookup

"#;

/// `cmd_config_actions()`'s `opts[]` (builtin/config.c:1368-1393), flattened
/// through the three macros it is built from, in declaration order — which is
/// the order the block above lists them in and the order an `ambiguous option:`
/// sentence reports its two candidates in.
///
/// The outer `cmd_config()` table is not represented: it is `OPT_SUBCOMMAND`s
/// only (skipped by `parse_long_opt()`, parse-options.c:542-543) and is parsed
/// with `PARSE_OPT_KEEP_UNKNOWN_OPT`, under which `register_abbrev()` returns
/// without recording anything (parse-options.c:509-510). Nothing abbreviates
/// there; every long option this command has resolves here.
pub(super) const ACTION_OPTS: &[super::LongOpt] = {
    use super::{Arg, LongOpt};
    /// `OPT_CMDMODE` is `PARSE_OPT_CMDMODE|PARSE_OPT_NOARG|PARSE_OPT_NONEG`
    /// (parse-options.h:274-283): no value, and no `--no-` spelling.
    const fn cmdmode(name: &'static str) -> LongOpt {
        LongOpt { name, neg: false, arg: Arg::None }
    }
    /// `OPT_CALLBACK_VALUE` (builtin/config.c:140-149) is
    /// `PARSE_OPT_NOARG | PARSE_OPT_NONEG` — the six legacy type spellings.
    const fn type_value(name: &'static str) -> LongOpt {
        LongOpt { name, neg: false, arg: Arg::None }
    }
    &[
        // CONFIG_LOCATION_OPTIONS (builtin/config.c:68-74)
        LongOpt { name: "global", neg: true, arg: Arg::None },
        LongOpt { name: "system", neg: true, arg: Arg::None },
        LongOpt { name: "local", neg: true, arg: Arg::None },
        LongOpt { name: "worktree", neg: true, arg: Arg::None },
        LongOpt { name: "file", neg: true, arg: Arg::Required },
        LongOpt { name: "blob", neg: true, arg: Arg::Required },
        // Action
        cmdmode("get"),
        cmdmode("get-all"),
        cmdmode("get-regexp"),
        cmdmode("get-urlmatch"),
        cmdmode("replace-all"),
        cmdmode("add"),
        cmdmode("unset"),
        cmdmode("unset-all"),
        cmdmode("rename-section"),
        cmdmode("remove-section"),
        cmdmode("list"),
        cmdmode("edit"),
        cmdmode("get-color"),
        cmdmode("get-colorbool"),
        // CONFIG_DISPLAY_OPTIONS (builtin/config.c:111-118)
        LongOpt { name: "null", neg: true, arg: Arg::None },
        LongOpt { name: "name-only", neg: true, arg: Arg::None },
        LongOpt { name: "show-origin", neg: true, arg: Arg::None },
        LongOpt { name: "show-scope", neg: true, arg: Arg::None },
        LongOpt { name: "show-names", neg: true, arg: Arg::None },
        // CONFIG_TYPE_OPTIONS (builtin/config.c:102-109)
        LongOpt { name: "type", neg: true, arg: Arg::Required },
        type_value("bool"),
        type_value("int"),
        type_value("bool-or-int"),
        type_value("bool-or-str"),
        type_value("path"),
        type_value("expiry-date"),
        // Other
        LongOpt { name: "default", neg: true, arg: Arg::Required },
        LongOpt { name: "comment", neg: true, arg: Arg::Required },
        LongOpt { name: "fixed-value", neg: true, arg: Arg::None },
        LongOpt { name: "includes", neg: true, arg: Arg::None },
    ]
};

/// Refuse a dashed argument the legacy table did not dispatch on.
///
/// Two different refusals hide behind one arm, and telling them apart is the
/// whole point: a name [`ACTION_OPTS`] claims is a flag stock git *has* and this
/// port has not ported, so it is refused by its full name at every spelling that
/// reaches it; a name no entry claims is not this port's business at all and
/// gets git's own `unknown option` / `unknown switch`, with the legacy block on
/// stderr and exit 129.
/// `option_parse_type()` (builtin/config.c:151-200): the callback behind
/// `--type=<t>`, behind `-t`, and behind each of the six `--<type>` spellings —
/// whose `OPT_CALLBACK_VALUE` `defval` names the type outright instead of
/// parsing an argument.
///
/// Two refusals belong to it rather than to any later check, and both were
/// verified against stock 2.55.0:
///
/// * an unrecognized name is a `die()`: `fatal: unrecognized --type argument, zz`
///   on stderr, exit 128 — not a usage error.
/// * a *second, different* type is rejected the moment it is parsed:
///   `git config --bool --type=int` is `error: only one type at a time` at 129,
///   while `--int --type=int` and `--type=int --type=int` are both fine.
///
/// The slot holds git's own name for the type, not this port's [`ValueType`],
/// so that a type git has and this port has not canonicalized is refused by
/// name instead of being reported as unrecognized.
fn select_type(slot: &mut Option<&'static str>, arg: &str) -> std::result::Result<(), ExitCode> {
    // `TYPE_COLOR` is the one with no `--<type>` spelling of its own.
    const TYPES: [&str; 7] = [
        "bool",
        "int",
        "bool-or-int",
        "bool-or-str",
        "path",
        "expiry-date",
        "color",
    ];
    let Some(&new) = TYPES.iter().find(|t| **t == arg) else {
        eprintln!("fatal: unrecognized --type argument, {arg}");
        return Err(ExitCode::from(128));
    };
    if slot.is_some_and(|old| old != new) {
        eprintln!("error: only one type at a time");
        return Err(ExitCode::from(129));
    }
    *slot = Some(new);
    Ok(())
}

fn reject(tok: &str) -> Result<ExitCode> {
    if let Some(body) = tok.strip_prefix("--") {
        let stem = body.split_once('=').map_or(body, |(n, _)| n);
        if matches!(super::resolve_long(ACTION_OPTS, stem), super::Resolved::One(..)) {
            bail!("{tok} is not implemented");
        }
    }
    // Every switch the table declares (`-f`, `-z`, `-t`, `-l`, `-e`) is handled
    // by the caller, so a short one reaching here really is unknown.
    Ok(super::unknown_option(tok, USAGE_ACTIONS))
}

#[derive(PartialEq, Clone, Copy)]
enum Mode {
    Auto,
    Get,
    GetAll,
    GetRegexp,
    GetUrlMatch,
    GetColor,
    GetColorBool,
    List,
    Add,
    ReplaceAll,
    Unset,
    UnsetAll,
    RenameSection,
    RemoveSection,
    Edit,
    /// `git config get --regexp <name>`: `get_value()` with `GET_VALUE_KEY_REGEXP`
    /// (builtin/config.c:1086), which is the `--get`/`--get-all` reader with a
    /// regexp *name* — values only, no keys, unlike the legacy `--get-regexp`.
    /// Not spellable on the command line; the `get` subcommand maps to it.
    GetKeyRegexp,
    /// [`Mode::GetKeyRegexp`] with `--all`.
    GetKeyRegexpAll,
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
            Mode::GetKeyRegexp | Mode::GetKeyRegexpAll => "--regexp",
            Mode::GetUrlMatch => "--get-urlmatch",
            Mode::GetColor => "--get-color",
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
    /// `--blob <blob-id>`: the config text is an object in the repository, named
    /// by any revision spec that resolves to a blob. Read-only — `check_write()`
    /// (builtin/config.c:820-821) refuses to write one, and `show_editor()`
    /// (:1299-1300) refuses to edit one.
    Blob(String),
    /// `--worktree`: `$GIT_DIR/config.worktree` when `extensions.worktreeConfig`
    /// is on, and the repository's own `config` when it is not — see
    /// [`worktree_config_file`], which is `location_options_init()`'s arm for it
    /// (builtin/config.c:975-991).
    Worktree,
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
#[derive(Default, Clone)]
struct Display {
    /// `--fixed-value` (`CONFIG_FLAGS_FIXED_VALUE`): every `<value-pattern>` is compared
    /// literally instead of as a POSIX ERE.
    fixed_value: bool,
    show_origin: bool,
    show_scope: bool,
    /// `--show-names` (`display_opts.show_keys`, builtin/config.c:118): a `get` prints the key
    /// ahead of the value, which is what `list` and `--get-regexp` do unconditionally.
    show_names: bool,
    null: bool,
    name_only: bool,
    ty: Option<ValueType>,
    /// `--default=<value>` (`display_opts.default_value`, builtin/config.c:127):
    /// the value a `--get` that finds nothing formats and prints instead. It goes
    /// through the *same* `format_config()` the real values do, so `--type` applies
    /// to it and a default the type rejects is a fatal error.
    default_value: Option<String>,
    /// git's `startup_info->prefix` (slash-terminated), which `--show-origin`
    /// needs because git prints its paths from the top of the work tree and the
    /// port never moved there. See [`write_origin`].
    prefix: Option<String>,
    /// `--blob <blob-id>` as it was given. Every entry a blob read produces
    /// carries `CONFIG_ORIGIN_BLOB` and this name in its `key_value_info`, which
    /// is what `--show-origin` prints instead of a file.
    blob: Option<String>,
}

/// `--type=<t>` and its legacy spellings (`--bool`, `--int`, `--bool-or-int`,
/// `--bool-or-str`, `--path`, `--expiry-date`), plus `--type=color`, which has no
/// legacy spelling of its own. These are git's seven `format_config()` arms
/// (builtin/config.c:425-452).
#[derive(Clone, Copy, PartialEq)]
enum ValueType {
    Bool,
    Int,
    BoolOrInt,
    BoolOrStr,
    Path,
    ExpiryDate,
    Color,
}

/// Why a value would not canonicalize, and therefore which of git's three
/// different failure shapes the caller has to reproduce.
enum TypeError {
    /// `die_bad_number()` (config.c:1188): one self-contained `fatal:` naming the
    /// value, the key, the file it came from and whether the number was out of
    /// range or carried a unit suffix git does not know.
    BadNumber { out_of_range: bool },
    /// `git_config_bool()` (config.c:1292): a bare `die()` with no origin.
    BadBool,
    /// The arms that `error()` and hand a negative return back to the config
    /// machinery, which then aborts the parse with its own second line — the
    /// physical config line for a file, or `unable to parse command-line config`
    /// for a `-c` / `GIT_CONFIG_*` value.
    Callback(String),
    /// ```c
    /// *dest = interpolate_path(value, 0);
    /// if (!*dest)
    ///         die(_("failed to expand user dir in: '%s'"), value);
    /// ```
    ///
    /// (`git_config_pathname()`, config.c.) `interpolate_path()` answers NULL for
    /// a `~user` no passwd entry names and for a bare `~` with no `$HOME`; both
    /// are one bare `die()` naming the *unexpanded* value.
    ExpandUser,
}

impl ValueType {
    fn parse(name: &str) -> Option<ValueType> {
        match name {
            "bool" => Some(ValueType::Bool),
            "int" => Some(ValueType::Int),
            "bool-or-int" => Some(ValueType::BoolOrInt),
            "bool-or-str" => Some(ValueType::BoolOrStr),
            "path" => Some(ValueType::Path),
            "expiry-date" => Some(ValueType::ExpiryDate),
            "color" => Some(ValueType::Color),
            _ => None,
        }
    }

    /// Canonicalize `value` the way git prints it under this type.
    ///
    /// The `gently` flag is `format_config()`'s: `show_all_config()` passes 1 —
    /// `git config --list --type=<t>` silently drops every entry that does not
    /// parse — while `collect_config()` passes 0, so a read of a named key dies on
    /// the first value that does not. Only the message differs; what parses and
    /// what does not is the same either way, so the error is always described and
    /// the caller decides whether to print it.
    fn canonicalize(
        self,
        key: &str,
        value: &[u8],
        implicit: bool,
    ) -> std::result::Result<Vec<u8>, TypeError> {
        // Verbatim: git hands the stored bytes to each type's reader untouched,
        // and the number grammar itself skips *leading* blanks only — so a
        // trailing one is an unreadable unit rather than something to trim away.
        //
        // A key written with no `=` is git's `NULL` value, and the boolean readers answer
        // *true* for it: `git_config_bool(key, NULL)` reaches `git_parse_maybe_bool(NULL)`,
        // which returns 1 (config.c). The empty string is a different thing entirely — it
        // reads as false — so the two have to be told apart before the text is examined.
        if implicit {
            match self {
                ValueType::Bool | ValueType::BoolOrInt | ValueType::BoolOrStr => {
                    return Ok(b"true".to_vec());
                }
                _ => {}
            }
        }
        let text = String::from_utf8_lossy(value).to_string();
        match self {
            ValueType::Bool => optint::maybe_bool(&text)
                .map(|b| b.to_string().into_bytes())
                .ok_or(TypeError::BadBool),
            ValueType::Int => canonical_int(&text, Width::Int64),
            // `git_config_bool_or_int()` (config.c:1280) asks
            // `git_parse_maybe_bool_text()` — the *word* half of the boolean
            // grammar, with no number fallback — and prints `true`/`false` when it
            // answers. A digit string therefore falls through to the integer arm
            // and prints as a number, which is the whole point of the type.
            ValueType::BoolOrInt => match optint::maybe_bool_text(&text) {
                Some(b) => Ok(b.to_string().into_bytes()),
                None => canonical_int(&text, Width::Int),
            },
            // `format_config_bool_or_str()` (builtin/config.c:332) is documented as
            // "always gentle": a value that is not a boolean is simply itself, so
            // this arm cannot fail.
            ValueType::BoolOrStr => Ok(match optint::maybe_bool(&text) {
                Some(b) => b.to_string().into_bytes(),
                None => value.to_vec(),
            }),
            ValueType::Path => expand_config_path(&text).map(String::into_bytes),
            // `git_config_expiry_date()` (config.c) — `parse_expiry_date()` with an
            // `error()` in front of the failure, and the epoch seconds printed raw.
            ValueType::ExpiryDate => match crate::date::parse_expiry_date(&text) {
                Some(t) => Ok(t.to_string().into_bytes()),
                None => Err(TypeError::Callback(format!(
                    "error: '{text}' for '{key}' is not a valid timestamp"
                ))),
            },
            // `git_config_color()` → `color_parse()`, which prints the ANSI escape
            // the spec resolves to. A spec selecting nothing yields the empty
            // string, which git prints as an empty line.
            ValueType::Color => match super::color::parse_color_spec(&text) {
                Some(sgr) => Ok(sgr.into_bytes()),
                None => Err(TypeError::Callback(format!("error: invalid color value: {text}"))),
            },
        }
    }
}

/// The width the reader bounds its value by, which is the one thing git's two
/// integer entry points differ in: `git_config_int64()` for `--type=int`,
/// `git_config_int()` (a C `int`) everywhere `git_parse_maybe_bool()` and
/// `git_config_bool_or_int()` reach the number.
#[derive(Clone, Copy)]
enum Width {
    Int,
    Int64,
}

/// `git_config_int()` / `git_config_int64()` (config.c): the base-0 `strtoimax`
/// grammar with a `k`/`m`/`g` unit, rendered back as the decimal git prints.
///
/// The two failures are distinct to git: a value that does not fit the target
/// width sets `errno` to `ERANGE` and is reported as `out of range`, while
/// anything else is `invalid unit`.
fn canonical_int(text: &str, width: Width) -> std::result::Result<Vec<u8>, TypeError> {
    let parsed = match width {
        Width::Int => optint::config_int(text),
        Width::Int64 => optint::config_int64(text),
    };
    match parsed {
        Ok(n) => Ok(n.to_string().into_bytes()),
        Err(e) => Err(TypeError::BadNumber {
            out_of_range: e == optint::NumError::OutOfRange,
        }),
    }
}

/// `--type=path`: `interpolate_path(value, 0)` (`path.c`). A leading `~/`
/// expands to `$HOME`, a leading `~user` to that user's passwd home directory,
/// and everything else is returned as it stands. A `~user` no passwd entry
/// names — and a bare `~` with no `$HOME` — is git's NULL return, which
/// `git_config_pathname()` turns into a `die()`; answering with the unexpanded
/// text instead made `git config --type=path --get pa.k` print
/// `~nosuchuser000/x` and exit 0 where stock is fatal at 128.
fn expand_config_path(text: &str) -> std::result::Result<String, TypeError> {
    if let Some(rest) = text.strip_prefix("~/") {
        return match std::env::var_os("HOME") {
            Some(home) => Ok(format!(
                "{}/{}",
                home.to_string_lossy().trim_end_matches('/'),
                rest
            )),
            None => Err(TypeError::ExpandUser),
        };
    }
    let Some(rest) = text.strip_prefix('~') else {
        return Ok(text.to_string());
    };
    // `const char *first_slash = strchrnul(path, '/');` — the username runs to
    // the first slash, and everything from that slash on is copied verbatim.
    let (user, tail) = match rest.find('/') {
        Some(at) => (&rest[..at], &rest[at..]),
        None => (rest, ""),
    };
    if user.is_empty() {
        return match std::env::var_os("HOME") {
            Some(home) => Ok(format!("{}{tail}", home.to_string_lossy())),
            None => Err(TypeError::ExpandUser),
        };
    }
    match passwd_home(user) {
        Some(home) => Ok(format!("{home}{tail}")),
        None => Err(TypeError::ExpandUser),
    }
}

/// `getpw_str()` (`path.c`): a user's home directory out of the passwd database,
/// or `None` when there is no such user.
fn passwd_home(user: &str) -> Option<String> {
    let name = std::ffi::CString::new(user).ok()?;
    // SAFETY: `getpwnam` returns a pointer into libc's static passwd storage,
    // which is copied out here before any other libc call can overwrite it.
    unsafe {
        let pw = libc::getpwnam(name.as_ptr());
        if pw.is_null() || (*pw).pw_dir.is_null() {
            return None;
        }
        Some(
            std::ffi::CStr::from_ptr((*pw).pw_dir)
                .to_string_lossy()
                .into_owned(),
        )
    }
}

/// The `--show-scope` word for a config source, matching git's scope names.
fn scope_word(source: Source) -> &'static str {
    match source {
        Source::System => "system",
        Source::Git | Source::User => "global",
        Source::Local => "local",
        Source::Worktree => "worktree",
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
pub(super) fn errno_text(err: &std::io::Error) -> String {
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
///   * `--worktree` → `$GIT_DIR/config.worktree` when `extensions.worktreeConfig`
///     is on, else the repository's own `config` — and with the extension off and
///     more than one working tree, the three-line refusal that names the
///     extension. See [`worktree_config_file`].
///   * `-f <path>` / `--file <path>` → exactly that file, `include.path`
///     directives followed only under `--includes` (every other scope follows
///     them unless `--no-includes` says otherwise). Never needs a repo; created
///     on write, but its parent directory is not — a missing one is git's
///     `could not lock config file <path>: <errno>` at exit 255. Reading a
///     missing file is exit 1 for the get forms and
///     `fatal: unable to read config file '<path>': <errno>` at exit 128 for
///     `--list`, exactly as git splits those two paths. One that will not parse
///     is `bad config line <n> in file <path>` before the action runs, a write
///     included.
///   * `--blob <rev>` → configuration read out of an object, never written; see
///     [`blob_config`].
/// ```
/// The default (no scope) write still targets the repository-local file and so
/// still needs a repo — attempting one without one fails with `not in a git
/// directory`. Two *different* scope flags at once → `only one config
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
/// The subcommand spellings `cmd_config()` dispatches on (builtin/config.c:1631-1637). Each has
/// its own option table and its own usage line, but every one of them ends up in the same
/// `get_value()` / `set_multivar()` machinery the legacy options reach — so this rewrites the
/// subcommand form into the legacy form and lets one implementation answer both.
///
/// The rewrite is exact, not approximate: `git config get --all <name>` *is*
/// `git config --get-all <name>` (`GET_VALUE_ALL`), `set --append` *is* `--add`
/// (`value_pattern = CONFIG_REGEX_NONE`), `set --all` *is* `--replace-all`
/// (`CONFIG_FLAGS_MULTI_REPLACE`), and `unset --all` *is* `--unset-all`. What the subcommands do
/// **not** share with the legacy form is their argument checking, which is why the refusals below
/// are spelled here rather than left to the legacy parser.
fn rewrite_subcommand(args: &[String]) -> Option<std::result::Result<Vec<String>, ExitCode>> {
    let sub = args.first()?.as_str();
    if !matches!(
        sub,
        "list" | "get" | "set" | "unset" | "rename-section" | "remove-section" | "edit"
    ) {
        return None;
    }
    let rest = &args[1..];

    // `parse_options(..., PARSE_OPT_STOP_AT_NON_OPTION)`: option scanning stops at the first
    // operand, and `--` is consumed.
    let mut opts: Vec<String> = Vec::new();
    let mut operands: Vec<String> = Vec::new();
    let mut all = false;
    let mut regexp = false;
    let mut append = false;
    let mut value_pattern: Option<String> = None;
    let mut url: Option<String> = None;
    let mut i = 0;
    while i < rest.len() {
        let a = rest[i].as_str();
        if !operands.is_empty() {
            operands.push(a.to_string());
            i += 1;
            continue;
        }
        match a {
            "--" => {
                operands.extend(rest[i + 1..].iter().cloned());
                break;
            }
            "--all" => all = true,
            "--no-all" => all = false,
            "--regexp" => regexp = true,
            "--no-regexp" => regexp = false,
            "--append" => append = true,
            "--no-append" => append = false,
            "-h" | "--help-all" => return Some(Err(super::show_usage(subcommand_usage(sub)))),
            _ if a.starts_with("--value=") => value_pattern = Some(a["--value=".len()..].to_string()),
            "--value" => {
                let Some(v) = rest.get(i + 1) else {
                    return Some(Err(super::missing_option_value(a)));
                };
                value_pattern = Some(v.clone());
                i += 1;
            }
            _ if a.starts_with("--url=") => url = Some(a["--url=".len()..].to_string()),
            "--url" => {
                let Some(v) = rest.get(i + 1) else {
                    return Some(Err(super::missing_option_value(a)));
                };
                url = Some(v.clone());
                i += 1;
            }
            // Location, display and type options are the legacy ones verbatim; hand them on and
            // let the one parser answer for them. `-f`/`--file`, `--blob`, `--type`, `--default`
            // and `--comment` take a separate argument.
            "-f" | "--file" | "--blob" | "-t" | "--type" | "--default" | "--comment" => {
                let Some(v) = rest.get(i + 1) else {
                    return Some(Err(super::missing_option_value(a)));
                };
                opts.push(a.to_string());
                opts.push(v.clone());
                i += 1;
            }
            _ if a.starts_with('-') && a != "-" => opts.push(a.to_string()),
            _ => operands.push(a.to_string()),
        }
        i += 1;
    }

    // `check_argc()` (builtin/config.c:202): `error()` then exit 129, with no usage block.
    let wrong_argc = |want: usize| -> Option<std::result::Result<Vec<String>, ExitCode>> {
        eprintln!("error: wrong number of arguments, should be {want}");
        Some(Err(ExitCode::from(129)))
    };

    let mut out: Vec<String> = Vec::new();
    match sub {
        "list" => {
            if !operands.is_empty() {
                return wrong_argc(0);
            }
            out.push("--list".into());
        }
        "get" => {
            if operands.len() != 1 {
                return wrong_argc(1);
            }
            // The three refusals `cmd_config_get()` owns, in its order (:1104-1112).
            if opts.iter().any(|o| o == "--fixed-value") && value_pattern.is_none() {
                return Some(Err(fatal("--fixed-value only applies with 'value-pattern'")));
            }
            if opts.windows(2).any(|w| w[0] == "--default") && (all || url.is_some()) {
                return Some(Err(fatal("--default= cannot be used with --all or --url=")));
            }
            if url.is_some() && (all || regexp || value_pattern.is_some()) {
                return Some(Err(fatal("--url= cannot be used with --all, --regexp or --value")));
            }
            match (&url, regexp, all) {
                (Some(u), _, _) => {
                    out.push("--get-urlmatch".into());
                    out.push(operands[0].clone());
                    out.push(u.clone());
                }
                // `get --regexp` is `get_value()` with `GET_VALUE_KEY_REGEXP`
                // (builtin/config.c:1086,1125) — the same reader `--get`/`--get-all` use,
                // so it prints *values*, the last one unless `--all` asked for them all.
                // The legacy `--get-regexp` is a different display (`key value` pairs).
                (None, true, all) => {
                    out.push(match all {
                        true => "--get-key-regexp-all".into(),
                        false => "--get-key-regexp".into(),
                    });
                    out.push(operands[0].clone());
                    out.extend(value_pattern.clone());
                }
                (None, false, true) => {
                    out.push("--get-all".into());
                    out.push(operands[0].clone());
                    out.extend(value_pattern.clone());
                }
                (None, false, false) => {
                    out.push("--get".into());
                    out.push(operands[0].clone());
                    out.extend(value_pattern.clone());
                }
            }
        }
        "set" => {
            if operands.len() == 1 {
                return Some(Err(missing_set_value(&operands[0])));
            }
            if operands.len() != 2 {
                return wrong_argc(2);
            }
            if opts.iter().any(|o| o == "--fixed-value") && value_pattern.is_none() {
                return Some(Err(fatal("--fixed-value only applies with --value=<pattern>")));
            }
            if append && value_pattern.is_some() {
                return Some(Err(fatal("--append cannot be used with --value=<pattern>")));
            }
            if append {
                out.push("--add".into());
                out.extend(operands.iter().cloned());
            } else if all {
                out.push("--replace-all".into());
                out.extend(operands.iter().cloned());
                out.extend(value_pattern.clone());
            } else {
                // No `--all` and no pattern is `repo_config_set_in_file_gently()`, the plain
                // two-operand legacy write; a pattern alone replaces just what it matches, which
                // is the three-operand one.
                out.extend(operands.iter().cloned());
                out.extend(value_pattern.clone());
            }
        }
        "unset" => {
            if operands.len() != 1 {
                return wrong_argc(1);
            }
            if opts.iter().any(|o| o == "--fixed-value") && value_pattern.is_none() {
                return Some(Err(fatal("--fixed-value only applies with 'value-pattern'")));
            }
            out.push(if all { "--unset-all".into() } else { "--unset".into() });
            out.push(operands[0].clone());
            out.extend(value_pattern.clone());
        }
        "rename-section" => {
            if operands.len() != 2 {
                return wrong_argc(2);
            }
            out.push("--rename-section".into());
            out.extend(operands.iter().cloned());
        }
        "remove-section" => {
            if operands.len() != 1 {
                return wrong_argc(1);
            }
            out.push("--remove-section".into());
            out.push(operands[0].clone());
        }
        "edit" => {
            if !operands.is_empty() {
                return wrong_argc(0);
            }
            out.push("--edit".into());
        }
        _ => unreachable!("checked above"),
    }

    // The location/display/type options go first, so the legacy parser sees them before the
    // action's own operands — `PARSE_OPT_STOP_AT_NON_OPTION` would otherwise read them as data.
    let mut rewritten = opts;
    rewritten.extend(out);
    Some(Ok(rewritten))
}

/// The usage line `-h` prints for each subcommand (builtin/config.c:32-65).
fn subcommand_usage(sub: &str) -> &'static str {
    match sub {
        "list" => "usage: git config list [<file-option>] [<display-option>] [--includes]\n\n",
        "get" => "usage: git config get [<file-option>] [<display-option>] [--includes] [--all] [--regexp=<regexp>] [--value=<pattern>] [--fixed-value] [--default=<default>] <name>\n\n",
        "set" => "usage: git config set [<file-option>] [--type=<type>] [--comment=<message>] [--all] [--value=<pattern>] [--fixed-value] <name> <value>\n\n",
        "unset" => "usage: git config unset [<file-option>] [--all] [--value=<pattern>] [--fixed-value] <name>\n\n",
        "rename-section" => "usage: git config rename-section [<file-option>] <old-name> <new-name>\n\n",
        "remove-section" => "usage: git config remove-section [<file-option>] <name>\n\n",
        _ => "usage: git config edit [<file-option>]\n\n",
    }
}

/// `die()` from inside a subcommand: `fatal: <message>`, exit 128.
fn fatal(message: &str) -> ExitCode {
    eprintln!("fatal: {message}");
    ExitCode::from(128)
}

/// `die_missing_set_value()` (builtin/config.c:214): `git config set <name>` with no value names
/// the variable, and an `<key>=<value>` spelling earns the hint that says how it was meant.
fn missing_set_value(arg: &str) -> ExitCode {
    let eq = arg.rfind('.').and_then(|dot| arg[dot + 1..].find('=').map(|e| dot + 1 + e));
    match eq {
        Some(at) if valid_config_key(&arg[..at]) => {
            eprintln!("error: missing value to set to the variable '{arg}'");
            eprintln!(
                "hint: did you mean \"git config set {} {}\"?",
                &arg[..at],
                &arg[at + 1..]
            );
        }
        _ if valid_config_key(arg) => {
            eprintln!("error: missing value to set to the variable '{arg}'");
        }
        _ => {
            eprintln!("error: missing value to set to a variable with an invalid name '{arg}'");
        }
    }
    ExitCode::from(129)
}

/// `git_config_key_is_valid()`: a key is `<section>.<name>` (with an optional subsection), the
/// section and name are non-empty, and the name is alphanumeric-or-dash.
fn valid_config_key(key: &str) -> bool {
    let Some(dot) = key.find('.') else { return false };
    let last = key.rfind('.').expect("a first dot implies a last dot");
    let section = &key[..dot];
    let name = &key[last + 1..];
    !section.is_empty()
        && !name.is_empty()
        && section.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
        && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
}

/// The repository at the current directory, opened with gix's include
/// permission cleared so its merged snapshot is the one `--no-includes` asks
/// for. `None` when there is no repository here, exactly as `gix::discover`
/// answers for the including case.
fn discover_without_includes() -> Option<gix::Repository> {
    let mut open = gix::open::Options::default();
    open.permissions.config.includes = false;
    let trust_map = gix::sec::trust::Mapping { full: open.clone(), reduced: open };
    gix::ThreadSafeRepository::discover_with_environment_overrides_opts(
        ".",
        Default::default(),
        trust_map,
    )
    .ok()
    .map(|r| r.to_thread_local())
}

/// [`crate::config::global_config`] without following includes — the repo-less
/// half of `--no-includes`. `File::from_globals()` hard-codes the following, so
/// the source list it walks is reproduced here with `no_follow` instead.
fn global_config_without_includes() -> gix::config::File {
    use gix::config::file::{Metadata, includes, init};
    use gix::config::source::Kind;

    // `gix_path::env::var`, which is not re-exported through `gix`.
    let mut env_var = |name: &str| -> Option<std::ffi::OsString> {
        match name {
            "HOME" => home_dir().map(std::path::PathBuf::into_os_string),
            _ => std::env::var_os(name),
        }
    };
    let metas = [Kind::GitInstallation, Kind::System, Kind::Global]
        .iter()
        .flat_map(|kind| kind.sources())
        .filter_map(|source| {
            let path = source.storage_location(&mut env_var).filter(|p| p.is_file());
            Metadata { path, source: *source, level: 0, trust: gix::sec::Trust::Full }.into()
        });
    let options =
        init::Options { includes: includes::Options::no_follow(), ..Default::default() };
    let mut file = gix::config::File::from_paths_metadata(metas, options)
        .ok()
        .flatten()
        .unwrap_or_default();
    if let Ok(env) = gix::config::File::from_environment_overrides() {
        let _ = file.append(env);
    }
    file
}

/// `gix_path::env::home_dir`: `HOME` when set, else the platform's own answer.
fn home_dir() -> Option<std::path::PathBuf> {
    std::env::var_os("HOME").map(Into::into).or_else(std::env::home_dir)
}

pub fn config(args: &[String]) -> Result<ExitCode> {
    // `cmd_config()`'s outer `parse_options()` is nothing but `OPT_SUBCOMMAND`s: a leading `list`,
    // `get`, `set`, `unset`, `rename-section`, `remove-section` or `edit` selects a subcommand
    // with its own table, and anything else falls through to `cmd_config_actions()` — the legacy
    // option form this file already implements.
    let rewritten;
    let mut from_subcommand = false;
    let args = match rewrite_subcommand(args) {
        Some(Ok(argv)) => {
            rewritten = argv;
            from_subcommand = true;
            &rewritten[..]
        }
        Some(Err(code)) => return Ok(code),
        None => args,
    };

    let mut mode = Mode::Auto;
    let mut scope = Scope::Default;
    let mut name_only = false;
    let mut d = Display::default();
    // git's `display_opts.type` slot, holding its own name for the type so that
    // `--bool --type=int` can be rejected and an unported type named. Mapped to
    // a [`ValueType`] once every usage check has run.
    let mut ty_name: Option<&'static str> = None;
    // `--comment=<message>` (builtin/config.c) and `--fixed-value`, which turns every
    // `<value-pattern>` from a POSIX ERE into a literal string comparison.
    let mut comment: Option<String> = None;
    let mut fixed_value = false;
    // `include.path` / `includeIf` following, as git's tri-state
    // `respect_includes_opt`: unset until `--includes`/`--no-includes` says
    // otherwise, and resolved against the scope below.
    let mut respect_includes: Option<bool> = None;
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

        // Every long option below is reached through the name `parse_long_opt()`
        // resolves, so an unambiguous prefix lands on exactly the arm its full
        // spelling lands on — including the arms that refuse.
        let orig: &str = a.as_str();
        let resolved = match super::canonical_long(orig, ACTION_OPTS) {
            super::Long::Name(name) => name,
            super::Long::Ambiguous(first, second) => {
                return Ok(super::ambiguous_option(orig, &first, &second, USAGE_ACTIONS))
            }
        };
        let a: &str = resolved.as_ref();

        let action = match a {
            "-l" | "--list" => Some(Mode::List),
            "--get" => Some(Mode::Get),
            "--get-all" => Some(Mode::GetAll),
            "--get-regexp" => Some(Mode::GetRegexp),
            // Internal spellings the `get` subcommand mapping emits; git has no
            // command-line form for them.
            "--get-key-regexp" => Some(Mode::GetKeyRegexp),
            "--get-key-regexp-all" => Some(Mode::GetKeyRegexpAll),
            "--get-urlmatch" => Some(Mode::GetUrlMatch),
            "--get-color" => Some(Mode::GetColor),
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
        let new_scope = match a {
            "--local" => Some(Scope::Local),
            "--worktree" => Some(Scope::Worktree),
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
        let file_value = match a {
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
        let blob_value = match a {
            "--blob" => match args.get(i) {
                Some(v) => {
                    i += 1;
                    Some(v.clone())
                }
                None => return usage_error("option `blob' requires a value"),
            },
            other => other.strip_prefix("--blob=").map(ToOwned::to_owned),
        };
        if let Some(blob) = blob_value {
            if !matches!(scope, Scope::Default | Scope::Blob(_)) {
                return usage_error("only one config file at a time");
            }
            scope = Scope::Blob(blob);
            continue;
        }

        if let Some(path) = file_value {
            // git counts `--file` once no matter how often it is given, so only
            // a *different* kind of scope flag collides with it.
            if !matches!(scope, Scope::Default | Scope::File(_)) {
                return usage_error("only one config file at a time");
            }

            scope = Scope::File(path.into());
            continue;
        }

        if let Some(v) = a.strip_prefix("--default=") {
            d.default_value = Some(v.to_string());
            continue;
        }

        // `--type=<t>` and its legacy spellings canonicalize the value on the way
        // out; see [`select_type`] for the two refusals it owns.
        // `-t<type>` is parse-options' sticky short form; `--type` never matches
        // it, since its second character is another dash.
        let sticky_type = a.strip_prefix("--type=").or_else(|| match a.strip_prefix("-t") {
            Some(t) if !t.is_empty() => Some(t),
            _ => None,
        });
        if let Some(t) = sticky_type {
            if let Err(code) = select_type(&mut ty_name, t) {
                return Ok(code);
            }
            continue;
        }

        match a {
            "--includes" => respect_includes = Some(true),
            "--no-includes" => respect_includes = Some(false),
            "--show-origin" => d.show_origin = true,
            // parse_options_step()'s `internal_help`, ahead of the
            // subcommand dispatch: the block on stdout at 129.
            // `--help-all` reaches the same renderer with USAGE_FULL, which this
            // table renders identically: it has no `PARSE_OPT_HIDDEN` entry.
            "-h" | "--help-all" => return Ok(super::show_usage(USAGE)),
            "--show-scope" => d.show_scope = true,
            "-z" | "--null" => d.null = true,
            // The six `OPT_CALLBACK_VALUE` spellings reach `option_parse_type()`
            // with the type in `defval` and no argument of their own.
            "--bool" | "--int" | "--bool-or-int" | "--bool-or-str" | "--path"
            | "--expiry-date" => {
                if let Err(code) = select_type(&mut ty_name, &a[2..]) {
                    return Ok(code);
                }
            }
            // `OPT_STRING(0, "default", …)`: an attached `--default=<v>` is handled
            // with the other `=`-carrying options above; this is the separated form.
            "--default" => {
                let Some(v) = args.get(i) else {
                    return Ok(super::missing_option_value(a));
                };
                i += 1;
                d.default_value = Some(v.clone());
            }
            "-t" | "--type" => {
                let Some(v) = args.get(i) else {
                    // `get_arg()`'s PARSE_OPT_ERROR: no usage block, and the
                    // wording follows the spelling that was used.
                    return Ok(super::missing_option_value(a));
                };
                i += 1;
                if let Err(code) = select_type(&mut ty_name, v) {
                    return Ok(code);
                }
            }
            "--name-only" => name_only = true,
            "--show-names" => d.show_names = true,
            // The unset sense of every display/type entry. `option_parse_type()`
            // stores `TYPE_NONE` when `unset` (builtin/config.c:156-159) and the
            // four `OPT_BOOL`s clear their int, so each of these is the option's
            // default — including for the display flags this port does not
            // otherwise honour (`--show-names`), whose default *is* off.
            "--no-null" => d.null = false,
            "--no-name-only" => name_only = false,
            "--no-show-origin" => d.show_origin = false,
            "--no-show-scope" => d.show_scope = false,
            "--no-show-names" => d.show_names = false,
            "--no-type" => ty_name = None,
            // `--no-<location>` clears the corresponding `use_*_config` int (or
            // NULLs the `--file`/`--blob` string), which is exactly "unpin the
            // file again": back to git's implicit merged read.
            "--no-global" | "--no-local" | "--no-system" | "--no-file" | "--no-worktree" => {
                scope = Scope::Default;
            }
            "--no-blob" => {
                if matches!(scope, Scope::Blob(_)) {
                    scope = Scope::Default;
                }
            }
            // `--no-default` NULLs the `OPT_STRING` behind `--default`; the other
            // two clear slots this port does not read.
            "--no-default" => d.default_value = None,
            "--no-comment" => comment = None,
            "--no-fixed-value" => {
                fixed_value = false;
                d.fixed_value = false;
            }
            "--fixed-value" => {
                fixed_value = true;
                d.fixed_value = true;
            }
            "--comment" => {
                let Some(v) = args.get(i) else {
                    return Ok(super::missing_option_value(a));
                };
                i += 1;
                comment = Some(v.clone());
            }
            _ if a.starts_with("--comment=") => comment = Some(a["--comment=".len()..].to_string()),

            other if other.starts_with('-') => return reject(other),
            // Unreachable: a token that does not start with `-` ended option
            // parsing above. Push the argument as typed, never the respelling.
            _ => positional.push(orig),
        }
    }

    // Post-parse validation, in git's own order and — like git — ahead of any
    // repository lookup, so a usage error reports the same way outside a repo.
    //
    // `if ((actions & (ACTION_GET_COLOR|ACTION_GET_COLORBOOL)) && display_opts.type)`
    // (builtin/config.c:1407-1410) runs before every other check, including the
    // actionless one, and exits 129 through `error()` + `exit()` rather than
    // through `usage_with_options()`, so it carries no usage block.
    if matches!(mode, Mode::GetColor | Mode::GetColorBool) && ty_name.is_some() {
        return usage_error("--get-color and variable type are incoherent");
    }
    //
    // An entirely actionless invocation is reported next. Without an action
    // flag the form is `<name> [value [value-pattern]]`, and git recognizes no
    // action at all outside that 1..=3 window, the zero-argument case included.
    if mode == Mode::Auto && !(1..=3).contains(&positional.len()) {
        return usage_error("no action specified");
    }
    // The two applicability checks below belong to `cmd_config_actions()`, the
    // legacy form; `cmd_config_get()` has its own option table where `--name-only`
    // and `--default` are unconditional (builtin/config.c:1082-1097) and its own
    // three refusals, already applied during the rewrite. So a `get` that came
    // through the subcommand table skips them.
    let get_subcommand = from_subcommand
        && matches!(
            mode,
            Mode::Get
                | Mode::GetAll
                | Mode::GetKeyRegexp
                | Mode::GetKeyRegexpAll
                | Mode::GetUrlMatch
        );
    // `--fixed-value` only says *how* a `<value-pattern>` is compared, so the
    // legacy form refuses it when the command line carries none — one `error:`
    // line and exit 129, for every action alike, verified against stock 2.55.0
    // for `--list`, `--get`, `--get-regexp`, `--get-color`, `--replace-all`,
    // `--unset-all` and `--remove-section`. The `get`/`set`/`unset` subcommands
    // make the same refusal in their own words (`fatal:` at 128) during the
    // rewrite above, so they are excluded here.
    if d.fixed_value && !from_subcommand {
        let has_pattern = match mode {
            Mode::Get
            | Mode::GetAll
            | Mode::GetRegexp
            | Mode::GetKeyRegexp
            | Mode::GetKeyRegexpAll
            | Mode::Unset
            | Mode::UnsetAll => positional.len() >= 2,
            Mode::ReplaceAll | Mode::Auto => positional.len() >= 3,
            _ => false,
        };
        if !has_pattern {
            return usage_error("--fixed-value only applies with 'value-pattern'");
        }
    }
    d.name_only = name_only;
    if name_only && !get_subcommand && !matches!(mode, Mode::List | Mode::GetRegexp) {
        return usage_error("--name-only is only applicable to --list or --get-regexp");
    }
    // ```c
    // if (display_opts.default_value && !(actions & ACTION_GET)) {
    //         error(_("--default is only applicable to --get"));
    //         exit(129);
    // }
    // ```
    // (`builtin/config.c:1440-1443`.) It runs *after* the implicit action is
    // resolved (`case 1: actions = ACTION_GET`), so the bare one-operand read —
    // `git config --default=x some.missing` — is a `--get` by then and is allowed.
    if d.default_value.is_some()
        && !get_subcommand
        && !(mode == Mode::Get || (mode == Mode::Auto && positional.len() == 1))
    {
        return usage_error("--default is only applicable to --get");
    }
    match mode {
        Mode::List if !positional.is_empty() => {
            return usage_error("wrong number of arguments, should be 0");
        }
        Mode::Get
        | Mode::GetAll
        | Mode::GetRegexp
        | Mode::GetKeyRegexp
        | Mode::GetKeyRegexpAll
            if !(1..=2).contains(&positional.len()) =>
        {
            return usage_error("wrong number of arguments, should be from 1 to 2");
        }
        // `check_argc(argc, 1, 2)` (builtin/config.c:1607) — the same window and
        // the same wording as the get forms.
        Mode::GetColor if !(1..=2).contains(&positional.len()) => {
            return usage_error("wrong number of arguments, should be from 1 to 2");
        }
        _ => {}
    }

    // `select_type()` has already rejected any name git does not know, so every
    // surviving name is one of the seven [`ValueType`] arms.
    d.ty = ty_name.and_then(ValueType::parse);

    // A repository is optional: reads resolve fine outside one (git reads global
    // and system config with no repo present), while writes target the local
    // scope and still require a repo. Discovery failure is therefore not fatal
    // here — only an attempted write without a repo is.
    // ```c
    // if (verify_repository_format(candidate, &err) < 0) {
    //         if (nongit_ok) {
    //                 warning("%s", err.buf);
    //                 *nongit_ok = -1;
    //                 return -1;
    //         }
    //         die("%s", err.buf);
    // }
    // ```
    //
    // (`check_repository_format_gently()`, setup.c.) `git config` is
    // `RUN_SETUP_GENTLY`, so a repository whose format this build cannot honour
    // is a *warning* and the command carries on outside the repository — which
    // is already what discovery failing leaves behind here. Only the warning was
    // missing, so `git config --get extensions.objectFormat` in a repository
    // that declares `sha256` at format version 0 exited 1 in silence where stock
    // says which extension it objected to.
    let repo = match crate::setup::discover() {
        Ok(repo) => Some(repo),
        Err(err) => {
            if let gix::discover::Error::Open(gix::open::Error::Config(config)) = &err {
                if let Some(message) = crate::config::repository_format_message(config) {
                    eprintln!("warning: {message}");
                }
            }
            None
        }
    };
    d.prefix = repo.as_ref().and_then(crate::setup::prefix).map(|p| format!("{}/", p.display()));
    d.blob = match &scope {
        Scope::Blob(spec) => Some(spec.clone()),
        _ => None,
    };

    // ```c
    // if (opts->respect_includes_opt == -1)
    //         opts->options.respect_includes = !opts->source.file;
    // else
    //         opts->options.respect_includes = opts->respect_includes_opt;
    // ```
    //
    // (`builtin/config.c:1001-1004`, `location_options_init()`.) `source.file` is
    // set only by `--file`/`--blob`, so every other scope follows includes
    // unless `--no-includes` says not to, and a named file does not unless
    // `--includes` says it should.
    let includes = respect_includes.unwrap_or(!matches!(scope, Scope::File(_)));

    // The `Default` cascade resolves its includes while it is being built, so
    // turning them off means building it again — the repository re-opened with
    // gix's own include permission cleared, or the repo-less global cascade
    // re-read the same way.
    let no_include_repo = match (&scope, includes) {
        (Scope::Default, false) => discover_without_includes(),
        _ => None,
    };

    // The config to READ from, by scope. Owned holders live to the end of the
    // function so `file` can borrow whichever one this scope selects:
    //   * Default → the repo's fully-merged snapshot inside one, else the
    //     global+system+env cascade git falls back to.
    //   * Local   → the repository-local file alone (requires a repo).
    //   * Global  → the ONE file `git_global_config()` names; see [`global_config_file`].
    //   * System  → `$GIT_CONFIG_SYSTEM`, else `$(prefix)/etc/gitconfig`, alone.
    //   * File    → the named file alone, includes not followed.
    let snapshot = no_include_repo.as_ref().or(repo.as_ref()).map(gix::Repository::config_snapshot);
    let default_global;
    let scoped;
    let scope_file;
    // Set when a scope that names a single file could not read it. git only makes
    // that fatal for `--list`; the get forms treat it as "key not found" (exit 1),
    // so the error is carried to the dispatch below rather than raised here.
    let mut unreadable: Option<std::io::Error> = None;
    // Whether `config_with_options()` failed on a `--blob`, which `--list` turns
    // into `error processing config file(s)` and every other read leaves as an
    // empty configuration.
    let mut blob_failed = false;
    // git's `location_opts.source.file` — the single file the scope resolved to, or
    // `None` for the scopes that read a cascade. `cmd_config_list()` names it in the
    // fatal it dies with (builtin/config.c:1063-1065), so the two have to travel
    // together.
    let mut source_file: Option<&std::path::Path> = None;
    // A pure write skips the read side entirely: the write path re-reads its
    // target under the lock, and reading here as well would repeat git's
    // `warning: unable to access …` diagnostic for an unreadable file.
    let reads_config = match mode {
        Mode::List
        | Mode::Get
        | Mode::GetAll
        | Mode::GetRegexp
        | Mode::GetKeyRegexp
        | Mode::GetKeyRegexpAll
        | Mode::GetUrlMatch
        | Mode::GetColor
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

    // ```c
    // if (inc->depth > MAX_INCLUDE_DEPTH)
    //         die(_(include_depth_advice), MAX_INCLUDE_DEPTH, path,
    //             !cf ? "<unknown>" : cf->name ? cf->name : "the command line");
    // ```
    //
    // (`handle_path_include()`, config.c.) A configuration that includes itself
    // is fatal at 128, naming the include and the file that asked for it. gix
    // answers a circular chain two other ways — silently capping it for the
    // repository cascade, and with its own wording for an explicit
    // `--file … --includes` — so the chain is walked here for the diagnostic.
    if includes && reads_config {
        let cyclic = match &scope {
            Scope::File(path) => include_depth_overflow(path),
            // A circular include also stops the repository from *opening* — gix
            // gives up on the configuration and discovery fails with it — so the
            // git directory is located directly rather than through `repo`,
            // which is `None` in exactly the case this diagnostic is for.
            Scope::Default | Scope::Local => {
                let git_dir = match repo.as_ref() {
                    Some(repo) => Some(repo.common_dir().to_path_buf()),
                    None => gix::discover::upwards(std::path::Path::new("."))
                        .ok()
                        .map(|(path, _trust)| path.into_repository_and_work_tree_directories().0),
                };
                git_dir.and_then(|dir| include_depth_overflow(&dir.join("config")))
            }
            _ => None,
        };
        if let Some((path, from)) = cyclic {
            eprintln!(
                "fatal: exceeded maximum include depth ({MAX_INCLUDE_DEPTH}) while including\n\
                 \t{path}\nfrom\n\t{from}\nThis might be due to circular includes."
            );
            return Ok(ExitCode::from(128));
        }
    }

    let file: &gix::config::File = match &scope {
        Scope::Default => match snapshot.as_ref() {
            Some(s) => s.plumbing(),
            None => {
                default_global = match includes {
                    true => crate::config::global_config(),
                    false => global_config_without_includes(),
                };
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
        // `opts->source.scope = CONFIG_SCOPE_LOCAL` for this one too
        // (builtin/config.c:990), so `--show-scope` says `local` here even when
        // the file it picked is `config.worktree` — which the *merged* read
        // reports as `worktree`, because there it is the sequence that assigns
        // the scope rather than this option.
        Scope::Worktree => {
            let repo = repo.as_ref().ok_or_else(|| {
                anyhow::anyhow!("--worktree can only be used inside a git repository")
            })?;
            scope_file = worktree_config_file(repo)?;
            source_file = Some(&scope_file);
            // Read like every other named scope, so a `config.worktree` that is
            // not there is carried as the `io::Error` `--list` turns into
            // `fatal: unable to read config file '<path>': <errno>`. It is
            // reachable: `extensions.worktreeConfig` is a repository-wide switch
            // while the file it names is per-worktree, so a linked worktree that
            // has never been written to has none — where reading it as empty
            // answered 0 and printed nothing, stock dies at 128.
            scoped =
                read_single_scope_file(&scope_file, Source::Local, reads_config, &mut unreadable)?;
            &scoped
        }
        // `location_options_init()` (builtin/config.c:959-972): a scope flag does
        // not select a *cascade*, it selects one file — `git_global_config()` for
        // `--global`, `git_system_config()` for `--system` — and everything after
        // reads that file alone. Reading the pair merged made `--global --list`
        // print the XDG file's entries alongside `~/.gitconfig`'s, where git prints
        // only the one it picked, and made a `$GIT_CONFIG_GLOBAL` that names
        // nothing look like an empty configuration instead of a fatal.
        Scope::Global => {
            let Some(path) = global_config_file() else {
                // `if (!opts->source.file) die(_("$HOME not set"))`: with no `$HOME`
                // it is unknown whether `~/.gitconfig` exists, so git will not guess
                // at the XDG location even when `$XDG_CONFIG_HOME` is set.
                return Err(crate::fatal::Fatal("$HOME not set".to_owned()).into());
            };
            scope_file = path;
            source_file = Some(&scope_file);
            scoped = read_single_scope_file(&scope_file, Source::User, reads_config, &mut unreadable)?;
            &scoped
        }
        Scope::System => {
            scope_file = system_config_file();
            source_file = Some(&scope_file);
            scoped =
                read_single_scope_file(&scope_file, Source::System, reads_config, &mut unreadable)?;
            &scoped
        }
        // `--blob` is `CONFIG_SCOPE_COMMAND` too, so its entries are `Source::Cli`
        // — `--show-scope` says `command` for them — and `--show-origin` takes
        // the blob name from [`Display::blob`] rather than from a path.
        Scope::Blob(spec) => {
            let Some(repo) = repo.as_ref() else {
                return Err(crate::fatal::Fatal(
                    "--blob can only be used inside a git repository".to_owned(),
                )
                .into());
            };
            scoped = match blob_config(repo, spec) {
                Ok(file) => file,
                // The three `error()`s are already on stderr; what happens next is
                // the caller's, and only `--list` makes it fatal.
                Err(()) => {
                    blob_failed = true;
                    ConfigFile::new(gix::config::file::Metadata::from(Source::Cli))
                }
            };
            &scoped
        }
        // `--file` is git's `CONFIG_SCOPE_COMMAND`, hence `Source::Cli`. Read
        // through `fs::read` so a missing or unreadable path surfaces as a
        // plain `io::Error` whose errno git reports verbatim.
        Scope::File(path) if !reads_config => {
            source_file = Some(path);
            scoped = empty_config(path, Source::Cli);
            &scoped
        }
        Scope::File(path) => {
            source_file = Some(path);
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
                        // `include_condition_is_true()` (config.c) is handed
                        // both halves of the context: `gitdir:` matches against
                        // the git directory, `onbranch:` against the checked-out
                        // branch. Leaving the branch out made every `onbranch:`
                        // condition false, so an include chain that passed
                        // through one stopped there and the values behind it
                        // never reached the snapshot.
                        let branch_name = repo.as_ref().and_then(|r| r.head_name().ok().flatten());
                        let conditional = gix::config::file::includes::conditional::Context {
                            git_dir: git_dir.as_deref(),
                            branch_name: branch_name.as_ref().map(|n| n.as_ref()),
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

    // `git_config_from_file_with_options()` leaves `default_error_action` at
    // `CONFIG_ERROR_DIE` (config.c:1394), so a `--file` that will not parse is
    // fatal before the action runs — a write to it included, which is why git
    // refuses to append to a file it cannot read back. `--edit` never parses the
    // file, so it is the one action a malformed file survives.
    if let Scope::File(path) = &scope {
        if mode != Mode::Edit {
            // Read straight through rather than with [`read_config_bytes`]: an
            // unreadable file is not this diagnostic, and its warning belongs to
            // the read that follows, once.
            if let Some(line) =
                std::fs::read(path).ok().and_then(|b| crate::config::first_bad_config_line(&b))
            {
                let text = path.to_string_lossy();
                let shown = origin_path(&d, Source::Cli, &text);
                return Ok(fatal(&format!("bad config line {line} in file {shown}")));
            }
        }
    }

    // Resolve the write destination for this scope, erroring like git when a
    // repository is required but absent.
    let write_target = || resolve_write_target(&scope, repo.as_ref());

    match mode {
        // Unlike the get forms, `--list` reports an unreadable scope file as a
        // fatal error rather than as an empty result — `cmd_config_list()`
        // (builtin/config.c:1060-1068) dies whenever `config_with_options()`
        // failed and a file was named, which covers `--file`, `--global` and
        // `--system` alike.
        Mode::List => match (source_file, &unreadable) {
            (Some(path), Some(err)) => {
                eprintln!(
                    "fatal: unable to read config file '{}': {}",
                    path.display(),
                    errno_text(err)
                );
                Ok(ExitCode::from(128))
            }
            // ```c
            // if (location_opts.source.file)
            //         die_errno(_("unable to read config file '%s'"), location_opts.source.file);
            // else
            //         die(_("error processing config file(s)"));
            // ```
            //
            // (`builtin/config.c:1063-1067`.) A blob names no file, so it takes
            // the second arm.
            _ if blob_failed => Ok(fatal("error processing config file(s)")),
            _ => list(file, &d),
        },
        // `--get`/`--get-all`/`--get-regexp <name> <value-pattern>`: the optional
        // second operand filters the returned values by an ERE (`!` inverts).
        Mode::Get => get(file, positional[0], false, positional.get(1).copied(), &d),
        Mode::GetAll => get(file, positional[0], true, positional.get(1).copied(), &d),
        Mode::GetKeyRegexp | Mode::GetKeyRegexpAll => get_regexp(
            file,
            positional[0],
            positional.get(1).copied(),
            &d,
            mode == Mode::GetKeyRegexpAll,
            false,
        ),
        Mode::GetRegexp => {
            get_regexp(file, positional[0], positional.get(1).copied(), &d, true, true)
        }
        Mode::GetUrlMatch => get_urlmatch(file, &positional, &d),
        Mode::GetColor => get_color(file, positional[0], positional.get(1).copied()),
        Mode::GetColorBool => get_colorbool(file, &positional),
        // `show_editor()` refuses a blob before `check_write()` would have
        // (builtin/config.c:1299-1300), so this is not the write message.
        Mode::Edit if matches!(scope, Scope::Blob(_)) => {
            Ok(fatal("editing blobs is not supported"))
        }
        Mode::Edit => edit_config(&write_target()?),
        Mode::RenameSection => rename_section(&write_target()?, &positional),
        Mode::RemoveSection => remove_section(&write_target()?, &positional),
        // Every write of a *value* runs it through `normalize_value()` first, so
        // `--type=int x.y 1k` stores `1024` and `--type=bool x.y zzz` is refused
        // before the file is touched. `--unset`/`--unset-all` carry no value and
        // are left alone.
        Mode::ReplaceAll => {
            let name = positional.first().copied().unwrap_or_default();
            let value = positional.get(1).copied().unwrap_or_default();
            match normalized(d.ty, name, value) {
                Ok(value) => replace_all(&write_target()?, name, &value, positional.get(2).copied(), d.fixed_value),
                Err(code) => Ok(code),
            }
        }
        // No action flag: one positional reads, two set the value.
        Mode::Auto if positional.len() == 1 => get(file, positional[0], false, None, &d),
        Mode::Auto if positional.len() == 2 => match normalized(d.ty, positional[0], positional[1]) {
            Ok(value) => write_scoped(&write_target()?, positional[0], &value, WriteOp::Set, comment.as_deref()),
            Err(code) => Ok(code),
        },
        // `<name> <value> <value-pattern>` rewrites the values whose text matches
        // the POSIX ERE, or adds a new value when none match.
        Mode::Auto => match normalized(d.ty, positional[0], positional[1]) {
            Ok(value) => {
                set_with_value_pattern(&write_target()?, positional[0], &value, positional[2])
            }
            Err(code) => Ok(code),
        },
        Mode::Add => {
            let (name, value) = name_and_value(&positional)?;
            match normalized(d.ty, name, value) {
                Ok(value) => write_scoped(&write_target()?, name, &value, WriteOp::Add, comment.as_deref()),
                Err(code) => Ok(code),
            }
        }
        // `git config --unset <name> [<value-pattern>]`: the optional third operand narrows
        // which values are removed, exactly as it narrows a `--replace-all`
        // (`repo_config_set_multivar_in_file_gently(..., value_pattern, ...)`, config.c).
        Mode::Unset => {
            let (name, pattern) = name_and_pattern(&positional)?;
            unset_scoped(&write_target()?, name, pattern, false, d.fixed_value)
        }
        Mode::UnsetAll => {
            let (name, pattern) = name_and_pattern(&positional)?;
            unset_scoped(&write_target()?, name, pattern, true, d.fixed_value)
        }
    }
}


/// `<name> [<value-pattern>]`, the operand shape of `--unset`/`--unset-all`.
fn name_and_pattern<'a>(positional: &[&'a str]) -> Result<(&'a str, Option<&'a str>)> {
    match positional {
        [name] => Ok((*name, None)),
        [name, pattern] => Ok((*name, Some(*pattern))),
        [] => crate::git_fatal!("no config key given"),
        _ => crate::git_fatal!("too many arguments, expected a single `<name>`"),
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

/// Parse `section[.subsection].name`.
///
/// ```c
/// if (last_dot == NULL || last_dot == key) {
///         if (!quiet)
///                 error(_("key does not contain a section: %s"), key);
///         return -CONFIG_NO_SECTION_OR_NAME;
/// }
///
/// if (!last_dot[1]) {
///         if (!quiet)
///                 error(_("key does not contain variable name: %s"), key);
///         return -CONFIG_NO_SECTION_OR_NAME;
/// }
/// ```
///
/// (`do_parse_config_key()`, config.c:545-567.) Both are `error()`s — the `error:` prefix,
/// no usage block — and both exit 2 (`CONFIG_NO_SECTION_OR_NAME`, config.h:28), where this
/// port reported its own `zvcs:` line at exit 1.
fn parse_key(name: &str) -> Result<KeyRef<'_>> {
    parse_key_as(name, 1)
}

/// [`parse_key`] for the write actions, where a key with no section or no
/// variable name is `-CONFIG_NO_SECTION_OR_NAME` and surfaces as exit 2, rather
/// than the read path's flat `CONFIG_INVALID_KEY` (1) for every shape of bad
/// key. A key that parses into a section and a name but carries a byte
/// `git_config_parse_key()` refuses — a space, say — is `CONFIG_INVALID_KEY` on
/// both paths, so only the first class differs.
fn parse_key_write(name: &str) -> Result<KeyRef<'_>> {
    parse_key_as(name, 2)
}

/// The shared body: validate with the full `git_config_parse_key()` port (which
/// `KeyRef::parse_unvalidated` does not do — it accepts `a.` and `ec.a b` alike)
/// and report git's `error:` line at the caller's exit code.
fn parse_key_as(name: &str, no_section_or_name: u8) -> Result<KeyRef<'_>> {
    if let Err(message) = crate::config::parse_config_key(name) {
        let code = match message.starts_with("invalid key") {
            true => 1,
            false => no_section_or_name,
        };
        eprintln!("error: {message}");
        return Err(anyhow::Error::new(crate::fatal::Silent(code)));
    }
    match KeyRef::parse_unvalidated(name.into()) {
        Some(key) => Ok(key),
        // Unreachable in practice: every shape `parse_config_key` accepts has a
        // section and a variable name. Reported as git reports the same class.
        None => {
            eprintln!("error: key does not contain a section: {name}");
            Err(anyhow::Error::new(crate::fatal::Silent(no_section_or_name)))
        }
    }
}

/// A compiled `<value-pattern>`: the optional second operand of a read, an
/// unanchored POSIX ERE matched against the value bytes, inverted by a leading
/// `!` — the same grammar the value-pattern *set* form uses.
struct ValueFilter {
    /// `--fixed-value`: the pattern is compared literally rather than compiled, which is
    /// `CONFIG_FLAGS_FIXED_VALUE` — `matches()` in config.c calls `strcmp()` instead of
    /// `regexec()`. A leading `!` still inverts.
    fixed: Option<Vec<u8>>,
    re: Option<regex::bytes::Regex>,
    invert: bool,
}

impl ValueFilter {
    /// Compile `pattern`, or report git's `error: invalid pattern: <p>` at exit 6.
    fn parse(pattern: &str, fixed: bool) -> Result<Self, ExitCode> {
        let (invert, pat) = match pattern.strip_prefix('!') {
            Some(rest) => (true, rest),
            None => (false, pattern),
        };
        if fixed {
            return Ok(Self {
                fixed: Some(pat.as_bytes().to_vec()),
                re: None,
                invert,
            });
        }
        match regex::bytes::Regex::new(pat) {
            Ok(re) => Ok(Self { fixed: None, re: Some(re), invert }),
            Err(_) => {
                eprintln!("error: invalid pattern: {pat}");
                Err(ExitCode::from(6))
            }
        }
    }

    fn matches(&self, value: &[u8]) -> bool {
        let hit = match (&self.fixed, &self.re) {
            (Some(literal), _) => literal.as_slice() == value,
            (None, Some(re)) => re.is_match(value),
            (None, None) => false,
        };
        hit != self.invert
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
    let filter = match value_pattern.map(|p| ValueFilter::parse(p, d.fixed_value)) {
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
    // `collect_config()` hands `format_config()` the *entry's* key, not the one
    // that was asked for (builtin/config.c:527-528), so `--show-names` and the
    // type diagnostics both name the git-normalized spelling.
    let wanted = key_of(&key);
    let mut selected: Vec<(String, Vec<u8>, bool, gix::config::file::Metadata)> = Vec::new();
    for_each_entry(file, |k, value, implicit, meta| {
        if k == wanted && filter.as_ref().is_none_or(|f| f.matches(value)) {
            selected.push((k.to_owned(), value.to_vec(), implicit, meta.clone()));
        }
        Ok(())
    })?;
    // Only a `--get` that found *nothing* reaches the `--default` arm; a key that
    // exists but was filtered out by a value-pattern still exits 1.
    if selected.is_empty() {
        return emit_default(&mut out, d, name);
    }

    // git canonicalizes in file order and dies on the first value that does not
    // parse as the requested type — even when `--get` would have returned a
    // later one, so the error names the same value stock git names.
    let mut canonical: Vec<(String, Vec<u8>, bool, gix::config::file::Metadata)> = Vec::new();
    for (k, v, implicit, meta) in &selected {
        match typed(d, k, v, *implicit, meta) {
            Ok(v) => canonical.push((k.clone(), v, *implicit, meta.clone())),
            Err(code) => return Ok(code),
        }
    }

    let emit: &[_] = if all { &canonical } else { &canonical[canonical.len() - 1..] };
    for (name, v, implicit, meta) in emit {
        // A key with no `=` prints as its name alone under `--show-names`, and as an empty line
        // without it — `format_config()` backs the delimiter out (builtin/config.c:454-461).
        let shown = (!implicit || d.ty.is_some()).then_some(v.as_slice());
        emit_kv_opt(&mut out, d, name, shown, meta, b' ', d.show_names)?;
    }
    Ok(ExitCode::SUCCESS)
}

/// `get_value()`'s `--default` arm, reached when the walk collected nothing:
///
/// ```c
/// if (!values.nr && display_opts->default_value) {
///         struct key_value_info kvi = KVI_INIT;
///         struct strbuf *item;
///         int status;
///
///         kvi_from_param(&kvi);
///         …
///         status = format_config(display_opts, item, key_,
///                                display_opts->default_value, &kvi, 0);
///         if (status < 0)
///                 die(_("failed to format default config value: %s"),
///                     display_opts->default_value);
///         if (status) {
///                 /* default was a missing optional value */
///                 values.nr--;
///                 strbuf_release(item);
///         }
/// }
/// ```
///
/// (`builtin/config.c:608-628`.) The default is formatted by the same
/// `format_config()` the stored values go through, so `--type` applies to it —
/// and `kvi_from_param()` gives it no file of origin, which is what shortens the
/// diagnostic when the type rejects it. `key_` is the name as it was asked for,
/// the raw pattern included, since no entry was matched to supply one.
///
/// Exit 1 with no output when no default was configured; a key that exists but
/// was filtered out by a value-pattern never reaches here.
fn emit_default(out: &mut impl std::io::Write, d: &Display, name: &str) -> Result<ExitCode> {
    let Some(default) = d.default_value.as_deref() else {
        return Ok(ExitCode::from(1));
    };
    let formatted = match d.ty {
        None => Ok(default.as_bytes().to_vec()),
        Some(t) => t.canonicalize(name, default.as_bytes(), false),
    };
    match formatted {
        Ok(value) => {
            emit_kv(out, d, name, &value, &param_metadata(), b' ', d.show_names)?;
            Ok(ExitCode::SUCCESS)
        }
        // A `format_config()` that returns < 0 is the `die()` above rather than
        // the callback's usual `bad config line` follow-up, because the default
        // came from the command line and has no config line behind it.
        Err(TypeError::Callback(message)) => {
            eprintln!("{message}");
            eprintln!("fatal: failed to format default config value: {default}");
            Ok(ExitCode::from(128))
        }
        Err(err) => Ok(report_type_error(err, name, default.as_bytes(), None)),
    }
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
    emit_kv_opt(out, d, key, Some(value), meta, sep, with_key)
}

/// [`emit_kv`] for an entry that may have no value at all — a name written with no `=`.
///
/// `format_config()`'s `TYPE_NONE` arm adds the key delimiter first and then takes it back when
/// the value is NULL (builtin/config.c:454-461), so `flag` prints as its key alone while
/// `empty =` prints the key, the delimiter and nothing else.
fn emit_kv_opt(
    out: &mut impl Write,
    d: &Display,
    key: &str,
    value: Option<&[u8]>,
    meta: &gix::config::file::Metadata,
    sep: u8,
    with_key: bool,
) -> Result<()> {
    let Some(value) = value else {
        if d.show_scope {
            out.write_all(scope_word(meta.source).as_bytes())?;
            out.write_all(column_term(d))?;
        }
        if d.show_origin {
            write_origin(out, d, meta)?;
        }
        if with_key {
            out.write_all(key.as_bytes())?;
        }
        out.write_all(if d.null { b"\0" } else { b"\n" })?;
        return Ok(());
    };
    if d.show_scope {
        out.write_all(scope_word(meta.source).as_bytes())?;
        out.write_all(column_term(d))?;
    }
    if d.show_origin {
        write_origin(out, d, meta)?;
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

/// The `--show-origin` column: `file:<path>` for a real file, else the source's own word, then a
/// tab.
fn write_origin(out: &mut impl Write, d: &Display, meta: &gix::config::file::Metadata) -> Result<()> {
    // `--blob` is a whole scope, so every entry in it has the same origin and
    // there is no file to name.
    if let Some(spec) = &d.blob {
        out.write_all(b"blob:")?;
        out.write_all(spec.as_bytes())?;
        out.write_all(column_term(d))?;
        return Ok(());
    }
    match &meta.path {
        Some(path) => {
            out.write_all(b"file:")?;
            out.write_all(origin_path(d, meta.source, &path.to_string_lossy()).as_bytes())?;
        }
        None => out.write_all(origin_word(meta.source).as_bytes())?,
    }
    out.write_all(column_term(d))?;
    Ok(())
}

/// `const char term = opts->end_nul ? '\0' : '\t';` — the separator both
/// `show_config_origin()` and `show_config_scope()` put after their column
/// (builtin/config.c:238, 253). Under `--null` every field boundary is a NUL,
/// these two included.
fn column_term(d: &Display) -> &'static [u8] {
    match d.null {
        true => b"\0",
        false => b"\t",
    }
}

/// The path `--show-origin` prints, respelled from the top of the work tree.
///
/// `setup_git_directory()` has already chdir'd there by the time any of these
/// strings is built, so every relative path git prints is relative to the top
/// level; the port stays in the directory the command was typed in, so the same
/// file comes out one `../` per prefix component too long. Both spellings are
/// otherwise textual — neither git nor this normalizes them, which is why an
/// include shows as `.git/../extra.cfg` — so the respelling is textual as well:
/// drop exactly the climb that leads back to the top.
///
/// A path that came from `--file` goes the other way. `OPT_FILENAME` runs
/// `prefix_filename()` over it during option parsing, which prepends the prefix
/// to the string as typed and leaves an absolute one alone, so `-f ../up.cfg`
/// from `src/` prints as `src/../up.cfg`.
fn origin_path<'a>(d: &Display, source: Source, path: &'a str) -> std::borrow::Cow<'a, str> {
    // A `--file` path is printed as it was typed, `./` and all, once the prefix
    // is on the front. Everywhere else the path came from discovery, and gix
    // leaves a `./` on the front of one it opened relative to the cwd where git
    // has none.
    if source == Source::Cli {
        return match (&d.prefix, std::path::Path::new(path).is_absolute()) {
            (Some(prefix), false) => format!("{prefix}{path}").into(),
            _ => path.into(),
        };
    }
    let path = path.strip_prefix("./").unwrap_or(path);
    let Some(prefix) = d.prefix.as_deref() else {
        return path.into();
    };
    let climb: String = std::path::Path::new(prefix).components().map(|_| "../").collect();
    path.strip_prefix(&climb).unwrap_or(path).into()
}

/// git's `--show-origin` word for a source with no file behind it.
fn origin_word(source: Source) -> &'static str {
    match source {
        Source::Cli => "command line:",
        Source::Env | Source::EnvOverride => "environment:",
        _ => "blob:",
    }
}

/// Apply `--type` to a value — `format_config()` (builtin/config.c:406) for one
/// entry, in its non-gentle mode.
///
/// A value that will not parse is fatal at exit 128, in whichever of git's three
/// shapes the type uses. Two of them name the source the value came from, so the
/// entry's metadata is needed as well as its key.
fn typed(
    d: &Display,
    key: &str,
    value: &[u8],
    implicit: bool,
    meta: &gix::config::file::Metadata,
) -> std::result::Result<Vec<u8>, ExitCode> {
    let Some(t) = d.ty else { return Ok(value.to_vec()) };
    t.canonicalize(key, value, implicit)
        .map_err(|err| report_type_error(err, key, value, meta.path.as_deref()))
}

/// Put one of git's three type-failure shapes on stderr and hand back its exit
/// code. `origin` is the file the value came from, absent for a `-c` /
/// `GIT_CONFIG_*` value or one typed on the command line.
fn report_type_error(
    err: TypeError,
    key: &str,
    value: &[u8],
    origin: Option<&std::path::Path>,
) -> ExitCode {
    let shown = String::from_utf8_lossy(value);
    match err {
        TypeError::BadNumber { out_of_range } => {
            let reason = if out_of_range { "out of range" } else { "invalid unit" };
            // `die_bad_number()` splits on whether the value has a file behind it:
            // one that does not gets the short form (config.c:1201-1202).
            match origin {
                Some(path) => eprintln!(
                    "fatal: bad numeric config value '{shown}' for '{key}' in file {}: {reason}",
                    display_origin_path(path)
                ),
                None => eprintln!("fatal: bad numeric config value '{shown}' for '{key}': {reason}"),
            }
        }
        TypeError::BadBool => eprintln!("fatal: bad boolean config value '{shown}' for '{key}'"),
        TypeError::ExpandUser => eprintln!("fatal: failed to expand user dir in: '{shown}'"),
        // The callback's own `error()`, then the line the config machinery adds
        // when a callback aborts the parse (config.c's `git_parse_source` /
        // `git_config_from_parameters`).
        TypeError::Callback(message) => {
            eprintln!("{message}");
            match origin {
                Some(path) => eprintln!(
                    "fatal: bad config line {} in file {}",
                    config_line_of(path, key, value).unwrap_or(0),
                    display_origin_path(path)
                ),
                None => eprintln!("fatal: unable to parse command-line config"),
            }
        }
    }
    ExitCode::from(128)
}

/// `normalize_value()` (builtin/config.c:654): what a `--type`d *write* stores.
///
/// This is deliberately not the same as the read-side canonicalization:
///
///   * no type at all, `--type=path` and `--type=expiry-date` store the value
///     verbatim — git keeps `~/foobar` in the file and expands the `~` on the way
///     back out, and says outright that expiry dates are not normalized either
///     (so an unparsable one is written without complaint);
///   * `--type=color` *validates* the spec and then stores the original text,
///     since the escape sequence it parses to is not something a config file can
///     hold;
///   * the rest store the canonical form — `1k` becomes `1024`, `2` becomes
///     `true`.
///
/// `None` means the value was rejected and the diagnostic is already on stderr;
/// every one of git's rejections here is a `die()`, so the caller exits 128.
fn normalized(
    ty: Option<ValueType>,
    key: &str,
    value: &str,
) -> std::result::Result<String, ExitCode> {
    match normalize_value(ty, key, value) {
        Some(v) => Ok(v),
        None => Err(ExitCode::from(128)),
    }
}

/// See [`normalized`], which is the same thing with the exit code attached.
fn normalize_value(ty: Option<ValueType>, key: &str, value: &str) -> Option<String> {
    let Some(ty) = ty else { return Some(value.to_string()) };
    if matches!(ty, ValueType::Path | ValueType::ExpiryDate) {
        return Some(value.to_string());
    }
    match ty.canonicalize(key, value.as_bytes(), false) {
        // The parsed escape sequence is a "sanity-check" only; git returns the
        // value it was given (builtin/config.c:693-700).
        Ok(_) if ty == ValueType::Color => Some(value.to_string()),
        Ok(canonical) => Some(String::from_utf8_lossy(&canonical).into_owned()),
        Err(err) => {
            match err {
                // A value handed to `git config` on the command line has no file
                // behind it, so `die_bad_number()` takes its short form.
                TypeError::BadNumber { .. } | TypeError::BadBool | TypeError::ExpandUser => {
                    report_type_error(err, key, value.as_bytes(), None);
                }
                TypeError::Callback(message) => {
                    eprintln!("{message}");
                    eprintln!("fatal: cannot parse color '{value}'");
                }
            }
            None
        }
    }
}

/// A config file's path as git prints it in a diagnostic: exactly as it resolved
/// it, without the `./` a relative discovery leaves on the front.
fn display_origin_path(path: &std::path::Path) -> String {
    let text = path.to_string_lossy();
    text.strip_prefix("./").unwrap_or(&text).to_string()
}

/// The physical line an entry sits on, which is what `cf->linenr` holds when a
/// callback aborts the parse and git reports `bad config line <n> in file <f>`.
///
/// The file is re-read and walked as a token stream because `gix_config` keeps a
/// parsed section/value model with no source positions in it. Every `Newline`
/// event advances the counter, so a value continued over several lines is blamed
/// on the line it *ends* on — the line git's reader had just consumed when it
/// handed the value to the callback.
fn config_line_of(path: &std::path::Path, key: &str, value: &[u8]) -> Option<usize> {
    use gix::bstr::ByteSlice;
    use gix::config::parse::EventRef;

    let bytes = std::fs::read(path).ok()?;
    let events = gix::config::parse::Events::from_bytes(&bytes, None).ok()?;
    let mut line = 1usize;
    // The git-normalized `section[.subsection].` prefix the current header sets.
    let mut prefix: Option<String> = None;
    let mut name: Option<String> = None;
    let mut collected: Vec<u8> = Vec::new();
    for event in events.iter() {
        match event {
            // A run of consecutive line endings arrives as one event, so the
            // counter advances by however many the run holds.
            EventRef::Newline(nl) => line += nl.iter().filter(|b| **b == b'\n').count(),
            EventRef::SectionHeader {
                name: section,
                subsection_name,
                ..
            } => {
                prefix = Some(match subsection_name {
                    Some(sub) => format!("{}.{}.", section.to_str_lossy().to_lowercase(), sub),
                    None => format!("{}.", section.to_str_lossy().to_lowercase()),
                });
                name = None;
            }
            EventRef::SectionValueName(n) => {
                name = Some(n.to_str_lossy().to_lowercase());
                collected.clear();
            }
            EventRef::ValueNotDone(v) => collected.extend_from_slice(v.as_bytes()),
            EventRef::Value(v) | EventRef::ValueDone(v) => {
                collected.extend_from_slice(v.as_bytes());
                let (Some(p), Some(n)) = (&prefix, &name) else { continue };
                if format!("{p}{n}") == key && collected == value {
                    return Some(line);
                }
            }
            _ => {}
        }
    }
    None
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

    // `show_all_config()` (builtin/config.c:473) is the one caller that passes
    // `gently = 1` to `format_config()` and prints only when it returns `>= 0`, so
    // a `--list --type=<t>` quietly drops every entry the type cannot read rather
    // than dying on the first one.
    for_each_entry(file, |key, value, implicit, meta| {
        let canonical = match d.ty {
            None => value.to_vec(),
            // A valueless key is git's `NULL`, and the boolean readers answer *true*
            // for it — the same distinction `--get` already makes.
            Some(t) => match t.canonicalize(key, value, implicit) {
                Ok(v) => v,
                Err(_) => return Ok(()),
            },
        };
        // ```c
        // case TYPE_NONE:
        //         if (value_) {
        //                 strbuf_addstr(buf, value_);
        //         } else {
        //                 /* Just show the key name; back out delimiter */
        //                 if (opts->show_keys)
        //                         strbuf_setlen(buf, buf->len - 1);
        //         }
        // ```
        //
        // (builtin/config.c:454-461.) A name written with no `=` has *no* value, which is
        // not the same as an empty one: `flag` lists as `demo.flag` and `empty =` as
        // `demo.empty=`. Any `--type` takes the entry out of the `TYPE_NONE` arm, and
        // the delimiter stays — which is what makes `--type=bool` print `demo.flag=true`.
        let shown = (!implicit || d.ty.is_some()).then_some(canonical.as_slice());
        emit_kv_opt(&mut out, d, key, shown, meta, b'=', true)
    })?;

    Ok(ExitCode::SUCCESS)
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
///
/// `all` and `show_keys` are what separates the two spellings that share this
/// reader: the legacy `--get-regexp` sets both (every match, `key value`), while
/// `git config get --regexp` sets neither, so it prints bare values and only the
/// last one unless `--all` was given.
fn get_regexp(
    file: &gix::config::File,
    pattern: &str,
    value_pattern: Option<&str>,
    d: &Display,
    all: bool,
    show_keys: bool,
) -> Result<ExitCode> {
    let re = match regex::bytes::Regex::new(&lowercase_key_pattern(pattern)) {
        Ok(re) => re,
        Err(_) => {
            eprintln!("error: invalid key pattern: {pattern}");
            return Ok(ExitCode::from(6));
        }
    };
    // The optional second operand narrows by VALUE, on top of the key match.
    let filter = match value_pattern.map(|p| ValueFilter::parse(p, d.fixed_value)) {
        Some(Err(code)) => return Ok(code),
        Some(Ok(f)) => Some(f),
        None => None,
    };

    // `get_value()` (builtin/config.c:538) collects every match into a strbuf list
    // and writes the list only once the whole walk succeeded, so a value that will
    // not canonicalize suppresses the matches found *before* it as well as the
    // ones after. Collecting here rather than streaming reproduces that.
    let mut collected: Vec<(String, Vec<u8>, bool, gix::config::file::Metadata)> = Vec::new();
    let mut failed: Option<ExitCode> = None;
    for_each_entry(file, |key, value, implicit, meta| {
        if failed.is_some() || !re.is_match(key.as_bytes()) {
            return Ok(());
        }
        if filter.as_ref().is_some_and(|f| !f.matches(value)) {
            return Ok(());
        }
        match typed(d, key, value, implicit, meta) {
            Ok(v) => collected.push((key.to_owned(), v, implicit, meta.clone())),
            Err(code) => failed = Some(code),
        }
        Ok(())
    })?;
    if let Some(code) = failed {
        return Ok(code);
    }

    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    if collected.is_empty() {
        return emit_default(&mut out, d, pattern);
    }
    // ```c
    // if ((get_value_flags & GET_VALUE_ALL) || i == values.nr - 1)
    //         fwrite(buf->buf, 1, buf->len, stdout);
    // ```
    //
    // (`builtin/config.c:632-637`.)
    let emit: &[_] = if all { &collected } else { &collected[collected.len() - 1..] };
    for (key, value, implicit, meta) in emit {
        // A key with no `=` prints as its name alone under `--show-names`, and as an
        // empty line without it — `format_config()` backs the delimiter out
        // (builtin/config.c:454-461) — but a `--type` reader answers for the missing
        // value, so it has one to print.
        let shown = (!implicit || d.ty.is_some()).then_some(value.as_slice());
        emit_kv_opt(&mut out, d, key, shown, meta, b' ', show_keys || d.show_names)?;
    }

    Ok(ExitCode::SUCCESS)
}

/// git lower-cases a key *pattern* the way it lower-cases a key: the section
/// name and the variable name, leaving the subsection between them alone.
///
/// ```c
/// key = xstrdup(key_);
/// for (tl = key + strlen(key) - 1; tl >= key && *tl != '.'; tl--)
///         *tl = tolower(*tl);
/// for (tl = key; *tl && *tl != '.'; tl++)
///         *tl = tolower(*tl);
/// ```
///
/// (`builtin/config.c:563-569`.) The first loop walks back from the end to the
/// last `.`, the second forward from the start to the first one; a pattern with
/// no `.` at all is lower-cased entirely by the first. Regexp metacharacters are
/// carried along untouched, which is why `DEMO\.ONE` matches `demo.one`.
fn lowercase_key_pattern(pattern: &str) -> String {
    let mut bytes = pattern.as_bytes().to_vec();
    for b in bytes.iter_mut().rev() {
        if *b == b'.' {
            break;
        }
        b.make_ascii_lowercase();
    }
    for b in bytes.iter_mut() {
        if *b == b'.' {
            break;
        }
        b.make_ascii_lowercase();
    }
    String::from_utf8_lossy(&bytes).into_owned()
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
    mut emit: impl FnMut(&str, &[u8], bool, &gix::config::file::Metadata) -> Result<()>,
) -> Result<()> {
    let mut echoes = crate::config::CliEcho::new();
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
            // A name written with no `=` has *no* value, which is not the same as an empty one:
            // `format_config()` backs the delimiter out again for the first and keeps it for the
            // second (builtin/config.c:454-461). The empty slice stands for both here, so the
            // implicit ones are recorded separately.
            let Some(value) = section.values_implicit(value_name).into_iter().nth(*nth) else {
                continue;
            };
            let implicit = value.is_none();
            let value = value.unwrap_or_default();
            let key = match &subsection {
                Some(sub) => format!("{section_name}.{sub}.{value_name}"),
                None => format!("{section_name}.{value_name}"),
            };
            // The two copies of a `-c key=value` are one configured value, not
            // two; git prints it once. See `crate::config::CliEcho`.
            let echoed = !implicit
                && echoes.is_echo(
                    section.meta().source,
                    &key,
                    std::str::from_utf8(&value).ok(),
                );
            if echoed {
                continue;
            }
            emit(&key, &value, implicit, section.meta())?;
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
///   1. a longer matched host wins,
///   2. a longer matched path wins,
///   3. a pattern that names a user beats one that does not.
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
    // `url_normalize()` fills in `out_info->err` and `cmd_config_get_urlmatch()`
    // prints that message and nothing else.
    let want = match url_normalize(url, false) {
        Ok(info) => info,
        Err(message) => return Ok(fatal(message)),
    };

    // key -> (match, value): the winner for each key seen so far.
    let mut best: std::collections::BTreeMap<String, (UrlMatch, Vec<u8>)> =
        std::collections::BTreeMap::new();

    for sec in file.sections() {
        if is_synthetic(sec.meta().source) || sec.header().name() != section.as_str() {
            continue;
        }
        let score = match sec.header().subsection_name() {
            // A section with no subsection carries no URL to match, so
            // `urlmatch_config_entry()` never calls `match_urls()` for it and its
            // `urlmatch_item` stays all-zero: it matches every URL, at the lowest
            // specificity there is.
            None => UrlMatch::default(),
            Some(pattern) => {
                let text = pattern.to_string();
                // A subsection that will not normalize is simply not a match —
                // `url_normalize_1()` returning NULL is `retval = 0` there
                // (urlmatch.c:707-714), never a diagnostic.
                match url_normalize(&text, true).ok().and_then(|p| match_urls(&want, &p)) {
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
            let entry = best.entry(lname).or_insert_with(|| (UrlMatch::default(), Vec::new()));
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

/// The RFC 3986 character classes `urlmatch.c` works from (urlmatch.c:10-18).
mod url_chars {
    pub const SCHEME: &str = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+.-";
    /// IPv6 literals need `[:]`.
    pub const HOST: &str = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789.-_[:]";
    pub const UNSAFE: &str = " <>\"%{}|\\^`";
    /// `URL_GEN_RESERVED URL_SUB_RESERVED` — the only allowed delimiters.
    pub const RESERVED: &str = ":/?#[]@!$&'()*+,;=";
}

/// `append_normalized_escapes()` (urlmatch.c:20-70): copy `from` into `buf`,
/// unescaping what does not need escaping and escaping what does.
///
/// The unsafe set is RFC 3986's (`0x00-0x1F`, `0x7F-0xFF` and
/// [`url_chars::UNSAFE`]); characters in `esc_ok` are left escaped when they
/// arrived escaped but are never escaped otherwise, which is how delimiters
/// survive normalization without being introduced by it. Every `%XX` comes out
/// upper-case. `None` when a `%` is not followed by two hex digits.
fn append_normalized_escapes(buf: &mut Vec<u8>, from: &[u8], esc_ok: &str) -> Option<()> {
    let mut i = 0;
    while i < from.len() {
        let mut ch = from[i];
        i += 1;
        let mut was_esc = false;
        if ch == b'%' {
            let hex = from.get(i..i + 2)?;
            ch = u8::from_str_radix(std::str::from_utf8(hex).ok()?, 16).ok()?;
            i += 2;
            was_esc = true;
        }
        let escape = ch <= 0x1F
            || ch >= 0x7F
            || url_chars::UNSAFE.as_bytes().contains(&ch)
            || (was_esc && esc_ok.as_bytes().contains(&ch));
        match escape {
            true => buf.extend_from_slice(format!("%{ch:02X}").as_bytes()),
            false => buf.push(ch),
        }
    }
    Some(())
}

/// A normalized URL and the offsets of its parts — git's `struct url_info`,
/// filled in by [`url_normalize`].
#[derive(Default)]
struct UrlInfo {
    url: Vec<u8>,
    scheme_len: usize,
    user_off: usize,
    user_len: usize,
    host_off: usize,
    host_len: usize,
    port_off: usize,
    port_len: usize,
    path_off: usize,
}

/// `url_normalize_1()` (urlmatch.c:115-437), whose own comment lists what it
/// does: lower-case the case-insensitive parts, unescape what needs no escape
/// and escape what does, upper-case every `%XX`, drop leading zeroes and default
/// ports, give a path-less URL a `/`, and resolve `.`/`..` segments. IPv6
/// literals are passed through unvalidated.
///
/// `allow_globs` is what separates a config subsection from the URL being looked
/// up: a pattern may carry `*` in its host (urlmatch.c:244-247), which
/// [`match_host`] then matches component by component.
///
/// The `Err` is git's own message, which `cmd_config_get_urlmatch()` prints
/// verbatim, so the failures are named exactly as git names them.
fn url_normalize(url: &str, allow_globs: bool) -> std::result::Result<UrlInfo, &'static str> {
    let src = url.as_bytes();
    let mut info = UrlInfo::default();
    let norm = &mut info.url;

    // The scheme is `URL_SCHEME_CHARS` starting with a letter, then `://`; no
    // %-escapes are allowed in it.
    let spanned = src.iter().take_while(|c| url_chars::SCHEME.as_bytes().contains(c)).count();
    if spanned == 0
        || !src[0].is_ascii_alphabetic()
        || spanned + 3 > src.len()
        || &src[spanned..spanned + 3] != b"://"
    {
        return Err("invalid URL scheme name or missing '://' suffix");
    }
    info.scheme_len = spanned;
    norm.extend(src[..spanned + 3].iter().map(u8::to_ascii_lowercase));
    let mut rest = &src[spanned + 3..];

    // `user[:password]@`, if the `@` comes before the path starts.
    let slash_at = |s: &[u8]| s.iter().position(|c| b"/?#".contains(c)).unwrap_or(s.len());
    let at = rest.iter().position(|c| *c == b'@');
    if let Some(at) = at.filter(|at| *at < slash_at(rest)) {
        info.user_off = norm.len();
        if at > 0 {
            append_normalized_escapes(norm, &rest[..at], url_chars::RESERVED)
                .ok_or("invalid %XX escape sequence")?;
            // A `:` in what was just appended splits user from password.
            match norm[info.scheme_len + 3..].iter().position(|c| *c == b':') {
                Some(colon) => info.user_len = colon,
                None => info.user_len = norm.len() - (info.scheme_len + 3),
            }
        }
        norm.push(b'@');
        rest = &rest[at + 1..];
    }

    // The host, without its port; no %-escapes allowed here either.
    let slash = slash_at(rest);
    if rest.is_empty() || b":/?#".contains(&rest[0]) {
        // Only `file:` may have no host.
        if !norm.starts_with(b"file:") {
            return Err("missing host and scheme is not 'file:'");
        }
    } else {
        info.host_off = norm.len();
    }
    // Scan back from the path for a port colon, stopping at an IPv6 `]`.
    let mut colon = slash;
    while colon > 0 && rest[colon - 1] != b':' && rest[colon - 1] != b']' {
        colon -= 1;
    }
    colon = match colon > 0 && rest[colon - 1] == b':' {
        true => colon - 1,
        false => slash,
    };
    if info.host_off == 0 && colon < slash && colon + 1 != slash {
        return Err("a 'file:' URL may not have a port number");
    }
    let host_chars = match allow_globs {
        true => &format!("{}*", url_chars::HOST),
        false => url_chars::HOST,
    };
    let spanned = rest.iter().take_while(|c| host_chars.as_bytes().contains(c)).count();
    if spanned < colon {
        return Err("invalid characters in host name");
    }
    norm.extend(rest[..colon].iter().map(u8::to_ascii_lowercase));

    // The port, kept only when it is not the scheme's default. Leading zeroes go
    // first, and what is left must be 1..=65535.
    if colon < slash {
        let mut port = &rest[colon + 1..slash];
        let zeros = port.iter().take_while(|c| **c == b'0').count();
        port = &port[zeros..];
        if port.is_empty() && zeros > 0 {
            // All zeroes: keep the last one so the range check refuses it.
            port = &rest[slash - 1..slash];
        }
        let default = (port == b"80" && norm.starts_with(b"http:"))
            || (port == b"443" && norm.starts_with(b"https:"));
        if !port.is_empty() && !default {
            if !port.iter().all(u8::is_ascii_digit) {
                return Err("invalid port number");
            }
            let number = match port.len() <= 5 {
                true => std::str::from_utf8(port).ok().and_then(|p| p.parse::<u32>().ok()),
                false => Some(0),
            };
            // 0 means "next available" on just about every system, so it is not a
            // port a URL may name.
            if !matches!(number, Some(1..=65535)) {
                return Err("invalid port number");
            }
            norm.push(b':');
            info.port_off = norm.len();
            info.port_len = port.len();
            norm.extend_from_slice(port);
        }
    }
    if info.host_off != 0 {
        info.host_len =
            norm.len() - info.host_off - if info.port_len != 0 { info.port_len + 1 } else { 0 };
    }

    // The path, with a leading `/` added if it is missing and `.`/`..` resolved.
    // The delimiters must survive, so the segments are unescaped for the
    // comparison only (RFC 3986 asks for exactly that).
    info.path_off = norm.len();
    let path_start = info.path_off;
    norm.push(b'/');
    let mut tail = &rest[slash..];
    if tail.first() == Some(&b'/') {
        tail = &tail[1..];
    }
    loop {
        let seg_start = norm.len();
        let next = slash_at(tail);
        append_normalized_escapes(norm, &tail[..next], url_chars::RESERVED)
            .ok_or("invalid %XX escape sequence")?;
        let mut skip_add_slash = false;
        match &norm[seg_start..] {
            b"." => {
                // Be careful not to remove the initial `/`.
                match seg_start == path_start + 1 {
                    true => {
                        norm.truncate(norm.len() - 1);
                        skip_add_slash = true;
                    }
                    false => norm.truncate(norm.len() - 2),
                }
            }
            b".." => {
                let mut prev = norm.len() - 3;
                if prev == path_start {
                    return Err("invalid '..' path segment");
                }
                // `while (*--prev_slash != '/') {}` — the byte it starts on is the
                // `/` this segment opened with, so the scan steps back first.
                loop {
                    prev -= 1;
                    if norm[prev] == b'/' {
                        break;
                    }
                }
                match prev == path_start {
                    true => {
                        norm.truncate(prev + 1);
                        skip_add_slash = true;
                    }
                    false => norm.truncate(prev),
                }
            }
            _ => {}
        }
        tail = &tail[next..];
        // Anything but another `/` ends the path.
        if tail.first() != Some(&b'/') {
            break;
        }
        tail = &tail[1..];
        if !skip_add_slash {
            norm.push(b'/');
        }
    }

    // Whatever is left (a query or fragment) is copied with its escapes
    // normalized and nothing else touched.
    if !tail.is_empty() {
        append_normalized_escapes(norm, tail, url_chars::RESERVED)
            .ok_or("invalid %XX escape sequence")?;
    }
    Ok(info)
}

impl UrlInfo {
    fn part(&self, off: usize, len: usize) -> &[u8] {
        &self.url[off..off + len]
    }

    /// The path and everything after it, which is what `match_urls()` compares
    /// (`url_prefix->url_len - url_prefix->path_off`).
    fn path(&self) -> &[u8] {
        &self.url[self.path_off..]
    }
}

/// `match_host()` (urlmatch.c:80-113): host names match component by component,
/// and a pattern component of exactly `*` matches any one component.
fn match_host(url: &UrlInfo, pattern: &UrlInfo) -> bool {
    let mut url = url.part(url.host_off, url.host_len);
    let mut pat = pattern.part(pattern.host_off, pattern.host_len);
    while !url.is_empty() && !pat.is_empty() {
        let url_next = url.iter().position(|c| *c == b'.').unwrap_or(url.len());
        let pat_next = pat.iter().position(|c| *c == b'.').unwrap_or(pat.len());
        if !(pat[..pat_next] == *b"*" || url[..url_next] == pat[..pat_next]) {
            return false;
        }
        url = &url[(url_next + 1).min(url.len())..];
        pat = &pat[(pat_next + 1).min(pat.len())..];
    }
    url.is_empty() && pat.is_empty()
}

/// `url_match_prefix()` (urlmatch.c:570-600): `prefix` matches `url` when it is
/// the whole of it or a prefix ending on a path-component boundary. Both are
/// treated as having a trailing `/` they may not carry.
///
/// The answer is the length of the match *including* that final `/`, so a
/// prefix that matched nothing but the root still scores 1 — which is what
/// separates a generic section from no match at all.
fn url_match_prefix(url: &[u8], prefix: &[u8]) -> usize {
    if prefix.is_empty() || prefix == b"/" {
        return match url.is_empty() || url[0] == b'/' {
            true => 1,
            false => 0,
        };
    }
    let prefix = match prefix.last() {
        Some(b'/') => &prefix[..prefix.len() - 1],
        _ => prefix,
    };
    if !url.starts_with(prefix) {
        return 0;
    }
    match url.len() == prefix.len() || url[prefix.len()] == b'/' {
        true => prefix.len() + 1,
        false => 0,
    }
}

/// How well a config subsection matched, and therefore which of two candidates
/// wins — git's `struct urlmatch_item` ordered by `cmp_matches()`
/// (urlmatch.c:671-681): the longer matched host first, then the longer matched
/// path, and only then a pattern that named a user over one that did not.
#[derive(Default, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct UrlMatch {
    hostmatch_len: usize,
    pathmatch_len: usize,
    user_matched: bool,
}

/// `match_urls()` (urlmatch.c:602-669): a pattern matches when its scheme, host
/// and port are the URL's and its path is the URL's or a prefix of it at a `/`
/// boundary. A user name in the pattern must match exactly; one in the URL alone
/// does not stop a pattern without one from matching.
fn match_urls(url: &UrlInfo, pattern: &UrlInfo) -> Option<UrlMatch> {
    if pattern.scheme_len != url.scheme_len
        || url.url[..url.scheme_len] != pattern.url[..pattern.scheme_len]
    {
        return None;
    }
    let mut user_matched = false;
    if pattern.user_off != 0 {
        if url.user_off == 0
            || url.user_len != pattern.user_len
            || url.part(url.user_off, url.user_len) != pattern.part(pattern.user_off, pattern.user_len)
        {
            return None;
        }
        user_matched = true;
    }
    if !match_host(url, pattern) {
        return None;
    }
    if url.port_len != pattern.port_len
        || url.part(url.port_off, url.port_len) != pattern.part(pattern.port_off, pattern.port_len)
    {
        return None;
    }
    let pathmatch_len = url_match_prefix(url.path(), pattern.path());
    if pathmatch_len == 0 {
        return None;
    }
    Some(UrlMatch { hostmatch_len: pattern.host_len, pathmatch_len, user_matched })
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
            crate::setup::discover()
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

    // `if (strcmp(editor, ":"))` (editor.c:66): the no-op editor spawns nothing.
    if editor == ":" {
        return Ok(ExitCode::SUCCESS);
    }
    let status =
        crate::external::prepare_shell_cmd_str(&editor, [&target.path]).status()?;
    Ok(match status.code() {
        Some(0) | None => ExitCode::SUCCESS,
        Some(code) => ExitCode::from(code as u8),
    })
}

/// `kvi_from_param()` (`config.c`): the origin a value typed on the command line
/// carries — no file, no line number. `--show-origin` renders it as `command line:`
/// and the type diagnostics take their short form.
fn param_metadata() -> gix::config::file::Metadata {
    gix::config::file::Metadata::from(Source::Cli)
}

/// ```c
/// static int git_get_color_config(const char *var, const char *value,
///                                 const struct config_context *ctx UNUSED, void *cb)
/// {
///         struct get_color_config_data *data = cb;
///
///         if (!strcmp(var, data->get_color_slot)) {
///                 if (!value)
///                         config_error_nonbool(var);
///                 if (color_parse(value, data->parsed_color) < 0)
///                         return -1;
///                 data->get_color_found = 1;
///         }
///         return 0;
/// }
///
/// static int get_color(const struct config_location_options *opts,
///                       const char *var, const char *def_color)
/// {
///         …
///         config_with_options(git_get_color_config, &data, …);
///
///         if (!data.get_color_found && def_color) {
///                 if (color_parse(def_color, data.parsed_color) < 0) {
///                         ret = error(_("unable to parse default color value"));
///                         goto out;
///                 }
///         }
///         ret = 0;
/// out:
///         fputs(data.parsed_color, stdout);
///         return ret;
/// }
/// ```
///
/// (`builtin/config.c:712-753`.) Four things the shape of that function decides:
///
/// * the output is `fputs` of the escape sequence with **no newline**, and an unset
///   slot with no default prints nothing at all and still exits 0.
/// * every occurrence of the slot is parsed as the callback walks the file, so a
///   value the parser rejects is fatal even when a later one would have won — and
///   it fails as a config-callback error, `fatal: bad config line <n> in file <f>`.
/// * the *last* occurrence is the one that survives into `parsed_color`.
/// * a `<default>` the parser rejects is a plain `error()` return, not a `die()`:
///   `unable to parse default color value` and `main()`'s `-1`, which the shell
///   sees as 255.
fn get_color(file: &gix::config::File, slot: &str, def_color: Option<&str>) -> Result<ExitCode> {
    let key = parse_key(slot)?;
    let wanted = key_of(&key);

    let stdout = std::io::stdout();
    let mut out = stdout.lock();

    let mut parsed: Option<String> = None;
    let mut failure: Option<(Vec<u8>, Option<std::path::PathBuf>)> = None;
    for_each_entry(file, |k, value, _implicit, meta| {
        if k != wanted || failure.is_some() {
            return Ok(());
        }
        match super::color::parse_color_spec(&String::from_utf8_lossy(value)) {
            Some(sgr) => parsed = Some(sgr),
            None => failure = Some((value.to_vec(), meta.path.as_deref().map(ToOwned::to_owned))),
        }
        Ok(())
    })?;
    if let Some((value, origin)) = failure {
        let text = String::from_utf8_lossy(&value).into_owned();
        return Ok(report_type_error(
            TypeError::Callback(format!("error: invalid color value: {text}")),
            slot,
            &value,
            origin.as_deref(),
        ));
    }

    if parsed.is_none() {
        if let Some(def) = def_color {
            match super::color::parse_color_spec(def) {
                Some(sgr) => parsed = Some(sgr),
                None => {
                    eprintln!("error: invalid color value: {def}");
                    eprintln!("error: unable to parse default color value");
                    // `main()` hands the shell `-1`, which is 255.
                    return Ok(ExitCode::from(255));
                }
            }
        }
    }
    out.write_all(parsed.unwrap_or_default().as_bytes())?;
    Ok(ExitCode::SUCCESS)
}

/// ```c
/// static int git_get_colorbool_config(const char *var, const char *value,
///                                     const struct config_context *ctx UNUSED, void *cb)
/// {
///         struct get_colorbool_config_data *data = cb;
///
///         if (!strcmp(var, data->get_colorbool_slot))
///                 data->get_colorbool_found = git_config_colorbool(var, value);
///         else if (!strcmp(var, "diff.color"))
///                 data->get_diff_color_found = git_config_colorbool(var, value);
///         else if (!strcmp(var, "color.ui"))
///                 data->get_color_ui_found = git_config_colorbool(var, value);
///         return 0;
/// }
///
/// static int get_colorbool(const struct config_location_options *opts,
///                          const char *var, int print)
/// {
///         …
///         if (data.get_colorbool_found == GIT_COLOR_UNKNOWN) {
///                 if (!strcmp(data.get_colorbool_slot, "color.diff"))
///                         data.get_colorbool_found = data.get_diff_color_found;
///                 if (data.get_colorbool_found == GIT_COLOR_UNKNOWN)
///                         data.get_colorbool_found = data.get_color_ui_found;
///         }
///
///         if (data.get_colorbool_found == GIT_COLOR_UNKNOWN)
///                 /* default value if none found in config */
///                 data.get_colorbool_found = GIT_COLOR_AUTO;
///
///         result = want_color(data.get_colorbool_found);
///
///         if (print) {
///                 printf("%s\n", result ? "true" : "false");
///                 return 0;
///         } else
///                 return result ? 0 : 1;
/// }
/// ```
///
/// (`builtin/config.c:762-810`.) The fallback chain is the whole point of the
/// option: an unset slot inherits `diff.color` (only for the slot literally named
/// `color.diff`, git's historical spelling) and then `color.ui`, and only a slot
/// that none of the three set falls through to `auto`. `never`/`always` are
/// answers in their own right, so `color.ui = never` makes every unset slot false
/// even on a terminal.
///
/// The exit code is inverted from the usual convention so a shell `if` reads
/// naturally: 0 when color is on, 1 when it is off. The value is *printed* only
/// when the caller states whether stdout is a terminal, which is git's `print`
/// argument (`argc == 2`).
fn get_colorbool(file: &gix::config::File, positional: &[&str]) -> Result<ExitCode> {
    let Some(name) = positional.first() else {
        return usage_error("wrong number of arguments, should be from 1 to 2");
    };
    if positional.len() > 2 {
        return usage_error("wrong number of arguments, should be from 1 to 2");
    }
    // `color_stdout_is_tty = git_config_bool("command line", argv[1])` — the caller
    // overrides the `isatty()` probe, and a word that is not a boolean is
    // `git_config_bool()`'s `die()`, naming `command line` as the origin.
    let stated = positional.get(1);
    let tty = match stated {
        Some(v) => match optint::maybe_bool(v) {
            Some(b) => b,
            None => {
                eprintln!("fatal: bad boolean config value '{v}' for 'command line'");
                return Ok(ExitCode::from(128));
            }
        },
        None => std::io::IsTerminal::is_terminal(&std::io::stdout()),
    };

    let key = parse_key(name)?;
    let wanted = key_of(&key);
    let (mut slot, mut diff_color, mut color_ui) = (None, None, None);
    // The value that is not one of the three words and not a boolean either —
    // `color.diff = red`, say — is `git_config_bool()`'s `die()`, and it fires
    // while the config is being walked rather than at the end.
    let mut bad: Option<(String, String)> = None;
    for_each_entry(file, |k, value, _implicit, _| {
        if k != wanted && k != "diff.color" && k != "color.ui" {
            return Ok(());
        }
        let text = String::from_utf8_lossy(value).trim().to_string();
        let Some(decided) = colorbool_of(&text) else {
            if bad.is_none() {
                bad = Some((k.to_string(), text));
            }
            return Ok(());
        };
        if k == wanted {
            slot = Some(decided);
        } else if k == "diff.color" {
            diff_color = Some(decided);
        } else {
            color_ui = Some(decided);
        }
        Ok(())
    })?;
    if let Some((key, value)) = bad {
        eprintln!("fatal: bad boolean config value '{value}' for '{key}'");
        return Ok(ExitCode::from(128));
    }

    let found = slot
        .or_else(|| if wanted == "color.diff" { diff_color } else { None })
        .or(color_ui)
        // `GIT_COLOR_AUTO` is the default when nothing in the config decided.
        .unwrap_or(ColorBool::Auto);
    // `want_color()`: `ALWAYS` and `NEVER` answer outright, `AUTO` goes through
    // `check_auto_color()` (`color.c`), which is not the terminal alone —
    // a tty still answers `false` unless `TERM` names a terminal that can do
    // more than `dumb`:
    // ```c
    // if (color_stdout_is_tty || (pager_in_use() && pager_use_color)) {
    //         char *term = getenv("TERM");
    //         if (term && strcmp(term, "dumb"))
    //                 return 1;
    // }
    // return 0;
    // ```
    let on = match found {
        ColorBool::Always => true,
        ColorBool::Never => false,
        ColorBool::Auto => {
            tty && std::env::var("TERM").is_ok_and(|term| term != "dumb")
        }
    };
    if stated.is_some() {
        println!("{on}");
        return Ok(ExitCode::SUCCESS);
    }
    Ok(if on { ExitCode::SUCCESS } else { ExitCode::from(1) })
}

/// `enum git_colorbool`, minus the `GIT_COLOR_UNKNOWN` that `Option::None` carries.
#[derive(Clone, Copy, PartialEq, Eq)]
enum ColorBool {
    Always,
    Never,
    Auto,
}

/// ```c
/// enum git_colorbool git_config_colorbool(const char *var, const char *value)
/// {
///         if (value) {
///                 if (!strcasecmp(value, "never"))  return GIT_COLOR_NEVER;
///                 if (!strcasecmp(value, "always")) return GIT_COLOR_ALWAYS;
///                 if (!strcasecmp(value, "auto"))   return GIT_COLOR_AUTO;
///         }
///         if (!var) return GIT_COLOR_UNKNOWN;
///         /* Missing or explicit false to turn off colorization */
///         if (!git_config_bool(var, value)) return GIT_COLOR_NEVER;
///         /* any normal truth value defaults to 'auto' */
///         return GIT_COLOR_AUTO;
/// }
/// ```
///
/// (`color.c:382-403`.) Anything that is not one of the three words goes through
/// `git_config_bool()`, which `die()`s on a value that is not a boolean at all —
/// so `color.diff = red` is `fatal: bad boolean config value 'red' for 'color.diff'`
/// rather than a colorized `auto`. `None` reports that failure to the caller. A
/// false boolean is `never`; a true one is `auto`, not `always`, because the slot
/// still has to agree with the terminal.
fn colorbool_of(value: &str) -> Option<ColorBool> {
    if value.eq_ignore_ascii_case("never") {
        return Some(ColorBool::Never);
    }
    if value.eq_ignore_ascii_case("always") {
        return Some(ColorBool::Always);
    }
    if value.eq_ignore_ascii_case("auto") {
        return Some(ColorBool::Auto);
    }
    match optint::maybe_bool(value)? {
        false => Some(ColorBool::Never),
        true => Some(ColorBool::Auto),
    }
}

/// `git config --rename-section <old> <new>` — rewrite the section header in
/// place, keeping every value. Missing section is git's
/// `fatal: no such section: <old>` at exit 128.
fn rename_section(target: &WriteTarget, positional: &[&str]) -> Result<ExitCode> {
    let (old, new) = match positional {
        [old, new] => (*old, *new),
        _ => return usage_error("wrong number of arguments, should be 2"),
    };
    // ```c
    // if (new_name && !section_name_is_valid(new_name)) {
    //         ret = error(_("invalid section name: %s"), new_name);
    //         goto out_no_rollback;
    // }
    // ```
    //
    // (`git_config_copy_or_rename_section_in_file()`, config.c.) It is the first
    // thing the rename does — before the lock, before the file is read — and it
    // is an `error()` returning -1, which surfaces as exit 255. Only the *new*
    // name is checked; `--remove-section` passes a NULL new name and skips it.
    if !section_name_is_valid(new) {
        eprintln!("error: invalid section name: {new}");
        return Ok(ExitCode::from(255));
    }
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
    fixed_value: bool,
) -> Result<ExitCode> {
    let key = parse_key_write(name)?;
    let section_lc = key.section_name.to_lowercase();
    let value_lc = key.value_name.to_lowercase();
    let filter = match value_pattern.map(|p| ValueFilter::parse(p, fixed_value)) {
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
/// Port of config.c's `section_name_is_valid()`: an empty name is bogus, and up
/// to the first dot every byte must be ASCII alphanumeric or `-`. Past that dot
/// lies the subsection, where "anything goes, so we can stop checking".
fn section_name_is_valid(name: &str) -> bool {
    if name.is_empty() {
        return false;
    }
    name.bytes()
        .take_while(|&c| c != b'.')
        .all(|c| c == b'-' || c.is_ascii_alphanumeric())
}

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

/// `git_config_from_blob_ref()` (config.c:1483-1494) and the
/// `git_config_from_blob_oid()` it calls (:1456-1481): resolve `spec`, insist the
/// object is a blob, and parse its bytes as configuration.
///
/// Each of the three failures is an `error()` returning -1, never a `die()`, so
/// the caller decides what a failed read means; `Err(())` says the message is
/// already on stderr.
fn blob_config(repo: &gix::Repository, spec: &str) -> std::result::Result<ConfigFile, ()> {
    let Ok(id) = repo.rev_parse_single(spec) else {
        eprintln!("error: unable to resolve config blob '{spec}'");
        return Err(());
    };
    let Ok(object) = id.object() else {
        eprintln!("error: unable to load config blob object '{spec}'");
        return Err(());
    };
    if object.kind != gix::object::Kind::Blob {
        eprintln!("error: reference '{spec}' does not point to a blob");
        return Err(());
    }
    // `git_config_from_mem()` reports a parse failure against the blob name,
    // where a file would have been named as a file (config.c's
    // `git_parse_source()`, `CONFIG_ORIGIN_BLOB`).
    if let Some(line) = crate::config::first_bad_config_line(&object.data) {
        eprintln!("error: bad config line {line} in blob {spec}");
        return Err(());
    }
    ConfigFile::from_bytes_no_includes(
        &object.data,
        gix::config::file::Metadata::from(Source::Cli),
        Default::default(),
    )
    .map_err(|_| ())
}

/// The file `--worktree` names:
///
/// ```c
/// struct worktree **worktrees = get_worktrees();
/// if (the_repository->repository_format_worktree_config)
///         opts->source.file = opts->file_to_free =
///                 repo_git_path(the_repository, "config.worktree");
/// else if (worktrees[0] && worktrees[1])
///         die(_("--worktree cannot be used with multiple working trees unless the config\n"
///               "extension worktreeConfig is enabled. Please read \"CONFIGURATION FILE\"\n"
///               "section in \"git help worktree\" for details"));
/// else
///         opts->source.file = opts->file_to_free =
///                 repo_git_path(the_repository, "config");
/// ```
///
/// (`builtin/config.c:975-991`.) `config.worktree` is per-worktree, so it is
/// `$GIT_DIR/config.worktree`; `config` is a common file, which `adjust_git_path()`
/// sends to `$GIT_COMMON_DIR` — the same file `--local` writes. Without the
/// extension the option is therefore only meaningful while there is exactly one
/// working tree, and `get_worktrees()` always lists the main one first, so a
/// single linked worktree already makes two.
fn worktree_config_file(repo: &gix::Repository) -> Result<std::path::PathBuf> {
    if repo.config_snapshot().boolean("extensions.worktreeConfig").unwrap_or(false) {
        // In the main working tree stock quotes the path git discovered, which is
        // relative to the current directory: `--show-origin` prints
        // `file:.git/config.worktree` and a parse error names
        // `.git/config.worktree`, so the discovered `repo.git_dir()` is used as it
        // stands. A linked worktree is the case that needs resolving: `git_dir()`
        // is then reached through the main tree and comes out as
        // `./../.git/worktrees/wt`, where stock prints
        // `<repo>/.git/worktrees/wt/config.worktree`.
        let git_dir = repo.git_dir();
        let linked = git_dir.components().any(|c| c.as_os_str() == "worktrees");
        let base =
            if linked { absolute_git_path(git_dir) } else { git_dir.to_path_buf() };
        return Ok(base.join("config.worktree"));
    }
    if !repo.worktrees().map(|w| w.is_empty()).unwrap_or(true) {
        let message = concat!(
            "--worktree cannot be used with multiple working trees unless the config\n",
            "extension worktreeConfig is enabled. Please read \"CONFIGURATION FILE\"\n",
            "section in \"git help worktree\" for details",
        );
        return Err(crate::fatal::Fatal(message.to_owned()).into());
    }
    Ok(repo.common_dir().join("config"))
}

/// `MAX_INCLUDE_DEPTH` (config.c): how many nested `include.path` hops git will
/// follow before it decides the chain is circular.
const MAX_INCLUDE_DEPTH: usize = 10;

/// Walk `start`'s unconditional `include.path` chain looking for the hop that
/// would exceed [`MAX_INCLUDE_DEPTH`], and return the pair git's diagnostic names:
/// the include it refused to follow and the file that asked for it. `None` is
/// every chain that terminates within the limit.
///
/// Only `include.path` is followed. A conditional `includeIf` hop is decided by
/// `include_condition_is_true()` against a context this walk does not have, and
/// a chain that is only circular through a condition would be mis-reported;
/// leaving it out means such a chain keeps whatever answer the resolver gives it
/// rather than getting a fabricated one.
fn include_depth_overflow(start: &std::path::Path) -> Option<(String, String)> {
    let mut current = start.to_path_buf();
    for depth in 0..=MAX_INCLUDE_DEPTH {
        let file =
            gix::config::File::from_path_no_includes(current.clone(), Source::Local).ok()?;
        let raw = file.string_by("include", None, "path")?.to_string();
        // `handle_path_include()` resolves a relative include against the
        // directory of the file that named it, not against the current directory.
        let next = match std::path::Path::new(&raw).is_absolute() {
            true => std::path::PathBuf::from(&raw),
            false => match current.parent().filter(|d| !d.as_os_str().is_empty()) {
                Some(dir) => dir.join(&raw),
                None => std::path::PathBuf::from(&raw),
            },
        };
        if depth == MAX_INCLUDE_DEPTH {
            return Some((display_origin_path(&next), display_origin_path(&current)));
        }
        current = next;
    }
    None
}

/// A git directory as an absolute, symlink-resolved path — what
/// `repo_git_path()` hands back and what git's diagnostics quote. Falls back to
/// the path as given when it cannot be resolved.
fn absolute_git_path(git_dir: &std::path::Path) -> std::path::PathBuf {
    std::fs::canonicalize(git_dir).unwrap_or_else(|_| git_dir.to_path_buf())
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

/// `git_global_config()` (config.c:1505-1523): the ONE file `--global` reads and
/// writes.
///
/// ```c
/// git_global_config_paths(&user_config, &xdg_config);
/// if (!user_config) { free(xdg_config); return NULL; }
///
/// if (access_or_warn(user_config, R_OK, 0) && xdg_config &&
///     !access_or_warn(xdg_config, R_OK, 0)) {
///         free(user_config);
///         return xdg_config;
/// } else {
///         free(xdg_config);
///         return user_config;
/// }
/// ```
///
/// with `git_global_config_paths()` (config.c:1525-1537) above it:
///
/// ```c
/// char *user_config = xstrdup_or_null(getenv("GIT_CONFIG_GLOBAL"));
/// char *xdg_config = NULL;
///
/// if (!user_config) {
///         user_config = interpolate_path("~/.gitconfig", 0);
///         xdg_config = xdg_config_home("config");
/// }
/// ```
///
/// Two things fall out of that shape and both are observable:
///
/// * **`$GIT_CONFIG_GLOBAL` suppresses the XDG file entirely** — it is only
///   computed on the branch where the variable is unset. So a `$GIT_CONFIG_GLOBAL`
///   naming a file that is not there is not "fall back to XDG", it is the file
///   `--list` then fails to read.
/// * **The XDG file is a fallback, not a peer.** `~/.gitconfig` wins whenever it
///   is readable, and the XDG file is used only when it is not — never both. This
///   port used to merge the pair, which showed entries under `--global --list`
///   that stock git does not show.
///
/// `None` is git's `NULL`, which the caller turns into `die(_("$HOME not set"))`.
fn global_config_file() -> Option<std::path::PathBuf> {
    if let Some(configured) = std::env::var_os("GIT_CONFIG_GLOBAL") {
        return Some(std::path::PathBuf::from(configured));
    }
    // `interpolate_path("~/.gitconfig", 0)` is NULL without `$HOME`.
    let user = std::path::PathBuf::from(std::env::var_os("HOME")?).join(".gitconfig");
    let xdg = gix::path::env::xdg_config("config", &mut gix::path::env::var);
    // `access_or_warn(…, R_OK, 0)`: readable wins, and only the *unreadable*
    // `~/.gitconfig` hands over to a readable XDG file.
    match xdg {
        Some(xdg) if !readable(&user) && readable(&xdg) => Some(xdg),
        _ => Some(user),
    }
}

/// `git_system_config()` (config.c:1496-1503): `$GIT_CONFIG_SYSTEM`, else the
/// installed `$(prefix)/etc/gitconfig`.
///
/// `GIT_CONFIG_NOSYSTEM` is deliberately absent: it is checked by
/// `git_config_system()` in the *cascade* (config.c:1540-1542), not here, so
/// `git config --system --list` reads the file even under it — verified against
/// git 2.55.0.
fn system_config_file() -> std::path::PathBuf {
    if let Some(configured) = std::env::var_os("GIT_CONFIG_SYSTEM") {
        return std::path::PathBuf::from(configured);
    }
    Source::System
        .storage_location(&mut gix::path::env::var)
        .map_or_else(|| std::path::PathBuf::from("/etc/gitconfig"), |p| p)
}

/// `access(path, R_OK)` as `access_or_warn()` tests it.
fn readable(path: &std::path::Path) -> bool {
    std::fs::File::open(path).is_ok()
}

/// The one file a `--global`/`--system` scope reads, loaded the way `--file`
/// loads its own: through `fs::read`, so a missing or unreadable path is carried
/// as an `io::Error` whose errno git reports verbatim rather than being silently
/// treated as an empty configuration.
///
/// A pure write reads nothing, matching the `--file` arm — the write path re-reads
/// its target under the lock and would otherwise repeat the access warning.
fn read_single_scope_file(
    path: &std::path::Path,
    source: Source,
    reads_config: bool,
    unreadable: &mut Option<std::io::Error>,
) -> Result<ConfigFile> {
    if !reads_config {
        return Ok(empty_config(path, source));
    }
    match read_config_bytes(path) {
        Ok(bytes) => parse_config(&bytes, path, source),
        Err(err) => {
            *unreadable = Some(err);
            Ok(empty_config(path, source))
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
        // `check_write()` (builtin/config.c:812-822).
        Scope::Blob(_) => crate::git_fatal!("writing config blobs is not supported"),
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
        Scope::Worktree => {
            let repo = repo.ok_or_else(|| {
                anyhow::anyhow!("--worktree can only be used inside a git repository")
            })?;
            Ok(WriteTarget {
                path: worktree_config_file(repo)?,
                source: Source::Local,
                lock_key: repo.git_dir().to_path_buf(),
                create_parent: false,
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
            // ```c
            // char *git_system_config(void)
            // {
            //         char *system_config = xstrdup_or_null(getenv("GIT_CONFIG_SYSTEM"));
            //         if (system_config)
            //                 return system_config;
            //         return system_path(ETC_GITCONFIG);
            // }
            // ```
            //
            // (config.c.) `GIT_CONFIG_NOSYSTEM` gates *reading* the system file, not naming
            // it, so `git config --system <k> <v>` under that variable still tries to write
            // — and reports whatever locking the path it names says. gix's
            // `storage_location()` returns nothing at all there, which turned a lock error
            // into a refusal of this port's own.
            let path = match std::env::var_os("GIT_CONFIG_SYSTEM") {
                Some(path) => std::path::PathBuf::from(path),
                None => {
                    let mut env = |k: &str| std::env::var_os(k);
                    let mut without_nosystem = |k: &str| match k {
                        "GIT_CONFIG_NOSYSTEM" => None,
                        other => env(other),
                    };
                    Source::System
                        .storage_location(&mut without_nosystem)
                        .ok_or_else(|| anyhow::anyhow!("the system config is unavailable"))?
                }
            };
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
/// `--unset`/`--unset-all`, with the optional `<value-pattern>` that narrows which values
/// are removed.
///
/// The counting is git's: nothing matched is `CONFIG_NOTHING_SET` (exit 5), and more than
/// one match without `--unset-all` warns and changes nothing (`store_aux()`, config.c).
fn unset_scoped(
    target: &WriteTarget,
    name: &str,
    value_pattern: Option<&str>,
    all: bool,
    fixed_value: bool,
) -> Result<ExitCode> {
    let key = parse_key_write(name)?;
    let filter = match value_pattern.map(|p| ValueFilter::parse(p, fixed_value)) {
        Some(Err(code)) => return Ok(code),
        Some(Ok(f)) => Some(f),
        None => None,
    };

    let _lock = crate::lock::RepoLock::acquire(&target.lock_key);
    prepare_parent(target);
    let path = &target.path;
    let Some(mut file) = load_for_write(path, target.source)? else {
        return Ok(ExitCode::from(3));
    };

    {
        let Ok(mut section) = file.section_mut(key.section_name, key.subsection_name) else {
            return Ok(ExitCode::from(5));
        };
        let values = section.values(key.value_name);
        let matched: Vec<usize> = values
            .iter()
            .enumerate()
            .filter(|(_, v)| filter.as_ref().is_none_or(|f| f.matches(v)))
            .map(|(i, _)| i)
            .collect();
        if matched.is_empty() {
            return Ok(ExitCode::from(5));
        }
        if !all && matched.len() > 1 {
            eprintln!("warning: {name} has multiple values");
            return Ok(ExitCode::from(5));
        }
        // `SectionMut` removes by name, last occurrence first, so the surviving values are
        // put back in order rather than removed one by one.
        let keep: Vec<gix::bstr::BString> = values
            .iter()
            .enumerate()
            .filter(|(i, _)| !matched.contains(i))
            .map(|(_, v)| v.clone())
            .collect();
        while section.remove(key.value_name).is_some() {}
        for value in &keep {
            section.push(key.value_name, value.as_slice())?;
        }
    }
    persist_or_lock_error(path, &file)
}

/// ```c
/// if (strchr(comment, '\n'))
///         die(_("no multi-line comment allowed: '%s'"), comment);
///
/// leading_blanks = strspn(comment, " \t");
/// if (leading_blanks && comment[leading_blanks] == '#')
///         prepared = xstrdup(comment); /* use it as-is */
/// else if (comment[0] == '#')
///         prepared = xstrfmt(" %s", comment);
/// else
///         prepared = xstrfmt(" # %s", comment);
/// ```
///
/// (`git_config_prepare_comment_string()`, config.c:2921-2952.) The result is the whole
/// trailer that follows the value, `#` included.
fn prepare_comment(comment: &str) -> Result<String> {
    if comment.contains('\n') {
        crate::git_fatal!("no multi-line comment allowed: '{comment}'");
    }
    let blanks = comment.len() - comment.trim_start_matches([' ', '\t']).len();
    let rest = &comment[blanks..];
    Ok(match (blanks > 0, rest.starts_with('#')) {
        (true, true) => comment.to_string(),
        (_, true) => format!(" {comment}"),
        _ => format!(" # {comment}"),
    })
}

fn write_scoped(
    target: &WriteTarget,
    name: &str,
    value: &str,
    op: WriteOp,
    comment: Option<&str>,
) -> Result<ExitCode> {
    let key = parse_key_write(name)?;
    let section_lc = key.section_name.to_lowercase();
    let value_lc = key.value_name.to_lowercase();

    let _lock = crate::lock::RepoLock::acquire(&target.lock_key);

    prepare_parent(target);
    let path = &target.path;
    let Some(mut file) = load_for_write(path, target.source)? else {
        return Ok(ExitCode::from(3));
    };

    let comment = comment.map(prepare_comment).transpose()?;
    // `git_config_set_in_file_gently()` is `git_config_set_multivar_in_file_gently(key,
    // value, NULL, 0)`, and a NULL value-pattern makes `matches()` answer yes for
    // every existing value of the key. So a key that already carries more than one
    // value is not collapsed into one:
    //
    // ```c
    // if (store.seen_nr > 1 && !store.multi_replace) {
    //         error(_("cannot overwrite multiple values with a single value\n"
    //                 "       Use a regexp, --add or --replace-all to change %s."), key);
    //         ret = CONFIG_NOTHING_SET;
    //         goto out_free;
    // }
    // ```
    //
    // preceded by `store_aux()`'s `warning(_("%s has multiple values"), key)` on
    // the second match. Nothing is written and the exit code is 5.
    if matches!(op, WriteOp::Set) {
        let existing = file
            .section(&section_lc, key.subsection_name)
            .map(|section| section.values(&value_lc).len())
            .unwrap_or(0);
        if existing > 1 {
            eprintln!("warning: {name} has multiple values");
            eprintln!(
                "error: cannot overwrite multiple values with a single value\n       \
                 Use a regexp, --add or --replace-all to change {name}."
            );
            return Ok(ExitCode::from(5));
        }
    }
    match op {
        // A comment can only be attached to a line as it is written, so a `set` that would
        // have rewritten an existing value in place pushes a new one instead — which is
        // what git does too, since `git_config_set_multivar_in_file()` writes the whole
        // line (value and comment) whenever a comment is given.
        WriteOp::Set if comment.is_some() => {
            let comment = comment.as_deref().expect("checked above");
            let mut section = file.section_mut_or_create_new(&section_lc, key.subsection_name)?;
            while section.remove(&value_lc).is_some() {}
            section.push_with_prepared_comment(&value_lc, value, comment.into())?;
        }
        WriteOp::Set => {
            file.set_raw_value_by(&section_lc, key.subsection_name, &value_lc, value)?;
        }
        WriteOp::Add => {
            let mut section = file.section_mut_or_create_new(&section_lc, key.subsection_name)?;
            match comment.as_deref() {
                Some(comment) => {
                    section.push_with_prepared_comment(&value_lc, value, comment.into())?;
                }
                None => {
                    section.push(&value_lc, value)?;
                }
            }
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
            // ```c
            // if (store->seen_nr == 1 && store->multi_replace == 0) {
            //         warning(_("%s has multiple values"), key);
            // }
            // ```
            //
            // (`store_aux()`, config.c:2673-2677.) `git config --unset` on a key with more
            // than one value is not fatal: it warns, changes nothing, and returns
            // `CONFIG_NOTHING_SET` — exit 5 (config.h:33).
            if matches!(op, WriteOp::Unset) && count > 1 {
                eprintln!("warning: {name} has multiple values");
                return Ok(ExitCode::from(5));
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
    let key = parse_key_write(name)?;
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

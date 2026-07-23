//! `git interpret-trailers` — add or parse trailer lines in a commit message.
//!
//! A faithful port of git 2.55's `builtin/interpret-trailers.c` together with the
//! whole of `trailer.c` that it drives, plus the two helpers `trailer.c` borrows:
//! `ignored_log_message_bytes()` from `commit.c` and `wt_status_locate_end()` from
//! `wt-status.c`. The command is pure text processing — no object database is
//! touched — so the only repository substrate it needs is the merged
//! configuration, read through `gix::config`.
//!
//! The port keeps git's exact structure: `find_end_of_log_message` →
//! `find_trailer_block_start` → line folding → `parse_trailer` → the
//! arg/if-exists/if-missing state machine → `format_trailers`. The C linked-list
//! surgery (`list_add`, `list_add_tail`, `list_del` around a sentinel head) is
//! reproduced with index arithmetic on a `Vec`, insertion-for-insertion.
//!
//! ### Covered (byte-identical stdout, stderr and exit code against stock git)
//!
//! * reading from stdin or from one or more `<file>` arguments, each processed and
//!   emitted independently; input is completed with a final `\n` when it lacks one
//!   (`strbuf_complete_line`, which git 2.55 does and 2.39 did not)
//! * `--trailer <key>[(=|:)<value>]` (repeatable, `=` always accepted on the
//!   command line in addition to the configured separators) and `--no-trailer`,
//!   which resets the list
//! * `--where`/`--no-where`, `--if-exists`/`--no-if-exists`,
//!   `--if-missing`/`--no-if-missing` — positional, applying to every following
//!   `--trailer` until the next occurrence, exactly as git's static
//!   `where`/`if_exists`/`if_missing` globals do
//! * `--in-place`/`--no-in-place` (write through a `git-interpret-trailers-XXXXXX`
//!   temporary in the file's own directory, carrying the original mode, then
//!   rename), `--trim-empty`, `--only-trailers`, `--only-input`, `--unfold`,
//!   `--parse`, `--no-divider`/`--divider`, `--`, and `-h`
//! * `trailer.separators`, `trailer.where`, `trailer.ifexists`, `trailer.ifmissing`
//!   and the per-alias `trailer.<key-alias>.{key,where,ifexists,ifmissing}`,
//!   loaded in git's two passes so that per-alias items inherit the final defaults
//! * `core.commentChar` / `core.commentString` (`auto` and absent both resolve to
//!   `#`), which decide which lines are comments for the trailer-block scan
//! * git's warnings on stderr — `warning: unknown value '<v>' for key '<k>'` and
//!   `warning: more than one <key>` — and `error: empty trailer token in trailer
//!   '<t>'`, which skips that argument and still exits 0
//! * the fatal shapes: `fatal: no input file given for in-place editing` (128),
//!   `fatal: could not read input file '<f>': <strerror>` (128),
//!   `error: file <f> is not a regular file` / `is not writable by user` /
//!   `error: could not stat <f>: <strerror>` followed by a silent `die(NULL)` (128)
//! * the usage shapes: `-h` prints git's block on stdout (129); `` error: unknown
//!   option `x' ``, `` error: unknown switch `x' `` and `error: ambiguous option:
//!   …` print it on stderr after the message (129); `` error: option `x' takes no
//!   value `` and `` error: option `x' requires a value `` print no usage block
//!   (129); an unrecognised `--where`/`--if-exists`/`--if-missing` value exits 129
//!   silently, as git's `parse_options` callback failure does; and
//!   `fatal: --trailer with --only-input does not make sense` prints a blank line
//!   and the block (129)
//!
//! ### Not covered — these `bail!` rather than produce diverging output
//!
//! * `trailer.<key-alias>.cmd` and `trailer.<key-alias>.command`. Both require
//!   running a shell command per trailer and folding its stdout back into the
//!   value; the vendored crates have no `run-command.c` equivalent with git's
//!   `local_repo_env` scrubbing, and guessing at it would silently change values.
//!   Detected and refused only when the configuration is actually consulted, so
//!   `--only-input` and `--parse` keep working with such config present.
//! * `--help`, which stock git turns into `git help interpret-trailers` (a pager
//!   or man page), not something this process can reproduce.
//!
//! ### Known deviation
//!
//! Long-option abbreviation resolves against the canonical spellings and their
//! `no-` negations, and reports `error: ambiguous option:` in git's wording when
//! more than one matches. git's `parse_long_opt` additionally treats any prefix of
//! the literal string `no-` as an abbreviated negation of every negatable option,
//! so a lone `--n` names a different candidate pair there than here. Every
//! abbreviation that identifies a real option agrees.

use anyhow::{bail, Result};
use std::io::{Read, Write};
use std::process::ExitCode;

use gix::config::File as ConfigFile;

/// Stock git's `interpret-trailers` usage block, byte-for-byte, including the
/// trailing blank line. Printed on `-h` (stdout) and after most `error:` lines
/// (stderr).
const USAGE: &str = concat!(
    "usage: git interpret-trailers [--in-place] [--trim-empty]\n",
    "                              [(--trailer (<key>|<key-alias>)[(=|:)<value>])...]\n",
    "                              [--parse] [<file>...]\n",
    "\n",
    "    --[no-]in-place       edit files in place\n",
    "    --[no-]trim-empty     trim empty trailers\n",
    "    --[no-]where <placement>\n",
    "                          where to place the new trailer\n",
    "    --[no-]if-exists <action>\n",
    "                          action if trailer already exists\n",
    "    --[no-]if-missing <action>\n",
    "                          action if trailer is missing\n",
    "    --[no-]only-trailers  output only the trailers\n",
    "    --[no-]only-input     do not apply trailer.<key-alias> configuration variables\n",
    "    --[no-]unfold         reformat multiline trailer values as single-line values\n",
    "    --parse               alias for --only-trailers --only-input --unfold\n",
    "    --no-divider          do not treat \"---\" as the end of input\n",
    "    --divider             opposite of --no-divider\n",
    "    --[no-]trailer <trailer>\n",
    "                          trailer(s) to add\n",
    "\n",
);

/// git's exit code for a `die()`.
const FATAL: u8 = 128;
/// git's exit code for every `parse_options` failure and for `-h`.
const USAGE_CODE: u8 = 129;

/// The scissors line `wt_status_locate_end` looks for, after the comment string.
const CUT_LINE: &[u8] = b"------------------------ >8 ------------------------\n";

/// Line prefixes that make a block "recognized" even when it is mostly prose.
const GIT_GENERATED_PREFIXES: [&[u8]; 2] = [b"Signed-off-by: ", b"(cherry picked from commit "];

// ---------------------------------------------------------------------------
// Configuration model (trailer.c's `conf_info` / `default_conf_info`)
// ---------------------------------------------------------------------------

/// `enum trailer_where`. `Default` is git's `WHERE_DEFAULT` sentinel, which means
/// "not specified"; it is never `after_or_end`, so it behaves like `Start`.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Where {
    Default,
    After,
    Before,
    End,
    Start,
}

/// `enum trailer_if_exists`.
#[derive(Clone, Copy, PartialEq, Eq)]
enum IfExists {
    Default,
    AddIfDifferent,
    AddIfDifferentNeighbor,
    Add,
    Replace,
    DoNothing,
}

/// `enum trailer_if_missing`.
#[derive(Clone, Copy, PartialEq, Eq)]
enum IfMissing {
    Default,
    DoNothing,
    Add,
}

/// `trailer_set_where`.
fn set_where(slot: &mut Where, value: Option<&[u8]>) -> bool {
    let Some(v) = value else {
        *slot = Where::Default;
        return true;
    };
    *slot = if eq_ignore_case(v, b"after") {
        Where::After
    } else if eq_ignore_case(v, b"before") {
        Where::Before
    } else if eq_ignore_case(v, b"end") {
        Where::End
    } else if eq_ignore_case(v, b"start") {
        Where::Start
    } else {
        return false;
    };
    true
}

/// `trailer_set_if_exists`.
fn set_if_exists(slot: &mut IfExists, value: Option<&[u8]>) -> bool {
    let Some(v) = value else {
        *slot = IfExists::Default;
        return true;
    };
    *slot = if eq_ignore_case(v, b"addIfDifferent") {
        IfExists::AddIfDifferent
    } else if eq_ignore_case(v, b"addIfDifferentNeighbor") {
        IfExists::AddIfDifferentNeighbor
    } else if eq_ignore_case(v, b"add") {
        IfExists::Add
    } else if eq_ignore_case(v, b"replace") {
        IfExists::Replace
    } else if eq_ignore_case(v, b"doNothing") {
        IfExists::DoNothing
    } else {
        return false;
    };
    true
}

/// `trailer_set_if_missing`.
fn set_if_missing(slot: &mut IfMissing, value: Option<&[u8]>) -> bool {
    let Some(v) = value else {
        *slot = IfMissing::Default;
        return true;
    };
    *slot = if eq_ignore_case(v, b"doNothing") {
        IfMissing::DoNothing
    } else if eq_ignore_case(v, b"add") {
        IfMissing::Add
    } else {
        return false;
    };
    true
}

/// `after_or_end()`: the two placements that grow the block downwards.
fn after_or_end(w: Where) -> bool {
    w == Where::After || w == Where::End
}

/// `struct conf_info` — one `trailer.<key-alias>` entry, or the defaults.
#[derive(Clone)]
struct ConfInfo {
    /// The `<key-alias>` as written in configuration; empty for the defaults.
    name: Vec<u8>,
    key: Option<Vec<u8>>,
    command: Option<Vec<u8>>,
    cmd: Option<Vec<u8>>,
    where_: Where,
    if_exists: IfExists,
    if_missing: IfMissing,
}

impl ConfInfo {
    /// `token_from_item()`: the spelling a trailer built from this item gets.
    fn token_from_item(&self, tok: Option<Vec<u8>>) -> Vec<u8> {
        match (&self.key, tok) {
            (Some(k), _) => k.clone(),
            (None, Some(t)) => t,
            (None, None) => self.name.clone(),
        }
    }
}

/// Everything `trailer_config_init()` establishes, plus the comment string that
/// `git_default_config()` sets.
struct TrailerConfig {
    separators: Vec<u8>,
    comment: Vec<u8>,
    defaults: ConfInfo,
    /// `conf_head`, in the order the keys were seen in the merged configuration.
    items: Vec<ConfInfo>,
}

impl TrailerConfig {
    /// `token_matches_item()` scanned over `conf_head`, returning the first hit.
    fn lookup(&self, tok: &[u8], tok_len: usize) -> Option<usize> {
        self.items
            .iter()
            .position(|item| token_matches_item(tok, item, tok_len))
    }
}

/// `token_matches_item()`: `tok`'s first `tok_len` bytes, compared case-insensitively
/// against the alias name and then against its configured key.
fn token_matches_item(tok: &[u8], item: &ConfInfo, tok_len: usize) -> bool {
    if strncasecmp_eq(tok, &item.name, tok_len) {
        return true;
    }
    item.key
        .as_deref()
        .is_some_and(|k| strncasecmp_eq(tok, k, tok_len))
}

// ---------------------------------------------------------------------------
// Configuration loading
// ---------------------------------------------------------------------------

/// Read the merged configuration and distil git's two trailer passes out of it.
///
/// Pass one (`git_trailer_default_config`) handles the dotless `trailer.*` keys —
/// `separators`, `where`, `ifexists`, `ifmissing` — and must run to completion
/// first, because pass two (`git_trailer_config`) seeds every `trailer.<alias>`
/// item from the defaults it leaves behind.
fn load_config() -> Result<TrailerConfig> {
    // `setup_git_directory_gently()`: a repository is preferred, but the command
    // is legal outside one, where git reads the global set plus `GIT_CONFIG_*`.
    let config = match gix::discover(".") {
        Ok(repo) => repo.config_snapshot().plumbing().clone(),
        Err(_) => {
            let mut file = ConfigFile::from_globals()?;
            file.append(ConfigFile::from_environment_overrides()?)?;
            file
        }
    };

    let mut cfg = TrailerConfig {
        separators: b":".to_vec(),
        comment: comment_string(&config),
        defaults: ConfInfo {
            name: Vec::new(),
            key: None,
            command: None,
            cmd: None,
            where_: Where::End,
            if_exists: IfExists::AddIfDifferentNeighbor,
            if_missing: IfMissing::Add,
        },
        items: Vec::new(),
    };

    // Pass one: the dotless keys.
    for section in config.sections() {
        let header = section.header();
        if header.subsection_name().is_some()
            || !header.name().to_string().eq_ignore_ascii_case("trailer")
        {
            continue;
        }
        for (name, value) in ordered_values(&section) {
            let ok = match name.as_str() {
                "where" => set_where(&mut cfg.defaults.where_, Some(value.as_slice())),
                "ifexists" => set_if_exists(&mut cfg.defaults.if_exists, Some(value.as_slice())),
                "ifmissing" => set_if_missing(&mut cfg.defaults.if_missing, Some(value.as_slice())),
                "separators" => {
                    cfg.separators = value.clone();
                    true
                }
                _ => continue,
            };
            if !ok {
                warn_unknown_value(&value, &format!("trailer.{name}"));
            }
        }
    }

    // Pass two: the per-alias keys.
    for section in config.sections() {
        let header = section.header();
        if !header.name().to_string().eq_ignore_ascii_case("trailer") {
            continue;
        }
        let Some(alias) = header.subsection_name() else {
            continue;
        };
        let alias = alias.to_string().into_bytes();

        for (name, value) in ordered_values(&section) {
            if !matches!(
                name.as_str(),
                "key" | "command" | "cmd" | "where" | "ifexists" | "ifmissing"
            ) {
                continue;
            }
            let idx = get_conf_item(&mut cfg, &alias);
            let conf_key = format!("trailer.{}.{name}", String::from_utf8_lossy(&alias));
            let item = &mut cfg.items[idx];

            let ok = match name.as_str() {
                "key" => set_once(&mut item.key, value.clone(), &conf_key),
                "command" => set_once(&mut item.command, value.clone(), &conf_key),
                "cmd" => set_once(&mut item.cmd, value.clone(), &conf_key),
                "where" => set_where(&mut item.where_, Some(value.as_slice())),
                "ifexists" => set_if_exists(&mut item.if_exists, Some(value.as_slice())),
                "ifmissing" => set_if_missing(&mut item.if_missing, Some(value.as_slice())),
                _ => unreachable!("filtered above"),
            };
            if !ok {
                warn_unknown_value(&value, &conf_key);
            }
        }
    }

    Ok(cfg)
}

/// `get_conf_item()`: find the alias case-insensitively, or append a fresh item
/// seeded from the defaults.
fn get_conf_item(cfg: &mut TrailerConfig, name: &[u8]) -> usize {
    if let Some(i) = cfg
        .items
        .iter()
        .position(|item| item.name.eq_ignore_ascii_case(name))
    {
        return i;
    }
    let mut item = cfg.defaults.clone();
    item.name = name.to_vec();
    cfg.items.push(item);
    cfg.items.len() - 1
}

/// Assign a string-valued conf field, warning `more than one <key>` when it was
/// already set — git keeps the later value in both cases.
fn set_once(slot: &mut Option<Vec<u8>>, value: Vec<u8>, conf_key: &str) -> bool {
    if slot.is_some() {
        eprintln!("warning: more than one {conf_key}");
    }
    *slot = Some(value);
    true
}

/// `warning(_("unknown value '%s' for key '%s'"))`.
fn warn_unknown_value(value: &[u8], conf_key: &str) {
    let mut err = std::io::stderr().lock();
    let _ = err.write_all(b"warning: unknown value '");
    let _ = err.write_all(value);
    let _ = err.write_all(format!("' for key '{conf_key}'\n").as_bytes());
}

/// The `(lowercased name, value)` pairs of one section, in file order.
///
/// `value_names()` yields names in order and `values(name)` yields that name's
/// values in order, so advancing a per-name cursor while walking the names
/// interleaves them back into the order they were written. Valueless entries have
/// no value to hand back and are skipped, exactly as git's config callback never
/// sees a value for them.
fn ordered_values(section: &gix::config::file::SectionRef<'_>) -> Vec<(String, Vec<u8>)> {
    let body = section.body();
    let mut cursors: Vec<(String, usize)> = Vec::new();
    let mut out = Vec::new();

    for name in body.value_names() {
        let lower = name.to_ascii_lowercase();
        let values = body.values(&lower);
        let at = match cursors.iter().position(|(n, _)| *n == lower) {
            Some(at) => at,
            None => {
                cursors.push((lower.clone(), 0));
                cursors.len() - 1
            }
        };
        if let Some(v) = values.get(cursors[at].1) {
            out.push((lower, v.to_vec()));
        }
        cursors[at].1 += 1;
    }
    out
}

/// `core.commentChar` / `core.commentString`, which are the same knob in git 2.55.
/// The last one set across both spellings wins; `auto` and absence give `#`.
fn comment_string(config: &ConfigFile) -> Vec<u8> {
    let mut chosen = b"#".to_vec();
    for section in config.sections() {
        let header = section.header();
        if header.subsection_name().is_some()
            || !header.name().to_string().eq_ignore_ascii_case("core")
        {
            continue;
        }
        for (name, value) in ordered_values(&section) {
            if name != "commentchar" && name != "commentstring" {
                continue;
            }
            chosen = if value.eq_ignore_ascii_case(b"auto") || value.is_empty() {
                b"#".to_vec()
            } else {
                value
            };
        }
    }
    chosen
}

// ---------------------------------------------------------------------------
// Option parsing
// ---------------------------------------------------------------------------

/// One `--trailer` argument, carrying the `--where` / `--if-exists` /
/// `--if-missing` state that was in force when it was seen.
struct NewTrailer {
    text: Vec<u8>,
    where_: Where,
    if_exists: IfExists,
    if_missing: IfMissing,
}

/// `struct process_trailer_options`, restricted to what the builtin exposes.
struct Opts {
    in_place: bool,
    trim_empty: bool,
    only_trailers: bool,
    only_input: bool,
    unfold: bool,
    no_divider: bool,
}

/// Every long option the builtin declares, with how `parse_options` treats it.
///
/// `takes_value` distinguishes `OPT_CALLBACK` from `OPT_BOOL`; `negatable` is
/// false only for `--parse` (`PARSE_OPT_NONEG`). `no-divider` is spelled with the
/// negation baked into the name, so its "negated" form is `--divider`.
const LONG_OPTS: [(&str, bool, bool); 11] = [
    ("in-place", false, true),
    ("trim-empty", false, true),
    ("where", true, true),
    ("if-exists", true, true),
    ("if-missing", true, true),
    ("only-trailers", false, true),
    ("only-input", false, true),
    ("unfold", false, true),
    ("parse", false, false),
    ("no-divider", false, true),
    ("trailer", true, true),
];

/// A resolved long option: which entry, and whether the spelling was the negation.
struct Match {
    index: usize,
    unset: bool,
}

/// The result of parsing the command line.
enum Parsed {
    Run(Box<Run>),
    Exit(ExitCode),
}

/// A fully parsed invocation.
struct Run {
    opts: Opts,
    trailers: Vec<NewTrailer>,
    files: Vec<String>,
}

/// `git interpret-trailers` — apply or extract commit-message trailers.
///
/// The whole command is text in, text out: configuration is the only repository
/// input, and nothing but the `--in-place` target files is written.
pub fn interpret_trailers(args: &[String]) -> Result<ExitCode> {
    // Dispatch passes the subcommand itself at index 0.
    let args = match args.first() {
        Some(a) if a == "interpret-trailers" => &args[1..],
        _ => args,
    };

    let run = match parse_args(args)? {
        Parsed::Exit(code) => return Ok(code),
        Parsed::Run(run) => *run,
    };

    let cfg = load_config()?;

    // The value-producing hooks need a subprocess; refuse rather than drop them.
    if !run.opts.only_input
        && cfg
            .items
            .iter()
            .any(|i| i.command.is_some() || i.cmd.is_some())
    {
        bail!("unsupported config trailer.<key-alias>.cmd/.command (needs shell execution)");
    }

    if run.files.is_empty() {
        let mut input = Vec::new();
        if let Err(e) = std::io::stdin().lock().read_to_end(&mut input) {
            return Ok(fatal(&format!("could not read from stdin: {}", errno(&e))));
        }
        complete_line(&mut input);
        let out = process(&input, &run.opts, &run.trailers, &cfg);
        let mut stdout = std::io::stdout().lock();
        stdout.write_all(&out)?;
        stdout.flush()?;
        return Ok(ExitCode::SUCCESS);
    }

    for file in &run.files {
        let mut input = match std::fs::read(file) {
            Ok(bytes) => bytes,
            Err(e) => {
                return Ok(fatal(&format!(
                    "could not read input file '{file}': {}",
                    errno(&e)
                )))
            }
        };
        complete_line(&mut input);

        // git creates the temporary before processing, so a non-writable target
        // fails before any output is produced.
        let temp = if run.opts.in_place {
            match prepare_temp(file) {
                Ok(t) => Some(t),
                Err(code) => return Ok(code),
            }
        } else {
            None
        };

        let out = process(&input, &run.opts, &run.trailers, &cfg);

        match temp {
            Some((path, mode)) => {
                if let Err(e) = write_temp(&path, &out, mode) {
                    let _ = std::fs::remove_file(&path);
                    return Ok(fatal(&format!(
                        "could not write to temporary file '{file}': {}",
                        errno(&e)
                    )));
                }
                if let Err(e) = std::fs::rename(&path, file) {
                    let _ = std::fs::remove_file(&path);
                    return Ok(fatal(&format!(
                        "could not rename temporary file to {file}: {}",
                        errno(&e)
                    )));
                }
            }
            None => {
                let mut stdout = std::io::stdout().lock();
                stdout.write_all(&out)?;
                stdout.flush()?;
            }
        }
    }

    Ok(ExitCode::SUCCESS)
}

/// `parse_options` for this builtin's table, plus the two post-parse checks the
/// builtin performs before it touches any input.
fn parse_args(args: &[String]) -> Result<Parsed> {
    let mut opts = Opts {
        in_place: false,
        trim_empty: false,
        only_trailers: false,
        only_input: false,
        unfold: false,
        no_divider: false,
    };
    let mut trailers: Vec<NewTrailer> = Vec::new();
    let mut files: Vec<String> = Vec::new();

    // git keeps these in file-scope statics, so each one applies to every later
    // `--trailer` until it is changed again.
    let mut where_ = Where::Default;
    let mut if_exists = IfExists::Default;
    let mut if_missing = IfMissing::Default;

    let mut i = 0;
    let mut no_more_opts = false;

    while i < args.len() {
        let arg = args[i].as_str();
        i += 1;

        if no_more_opts || !arg.starts_with('-') || arg == "-" {
            files.push(arg.to_string());
            continue;
        }
        if arg == "--" {
            no_more_opts = true;
            continue;
        }

        // Short options: the table declares none, so only the built-in `-h`.
        let Some(long) = arg.strip_prefix("--") else {
            let c = arg[1..].chars().next().expect("`-` was handled as a file");
            if c == 'h' {
                print!("{USAGE}");
                return Ok(Parsed::Exit(ExitCode::from(USAGE_CODE)));
            }
            return Ok(Parsed::Exit(usage_error(&format!("unknown switch `{c}'"))));
        };

        if long == "help" {
            bail!("--help is not supported (stock git delegates it to `git help`)");
        }

        let (name, inline) = match long.split_once('=') {
            Some((n, v)) => (n, Some(v)),
            None => (long, None),
        };

        let m = match resolve_long(name) {
            Ok(m) => m,
            Err(code) => return Ok(Parsed::Exit(code)),
        };
        let (opt_name, takes_value, _) = LONG_OPTS[m.index];

        // Fetch the value for a value-taking option: `--opt=v` or `--opt v`.
        // A negated spelling (`--no-trailer`) never takes one.
        let value: Option<&str> = if takes_value && !m.unset {
            match inline {
                Some(v) => Some(v),
                None => match args.get(i) {
                    Some(v) => {
                        i += 1;
                        Some(v.as_str())
                    }
                    None => {
                        return Ok(Parsed::Exit(plain_error(&format!(
                            "option `{opt_name}' requires a value"
                        ))))
                    }
                },
            }
        } else {
            if inline.is_some() {
                return Ok(Parsed::Exit(plain_error(&format!(
                    "option `{opt_name}' takes no value"
                ))));
            }
            None
        };

        let set = !m.unset;
        match opt_name {
            "in-place" => opts.in_place = set,
            "trim-empty" => opts.trim_empty = set,
            "only-trailers" => opts.only_trailers = set,
            "only-input" => opts.only_input = set,
            "unfold" => opts.unfold = set,
            "no-divider" => opts.no_divider = set,
            "parse" => {
                opts.only_trailers = true;
                opts.only_input = true;
                opts.unfold = true;
            }
            "where" => {
                if !set_where(&mut where_, value.map(str::as_bytes)) {
                    return Ok(Parsed::Exit(ExitCode::from(USAGE_CODE)));
                }
            }
            "if-exists" => {
                if !set_if_exists(&mut if_exists, value.map(str::as_bytes)) {
                    return Ok(Parsed::Exit(ExitCode::from(USAGE_CODE)));
                }
            }
            "if-missing" => {
                if !set_if_missing(&mut if_missing, value.map(str::as_bytes)) {
                    return Ok(Parsed::Exit(ExitCode::from(USAGE_CODE)));
                }
            }
            "trailer" => match value {
                Some(text) => trailers.push(NewTrailer {
                    text: text.as_bytes().to_vec(),
                    where_,
                    if_exists,
                    if_missing,
                }),
                None => trailers.clear(), // `--no-trailer` resets the list
            },
            _ => unreachable!("resolve_long only returns table entries"),
        }
    }

    if opts.only_input && !trailers.is_empty() {
        eprint!("fatal: --trailer with --only-input does not make sense\n\n{USAGE}");
        return Ok(Parsed::Exit(ExitCode::from(USAGE_CODE)));
    }
    if files.is_empty() && opts.in_place {
        return Ok(Parsed::Exit(fatal("no input file given for in-place editing")));
    }

    Ok(Parsed::Run(Box::new(Run {
        opts,
        trailers,
        files,
    })))
}

/// Resolve a long-option spelling to a table entry, honouring negation and
/// unambiguous abbreviation.
///
/// Candidates are the canonical names and, for negatable options, their negated
/// spellings — `no-<name>`, or the `no-` stripped form when the name already
/// carries it (`no-divider` negates to `divider`).
fn resolve_long(name: &str) -> Result<Match, ExitCode> {
    let mut spellings: Vec<(String, Match)> = Vec::new();
    for (index, &(opt, _, negatable)) in LONG_OPTS.iter().enumerate() {
        spellings.push((
            opt.to_string(),
            Match {
                index,
                unset: false,
            },
        ));
        if !negatable {
            continue;
        }
        let negated = match opt.strip_prefix("no-") {
            Some(rest) => rest.to_string(),
            None => format!("no-{opt}"),
        };
        spellings.push((negated, Match { index, unset: true }));
    }

    if let Some((_, m)) = spellings.iter().find(|(s, _)| s.as_str() == name) {
        return Ok(Match {
            index: m.index,
            unset: m.unset,
        });
    }

    let hits: Vec<&(String, Match)> = spellings
        .iter()
        .filter(|(s, _)| s.starts_with(name))
        .collect();
    match hits.as_slice() {
        [] => Err(usage_error(&format!("unknown option `{name}'"))),
        [(_, m)] => Ok(Match {
            index: m.index,
            unset: m.unset,
        }),
        // Candidate spellings are stored already-negated, so they print as given.
        [(a, _), (b, _), ..] => Err(usage_error(&format!(
            "ambiguous option: {name} (could be --{a} or --{b})"
        ))),
    }
}

// ---------------------------------------------------------------------------
// The trailer block: locating it, folding it, parsing it
// ---------------------------------------------------------------------------

/// One entry of trailer.c's `head` list. `token` is `None` for a line inside the
/// block that is not a trailer at all.
struct Item {
    token: Option<Vec<u8>>,
    value: Vec<u8>,
}

/// `struct trailer_block`: where the block sits and what it holds.
struct Block {
    start: usize,
    end: usize,
    blank_line_before: bool,
    /// The block's lines after RFC-822 folding, each keeping its trailing `\n`.
    lines: Vec<Vec<u8>>,
}

/// `process_trailers()`: everything between reading the input and writing it out.
fn process(input: &[u8], opts: &Opts, new_trailers: &[NewTrailer], cfg: &TrailerConfig) -> Vec<u8> {
    let block = block_get(input, opts.no_divider, cfg);
    let mut head = parse_block(&block, opts, cfg);
    let mut out = Vec::with_capacity(input.len() + 64);

    if !opts.only_trailers {
        out.extend_from_slice(&input[..block.start]);
    }
    if !opts.only_trailers && !block.blank_line_before {
        out.push(b'\n');
    }

    if !opts.only_input {
        // `list_splice(&config_head, &arg_head)` puts the configured, command-driven
        // trailers ahead of the command-line ones. Those are refused up front, so
        // only the command-line arguments remain here.
        let args = parse_command_line_args(new_trailers, cfg);
        process_lists(&mut head, args);
    }

    format_trailers(&mut out, &head, opts, cfg);

    if !opts.only_trailers {
        out.extend_from_slice(&input[block.end..]);
    }
    out
}

/// `trailer_block_get()`: locate the block and fold its continuation lines.
fn block_get(input: &[u8], no_divider: bool, cfg: &TrailerConfig) -> Block {
    let cend = c_len(input);
    let end = find_end_of_log_message(input, cend, no_divider, cfg);
    // `end` is always line-aligned, so the start can never overshoot it; the clamp
    // only guards against the size_t underflow the C would suffer if it did.
    let start = find_trailer_block_start(input, end, cend, cfg).min(end);

    let mut lines: Vec<Vec<u8>> = Vec::new();
    let mut last: Option<usize> = None;
    for line in split_keep_lf(&input[start..end]) {
        // A leading blank continues the previous trailer, RFC-822 style.
        if let Some(idx) = last {
            if line.first().is_some_and(|&c| is_space(c)) {
                lines[idx].extend_from_slice(&line);
                continue;
            }
        }
        let is_trailer = find_separator(&line, &cfg.separators) >= 1;
        lines.push(line);
        last = if is_trailer { Some(lines.len() - 1) } else { None };
    }

    Block {
        start,
        end,
        blank_line_before: ends_with_blank_line(input, start, cend),
        lines,
    }
}

/// `parse_trailers()`: turn the block's folded lines into list items.
fn parse_block(block: &Block, opts: &Opts, cfg: &TrailerConfig) -> Vec<Item> {
    let mut head = Vec::new();
    for line in &block.lines {
        if line.starts_with(&cfg.comment) {
            continue;
        }
        let sep = find_separator(line, &cfg.separators);
        if sep >= 1 {
            let (tok, mut val, _) = parse_trailer(line, sep, cfg);
            if opts.unfold {
                unfold_value(&mut val);
            }
            head.push(Item {
                token: Some(tok),
                value: val,
            });
        } else if !opts.only_trailers {
            let mut val = line.clone();
            if val.last() == Some(&b'\n') {
                val.pop();
            }
            head.push(Item {
                token: None,
                value: val,
            });
        }
    }
    head
}

/// `parse_trailer()`: split at the separator, trim both halves, then rewrite the
/// token through the first matching `trailer.<key-alias>` entry.
///
/// Returns the token, the value, and the index of the configuration item that
/// claimed it (`None` for the defaults).
fn parse_trailer(trailer: &[u8], separator_pos: isize, cfg: &TrailerConfig) -> (Vec<u8>, Vec<u8>, Option<usize>) {
    let (mut tok, val) = if separator_pos >= 0 {
        let at = separator_pos as usize;
        (trim(&trailer[..at]), trim(&trailer[at + 1..]))
    } else {
        (trim(trailer), Vec::new())
    };

    let tok_len = token_len_without_separator(&tok);
    let found = cfg.lookup(&tok, tok_len);
    if let Some(idx) = found {
        tok = cfg.items[idx].token_from_item(Some(tok));
    }
    (tok, val, found)
}

/// `parse_trailers_from_command_line_args()`: each `--trailer` becomes an arg item,
/// with `=` accepted as a separator on top of the configured ones.
fn parse_command_line_args(new_trailers: &[NewTrailer], cfg: &TrailerConfig) -> Vec<ArgItem> {
    let mut cl_separators = vec![b'='];
    cl_separators.extend_from_slice(&cfg.separators);

    let mut args = Vec::new();
    for tr in new_trailers {
        let sep = find_separator(&tr.text, &cl_separators);
        if sep == 0 {
            let trimmed = trim(&tr.text);
            let mut err = std::io::stderr().lock();
            let _ = err.write_all(b"error: empty trailer token in trailer '");
            let _ = err.write_all(&trimmed);
            let _ = err.write_all(b"'\n");
            continue;
        }
        let (token, value, conf) = parse_trailer(&tr.text, sep, cfg);
        let base = conf.map_or(&cfg.defaults, |i| &cfg.items[i]);
        args.push(ArgItem {
            token,
            value,
            where_: if tr.where_ == Where::Default {
                base.where_
            } else {
                tr.where_
            },
            if_exists: if tr.if_exists == IfExists::Default {
                base.if_exists
            } else {
                tr.if_exists
            },
            if_missing: if tr.if_missing == IfMissing::Default {
                base.if_missing
            } else {
                tr.if_missing
            },
        });
    }
    args
}

/// `struct arg_item`, flattened: the conf fields that survive `add_arg_item`.
struct ArgItem {
    token: Vec<u8>,
    value: Vec<u8>,
    where_: Where,
    if_exists: IfExists,
    if_missing: IfMissing,
}

/// `process_trailers_lists()`: fold every argument into the input list in order.
fn process_lists(head: &mut Vec<Item>, args: Vec<ArgItem>) {
    for arg in args {
        if !find_same_and_apply_arg(head, &arg) {
            apply_arg_if_missing(head, arg);
        }
    }
}

/// `find_same_and_apply_arg()`: locate the first same-token entry, walking from
/// whichever end the placement implies, and apply the if-exists action there.
fn find_same_and_apply_arg(head: &mut Vec<Item>, arg: &ArgItem) -> bool {
    if head.is_empty() {
        return false;
    }
    let middle = arg.where_ == Where::After || arg.where_ == Where::Before;
    let backwards = after_or_end(arg.where_);
    let start_idx = if backwards { head.len() - 1 } else { 0 };

    let order: Vec<usize> = if backwards {
        (0..head.len()).rev().collect()
    } else {
        (0..head.len()).collect()
    };

    for idx in order {
        if !same_token(&head[idx], arg) {
            continue;
        }
        let on_idx = if middle { idx } else { start_idx };
        apply_arg_if_exists(head, idx, on_idx, arg);
        return true;
    }
    false
}

/// `apply_arg_if_exists()`.
fn apply_arg_if_exists(head: &mut Vec<Item>, in_idx: usize, on_idx: usize, arg: &ArgItem) {
    match arg.if_exists {
        IfExists::DoNothing => {}
        IfExists::Replace => {
            let ins = insert_at(on_idx, arg.where_);
            head.insert(ins, item_from_arg(arg));
            // The insert shifted `in_idx` when it landed at or before it.
            let del = if ins <= in_idx { in_idx + 1 } else { in_idx };
            head.remove(del);
        }
        IfExists::Add => {
            head.insert(insert_at(on_idx, arg.where_), item_from_arg(arg));
        }
        IfExists::AddIfDifferent => {
            if check_if_different(head, in_idx, arg, true) {
                head.insert(insert_at(on_idx, arg.where_), item_from_arg(arg));
            }
        }
        // `Default` never reaches here: an arg item always carries a concrete
        // action, inherited from the defaults when nothing overrode it.
        IfExists::AddIfDifferentNeighbor | IfExists::Default => {
            if check_if_different(head, on_idx, arg, false) {
                head.insert(insert_at(on_idx, arg.where_), item_from_arg(arg));
            }
        }
    }
}

/// `apply_arg_if_missing()`: no same-token entry exists, so place at one end.
fn apply_arg_if_missing(head: &mut Vec<Item>, arg: ArgItem) {
    match arg.if_missing {
        IfMissing::DoNothing => {}
        IfMissing::Add | IfMissing::Default => {
            let item = item_from_arg(&arg);
            if after_or_end(arg.where_) {
                head.push(item);
            } else {
                head.insert(0, item);
            }
        }
    }
}

/// `add_arg_to_input_list()`'s choice: after the anchor for `after`/`end`, before
/// it otherwise.
fn insert_at(on_idx: usize, where_: Where) -> usize {
    if after_or_end(where_) {
        on_idx + 1
    } else {
        on_idx
    }
}

/// `trailer_from_arg()`.
fn item_from_arg(arg: &ArgItem) -> Item {
    Item {
        token: Some(arg.token.clone()),
        value: arg.value.clone(),
    }
}

/// `check_if_different()`: true when no entry in the scanned direction repeats the
/// argument's whole `(token, value)` pair.
///
/// With `check_all` false only the starting entry is examined — git's `do {} while`
/// runs its body once, which is what makes `addIfDifferentNeighbor` a neighbour
/// test rather than a whole-block one.
fn check_if_different(head: &[Item], start: usize, arg: &ArgItem, check_all: bool) -> bool {
    let mut i = start as isize;
    loop {
        if same_token(&head[i as usize], arg) && same_value(&head[i as usize], arg) {
            return false;
        }
        let next = if after_or_end(arg.where_) { i - 1 } else { i + 1 };
        if next < 0 || next as usize >= head.len() {
            return true;
        }
        i = next;
        if !check_all {
            return true;
        }
    }
}

/// `same_token()`: compare over the shorter of the two punctuation-stripped tokens.
fn same_token(item: &Item, arg: &ArgItem) -> bool {
    let Some(a) = item.token.as_deref() else {
        return false;
    };
    let a_len = token_len_without_separator(a);
    let b_len = token_len_without_separator(&arg.token);
    strncasecmp_eq(a, &arg.token, a_len.min(b_len))
}

/// `same_value()`: a full case-insensitive comparison.
fn same_value(item: &Item, arg: &ArgItem) -> bool {
    eq_ignore_case(&item.value, &arg.value)
}

/// `format_trailers()` with the builtin's fixed options: no filter, no custom
/// separators, always newline-terminated.
fn format_trailers(out: &mut Vec<u8>, head: &[Item], opts: &Opts, cfg: &TrailerConfig) {
    for item in head {
        match &item.token {
            Some(tok) => {
                // `--trailer Reviewed-by` with no value yields an empty value.
                if opts.trim_empty && item.value.is_empty() {
                    continue;
                }
                out.extend_from_slice(tok);
                // A token that already ends in a separator keeps it; otherwise the
                // first configured separator plus one space is inserted.
                let c = last_non_space_char(tok);
                if c != 0 && !cfg.separators.contains(&c) {
                    out.push(*cfg.separators.first().unwrap_or(&0));
                    out.push(b' ');
                }
                out.extend_from_slice(&item.value);
                out.push(b'\n');
            }
            None => {
                if opts.only_trailers {
                    continue;
                }
                out.extend_from_slice(&item.value);
                out.push(b'\n');
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Block boundary detection (trailer.c / commit.c / wt-status.c)
// ---------------------------------------------------------------------------

/// `find_end_of_log_message()`: strip the `---` patch part, then the trailing
/// comments, blank lines and everything below a scissors line.
fn find_end_of_log_message(buf: &[u8], cend: usize, no_divider: bool, cfg: &TrailerConfig) -> usize {
    let mut end = cend;
    if !no_divider {
        let mut s = 0;
        while s < cend {
            if buf[s..cend].starts_with(b"---") {
                // `isspace(*v)` where `*v` may be the terminating NUL.
                let after = if s + 3 < cend { buf[s + 3] } else { 0 };
                if is_space(after) {
                    end = s;
                    break;
                }
            }
            s = next_line(buf, s, cend);
        }
    }
    end - ignored_log_message_bytes(buf, end, cend, cfg)
}

/// `ignored_log_message_bytes()`: the size of the trailing run of comment and
/// blank lines (and any old-style `Conflicts:` block), or of everything below the
/// scissors line.
fn ignored_log_message_bytes(buf: &[u8], len: usize, cend: usize, cfg: &TrailerConfig) -> usize {
    let cutoff = locate_end(buf, len, cend, cfg);
    // git uses 0 as "unset" here, so a run that starts at offset 0 reads as no run
    // at all. That quirk is part of the observable behaviour and is kept.
    let mut boc = 0usize;
    let mut in_old_conflicts_block = false;
    let mut bol = 0usize;

    while bol < cutoff {
        let next = match buf[bol..len].iter().position(|&b| b == b'\n') {
            Some(off) => bol + off + 1,
            None => len,
        };

        if buf[bol..cutoff].starts_with(&cfg.comment) || buf[bol] == b'\n' {
            if boc == 0 {
                boc = bol;
            }
        } else if buf[bol..cend].starts_with(b"Conflicts:\n") {
            in_old_conflicts_block = true;
            if boc == 0 {
                boc = bol;
            }
        } else if in_old_conflicts_block && buf[bol] == b'\t' {
            // a pathname in the conflicts block
        } else if boc != 0 {
            boc = 0;
            in_old_conflicts_block = false;
        }
        bol = next;
    }

    if boc != 0 {
        len - boc
    } else {
        len - cutoff
    }
}

/// `wt_status_locate_end()`: cut at the `<comment> ------------------------ >8 …`
/// scissors line, if the message has one.
fn locate_end(buf: &[u8], len: usize, cend: usize, cfg: &TrailerConfig) -> usize {
    let mut pattern = vec![b'\n'];
    pattern.extend_from_slice(&cfg.comment);
    pattern.push(b' ');
    pattern.extend_from_slice(CUT_LINE);

    let text = &buf[..cend];
    if text.starts_with(&pattern[1..]) {
        return 0;
    }
    if let Some(p) = find_sub(text, &pattern) {
        let newlen = p + 1;
        if newlen < len {
            return newlen;
        }
    }
    len
}

/// `find_trailer_block_start()`: scan upwards for a blank line before a run of
/// non-blank lines that is either all trailers, or at least a quarter trailers
/// with one git-generated line among them.
fn find_trailer_block_start(buf: &[u8], len: usize, cend: usize, cfg: &TrailerConfig) -> usize {
    // The first paragraph is the title and can never be trailers.
    let mut s = 0usize;
    while s < len {
        if buf[s..len].starts_with(&cfg.comment) {
            s = next_line(buf, s, cend);
            continue;
        }
        if is_blank_line(&buf[s..cend]) {
            break;
        }
        s = next_line(buf, s, cend);
    }
    let end_of_title = s as isize;

    let mut only_spaces = true;
    let mut recognized_prefix = false;
    let mut trailer_lines: i64 = 0;
    let mut non_trailer_lines: i64 = 0;
    let mut possible_continuation_lines: i64 = 0;

    let mut l = last_line(buf, len);
    while l >= end_of_title {
        let bol = l as usize;

        if buf[bol..len].starts_with(&cfg.comment) {
            non_trailer_lines += possible_continuation_lines;
            possible_continuation_lines = 0;
            l = last_line(buf, bol);
            continue;
        }
        if is_blank_line(&buf[bol..cend]) {
            if only_spaces {
                l = last_line(buf, bol);
                continue;
            }
            non_trailer_lines += possible_continuation_lines;
            if recognized_prefix && trailer_lines * 3 >= non_trailer_lines {
                return next_line(buf, bol, cend);
            }
            if trailer_lines > 0 && non_trailer_lines == 0 {
                return next_line(buf, bol, cend);
            }
            return len;
        }
        only_spaces = false;

        let line = &buf[bol..cend];
        if GIT_GENERATED_PREFIXES.iter().any(|p| line.starts_with(p)) {
            trailer_lines += 1;
            possible_continuation_lines = 0;
            recognized_prefix = true;
            l = last_line(buf, bol);
            continue;
        }

        let separator_pos = find_separator(line, &cfg.separators);
        if separator_pos >= 1 && !is_space(buf[bol]) {
            trailer_lines += 1;
            possible_continuation_lines = 0;
            if !recognized_prefix && cfg.lookup(line, separator_pos as usize).is_some() {
                recognized_prefix = true;
            }
        } else if is_space(buf[bol]) {
            possible_continuation_lines += 1;
        } else {
            non_trailer_lines += 1;
            non_trailer_lines += possible_continuation_lines;
            possible_continuation_lines = 0;
        }
        l = last_line(buf, bol);
    }

    len
}

/// `ends_with_blank_line()`.
fn ends_with_blank_line(buf: &[u8], len: usize, cend: usize) -> bool {
    let ll = last_line(buf, len);
    ll >= 0 && is_blank_line(&buf[ll as usize..cend])
}

// ---------------------------------------------------------------------------
// Small C-string helpers, ported with git's `sane_ctype` semantics
// ---------------------------------------------------------------------------

/// The length git's C code sees: everything up to the first NUL.
fn c_len(buf: &[u8]) -> usize {
    buf.iter().position(|&b| b == 0).unwrap_or(buf.len())
}

/// `strbuf_complete_line()`: a non-empty buffer always ends with a newline.
fn complete_line(buf: &mut Vec<u8>) {
    if !buf.is_empty() && buf.last() != Some(&b'\n') {
        buf.push(b'\n');
    }
}

/// git's `isspace`: the six ASCII space characters, never anything above 0x7f.
fn is_space(c: u8) -> bool {
    matches!(c, b' ' | b'\t' | b'\n' | 0x0b | 0x0c | b'\r')
}

/// `next_line()`: the offset just past the next newline, or the end of the string.
fn next_line(buf: &[u8], pos: usize, cend: usize) -> usize {
    match buf[pos..cend].iter().position(|&b| b == b'\n') {
        Some(off) => pos + off + 1,
        None => cend,
    }
}

/// `last_line()`: the start of the final line of `buf[..len]`, or -1 when empty.
fn last_line(buf: &[u8], len: usize) -> isize {
    if len == 0 {
        return -1;
    }
    if len == 1 {
        return 0;
    }
    // Skip the last character: a trailing newline belongs to the line before it.
    let mut i = len as isize - 2;
    while i >= 0 {
        if buf[i as usize] == b'\n' {
            return i + 1;
        }
        i -= 1;
    }
    0
}

/// `is_blank_line()`: nothing but spaces before the end of the line or the string.
fn is_blank_line(s: &[u8]) -> bool {
    for &c in s {
        if c == b'\n' {
            return true;
        }
        if !is_space(c) {
            return false;
        }
    }
    true
}

/// `find_separator()`: where the `<key><sep>` separator sits, or -1.
///
/// A key is alphanumerics and `-`, optionally followed by blanks; anything else
/// before a separator disqualifies the line.
fn find_separator(line: &[u8], separators: &[u8]) -> isize {
    let mut whitespace_found = false;
    for (i, &c) in line.iter().enumerate() {
        if separators.contains(&c) {
            return i as isize;
        }
        if !whitespace_found && (c.is_ascii_alphanumeric() || c == b'-') {
            continue;
        }
        if i != 0 && (c == b' ' || c == b'\t') {
            whitespace_found = true;
            continue;
        }
        break;
    }
    -1
}

/// `token_len_without_separator()`: the token minus its trailing punctuation.
fn token_len_without_separator(token: &[u8]) -> usize {
    let mut len = token.len();
    while len > 0 && !token[len - 1].is_ascii_alphanumeric() {
        len -= 1;
    }
    len
}

/// `last_non_space_char()`, with `0` standing in for git's `'\0'`.
fn last_non_space_char(s: &[u8]) -> u8 {
    s.iter().rev().copied().find(|&c| !is_space(c)).unwrap_or(0)
}

/// `strbuf_trim()`.
fn trim(s: &[u8]) -> Vec<u8> {
    let mut start = 0;
    let mut end = s.len();
    while start < end && is_space(s[start]) {
        start += 1;
    }
    while end > start && is_space(s[end - 1]) {
        end -= 1;
    }
    s[start..end].to_vec()
}

/// `unfold_value()`: collapse each folded continuation into a single space.
fn unfold_value(val: &mut Vec<u8>) {
    let src = std::mem::take(val);
    let mut out = Vec::with_capacity(src.len());
    let mut i = 0;
    while i < src.len() {
        let c = src[i];
        i += 1;
        if c == b'\n' {
            while i < src.len() && is_space(src[i]) {
                i += 1;
            }
            out.push(b' ');
        } else {
            out.push(c);
        }
    }
    // Empty lines may have left whitespace cruft at the edges.
    *val = trim(&out);
}

/// `strcasecmp(a, b) == 0` for ASCII.
fn eq_ignore_case(a: &[u8], b: &[u8]) -> bool {
    a.eq_ignore_ascii_case(b)
}

/// `strncasecmp(a, b, n) == 0`, with C's NUL semantics: a string that ends inside
/// the window only matches when the other ends there too.
fn strncasecmp_eq(a: &[u8], b: &[u8], n: usize) -> bool {
    for i in 0..n {
        let ca = a.get(i).copied().unwrap_or(0);
        let cb = b.get(i).copied().unwrap_or(0);
        if ca.to_ascii_lowercase() != cb.to_ascii_lowercase() {
            return false;
        }
        if ca == 0 {
            return true;
        }
    }
    true
}

/// `strstr()` over bytes.
fn find_sub(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || needle.len() > haystack.len() {
        return None;
    }
    (0..=haystack.len() - needle.len()).find(|&i| &haystack[i..i + needle.len()] == needle)
}

/// `strbuf_split_buf(…, '\n', 0)`: chunks that each keep their terminator, with an
/// unterminated tail allowed.
fn split_keep_lf(data: &[u8]) -> Vec<Vec<u8>> {
    let mut out = Vec::new();
    let mut start = 0;
    while start < data.len() {
        match data[start..].iter().position(|&b| b == b'\n') {
            Some(off) => {
                out.push(data[start..start + off + 1].to_vec());
                start += off + 1;
            }
            None => {
                out.push(data[start..].to_vec());
                break;
            }
        }
    }
    out
}

// ---------------------------------------------------------------------------
// In-place editing and error reporting
// ---------------------------------------------------------------------------

/// `trailer_create_in_place_tempfile()`: validate the target and pick a temporary
/// path beside it, returning the mode the temporary must carry.
///
/// On failure git reports with `error:` and then calls `die(NULL)`, which prints
/// nothing further and exits 128.
fn prepare_temp(file: &str) -> Result<(std::path::PathBuf, u32), ExitCode> {
    use std::os::unix::fs::PermissionsExt;

    let meta = match std::fs::metadata(file) {
        Ok(m) => m,
        Err(e) => {
            eprintln!("error: could not stat {file}: {}", errno(&e));
            return Err(ExitCode::from(FATAL));
        }
    };
    if !meta.is_file() {
        eprintln!("error: file {file} is not a regular file");
        return Err(ExitCode::from(FATAL));
    }
    let mode = meta.permissions().mode();
    if mode & 0o200 == 0 {
        eprintln!("error: file {file} is not writable by user");
        return Err(ExitCode::from(FATAL));
    }

    let path = std::path::Path::new(file);
    let dir = path.parent().filter(|p| !p.as_os_str().is_empty());
    let name = format!("git-interpret-trailers-{:06x}", std::process::id() & 0xff_ffff);
    let temp = match dir {
        Some(d) => d.join(name),
        None => std::path::PathBuf::from(name),
    };
    Ok((temp, mode))
}

/// Write the finished text to the temporary, carrying the original file's mode.
fn write_temp(path: &std::path::Path, data: &[u8], mode: u32) -> std::io::Result<()> {
    use std::os::unix::fs::OpenOptionsExt;

    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(mode & 0o7777)
        .open(path)?;
    file.write_all(data)?;
    file.flush()
}

/// The bare `strerror` text, without Rust's ` (os error N)` suffix.
fn errno(e: &std::io::Error) -> String {
    let text = e.to_string();
    match text.rfind(" (os error ") {
        Some(at) => text[..at].to_string(),
        None => text,
    }
}

/// git's `die()`: `fatal: <msg>` on stderr, exit 128.
fn fatal(msg: &str) -> ExitCode {
    eprintln!("fatal: {msg}");
    ExitCode::from(FATAL)
}

/// A `parse_options` failure that prints the usage block: `error: <msg>` then
/// the block, exit 129.
fn usage_error(msg: &str) -> ExitCode {
    eprint!("error: {msg}\n{USAGE}");
    ExitCode::from(USAGE_CODE)
}

/// A `parse_options` failure that prints no usage block, exit 129.
fn plain_error(msg: &str) -> ExitCode {
    eprintln!("error: {msg}");
    ExitCode::from(USAGE_CODE)
}

use anyhow::{anyhow, Result};
use std::collections::BTreeSet;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::atomic::AtomicBool;

use gix::bstr::{BString, ByteSlice};
use gix::config::{File as ConfigFile, Source};
use gix::glob::{pattern::Case, wildmatch::Mode as WildMode, Pattern};
use gix::index::entry::{Flags, Mode};

/// `git sparse-checkout` — restrict the worktree to a subset of tracked files.
///
/// Both sparsity dialects are served. In cone mode (git's default) the pattern
/// file (`<git-dir>/info/sparse-checkout`) is generated in git's exact layout —
/// the `/*` + `!/*/` root pair, then one `/<parent>/` + `!/<parent>/*/` pair per
/// ancestor directory (sorted), then one `/<dir>/` line per recursive directory
/// (sorted) — so a file written here is byte-identical to stock git's. In
/// non-cone mode the arguments are written verbatim as gitignore-syntax
/// patterns and matched with `gix-glob`, last matching pattern winning, each
/// path evaluated through every one of its directory prefixes the way git's
/// hierarchical index walk does.
///
/// Applying cone-mode sparsity also refreshes the cache tree, writing the
/// index's root tree and every sub-tree into the object database — git gets
/// there via `clean_tracked_sparse_directories()`, which returns early on a
/// non-cone pattern list, so non-cone `set` and `disable` deposit nothing.
/// `clean` writes the same trees on its way to the directory list.
///
/// Applying sparsity walks the index: entries outside the sparsity get the
/// `SKIP_WORKTREE` bit (and are deleted from disk, pruning directories that
/// become empty — including the parents of a path the user had already deleted
/// themselves), entries inside get it cleared and — only if they were skipped
/// before — are materialised through gitoxide's worktree checkout. Files with
/// local modifications are left alone and keep their bit clear, matching git's
/// refusal to sparsify dirty paths; unmerged entries are left entirely alone
/// and reported, exactly as git reports them. Config is written where git
/// writes it: `core.sparseCheckout` / `core.sparseCheckoutCone` (and
/// `index.sparse` when `--[no-]sparse-index` is given) into
/// `<git-dir>/config.worktree`, with `extensions.worktreeConfig=true` in the
/// repository-local config.
///
/// Option parsing mirrors git's `parse_options`: the top level accepts no
/// options at all, so anything dash-prefixed before the subcommand is a usage
/// error (exit 129), and each subcommand rejects unknown options against its
/// own usage block. The subcommands that require an existing sparse-checkout
/// (`list`, `add`, `reapply`, `clean`) check for one *before* parsing options,
/// which is why `git sparse-checkout list -z` reports "not sparse" rather than
/// an unknown switch.
///
/// The one place this port cannot follow git is `--sparse-index`: the config
/// key is written, but the index is always serialized in full, because the
/// vendored `gix-index` cannot write sparse-directory entries.
///
/// Paths are matched as lossy UTF-8, so a tracked path with invalid UTF-8 bytes
/// may be classified differently than git would classify it.
/// The subcommand names `cmd_sparse_checkout()` declares with `OPT_SUBCOMMAND`.
const SUBCOMMANDS: &[&str] =
    &["list", "set", "add", "init", "reapply", "disable", "check-rules", "clean"];

/// Whether the top-level `parse_options()` of `cmd_sparse_checkout()`
/// (builtin/sparse-checkout.c:938-940) refuses `args` with a usage error: no
/// subcommand, an option ahead of it, or a name that is not a subcommand. That
/// parse runs before `git_config(git_default_config)` at :942, so the dispatcher
/// consults this to keep its config gates from pre-empting the 129.
pub fn top_level_refused(args: &[String]) -> bool {
    let args: &[String] = match args.first() {
        Some(a) if a == "sparse-checkout" => &args[1..],
        _ => args,
    };
    args.first().is_none_or(|sub| !SUBCOMMANDS.contains(&sub.as_str()))
}

pub fn sparse_checkout(args: &[String]) -> Result<ExitCode> {
    // Dispatch hands us the subcommand at index 0; tolerate the command name
    // being present so the module works either way.
    let args: &[String] = match args.first() {
        Some(a) if a == "sparse-checkout" => &args[1..],
        _ => args,
    };

    let Some(sub) = args.first() else {
        eprint!("error: need a subcommand\n{USAGE_TOP}");
        return Ok(ExitCode::from(129));
    };
    // git's top level declares no options, so every dash-argument here is a
    // usage error — including the ones the subcommands themselves accept.
    if sub.starts_with('-') && sub.as_str() != "--" {
        return Ok(opt_error(sub, USAGE_TOP));
    }
    let rest = &args[1..];

    match sub.as_str() {
        "list" => cmd_list(rest),
        "set" => cmd_set(rest, false),
        "add" => cmd_set(rest, true),
        "init" => cmd_init(rest),
        "reapply" => cmd_reapply(rest),
        "disable" => cmd_disable(rest),
        "check-rules" => cmd_check_rules(rest),
        "clean" => cmd_clean(rest),
        other => {
            eprint!("error: unknown subcommand: `{other}'\n{USAGE_TOP}");
            Ok(ExitCode::from(129))
        }
    }
}

// --- usage blocks ----------------------------------------------------------
//
// Each ends with the blank line git's `parse_options` emits after the block.

const USAGE_TOP: &str = "usage: git sparse-checkout (init | list | set | add | reapply | disable | check-rules | clean) [<options>]\n\n";

const USAGE_LIST: &str = "usage: git sparse-checkout list\n\n";

const USAGE_SET: &str = "usage: git sparse-checkout set [--[no-]cone] [--[no-]sparse-index] [--skip-checks] (--stdin | <patterns>)\n\n    --[no-]cone           initialize the sparse-checkout in cone mode\n    --[no-]sparse-index   toggle the use of a sparse index\n    --skip-checks         skip some sanity checks on the given paths that might give false positives\n    --stdin               read patterns from standard in\n\n";

const USAGE_ADD: &str = "usage: git sparse-checkout add [--skip-checks] (--stdin | <patterns>)\n\n    --skip-checks         skip some sanity checks on the given paths that might give false positives\n    --[no-]stdin          read patterns from standard in\n\n";

const USAGE_INIT: &str = "usage: git sparse-checkout init [--cone] [--[no-]sparse-index]\n\n    --[no-]cone           initialize the sparse-checkout in cone mode\n    --[no-]sparse-index   toggle the use of a sparse index\n\n";

const USAGE_REAPPLY: &str = "usage: git sparse-checkout reapply [--[no-]cone] [--[no-]sparse-index]\n\n    --[no-]cone           initialize the sparse-checkout in cone mode\n    --[no-]sparse-index   toggle the use of a sparse index\n\n";

const USAGE_DISABLE: &str = "usage: git sparse-checkout disable\n\n";

const USAGE_CHECK_RULES: &str = "usage: git sparse-checkout check-rules [-z] [--skip-checks][--[no-]cone] [--rules-file <file>]\n\n    -z                    terminate input and output files by a NUL character\n    --[no-]cone           when used with --rules-file interpret patterns as cone mode patterns\n    --[no-]rules-file <file>\n                          use patterns in <file> instead of the current ones.\n\n";

const USAGE_CLEAN: &str = "usage: git sparse-checkout clean [-n|--dry-run]\n\n    -n, --[no-]dry-run    dry run\n    -f, --[no-]force      force\n    -v, --[no-]verbose    report each affected file, not just directories\n\n";

/// Report `arg` the way git's `parse_options` does and return its exit code:
/// `-h` prints the usage block on stdout at exit 0, anything else names the
/// offending option or switch on stderr above the block, at 129.
///
/// `--help-all` joins `-h` here because `parse_options_step()` tests it with a
/// `strcmp()` of its own, ahead of `parse_long_opt()`: the name never
/// abbreviates and never takes an `=<value>`, so `--help-a` and `--help-all=x`
/// still fall through to the unknown-option refusal below. None of
/// sparse-checkout's tables carries a `PARSE_OPT_HIDDEN` entry, so the
/// `USAGE_FULL` it renders is the same block `-h` prints in every subcommand.
fn opt_error(arg: &str, usage: &str) -> ExitCode {
    if arg == "-h" || arg == "--help-all" {
        return super::show_usage(usage);
    }
    if let Some(long) = arg.strip_prefix("--") {
        eprint!("error: unknown option `{long}'\n{usage}");
    } else {
        let switch = arg.chars().nth(1).unwrap_or('-');
        eprint!("error: unknown switch `{switch}'\n{usage}");
    }
    ExitCode::from(129)
}

// --- subcommands -----------------------------------------------------------

fn cmd_list(args: &[String]) -> Result<ExitCode> {
    let repo = crate::setup::discover()?;
    // `sparse_checkout_list()` calls `setup_work_tree()` before it looks at sparsity, so a bare
    // repository or a cwd inside the git dir reports the missing work tree, not the missing
    // sparsity.
    // `setup_work_tree()` refuses a work tree it cannot `chdir()` into the same way (setup.c:502-503).
    if repo.workdir().is_none_or(|wt| !wt.is_dir()) {
        return Err(crate::fatal::need_work_tree());
    }
    // git checks for sparsity before it parses options.
    if !is_sparse(&repo)? {
        eprintln!("fatal: this worktree is not sparse");
        return Ok(ExitCode::from(128));
    }
    for a in args {
        if a.starts_with('-') {
            return Ok(opt_error(a, USAGE_LIST));
        }
    }

    // ```c
    // if (get_sparse_checkout_patterns(&pl) < 0) {
    //         warning(_("this worktree is not sparse (sparse-checkout file may not exist)"));
    //         return 0;
    // }
    // ```
    //
    // (builtin/sparse-checkout.c:75-79.) `core.sparseCheckout=true` with no
    // pattern file is not fatal — the fatal above is for sparsity being off —
    // it is a warning and an empty listing at exit 0.
    if !pattern_path(&repo).exists() {
        eprintln!("warning: this worktree is not sparse (sparse-checkout file may not exist)");
        return Ok(ExitCode::SUCCESS);
    }

    // A cone definition lists its recursive directories, sorted and quoted; a
    // non-cone one — or a cone-mode file `add_pattern_to_hashsets()` gave up on,
    // after its warnings — is written back by `write_patterns_to_file()`, which
    // prints the patterns as `add_patterns_from_buffer()` kept them
    // (builtin/sparse-checkout.c:87-108).
    let Ok(bytes) = std::fs::read(pattern_path(&repo)) else {
        eprintln!("warning: this worktree is not sparse (sparse-checkout file may not exist)");
        return Ok(ExitCode::SUCCESS);
    };
    let patterns = file_patterns(&bytes);
    let mut warnings = String::new();
    let cone_dirs = match is_cone(&repo)? {
        true => {
            let ignore_case = repo.config_snapshot().boolean("core.ignoreCase").unwrap_or(false);
            cone_hashsets(&patterns, ignore_case, &mut warnings)
        }
        false => None,
    };
    eprint!("{warnings}");
    let mut out = Vec::new();
    match cone_dirs {
        Some(cone) => {
            for d in &cone.recursive {
                out.extend_from_slice(quote_path(d.as_bytes()).as_bytes());
                out.push(b'\n');
            }
        }
        None => {
            for p in &patterns {
                out.extend_from_slice(p);
                out.push(b'\n');
            }
        }
    }
    std::io::stdout().write_all(&out)?;
    Ok(ExitCode::SUCCESS)
}

/// `set` (`add == true` merges into the existing sparsity instead of replacing
/// it, and accepts the smaller option set git gives `add`).
fn cmd_set(args: &[String], add: bool) -> Result<ExitCode> {
    let repo = crate::setup::discover()?;
    // `sparse_checkout_set()` (builtin/sparse-checkout.c) opens with
    // `setup_work_tree()`, so a repository without one is refused **before** any
    // file is written. Leaving the check to `apply()` writes
    // `info/sparse-checkout` and `config.worktree` into a bare repository first
    // and only then dies — the message is the same, the repository is not.
    // A work tree that is configured but not there is refused alike (setup.c:502-503).
    if !add && repo.work_dir().is_none_or(|wt| !wt.is_dir()) {
        return Err(crate::fatal::need_work_tree());
    }
    // `add` demands an existing sparse-checkout before it looks at options.
    if add && !is_sparse(&repo)? {
        eprintln!("fatal: no sparse-checkout to add to");
        return Ok(ExitCode::from(128));
    }

    let usage = if add { USAGE_ADD } else { USAGE_SET };
    let mut stdin = false;
    let mut skip_checks = false;
    let mut cone: Option<bool> = None;
    let mut sparse_index: Option<bool> = None;
    let mut positional: Vec<&str> = Vec::new();
    let (opts, after_dashdash) = split_at_end_of_options(args);
    for a in opts {
        match a.as_str() {
            "--stdin" => stdin = true,
            "--no-stdin" if add => stdin = false,
            "--skip-checks" => skip_checks = true,
            "--cone" if !add => cone = Some(true),
            "--no-cone" if !add => cone = Some(false),
            "--sparse-index" if !add => sparse_index = Some(true),
            "--no-sparse-index" if !add => sparse_index = Some(false),
            _ if a.starts_with('-') => return Ok(opt_error(a, usage)),
            _ => positional.push(a.as_str()),
        }
    }
    positional.extend(after_dashdash.iter().map(String::as_str));
    // The arguments left after `parse_options()`. They are sanitized whether or not
    // `--stdin` is given, but with `--stdin` the patterns come from standard input
    // alone: `add_patterns_from_input()` reads `file` and never looks at `argv`
    // (builtin/sparse-checkout.c:577-633).
    let inputs: Vec<String> = positional.into_iter().map(str::to_owned).collect();

    // `add` never switches dialects; `set` honours an explicit flag and
    // otherwise keeps whatever the worktree is already configured for.
    // `update_modes()` (builtin/sparse-checkout.c:418-441) decides both the
    // dialect and whether it is *recorded*:
    //
    // ```c
    //      record_mode = (*cone_mode != -1) || !cfg->apply_sparse_checkout;
    //      mode = update_cone_mode(cone_mode);
    //      if (record_mode && set_config(repo, mode)) return 1;
    // ```
    //
    // so `set` over an already-sparse worktree, with no `--[no-]cone`, writes no
    // config at all — not `core.sparseCheckout`, not `core.sparseCheckoutCone`,
    // and not the `extensions.worktreeConfig` that `set_config` turns on along the
    // way. `add` never records either (`modify_pattern_list`'s ADD arm reads the
    // dialect and leaves it alone).
    let was_sparse = is_sparse(&repo)?;
    let record_mode = !add && (cone.is_some() || !was_sparse);
    let cone = if add { is_cone(&repo)? } else { update_cone_mode(&repo, cone)? };
    let prefix = worktree_prefix(&repo);

    // `sparse_checkout_set()` runs `update_modes()` (builtin/sparse-checkout.c:875)
    // *before* it collects the arguments and calls `sanitize_paths()` (:889), so a
    // definition rejected by the sanity checks still leaves `core.sparseCheckout`,
    // `core.sparseCheckoutCone`, `index.sparse` and `extensions.worktreeConfig`
    // recorded. Measured against git 2.55.0: `git sparse-checkout set /folder1`
    // in a non-sparse repo dies with "specify directories rather than patterns"
    // yet still turns the worktree config on, so the following `list` answers
    // "warning: this worktree is not sparse (sparse-checkout file may not exist)"
    // instead of dying. Writing the config after validation inverts both.
    enable_config(&repo, cone, sparse_index, record_mode)?;

    // builtin/sparse-checkout.c:881-890 — "Cone mode automatically specifies the
    // toplevel directory. For non-cone mode, if nothing is specified, manually
    // select just the top-level directory (much as 'init' would do)."
    //
    // ```c
    //      if (!cfg->core_sparse_checkout_cone && !set_opts.use_stdin && argc == 0) {
    //              for (int i = 0; i < default_patterns_nr; i++)
    //                      strvec_push(&patterns, default_patterns[i]);
    //      } else {
    //              …
    //              sanitize_paths(repo, &patterns, prefix, set_opts.skip_checks);
    //      }
    // ```
    //
    // with `default_patterns[] = {"/*", "!/*/"}` (:847). An empty `--stdin` is
    // *not* the same thing: the condition names `!use_stdin`, so reading nothing
    // from a pipe really does empty the definition. `sanitize_paths` is only
    // reached by the other branch, so the defaults are never vetted.
    let default_non_cone = !add && !cone && !stdin && inputs.is_empty();
    let inputs = if default_non_cone {
        vec!["/*".to_owned(), "!/*/".to_owned()]
    } else {
        // `sanitize_paths()` runs over the whole list first, before any argument
        // is turned into a pattern (builtin/sparse-checkout.c:822, :889).
        if let Some(code) = sanitize_paths(&repo, &inputs, &prefix, cone, skip_checks)? {
            return Ok(code);
        }
        inputs
    };
    // `strbuf_getline()` over standard input: one pattern per line, a `\r` before
    // the newline dropped, a final line without one kept.
    let stdin_lines = if stdin {
        let mut buf = Vec::new();
        std::io::stdin().read_to_end(&mut buf)?;
        let mut lines: Vec<Vec<u8>> = buf.split(|&b| b == b'\n').map(<[u8]>::to_vec).collect();
        if buf.is_empty() || buf.ends_with(b"\n") {
            lines.pop();
        }
        for line in &mut lines {
            if line.ends_with(b"\r") {
                line.pop();
            }
        }
        Some(lines)
    } else {
        None
    };

    let sparsity = if cone {
        // `add_patterns_from_input()`'s cone arm turns each line or argument into a
        // pattern with `strbuf_to_cone_pattern()`; only a standard-input line that
        // opens with a double quote is unquoted first (builtin/sparse-checkout.c:591-612).
        let mut dirs: BTreeSet<String> = BTreeSet::new();
        let converted = match &stdin_lines {
            Some(lines) => lines.iter().map(|l| cone_argument(&l.to_str_lossy(), "", skip_checks)).collect::<Vec<_>>(),
            None => inputs.iter().map(|a| cone_argument_verbatim(a, &prefix)).collect(),
        };
        for result in converted {
            match result? {
                Ok(Some(d)) => {
                    dirs.insert(d);
                }
                // An empty argument (e.g. a blank `--stdin` line) names the root.
                Ok(None) => {}
                Err(code) => return Ok(code),
            }
        }
        // `add_patterns_cone_mode()` loads the existing definition after the new
        // patterns and refuses one that is not a cone (builtin/sparse-checkout.c:651-658).
        if add {
            match load_pattern_file(&repo, true)? {
                None => {
                    eprintln!("fatal: unable to load existing sparse-checkout patterns");
                    return Ok(ExitCode::from(128));
                }
                Some((Sparsity::Cone(existing), _)) => dirs.extend(existing.recursive),
                Some((_, warnings)) => {
                    eprint!("{warnings}");
                    eprintln!("fatal: existing sparse-checkout patterns do not use cone mode");
                    return Ok(ExitCode::from(128));
                }
            }
        }
        let cone = Cone::new(dedup_nested(dirs));
        write_pattern_file(&repo, &cone_lines(&cone))?;
        Sparsity::Cone(cone)
    } else {
        // `add_patterns_literal()` loads the existing list first (dir.c's
        // `add_patterns_from_buffer()` rules), then `add_pattern()` takes every new
        // line or argument as it is, an empty one included; `write_patterns_to_file()`
        // writes each back as it was parsed (builtin/sparse-checkout.c:31-48, :675-685).
        let mut lines: Vec<String> = if add {
            match std::fs::read(pattern_path(&repo)) {
                Ok(bytes) => file_patterns(&bytes).iter().map(|p| p.to_str_lossy().into_owned()).collect(),
                Err(_) => {
                    eprintln!("fatal: unable to load existing sparse-checkout patterns");
                    return Ok(ExitCode::from(128));
                }
            }
        } else {
            Vec::new()
        };
        match &stdin_lines {
            Some(new) => lines.extend(new.iter().map(|l| c_string(l))),
            None => lines.extend(inputs.iter().cloned()),
        }
        write_pattern_file(&repo, &lines)?;
        Sparsity::Patterns(parse_patterns(&lines))
    };

    apply(&repo, &sparsity)?;
    Ok(ExitCode::SUCCESS)
}

fn cmd_init(args: &[String]) -> Result<ExitCode> {
    // git 2.55.0 refuses `init` without a work tree ahead of its own
    // `parse_options()` — measured from inside `.git/refs`, `init --bogus` and
    // `init -h` both answer `fatal: this operation must be run in a work tree`
    // at 128 — and so ahead of `update_modes()`, which is where
    // `extensions.worktreeConfig` is written. Checking later left that key
    // behind in a repository git leaves untouched.
    let repo = crate::setup::discover()?;
    if repo.workdir().is_none() {
        return Err(crate::fatal::need_work_tree());
    }
    let mut cone: Option<bool> = None;
    let mut sparse_index: Option<bool> = None;
    for a in args {
        match a.as_str() {
            "--cone" => cone = Some(true),
            "--no-cone" => cone = Some(false),
            "--sparse-index" => sparse_index = Some(true),
            "--no-sparse-index" => sparse_index = Some(false),
            _ if a.starts_with('-') => return Ok(opt_error(a, USAGE_INIT)),
            _ => {}
        }
    }
    let repo = crate::setup::discover()?;
    // `update_modes()` again — same `record_mode` rule as `set`.
    let record_mode = cone.is_some() || !is_sparse(&repo)?;
    let cone = update_cone_mode(&repo, cone)?;

    // `init` keeps an existing pattern file (that is how a `disable`d sparsity
    // is restored); only a missing one is seeded with the empty cone.
    if !pattern_path(&repo).exists() {
        write_pattern_file(&repo, &cone_lines(&Cone::new(BTreeSet::new())))?;
    }
    let (sparsity, warnings) = load_for_update(&repo, cone)?;

    enable_config(&repo, cone, sparse_index, record_mode)?;
    apply_reloaded(&repo, &sparsity, &warnings)?;
    Ok(ExitCode::SUCCESS)
}

fn cmd_reapply(args: &[String]) -> Result<ExitCode> {
    let repo = crate::setup::discover()?;
    if !is_sparse(&repo)? {
        eprintln!("fatal: must be in a sparse-checkout to reapply sparsity patterns");
        return Ok(ExitCode::from(128));
    }
    let mut cone: Option<bool> = None;
    let mut sparse_index: Option<bool> = None;
    for a in args {
        match a.as_str() {
            "--cone" => cone = Some(true),
            "--no-cone" => cone = Some(false),
            "--sparse-index" => sparse_index = Some(true),
            "--no-sparse-index" => sparse_index = Some(false),
            _ if a.starts_with('-') => return Ok(opt_error(a, USAGE_REAPPLY)),
            _ => {}
        }
    }
    // `reapply` is only reachable on an already-sparse worktree (it dies above
    // otherwise), so `record_mode` here is exactly "was `--[no-]cone` given".
    let record_mode = cone.is_some();
    let cone = update_cone_mode(&repo, cone)?;

    let (sparsity, warnings) = load_for_update(&repo, cone)?;
    enable_config(&repo, cone, sparse_index, record_mode)?;
    apply_reloaded(&repo, &sparsity, &warnings)?;
    Ok(ExitCode::SUCCESS)
}

fn cmd_disable(args: &[String]) -> Result<ExitCode> {
    for a in args {
        if a.starts_with('-') {
            return Ok(opt_error(a, USAGE_DISABLE));
        }
    }
    let repo = crate::setup::discover()?;
    // `give_advice_on_expansion = 0` (builtin/sparse-checkout.c:1068-1072): disabling expands
    // the index on purpose.
    crate::sparse_index::no_advice_on_expansion();
    // git leaves the pattern file in place so a later `init` can restore it.
    apply(&repo, &Sparsity::Full)?;
    disable_config(&repo)?;
    Ok(ExitCode::SUCCESS)
}

/// `strerror(errno)` without the ` (os error <n>)` tail Rust appends — git's
/// `xfopen()` dies with the bare text.
fn errno_text(e: &std::io::Error) -> String {
    let text = e.to_string();
    text.split(" (os error ").next().unwrap_or(&text).to_owned()
}

fn cmd_check_rules(args: &[String]) -> Result<ExitCode> {
    let mut nul = false;
    // `check_rules_opts.cone_mode = -1` (builtin/sparse-checkout.c:1155): unset,
    // not "cone".
    let mut cone: Option<bool> = None;
    // `--rules-file` is an `OPT_FILENAME` (builtin/sparse-checkout.c:1144): its
    // unset form changes nothing and its value goes through `opt_filename()`
    // (parse-options.c:209-235).
    let mut rules_file_arg: Option<String> = None;
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "-z" => nul = true,
            "--cone" => cone = Some(true),
            "--no-cone" => cone = Some(false),
            // `--skip-checks` is named in `check-rules`' usage string
            // (builtin/sparse-checkout.c:1093-1094) but is *not* in its option
            // table (:1136-1145), so stock answers it with `error: unknown option
            // `skip-checks'` and 129. The usage line is quoted verbatim below, so
            // the refusal and the block that follows it disagree exactly as git's
            // do.
            "--no-rules-file" => {}
            "--rules-file" => match it.next() {
                Some(v) => {
                    crate::parseopt::opt_filename(&mut rules_file_arg, v, crate::parseopt::discovered_prefix)?;
                }
                None => {
                    // git reports a missing value without the usage block.
                    eprintln!("error: option `rules-file' requires a value");
                    return Ok(ExitCode::from(129));
                }
            },
            _ if a.starts_with("--rules-file=") => {
                let v = &a["--rules-file=".len()..];
                crate::parseopt::opt_filename(&mut rules_file_arg, v, crate::parseopt::discovered_prefix)?;
            }
            _ if a.starts_with('-') => return Ok(opt_error(a, USAGE_CHECK_RULES)),
            _ => {}
        }
    }
    let rules_file = rules_file_arg.map(PathBuf::from);

    let sparsity = match &rules_file {
        // `--rules-file` holds a directory list in cone mode, patterns otherwise.
        Some(p) => {
            // `xfopen()`, not `add_patterns_from_file_to_list()`
            // (builtin/sparse-checkout.c:1164) — so the refusal names the file and
            // carries the errno, and the `unable to load existing sparse-checkout
            // patterns` wording belongs to the *other* branch, where the file read
            // is the worktree's own definition.
            let text = match std::fs::read(p) {
                Ok(text) => text,
                Err(e) => {
                    eprintln!(
                        "fatal: could not open '{}' for reading: {}",
                        p.display(),
                        errno_text(&e)
                    );
                    return Ok(ExitCode::from(128));
                }
            };
            // `strbuf_getline()` over the file, each line a C string from there on.
            let lines: Vec<String> = getlines(&text, b'\n').iter().map(|l| c_string(l)).collect();
            // ```c
            //      if (check_rules_opts.rules_file && check_rules_opts.cone_mode < 0)
            //              check_rules_opts.cone_mode = 1;
            //      update_cone_mode(&check_rules_opts.cone_mode);
            // ```
            // (builtin/sparse-checkout.c:1159-1162): with a rules file and no flag
            // the list is read as cone patterns, so `update_cone_mode` never has to
            // consult the worktree.
            if cone.unwrap_or(true) {
                let mut dirs = BTreeSet::new();
                for line in &lines {
                    match cone_argument(line, "", true)? {
                        Ok(Some(d)) => {
                            dirs.insert(d);
                        }
                        Ok(None) => {}
                        Err(code) => return Ok(code),
                    }
                }
                Sparsity::Cone(Cone::new(dedup_nested(dirs)))
            } else {
                Sparsity::Patterns(parse_patterns(&lines))
            }
        }
        None => {
            let repo = crate::setup::discover()?;
            if !pattern_path(&repo).exists() {
                eprintln!("fatal: unable to load existing sparse-checkout patterns");
                return Ok(ExitCode::from(128));
            }
            // `pl.use_cone_patterns = cfg->core_sparse_checkout_cone` *after*
            // `update_cone_mode()` has had its say (builtin/sparse-checkout.c:1162-1163).
            let cone = update_cone_mode(&repo, cone)?;
            let (sparsity, warnings) = load_for_update(&repo, cone)?;
            eprint!("{warnings}");
            sparsity
        }
    };

    let mut input = Vec::new();
    std::io::stdin().read_to_end(&mut input)?;
    let sep = if nul { b'\0' } else { b'\n' };

    let mut out = Vec::new();
    // `check_rules()` (builtin/sparse-checkout.c:1105-1134): every line, an empty
    // one included, is asked about; `path_in_sparse_checkout()` takes an empty path
    // as inside (dir.c:1588-1592).
    for raw in getlines(&input, sep) {
        // Without -z, a leading double quote marks a C-style quoted path.
        let path = if nul { BString::from(raw) } else { unquote_c(&raw)? };
        let path = BString::from(c_string(&path));
        if path.is_empty() || sparsity.includes(&path.to_str_lossy()) {
            if nul {
                out.extend_from_slice(&path);
                out.push(0);
            } else {
                out.extend_from_slice(quote_path(&path).as_bytes());
                out.push(b'\n');
            }
        }
    }
    std::io::stdout().write_all(&out)?;
    Ok(ExitCode::SUCCESS)
}

/// `clean` — drop worktree directories that sparsity says should not be there.
///
/// A tracked directory outside the sparsity is removed whole (including the
/// untracked and ignored files inside it) as long as none of the tracked files
/// beneath it are still on disk; if any is, the directory is descended into
/// instead, so a single stubborn path does not pin its siblings. Untracked
/// directories are never candidates: git only cleans what the index knows.
///
/// Cone mode is a hard precondition — git's directory candidates *are* the
/// sparse-index directory entries, which only exist in cone mode.
fn cmd_clean(args: &[String]) -> Result<ExitCode> {
    let repo = crate::setup::discover()?;
    if !is_sparse(&repo)? {
        eprintln!("fatal: must be in a sparse-checkout to clean directories");
        return Ok(ExitCode::from(128));
    }
    // Both premises are checked ahead of `parse_options`, so a bad switch in a
    // non-cone worktree still reports the dialect rather than the switch.
    if !is_cone(&repo)? {
        eprintln!("fatal: must be in a cone-mode sparse-checkout to clean directories");
        return Ok(ExitCode::from(128));
    }
    let mut dry_run = false;
    let mut force = false;
    let mut verbose = false;
    for a in args {
        match a.as_str() {
            "-n" | "--dry-run" => dry_run = true,
            "--no-dry-run" => dry_run = false,
            "-f" | "--force" => force = true,
            "--no-force" => force = false,
            "-v" | "--verbose" => verbose = true,
            "--no-verbose" => verbose = false,
            _ if a.starts_with('-') => return Ok(opt_error(a, USAGE_CLEAN)),
            _ => {}
        }
    }
    if !dry_run && !force && config_bool(&repo, "clean", "requireForce")?.unwrap_or(true) {
        eprintln!("fatal: for safety, refusing to clean without one of --force or --dry-run");
        return Ok(ExitCode::from(128));
    }

    let workdir = repo
        .workdir()
        .ok_or_else(|| crate::fatal::need_work_tree())?
        .to_owned();
    let sparsity = load_sparsity(&repo)?;
    let index = repo.open_index()?;
    // Serialize the odb append below against the other zvcs writers.
    let lock = crate::lock::RepoLock::acquire(repo.git_dir());
    // git reaches the directory list through `convert_to_sparse()`, which
    // refreshes the cache tree first — so `clean` deposits the same tree objects
    // `set` does, dry run or not.
    write_cache_tree(&repo, &index)?;
    drop(lock);
    let mut paths: Vec<String> = {
        let backing = index.path_backing();
        index
            .entries()
            .iter()
            .map(|e| e.path_in(backing).to_str_lossy().into_owned())
            .collect()
    };
    paths.sort();

    // Every directory the index knows about, which is the whole candidate set.
    let mut dirs: BTreeSet<String> = BTreeSet::new();
    for p in &paths {
        let comps: Vec<&str> = p.split('/').collect();
        let mut acc = String::new();
        // Every component but the last, which is the file name itself.
        for comp in &comps[..comps.len().saturating_sub(1)] {
            if !acc.is_empty() {
                acc.push('/');
            }
            acc.push_str(comp);
            dirs.insert(acc.clone());
        }
    }

    let ctx = CleanCtx { workdir, sparsity, paths, dirs, dry_run, verbose };
    let mut out = Vec::new();
    ctx.visit("", &mut out);
    std::io::stdout().write_all(&out)?;
    Ok(ExitCode::SUCCESS)
}

struct CleanCtx {
    workdir: PathBuf,
    sparsity: Sparsity,
    /// Sorted index paths, so a directory's entries are one contiguous slice.
    paths: Vec<String>,
    dirs: BTreeSet<String>,
    dry_run: bool,
    verbose: bool,
}

impl CleanCtx {
    fn verb(&self) -> &'static str {
        if self.dry_run {
            "Would remove"
        } else {
            "Removing"
        }
    }

    /// The index entries directly or indirectly under `dir`.
    fn entries_under(&self, dir: &str) -> &[String] {
        let lower = format!("{dir}/");
        // '/' + 1 == '0', so this is the first path that cannot share the prefix.
        let mut upper = lower.clone();
        upper.pop();
        upper.push('0');
        let start = self.paths.partition_point(|p| p.as_str() < lower.as_str());
        let end = self.paths.partition_point(|p| p.as_str() < upper.as_str());
        &self.paths[start..end]
    }

    /// A directory belongs to the sparsity when any tracked path under it does.
    fn in_sparsity(&self, dir: &str) -> bool {
        self.entries_under(dir).iter().any(|p| self.sparsity.includes(p))
    }

    fn holds_tracked_file(&self, dir: &str) -> bool {
        self.entries_under(dir)
            .iter()
            .any(|p| self.workdir.join(p).symlink_metadata().is_ok())
    }

    fn child_dirs(&self, dir: &str) -> Vec<String> {
        let prefix = if dir.is_empty() { String::new() } else { format!("{dir}/") };
        self.dirs
            .iter()
            .filter(|d| match d.strip_prefix(&prefix) {
                Some(tail) => !tail.is_empty() && !tail.contains('/'),
                None => false,
            })
            .cloned()
            .collect()
    }

    fn visit(&self, dir: &str, out: &mut Vec<u8>) {
        for child in self.child_dirs(dir) {
            let full = self.workdir.join(&child);
            if self.in_sparsity(&child) || !full.is_dir() || self.holds_tracked_file(&child) {
                self.visit(&child, out);
                continue;
            }
            if self.verbose {
                list_files(&full, &child, self.verb(), out);
            } else {
                out.extend_from_slice(format!("{} {}/\n", self.verb(), child).as_bytes());
            }
            if !self.dry_run {
                let _ = std::fs::remove_dir_all(&full);
            }
        }
    }
}

/// Name every file beneath `full` in directory order, the way `clean --verbose`
/// enumerates what a whole-directory removal would take with it.
fn list_files(full: &Path, rel: &str, verb: &str, out: &mut Vec<u8>) {
    let Ok(entries) = std::fs::read_dir(full) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let child = format!("{rel}/{}", name.to_string_lossy());
        match entry.file_type() {
            Ok(t) if t.is_dir() => list_files(&entry.path(), &child, verb, out),
            Ok(_) => out.extend_from_slice(format!("{verb} {child}\n").as_bytes()),
            Err(_) => {}
        }
    }
}

// --- sparsity model --------------------------------------------------------

/// What the worktree is currently restricted to.
pub(crate) enum Sparsity {
    /// No restriction at all — what `disable` applies.
    Full,
    Cone(Cone),
    /// Non-cone gitignore-syntax patterns, in file order.
    Patterns(Vec<Pattern>),
}

impl Sparsity {
    /// `cfg->core_sparse_checkout_cone`: whether the definition is a cone, which
    /// is the only shape some of git's sparse handling runs for.
    pub(crate) fn is_cone(&self) -> bool {
        matches!(self, Sparsity::Cone(_))
    }

    pub(crate) fn includes(&self, path: &str) -> bool {
        match self {
            Sparsity::Full => true,
            Sparsity::Cone(c) => c.matches(path),
            Sparsity::Patterns(p) => patterns_include(p, path),
        }
    }
}

/// [`load_sparsity`] gated on `core.sparseCheckout` — `cfg->apply_sparse_checkout`
/// (environment.c:549-551), the flag every caller in git tests before consulting a
/// pattern list at all. `None` means the worktree is not sparse, which is a
/// different answer from [`Sparsity::Full`]: the latter is the `/*` list `disable`
/// installs, and still counts as a definition to copy.
pub(crate) fn load_sparsity_if_enabled(repo: &gix::Repository) -> Result<Option<Sparsity>> {
    if !is_sparse(repo)? {
        return Ok(None);
    }
    load_sparsity(repo).map(Some)
}

/// [`load_sparsity`] for `path_in_sparse_checkout()`'s callers (`add`, `mv`). A
/// `core.sparseCheckout` worktree whose `info/sparse-checkout` is not there has no definition to
/// consult: `init_sparse_checkout_patterns()` fails to load it and every path counts as inside,
/// where an *empty* file is a definition that includes nothing.
pub(crate) fn load_sparsity_for_path_checks(repo: &gix::Repository) -> Result<Option<Sparsity>> {
    if std::fs::metadata(pattern_path(repo)).is_err() {
        return Ok(None);
    }
    load_sparsity(repo).map(Some)
}

pub(crate) fn load_sparsity(repo: &gix::Repository) -> Result<Sparsity> {
    load_sparsity_with(repo, is_cone(repo)?)
}

/// [`load_sparsity`] for a caller that already settled the dialect through
/// [`update_cone_mode`] rather than reading `core.sparseCheckoutCone` itself.
fn load_sparsity_with(repo: &gix::Repository, cone: bool) -> Result<Sparsity> {
    let lines = read_pattern_file(repo)?;
    Ok(if cone {
        Sparsity::Cone(Cone::new(cone_dirs(&lines)))
    } else {
        Sparsity::Patterns(parse_patterns(&lines))
    })
}

fn parse_patterns(lines: &[String]) -> Vec<Pattern> {
    lines
        .iter()
        .map(|l| l.trim_end_matches('\r'))
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .filter_map(|l| Pattern::from_bytes(l.as_bytes()))
        .collect()
}

/// Decide whether `path` is included by non-cone `patterns`.
///
/// git resolves sparsity by walking the index hierarchically, so a directory
/// pattern decides every path beneath it. Evaluating each of `path`'s prefixes
/// in turn — directories first, the file itself last, later matches overriding
/// earlier ones — reproduces that without the walk.
fn patterns_include(patterns: &[Pattern], path: &str) -> bool {
    let mut included = false;
    let mut offset = 0usize;
    loop {
        let (prefix, is_dir) = match path[offset..].find('/') {
            Some(i) => (&path[..offset + i], true),
            None => (path, false),
        };
        let basename_pos = prefix.rfind('/').map(|i| i + 1);
        for p in patterns {
            if p.matches_repo_relative_path(
                prefix.as_bytes().as_bstr(),
                basename_pos,
                Some(is_dir),
                Case::Sensitive,
                WildMode::NO_MATCH_SLASH_LITERAL,
            ) {
                included = !p.is_negative();
            }
        }
        if !is_dir {
            return included;
        }
        offset = prefix.len() + 1;
    }
}

// --- unpack_trees()' sparse loops --------------------------------------------

/// `enum pattern_match_result` (dir.h).
#[derive(Clone, Copy, PartialEq, Eq)]
enum PatternMatch {
    Undecided,
    NotMatched,
    Matched,
    MatchedRecursive,
}

/// The `struct pattern_list` `populate_from_existing_patterns()` hands
/// `unpack_trees()` (unpack-trees.c:1804-1811): `info/sparse-checkout` read with
/// `use_cone_patterns = cfg->core_sparse_checkout_cone` (dir.c:3513).
pub(crate) struct UnpackPatterns {
    sparsity: Sparsity,
    /// `pl->full_cone`: a bare `/*` line sets it, a `!/*/` line clears it
    /// (dir.c:731-741).
    full_cone: bool,
}

impl UnpackPatterns {
    /// The pattern file as `unpack_trees()` reads it, with `cone` the in-process
    /// `core_sparse_checkout_cone` rather than whatever the file's writer set since.
    ///
    /// `None` is `populate_from_existing_patterns()` (unpack-trees.c:1829-1836) finding
    /// no file to open: `add_patterns_from_file_to_list()` is handed a NULL `istate`, so
    /// a failed `open()` returns -1 (dir.c:1164-1170) and `o->skip_sparse_checkout` is
    /// set — the skip-worktree bits are left alone rather than every path excluded.
    pub(crate) fn load(repo: &gix::Repository, cone: bool) -> Option<Self> {
        let text = std::fs::read_to_string(pattern_path(repo)).ok()?;
        let lines: Vec<String> = text.lines().map(str::to_owned).collect();
        let mut full_cone = false;
        for l in &lines {
            match l.trim_end_matches('\r') {
                "/*" => full_cone = true,
                "!/*/" => full_cone = false,
                _ => {}
            }
        }
        Some(UnpackPatterns {
            sparsity: if cone {
                Sparsity::Cone(Cone::new(cone_dirs(&lines)))
            } else {
                Sparsity::Patterns(parse_patterns(&lines))
            },
            full_cone,
        })
    }

    /// `path_matches_pattern_list()` (dir.c:1502-1566) for `path`, whose last
    /// component starts at `basename`.
    fn matches(&self, path: &[u8], basename: usize, is_dir: bool) -> PatternMatch {
        match &self.sparsity {
            Sparsity::Patterns(patterns) => {
                // `last_matching_pattern_from_list()`: the last pattern that matches decides.
                let hit = patterns.iter().rev().find(|p| {
                    p.matches_repo_relative_path(
                        path.as_bstr(),
                        (basename != 0).then_some(basename),
                        Some(is_dir),
                        Case::Sensitive,
                        WildMode::NO_MATCH_SLASH_LITERAL,
                    )
                });
                match hit {
                    Some(p) if p.is_negative() => PatternMatch::NotMatched,
                    Some(_) => PatternMatch::Matched,
                    None => PatternMatch::Undecided,
                }
            }
            Sparsity::Cone(_) if self.full_cone => PatternMatch::Matched,
            Sparsity::Cone(cone) => cone.match_result(&path.to_str_lossy()),
            Sparsity::Full => PatternMatch::Matched,
        }
    }

    fn use_cone_patterns(&self) -> bool {
        self.sparsity.is_cone()
    }

    /// `path_in_sparse_checkout()` (dir.c:1576-1622): is `path` — a directory carrying its
    /// trailing `/` — inside the sparse-checkout definition? The empty path always is, and a
    /// pattern that leaves a path `UNDECIDED` defers to its parent directories, falling back to
    /// "outside" at the top.
    pub(crate) fn path_in_sparse_checkout(&self, path: &[u8]) -> bool {
        if path.is_empty() {
            return true;
        }
        // `dtype` starts as `DT_REG` and is `DT_DIR` for every parent tried after.
        let mut end = path.len();
        let mut is_dir = false;
        while end > 0 {
            // `for (slash = end - 1; slash > path && *slash != '/'; slash--)`: the scan
            // includes `end - 1` and never treats the first byte as a separator.
            let slash = path[1..end].iter().rposition(|b| *b == b'/').map_or(0, |p| p + 1);
            let basename = if slash > 0 { slash + 1 } else { 0 };
            match self.matches(&path[..end], basename, is_dir) {
                PatternMatch::Undecided => {}
                m => return m != PatternMatch::NotMatched,
            }
            is_dir = true;
            end = slash;
        }
        false
    }

    /// `mark_new_skip_worktree()` (unpack-trees.c:1799-1827) over the entries of
    /// `index` that `select` admits, setting `SKIP_WORKTREE` on each unmerged-free
    /// entry and clearing it again on every entry the patterns take in. The
    /// `Updating index flags` meter `clear_ce_flags()` starts when
    /// `show_progress` (unpack-trees.c:1765-1797) is drawn over every entry of the
    /// index, selected or not.
    pub(crate) fn mark_new_skip_worktree(
        &self,
        index: &mut gix::index::File,
        select: &dyn Fn(usize) -> bool,
        show_progress: bool,
    ) -> std::result::Result<(), crate::progress::DelayError> {
        let skip = self.new_skip_worktree(index, select, show_progress)?;
        for (entry, skip) in index.entries_mut().iter_mut().zip(skip) {
            if skip {
                entry.flags.insert(Flags::SKIP_WORKTREE | Flags::EXTENDED);
            } else {
                entry.flags.remove(Flags::SKIP_WORKTREE);
                if !entry.flags.contains(Flags::INTENT_TO_ADD) {
                    entry.flags.remove(Flags::EXTENDED);
                }
            }
        }
        Ok(())
    }

    /// The bit [`Self::mark_new_skip_worktree`] would leave on each entry of `index`, for a
    /// caller that keeps it apart from `CE_SKIP_WORKTREE` the way `unpack_trees()` keeps
    /// `CE_NEW_SKIP_WORKTREE` (unpack-trees.c:1974-1976, :2041-2043). An entry `select`
    /// leaves out answers with its current `CE_SKIP_WORKTREE`, unless the walk cleared it.
    pub(crate) fn new_skip_worktree(
        &self,
        index: &gix::index::State,
        select: &dyn Fn(usize) -> bool,
        show_progress: bool,
    ) -> std::result::Result<Vec<bool>, crate::progress::DelayError> {
        let cleared = self.clear_ce_flags(index, select, show_progress)?;
        Ok(index
            .entries()
            .iter()
            .zip(cleared)
            .enumerate()
            .map(|(i, (e, cleared))| {
                // 1. Pretend the narrowest worktree: only unmerged entries are checked out.
                let set = if select(i) { e.stage_raw() == 0 } else { e.flags.contains(Flags::SKIP_WORKTREE) };
                // 2. Widen worktree according to sparse-checkout file.
                set && !cleared
            })
            .collect())
    }

    /// `clear_ce_flags()` (unpack-trees.c:1765-1797): which entries of `index` the walk clears
    /// the flags of. That is every matched entry `select` admits — and, in cone mode, every
    /// entry under a directory the patterns take in whole, selected or not: the
    /// `MATCHED_RECURSIVE` arm of `clear_ce_flags_dir()` clears the whole range without asking
    /// `select_mask` (:1645-1652).
    pub(crate) fn clear_ce_flags(
        &self,
        index: &gix::index::State,
        select: &dyn Fn(usize) -> bool,
        show_progress: bool,
    ) -> std::result::Result<Vec<bool>, crate::progress::DelayError> {
        let paths: Vec<BString> = {
            let backing = index.path_backing();
            index.entries().iter().map(|e| e.path_in(backing).to_owned()).collect()
        };
        let selected: Vec<bool> = (0..paths.len()).map(select).collect();
        let dirs: Vec<bool> = index.entries().iter().map(|e| e.mode == Mode::COMMIT).collect();

        let mut cleared = vec![false; paths.len()];
        let meter = crate::progress::Meter::delayed("Updating index flags", paths.len(), show_progress)?;
        let mut walk = ClearFlags {
            pl: self,
            paths: &paths,
            dirs: &dirs,
            selected: &selected,
            cleared: &mut cleared,
            meter,
        };
        let mut prefix = Vec::new();
        walk.clear_1(0, paths.len(), &mut prefix, PatternMatch::Undecided, 0);
        walk.meter.stop("done");

        Ok(cleared)
    }
}

/// The state `clear_ce_flags_1()` and `clear_ce_flags_dir()` share through
/// their arguments and `istate->progress`.
struct ClearFlags<'a> {
    pl: &'a UnpackPatterns,
    paths: &'a [BString],
    dirs: &'a [bool],
    selected: &'a [bool],
    cleared: &'a mut [bool],
    meter: crate::progress::Meter,
}

impl ClearFlags<'_> {
    /// `clear_ce_flags_1()` (unpack-trees.c:1673-1755) over `nr` entries from
    /// `start`; returns how many it consumed.
    fn clear_1(&mut self, start: usize, nr: usize, prefix: &mut Vec<u8>, default_match: PatternMatch, mut progress_nr: usize) -> usize {
        let end = start + nr;
        let mut i = start;
        while i != end {
            self.meter.set(progress_nr);

            if !self.selected[i] {
                i += 1;
                progress_nr += 1;
                continue;
            }

            let name = self.paths[i].as_slice();
            if !prefix.is_empty() && !name.starts_with(prefix) {
                break;
            }

            if let Some(len) = name[prefix.len()..].find_byte(b'/') {
                // If it's a directory, try whole directory match first.
                prefix.extend_from_slice(&name[prefix.len()..prefix.len() + len]);
                let processed = self.clear_dir(i, end - i, prefix, len, default_match, progress_nr);
                if processed != 0 {
                    i += processed;
                    progress_nr += processed;
                    prefix.truncate(prefix.len() - len);
                    continue;
                }

                prefix.push(b'/');
                let processed = self.clear_1(i, end - i, prefix, default_match, progress_nr);
                i += processed;
                progress_nr += processed;
                prefix.truncate(prefix.len() - len - 1);
                continue;
            }

            // Non-directory.
            let mut ret = self.pl.matches(name, prefix.len(), self.dirs[i]);
            if ret == PatternMatch::Undecided {
                ret = default_match;
            }
            if matches!(ret, PatternMatch::Matched | PatternMatch::MatchedRecursive) {
                self.cleared[i] = true;
            }
            i += 1;
            progress_nr += 1;
        }

        self.meter.set(progress_nr);
        nr - (end - i)
    }

    /// `clear_ce_flags_dir()` (unpack-trees.c:1618-1666): `prefix` names the
    /// directory, its last `basename_len` bytes being its own name.
    fn clear_dir(&mut self, start: usize, nr: usize, prefix: &mut Vec<u8>, basename_len: usize, default_match: PatternMatch, progress_nr: usize) -> usize {
        let orig_ret = self.pl.matches(prefix, prefix.len() - basename_len, true);
        prefix.push(b'/');

        // If undecided, use matching result of parent dir in defval.
        let ret = if orig_ret == PatternMatch::Undecided { default_match } else { orig_ret };

        let count = self.paths[start..start + nr]
            .iter()
            .take_while(|name| name.starts_with(prefix))
            .count();

        let rc = if self.pl.use_cone_patterns() && orig_ret == PatternMatch::MatchedRecursive {
            self.cleared[start..start + count].iter_mut().for_each(|c| *c = true);
            count
        } else if self.pl.use_cone_patterns() && orig_ret == PatternMatch::NotMatched {
            count
        } else {
            self.clear_1(start, count, prefix, ret, progress_nr)
        };

        prefix.pop();
        rc
    }
}

// --- cone model ------------------------------------------------------------

/// A cone-mode sparsity definition: the recursive directories plus every
/// ancestor of them (the "parent" directories, which contribute only their
/// immediate files).
struct Cone {
    /// Recursive directories, repo-relative, no leading or trailing slash.
    recursive: BTreeSet<String>,
    /// Strict ancestors of `recursive`, plus the repository root as `""`.
    parents: BTreeSet<String>,
    /// `pl->full_cone`: a `/*` the file never took back with `!/*/`, which
    /// matches every path (dir.c:1505-1506).
    full: bool,
    /// `core.ignoreCase`, under which `pl_hashmap_cmp()` compares the names in both
    /// hashmaps with `fspathncmp()`.
    ignore_case: bool,
}

impl Cone {
    fn new(recursive: BTreeSet<String>) -> Self {
        let mut parents = BTreeSet::new();
        parents.insert(String::new());
        for d in &recursive {
            let mut acc = String::new();
            for comp in d.split('/') {
                if acc.is_empty() {
                    acc.push_str(comp);
                } else {
                    acc.push('/');
                    acc.push_str(comp);
                }
                if acc != *d {
                    parents.insert(acc.clone());
                }
            }
        }
        Cone { recursive, parents, full: false, ignore_case: false }
    }

    /// `name` is in `set` as `pl_hashmap_cmp()` compares.
    fn set_contains(&self, set: &BTreeSet<String>, name: &str) -> bool {
        match self.ignore_case {
            false => set.contains(name),
            true => set.iter().any(|s| s.eq_ignore_ascii_case(name)),
        }
    }

    /// Port of `path_matches_pattern_list()`'s cone branch (dir.c:1502-1546).
    ///
    /// ```c
    ///         strbuf_addch(&parent_pathname, '/');
    ///         strbuf_add(&parent_pathname, pathname, pathlen);
    ///         if (parent_pathname.len > 0 &&
    ///             parent_pathname.buf[parent_pathname.len - 1] == '/') {
    ///                 slash_pos = parent_pathname.len - 1;
    ///                 strbuf_add(&parent_pathname, "-", 1);
    ///         } else {
    ///                 const char *slash_ptr = strrchr(parent_pathname.buf, '/');
    ///                 slash_pos = slash_ptr ? slash_ptr - parent_pathname.buf : 0;
    ///         }
    ///         if (hashmap_contains_path(&pl->recursive_hashmap, &parent_pathname)) …
    ///         if (!slash_pos) { /* include every file in root */ … }
    ///         strbuf_setlen(&parent_pathname, slash_pos);
    ///         if (hashmap_contains_path(&pl->parent_hashmap, &parent_pathname)) …
    ///         if (hashmap_contains_parent(&pl->recursive_hashmap, pathname, &parent_pathname)) …
    /// ```
    ///
    /// The leading slash is the part that matters and the part a shortcut gets
    /// wrong: git prepends one to the path and stores its patterns with one, so
    /// `/root.txt` becomes `//root.txt`, whose parent is `/` — a name that is in
    /// neither hashmap, because the root is answered by the `!slash_pos` shortcut
    /// rather than by an entry. Reading the parent directory as "everything before
    /// the last slash" instead made `check-rules` answer `/root.txt` the same way
    /// as `root.txt`, and stock prints only the second.
    fn matches(&self, path: &str) -> bool {
        // dir.c:1584 — `if (!*path … ) return 1`.
        path.is_empty() || self.match_result(path) != PatternMatch::NotMatched
    }

    /// The cone branch of `path_matches_pattern_list()` itself, with the answer it
    /// gives rather than whether that answer includes the path.
    fn match_result(&self, path: &str) -> PatternMatch {
        if self.full {
            return PatternMatch::Matched;
        }
        let mut parent = String::with_capacity(path.len() + 2);
        parent.push('/');
        parent.push_str(path);
        // A directory entry is answered by a fake file inside it.
        let slash_pos = if parent.ends_with('/') {
            let pos = parent.len() - 1;
            parent.push('-');
            pos
        } else {
            parent.rfind('/').unwrap_or(0)
        };
        if self.has_recursive(&parent) {
            return PatternMatch::MatchedRecursive;
        }
        if slash_pos == 0 {
            return PatternMatch::Matched;
        }
        if self.has_parent(&parent[..slash_pos]) {
            return PatternMatch::Matched;
        }
        if self.contains_parent(path) {
            PatternMatch::MatchedRecursive
        } else {
            PatternMatch::NotMatched
        }
    }

    /// `hashmap_contains_path(&pl->recursive_hashmap, …)` for a name carrying
    /// git's leading slash; the set here holds the same names without it.
    fn has_recursive(&self, slashed: &str) -> bool {
        slashed.strip_prefix('/').is_some_and(|n| self.set_contains(&self.recursive, n))
    }

    /// `hashmap_contains_path(&pl->parent_hashmap, …)`. The root is never an
    /// entry there — `add_pattern_to_hashsets()` only ever moves a *named*
    /// directory across from the recursive set — so the empty name never matches,
    /// even though [`Cone::new`] keeps it in the set for `cone_lines`.
    fn has_parent(&self, slashed: &str) -> bool {
        slashed
            .strip_prefix('/')
            .is_some_and(|n| !n.is_empty() && self.set_contains(&self.parents, n))
    }

    /// `hashmap_contains_parent()` (dir.c:945-968): every proper directory prefix
    /// of `path`, longest first, stopping before the root.
    fn contains_parent(&self, path: &str) -> bool {
        let mut buf = if path.starts_with('/') { path.to_owned() } else { format!("/{path}") };
        loop {
            let Some(pos) = buf.rfind('/') else { return false };
            if pos == 0 {
                return false;
            }
            buf.truncate(pos);
            if self.has_recursive(&buf) {
                return true;
            }
        }
    }
}

/// Vet and normalize one cone-mode `set`/`add` argument.
///
/// The outer `Result` is for I/O-shaped failures; the inner one carries git's
/// own fatal (already reported) as the exit code to return. `Ok(Ok(None))`
/// means the argument named the repository root and contributes nothing.
fn cone_argument(raw: &str, prefix: &str, _skip_checks: bool) -> Result<Result<Option<String>, ExitCode>> {
    // The vetting that used to live here is `sanitize_paths()`, which git runs as
    // its own pass over the whole argument list before any of them is turned into
    // a pattern — see [`sanitize_paths`].
    // A leading double quote marks a C-style quoted path.
    let unquoted = unquote_c(raw.as_bytes())?;
    let s = c_string(&unquoted);
    let joined = match prefix.is_empty() {
        true => s,
        false => normalize_dir(prefix, &s).unwrap_or(s),
    };
    Ok(cone_pattern(&joined))
}

/// A cone-mode `set`/`add` argument from the command line, which
/// `add_patterns_from_input()` hands to `strbuf_to_cone_pattern()` as it is — no
/// C-style unquoting, that is for standard-input lines only
/// (builtin/sparse-checkout.c:608-612) — after `sanitize_paths()` put the prefix
/// in front of it (`prefix_path()`, :739-749).
fn cone_argument_verbatim(raw: &str, prefix: &str) -> Result<Result<Option<String>, ExitCode>> {
    let joined = match prefix.is_empty() {
        true => raw.to_owned(),
        false => normalize_dir(prefix, raw).unwrap_or_else(|| raw.to_owned()),
    };
    Ok(cone_pattern(&joined))
}

/// `strbuf_to_cone_pattern()` (builtin/sparse-checkout.c:559-575): trim the
/// whitespace around the path (`strbuf_trim()`, git's `isspace()` being space,
/// tab, newline and carriage return), drop trailing slashes, normalize, and die
/// on a path that climbs out of the worktree. `None` for the root.
fn cone_pattern(line: &str) -> Result<Option<String>, ExitCode> {
    let trimmed = line.trim_matches([' ', '\t', '\n', '\r']).trim_end_matches('/');
    match normalize_dir("", trimmed) {
        Some(d) if d.is_empty() => Ok(None),
        Some(d) => Ok(Some(d)),
        None => {
            eprintln!("fatal: could not normalize path {trimmed}");
            Err(ExitCode::from(128))
        }
    }
}

/// The lines `strbuf_getline()` (`sep == b'\n'`, which also drops a `\r` before the
/// newline) or `strbuf_getline_nul()` reads: a final line without a terminator is
/// kept, nothing after a final terminator is a line.
fn getlines(input: &[u8], sep: u8) -> Vec<Vec<u8>> {
    let mut lines: Vec<Vec<u8>> = input.split(|&b| b == sep).map(<[u8]>::to_vec).collect();
    if input.is_empty() || input.ends_with(&[sep]) {
        lines.pop();
    }
    if sep == b'\n' {
        for line in &mut lines {
            if line.ends_with(b"\r") {
                line.pop();
            }
        }
    }
    lines
}

/// `line.buf` read as the C string git hands on: everything up to the first NUL.
fn c_string(line: &[u8]) -> String {
    let end = line.iter().position(|&b| b == 0).unwrap_or(line.len());
    line[..end].to_str_lossy().into_owned()
}

/// `parse_options()` ends option parsing at the first `--` or `--end-of-options`
/// and drops it (parse-options.c:1113-1124): the arguments before it are options
/// and arguments mixed, the ones after it are all arguments.
fn split_at_end_of_options(args: &[String]) -> (&[String], &[String]) {
    match args.iter().position(|a| a == "--" || a == "--end-of-options") {
        Some(at) => (&args[..at], &args[at + 1..]),
        None => (args, &[]),
    }
}

/// Port of `sanitize_paths()` (builtin/sparse-checkout.c:725-783), the pass
/// `set` and `add` run over their whole argument list before any argument becomes
/// a pattern.
///
/// The order is git's and each step is load-bearing:
///
/// 1. `if (!args->nr) return;` — no arguments, no checks at all. So
///    `sparse-checkout set --no-cone` from a subdirectory does *not* die on the
///    toplevel rule below; it has nothing to check.
/// 2. In cone mode a non-empty `prefix` is prepended to every argument.
/// 3. `if (skip_checks) return;` — everything from here down is opt-out.
/// 4. Non-cone with a prefix: `please run from the toplevel directory in non-cone
///    mode`. This sits *below* the `--skip-checks` return, so the flag suppresses
///    it too.
/// 5. Cone only: the three pattern-shape refusals. Note the character set is
///    `strpbrk(args->v[i], "*?[]")` — a backslash is *not* in it, even though the
///    message lists one, so `set 'a\b'` is accepted.
/// 6. Both dialects: an argument that is an exact index entry, and not a sparse
///    directory, names a *file*. Cone mode dies; non-cone warns and carries on.
///
/// Returns `Some(code)` when git would have died, having already reported it.
fn sanitize_paths(
    repo: &gix::Repository,
    args: &[String],
    prefix: &str,
    cone: bool,
    skip_checks: bool,
) -> Result<Option<ExitCode>> {
    if args.is_empty() {
        return Ok(None);
    }
    let prefixed: Vec<String> = if cone && !prefix.is_empty() {
        args.iter()
            .map(|a| normalize_dir(prefix, a).unwrap_or_else(|| a.clone()))
            .collect()
    } else {
        args.to_vec()
    };
    if skip_checks {
        return Ok(None);
    }
    if !cone && !prefix.is_empty() {
        eprintln!("fatal: please run from the toplevel directory in non-cone mode");
        return Ok(Some(ExitCode::from(128)));
    }
    if cone {
        for a in &prefixed {
            if a.starts_with('/') {
                eprintln!("fatal: specify directories rather than patterns (no leading slash)");
                return Ok(Some(ExitCode::from(128)));
            }
            if a.starts_with('!') {
                eprintln!("fatal: specify directories rather than patterns.  If your directory starts with a '!', pass --skip-checks");
                return Ok(Some(ExitCode::from(128)));
            }
            if a.contains(['*', '?', '[', ']']) {
                eprintln!("fatal: specify directories rather than patterns.  If your directory really has any of '*?[]\\' in it, pass --skip-checks");
                return Ok(Some(ExitCode::from(128)));
            }
        }
    }
    let index = crate::index_open::or_empty(repo)?;
    for a in &prefixed {
        let Some(entry) = index.entry_by_path(BString::from(a.as_str()).as_ref()) else {
            continue;
        };
        // `S_ISSPARSEDIR(ce->ce_mode)`: a sparse-index directory entry is exactly
        // the directory the caller meant, so it is not the mistake this catches.
        if entry.mode.is_sparse() {
            continue;
        }
        if cone {
            eprintln!(
                "fatal: '{a}' is not a directory; to treat it as a directory anyway, rerun with --skip-checks"
            );
            return Ok(Some(ExitCode::from(128)));
        }
        eprintln!(
            "warning: pass a leading slash before paths such as '{a}' if you want a single file (see NON-CONE PROBLEMS in the git-sparse-checkout manual)."
        );
    }
    Ok(None)
}

/// Resolve `raw` against the worktree-relative `prefix`, collapsing `.` and
/// `..`. `None` means the path climbed out of the worktree, which git refuses.
fn normalize_dir(prefix: &str, raw: &str) -> Option<String> {
    let joined = if raw.starts_with('/') {
        raw.trim_start_matches('/').to_owned()
    } else if prefix.is_empty() {
        raw.to_owned()
    } else {
        format!("{prefix}/{raw}")
    };
    let mut comps: Vec<&str> = Vec::new();
    for comp in joined.split('/') {
        match comp {
            "" | "." => {}
            ".." => {
                comps.pop()?;
            }
            other => comps.push(other),
        }
    }
    Some(comps.join("/"))
}

/// Where the process sits inside the worktree, as a repo-relative directory.
fn worktree_prefix(repo: &gix::Repository) -> String {
    let (Some(workdir), Ok(cwd)) = (repo.workdir(), std::env::current_dir()) else {
        return String::new();
    };
    let (Ok(workdir), Ok(cwd)) = (workdir.canonicalize(), cwd.canonicalize()) else {
        return String::new();
    };
    match cwd.strip_prefix(&workdir) {
        Ok(rel) => rel.to_string_lossy().replace('\\', "/"),
        Err(_) => String::new(),
    }
}

/// Drop any directory whose ancestor is already present — a recursive parent
/// already covers it, and git's cone writer collapses them the same way
/// (`set a a/b` writes only `/a/`).
fn dedup_nested(dirs: BTreeSet<String>) -> BTreeSet<String> {
    dirs.iter()
        .filter(|d| {
            !dirs
                .iter()
                .any(|o| o.as_str() != d.as_str() && d.starts_with(&format!("{o}/")))
        })
        .cloned()
        .collect()
}

// --- pattern file ----------------------------------------------------------

fn pattern_path(repo: &gix::Repository) -> PathBuf {
    repo.git_dir().join("info").join("sparse-checkout")
}

fn read_pattern_file(repo: &gix::Repository) -> Result<Vec<String>> {
    let path = pattern_path(repo);
    let Ok(text) = std::fs::read_to_string(&path) else {
        return Ok(Vec::new());
    };
    Ok(text.lines().map(str::to_owned).collect())
}

/// Recover the recursive directory set from a cone pattern file.
///
/// Positive `/<dir>/` lines name both parents and recursive directories; the
/// parents are exactly those that also carry a `!/<dir>/*/` exclusion (and the
/// root, `!/*/`). The difference is the recursive set.
fn cone_dirs(lines: &[String]) -> BTreeSet<String> {
    let mut positive = BTreeSet::new();
    let mut parent = BTreeSet::new();
    for l in lines {
        let l = l.trim();
        if l == "/*" || l.is_empty() {
            continue;
        }
        if l == "!/*/" {
            parent.insert(String::new());
        } else if let Some(inner) = l.strip_prefix("!/").and_then(|r| r.strip_suffix("/*/")) {
            parent.insert(unescape_cone(inner));
        } else if let Some(inner) = l.strip_prefix('/').and_then(|r| r.strip_suffix('/')) {
            positive.insert(unescape_cone(inner));
        }
    }
    positive.difference(&parent).cloned().collect()
}

/// The pattern list `add_patterns_from_buffer()` (dir.c:1231-1259) builds from a
/// pattern file's bytes, each pattern as `add_pattern()` was handed it: an empty
/// file is no pattern at all (dir.c:1183-1191), a missing final newline is
/// supplied (:1198), a UTF-8 BOM is skipped, empty lines and `#` lines are
/// dropped, a `\r` before the newline is cut, and `trim_trailing_spaces()` runs
/// over what is left. The line is a C string from there on, so it ends at its
/// first NUL.
fn file_patterns(bytes: &[u8]) -> Vec<Vec<u8>> {
    if bytes.is_empty() {
        return Vec::new();
    }
    let mut buf = bytes.to_vec();
    buf.push(b'\n');
    let start = if buf.starts_with(b"\xef\xbb\xbf") { 3 } else { 0 };
    let mut out = Vec::new();
    let mut entry = start;
    for i in start..buf.len() {
        if buf[i] != b'\n' {
            continue;
        }
        if entry != i && buf[entry] != b'#' {
            let end = if buf[i - 1] == b'\r' { i - 1 } else { i };
            let mut line = buf[entry..end].to_vec();
            if let Some(nul) = line.iter().position(|&b| b == 0) {
                line.truncate(nul);
            }
            trim_trailing_spaces(&mut line);
            out.push(line);
        }
        entry = i + 1;
    }
    out
}

/// `trim_trailing_spaces()` (dir.c:1034-1055): drop a run of trailing spaces that
/// no backslash escapes.
fn trim_trailing_spaces(line: &mut Vec<u8>) {
    let mut last_space: Option<usize> = None;
    let mut i = 0;
    while i < line.len() {
        match line[i] {
            b' ' => {
                last_space.get_or_insert(i);
            }
            b'\\' => {
                i += 1;
                if i >= line.len() {
                    return;
                }
                last_space = None;
            }
            _ => last_space = None,
        }
        i += 1;
    }
    if let Some(at) = last_space {
        line.truncate(at);
    }
}

/// `is_glob_special()`: the characters `GIT_GLOB_SPECIAL` marks.
fn is_glob_special(b: u8) -> bool {
    matches!(b, b'*' | b'?' | b'[' | b'\\')
}

/// `dup_and_filter_pattern()` (dir.c:757-785): the pattern with each escaping
/// backslash dropped, and a trailing `/*` cut once more than two characters remain.
fn dup_and_filter_pattern(pattern: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(pattern.len());
    let mut i = 0;
    while i < pattern.len() {
        if pattern[i] == b'\\' {
            i += 1;
        }
        if i >= pattern.len() {
            // `*read` is the terminating NUL: copied, and the loop ends there.
            break;
        }
        out.push(pattern[i]);
        i += 1;
    }
    if out.len() > 2 && out.ends_with(b"/*") {
        out.truncate(out.len() - 2);
    }
    out
}

/// One of `pl->recursive_hashmap` / `pl->parent_hashmap`: entries compared by
/// `pl_hashmap_cmp()`, which is `fspathncmp()` and so folds case under
/// `core.ignoreCase`. `hashmap_add()` keeps duplicates and
/// `hashmap_remove_entry()` takes one away, hence the count; the stored spelling
/// is the first one added.
#[derive(Default)]
struct PatternMap {
    entries: std::collections::BTreeMap<Vec<u8>, (Vec<u8>, usize)>,
    ignore_case: bool,
}

impl PatternMap {
    fn key(&self, pattern: &[u8]) -> Vec<u8> {
        if self.ignore_case {
            pattern.to_ascii_lowercase()
        } else {
            pattern.to_vec()
        }
    }
    fn contains(&self, pattern: &[u8]) -> bool {
        self.entries.contains_key(&self.key(pattern))
    }
    fn add(&mut self, pattern: Vec<u8>) {
        let key = self.key(&pattern);
        self.entries.entry(key).or_insert((pattern, 0)).1 += 1;
    }
    fn remove_one(&mut self, pattern: &[u8]) {
        let key = self.key(pattern);
        if let Some(slot) = self.entries.get_mut(&key) {
            slot.1 -= 1;
            if slot.1 == 0 {
                self.entries.remove(&key);
            }
        }
    }
}

/// The cone half of loading a pattern list, `add_pattern_to_hashsets()`
/// (dir.c:798-936) run over each pattern `add_pattern()` parses
/// (`parse_path_pattern()`, dir.c:702-738): the recursive directories of a
/// well-formed cone file, or `None` once a pattern is not one — after the
/// warnings git prints for it, which are appended to `warnings`. From that pattern
/// on the list is matched as a non-cone list (`pl->use_cone_patterns = 0`), and the
/// patterns after it are not looked at.
fn cone_hashsets(patterns: &[Vec<u8>], ignore_case: bool, warnings: &mut String) -> Option<Cone> {
    let mut full = false;
    let mut recursive = PatternMap { ignore_case, ..Default::default() };
    let mut parent = PatternMap { ignore_case, ..Default::default() };
    for raw in patterns {
        // `parse_path_pattern()`.
        let (negative, p) = match raw.strip_prefix(b"!") {
            Some(rest) => (true, rest),
            None => (false, raw.as_slice()),
        };
        let (mustbedir, pattern) = match p.strip_suffix(b"/") {
            Some(rest) => (true, rest),
            None => (false, p),
        };
        let nodir = !pattern.contains(&b'/');
        let endswith = p.first() == Some(&b'*') && !p[1..].iter().any(|&b| is_glob_special(b));
        let no_flags = !negative && !mustbedir && !nodir && !endswith;
        let shown = pattern.to_str_lossy();

        if negative && mustbedir && pattern == b"/*" {
            full = false;
            continue;
        }
        if no_flags && pattern == b"/*" {
            full = true;
            continue;
        }
        let unrecognized = |warnings: &mut String, what: &str| {
            warnings.push_str(&format!("warning: {what}: '{shown}'\n"));
            warnings.push_str("warning: disabling cone pattern matching\n");
        };
        if pattern.len() < 2 || pattern[0] != b'/' || pattern.find(b"**").is_some() {
            unrecognized(warnings, "unrecognized pattern");
            return None;
        }
        if !mustbedir && pattern != b"/*" {
            unrecognized(warnings, "unrecognized pattern");
            return None;
        }
        // Glob characters are refused unless escaped, an escaping `\` itself, or the
        // trailing `/*`.
        for i in 1..pattern.len() {
            let (prev, cur) = (pattern[i - 1], pattern[i]);
            let next = pattern.get(i + 1).copied().unwrap_or(0);
            if !is_glob_special(cur)
                || prev == b'\\'
                || (cur == b'\\' && is_glob_special(next))
                || (prev == b'/' && cur == b'*' && next == 0)
            {
                continue;
            }
            unrecognized(warnings, "unrecognized pattern");
            return None;
        }
        if pattern.len() > 2 && pattern.ends_with(b"/*") {
            if !negative {
                unrecognized(warnings, "unrecognized pattern");
                return None;
            }
            let truncated = dup_and_filter_pattern(pattern);
            if !recursive.contains(&truncated) {
                unrecognized(warnings, "unrecognized negative pattern");
                return None;
            }
            recursive.remove_one(&truncated);
            parent.add(truncated);
            continue;
        }
        if negative {
            unrecognized(warnings, "unrecognized negative pattern");
            return None;
        }
        let translated = dup_and_filter_pattern(pattern);
        let repeated = parent.contains(&translated);
        recursive.add(translated);
        if repeated {
            warnings.push_str(&format!(
                "warning: your sparse-checkout file may have issues: pattern '{shown}' is repeated\n"
            ));
            warnings.push_str("warning: disabling cone pattern matching\n");
            return None;
        }
    }
    let dirs = recursive
        .entries
        .into_values()
        .map(|(pattern, _)| pattern[1..].to_str_lossy().into_owned())
        .collect();
    let mut cone = Cone::new(dirs);
    cone.full = full;
    cone.ignore_case = ignore_case;
    Some(cone)
}

/// The worktree's pattern file loaded as `get_sparse_checkout_patterns()`
/// (dir.c:3512-3523) loads it, with `use_cone_patterns` set to `cone`: a cone
/// file becomes its directories, anything else the list of patterns, and the
/// warnings a non-cone file draws in cone mode are returned for the caller to
/// print where git prints them. `None` when the file cannot be read.
fn load_pattern_file(repo: &gix::Repository, cone: bool) -> Result<Option<(Sparsity, String)>> {
    let Ok(bytes) = std::fs::read(pattern_path(repo)) else {
        return Ok(None);
    };
    let patterns = file_patterns(&bytes);
    let mut warnings = String::new();
    if cone {
        let ignore_case = repo.config_snapshot().boolean("core.ignoreCase").unwrap_or(false);
        if let Some(cone) = cone_hashsets(&patterns, ignore_case, &mut warnings) {
            return Ok(Some((Sparsity::Cone(cone), warnings)));
        }
    }
    let lines: Vec<String> = patterns.iter().map(|p| p.to_str_lossy().into_owned()).collect();
    Ok(Some((Sparsity::Patterns(parse_patterns(&lines)), warnings)))
}

/// The definition `update_working_directory(repo, NULL)` works from: `update_sparsity()`
/// loads the recorded patterns (`populate_from_existing_patterns()`), and the warnings
/// that load draws are returned with them, for [`apply_reloaded`] to print a second
/// time where `clean_tracked_sparse_directories()` loads the file again
/// (builtin/sparse-checkout.c:129-131).
fn load_for_update(repo: &gix::Repository, cone: bool) -> Result<(Sparsity, String)> {
    Ok(match load_pattern_file(repo, cone)? {
        Some(loaded) => loaded,
        None if cone => (Sparsity::Cone(Cone::new(BTreeSet::new())), String::new()),
        None => (Sparsity::Patterns(Vec::new()), String::new()),
    })
}

/// Render `cone` in git's cone layout: the root pair, then a
/// `/<parent>/` + `!/<parent>/*/` pair per ancestor, then the recursive lines.
fn cone_lines(cone: &Cone) -> Vec<String> {
    let mut out = vec!["/*".to_owned(), "!/*/".to_owned()];
    for p in &cone.parents {
        if p.is_empty() {
            continue; // the root pair is already written
        }
        let p = escape_cone(p);
        out.push(format!("/{p}/"));
        out.push(format!("!/{p}/*/"));
    }
    for d in &cone.recursive {
        if d.is_empty() {
            continue;
        }
        out.push(format!("/{}/", escape_cone(d)));
    }
    out
}

/// git escapes the glob metacharacters it would otherwise interpret when it
/// writes a directory name into a cone pattern. `]` is left alone: it is only
/// special after an unescaped `[`, which is itself escaped here.
fn escape_cone(dir: &str) -> String {
    let mut out = String::with_capacity(dir.len());
    for c in dir.chars() {
        if matches!(c, '*' | '?' | '[' | '\\') {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

fn unescape_cone(pattern: &str) -> String {
    let mut out = String::with_capacity(pattern.len());
    let mut chars = pattern.chars();
    while let Some(c) = chars.next() {
        if c == '\\' {
            if let Some(next) = chars.next() {
                out.push(next);
            }
        } else {
            out.push(c);
        }
    }
    out
}

fn write_pattern_file(repo: &gix::Repository, lines: &[String]) -> Result<()> {
    let mut out = String::new();
    for l in lines {
        out.push_str(l);
        out.push('\n');
    }

    let path = pattern_path(repo);
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension("zvcs-tmp");
    {
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(out.as_bytes())?;
        f.sync_all()?;
    }
    std::fs::rename(&tmp, &path)?;
    Ok(())
}

// --- config ----------------------------------------------------------------

fn worktree_config_path(repo: &gix::Repository) -> PathBuf {
    repo.git_dir().join("config.worktree")
}

/// Read a boolean from the worktree config, falling back to the repo-local one.
///
/// git reads `core.sparseCheckout` and `core.sparseCheckoutCone` through the
/// ordinary config machinery (`git_config_get_bool()`), so the highest-precedence
/// sources — `-c <key>=<value>` on the command line and the `GIT_CONFIG_*`
/// environment — answer before any file does. Reading the two files alone made
/// `git -c core.sparseCheckoutCone=false sparse-checkout list` report the cone
/// listing of a worktree whose config file still said `true`.
///
/// Below that the file order stands as it was: the worktree config outranks the
/// repository's own, which is the order git's `config.worktree` scope has.
fn config_bool(repo: &gix::Repository, section: &str, key: &str) -> Result<Option<bool>> {
    {
        let snapshot = repo.config_snapshot();
        let overridden = snapshot
            .plumbing()
            .boolean_filter(format!("{section}.{key}").as_str(), |meta| {
                matches!(meta.source, Source::Cli | Source::Env)
            });
        if let Ok(Some(v)) = overridden {
            return Ok(Some(v));
        }
    }
    for path in [worktree_config_path(repo), repo.common_dir().join("config")] {
        if !path.exists() {
            continue;
        }
        let file = ConfigFile::from_path_no_includes(path, Source::Local)?;
        if let Ok(v) = file.raw_value_by(section, None, key) {
            let v = v.to_str_lossy().to_ascii_lowercase();
            return Ok(Some(!matches!(v.as_str(), "false" | "no" | "off" | "0" | "")));
        }
    }
    Ok(None)
}

fn is_sparse(repo: &gix::Repository) -> Result<bool> {
    Ok(config_bool(repo, "core", "sparseCheckout")?.unwrap_or(false))
}

/// `cfg->core_sparse_checkout_cone` — nothing more than the config value.
/// `environment.c:554-556` sets it from `core.sparseCheckoutCone` and nothing
/// initialises it to anything but 0, so an **unset** key means non-cone. The
/// commands that accept `--[no-]cone` reach cone by a different route; see
/// [`update_cone_mode`].
fn is_cone(repo: &gix::Repository) -> Result<bool> {
    Ok(config_bool(repo, "core", "sparseCheckoutCone")?.unwrap_or(false))
}

/// Port of `update_cone_mode()` (builtin/sparse-checkout.c:401-416).
///
/// ```c
///         /* If not specified, use previous definition of cone mode */
///         if (*cone_mode == -1 && cfg->apply_sparse_checkout)
///                 *cone_mode = cfg->core_sparse_checkout_cone;
///
///         /* Set cone/non-cone mode appropriately */
///         cfg->apply_sparse_checkout = 1;
///         if (*cone_mode == 1 || *cone_mode == -1) {
///                 cfg->core_sparse_checkout_cone = 1;
///                 return MODE_CONE_PATTERNS;
///         }
///         cfg->core_sparse_checkout_cone = 0;
///         return MODE_ALL_PATTERNS;
/// ```
///
/// `requested` is git's `cone_mode`: `None` is its `-1`. An unspecified flag
/// therefore keeps whatever an already-sparse worktree is using — which for a
/// worktree made sparse by hand (`git config core.sparseCheckout true`) is
/// **non-cone**, since `core.sparseCheckoutCone` is unset — and only falls to cone
/// when there is no definition to keep. Defaulting to cone unconditionally made
/// `sparse-checkout set src` write a cone over a hand-enabled non-cone worktree.
fn update_cone_mode(repo: &gix::Repository, requested: Option<bool>) -> Result<bool> {
    Ok(match requested {
        Some(v) => v,
        None if is_sparse(repo)? => is_cone(repo)?,
        None => true,
    })
}

/// Load (creating if absent) and mutate a config file, then persist atomically.
fn edit_config(path: &Path, edits: &[(&str, &str, &str)]) -> Result<()> {
    if !path.exists() {
        std::fs::write(path, b"")?;
    }
    let mut file = ConfigFile::from_path_no_includes(path.to_path_buf(), Source::Local)?;
    for (section, key, value) in edits {
        file.set_raw_value_by(*section, None, *key, *value)?;
    }
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

/// Turn sparsity on exactly where git puts it: the per-worktree config, with
/// `extensions.worktreeConfig` opted in on the shared local config.
/// `sparse_index` is written only when the caller passed `--[no-]sparse-index`,
/// matching git, which leaves `index.sparse` untouched otherwise.
/// `update_modes()`'s two config writes (builtin/sparse-checkout.c:418-441).
///
/// `set_config()` — the `core.sparseCheckout` / `core.sparseCheckoutCone` pair and
/// the `extensions.worktreeConfig` its `init_worktree_config()` turns on — runs
/// only when `record_mode` says so. `set_sparse_index_config()` is separate and
/// runs whenever `--[no-]sparse-index` was given, whatever `record_mode` decided.
fn enable_config(
    repo: &gix::Repository,
    cone: bool,
    sparse_index: Option<bool>,
    record_mode: bool,
) -> Result<()> {
    if !record_mode && sparse_index.is_none() {
        return Ok(());
    }
    let _lock = crate::lock::RepoLock::acquire(repo.git_dir());
    edit_config(
        &repo.common_dir().join("config"),
        &[("extensions", "worktreeConfig", "true")],
    )?;
    let mut edits: Vec<(&str, &str, &str)> = Vec::new();
    if record_mode {
        edits.push(("core", "sparseCheckout", "true"));
        edits.push(("core", "sparseCheckoutCone", if cone { "true" } else { "false" }));
    }
    if let Some(si) = sparse_index {
        edits.push(("index", "sparse", if si { "true" } else { "false" }));
    }
    edit_config(&worktree_config_path(repo), &edits)
}

fn disable_config(repo: &gix::Repository) -> Result<()> {
    let _lock = crate::lock::RepoLock::acquire(repo.git_dir());
    edit_config(
        &repo.common_dir().join("config"),
        &[("extensions", "worktreeConfig", "true")],
    )?;
    edit_config(
        &worktree_config_path(repo),
        &[
            ("core", "sparseCheckout", "false"),
            ("core", "sparseCheckoutCone", "false"),
            ("index", "sparse", "false"),
        ],
    )
}

// --- applying sparsity to index + worktree ---------------------------------

/// One index entry's identity, snapshotted so the mutable pass below holds no
/// borrow on the path backing.
struct Snapshot {
    path: BString,
    unmerged: bool,
    was_skipped: bool,
}

/// Reconcile the index `SKIP_WORKTREE` bits and the worktree files with
/// `sparsity`.
fn apply(repo: &gix::Repository, sparsity: &Sparsity) -> Result<()> {
    apply_reloaded(repo, sparsity, "")
}

/// [`apply`] for a definition `update_working_directory()` loaded from the pattern
/// file itself: `load_warnings` is what each of its two loads prints, the first
/// before the index is touched and the second where
/// `clean_tracked_sparse_directories()` reloads the file. An unborn index loads
/// nothing.
fn apply_reloaded(repo: &gix::Repository, sparsity: &Sparsity, load_warnings: &str) -> Result<()> {
    let workdir = repo
        .workdir()
        .ok_or_else(|| crate::fatal::need_work_tree())?
        .to_owned();

    // The settings this subcommand just wrote, as git holds them in-process from here on:
    // `disable` runs with `repo->settings.sparse_index = 0` (builtin/sparse-checkout.c:1083-1084),
    // the others with the `index.sparse` and cone mode `enable_config()` recorded. The
    // repository's configuration snapshot predates those writes.
    crate::sparse_index::override_settings(match sparsity {
        Sparsity::Full => crate::sparse_index::Settings {
            apply_sparse_checkout: true,
            cone: false,
            sparse_index: false,
        },
        _ => crate::sparse_index::Settings {
            apply_sparse_checkout: true,
            cone: sparsity.is_cone(),
            sparse_index: config_bool(repo, "index", "sparse")?.unwrap_or(false),
        },
    });

    let _lock = crate::lock::RepoLock::acquire(repo.git_dir());
    // `update_working_directory()` (builtin/sparse-checkout.c:203-210):
    //
    // ```c
    // /* If no branch has been checked out, there are no updates to make. */
    // if (is_index_unborn(r->index))
    //         return UPDATE_SPARSITY_SUCCESS;
    // ```
    //
    // — and `is_index_unborn()` is `!istate->cache_nr && !istate->timestamp.sec`
    // (read-cache.c:2529-2532), i.e. no entries *and* no index file was read from
    // disk. An index file that exists and happens to be empty is not unborn; a
    // missing one is. Without the guard a `clone -n --sparse` (and a `--sparse`
    // whose remote `HEAD` cannot be resolved, which checks nothing out either)
    // grew a 65-byte index whose only content was a cache tree for the empty
    // tree, an object no repository stores — which stock `git fsck --strict`
    // then reports as `missing tree 4b825dc…` and exits 2 on.
    let index_file_exists = repo.index_path().exists();
    let mut index = crate::index_open::or_empty(repo)?;
    if index.entries().is_empty() && !index_file_exists {
        return Ok(());
    }
    eprint!("{load_warnings}");

    // A sparse index written by stock git stores each wholly-excluded directory as one
    // `040000` entry. `update_sparsity()` expands it before marking anything
    // (unpack-trees.c:2157-2158, `expand_index(o->src_index, pl)`), and this port only
    // ever writes a full index (see `index_racy::convert_to_sparse`), so every such
    // entry is expanded here. Without it the directory entry was checked out as if it
    // were a file, and a write left `040000` entries behind with no `sdir` extension.
    //
    // git's advice follows `expand_index()`: a cone pattern list expands in place and
    // says nothing, a non-cone one is `pl = NULL`, and so is `--no-sparse-index`
    // (`update_modes()`'s `ensure_full_index()`, builtin/sparse-checkout.c:432-441) —
    // both announce `advice.sparseIndexExpanded` (sparse-index.c:363-366). `disable`
    // clears `give_advice_on_expansion` first (builtin/sparse-checkout.c:1071).
    //
    // The expansion is unconditional on the index being collapsed, not on it holding a sparse
    // directory: reading an index with `index.sparse` on marks it `INDEX_COLLAPSED` even when
    // nothing collapsed yet (every entry still in the cone), and `expand_index()` is what turns
    // that back into `INDEX_EXPANDED` so the write below runs `convert_to_sparse()`.
    let advise = match sparsity {
        Sparsity::Full => false,
        Sparsity::Patterns(_) => true,
        Sparsity::Cone(_) => !config_bool(repo, "index", "sparse")?.unwrap_or(false),
    };
    crate::sparse_index::ensure_full_index_with_advice(repo, &mut index, advise);

    let snapshot: Vec<Snapshot> = {
        let backing = index.path_backing();
        index
            .entries()
            .iter()
            .map(|e| Snapshot {
                path: e.path_in(backing).to_owned(),
                unmerged: e.stage_raw() != 0,
                was_skipped: e.flags.contains(Flags::SKIP_WORKTREE),
            })
            .collect()
    };

    let mut to_materialize: Vec<BString> = Vec::new();
    let mut to_remove: Vec<BString> = Vec::new();
    let mut unmerged: Vec<BString> = Vec::new();
    // `verify_uptodate_sparse()` rejecting an entry `apply_sparse_checkout()` was
    // about to hide (unpack-trees.c:565-571, :2282): the skip bit is put back and
    // the path is queued for `WARNING_SPARSE_NOT_UPTODATE_FILE`.
    let mut not_uptodate: Vec<BString> = Vec::new();
    let stat_check = StatCheck::new(repo)?;

    for (i, snap) in snapshot.iter().enumerate() {
        // git never sparsifies an unmerged path: it leaves the conflict alone
        // and tells the user to resolve it and reapply.
        if snap.unmerged {
            if unmerged.last() != Some(&snap.path) {
                unmerged.push(snap.path.clone());
            }
            continue;
        }

        let included = sparsity.includes(&snap.path.to_str_lossy());
        let disk = repo.workdir_path(snap.path.as_bstr());
        let exists = disk
            .as_ref()
            .map(|p| p.symlink_metadata().is_ok())
            .unwrap_or(false);
        // `if (!was_skip_worktree && ce_skip_worktree(ce))` asks `verify_uptodate_sparse()`
        // (unpack-trees.c:566-580) before the entry may leave the worktree.
        let rejected = !included
            && !snap.was_skipped
            && disk
                .as_ref()
                .is_some_and(|p| verify_uptodate_sparse(&index, i, p, &stat_check));

        let entry = &mut index.entries_mut()[i];
        if included {
            entry.flags.remove(Flags::SKIP_WORKTREE);
            if !entry.flags.contains(Flags::INTENT_TO_ADD) {
                entry.flags.remove(Flags::EXTENDED);
            }
            // Only a path that sparsity had been hiding gets written back: a
            // file the user deleted themselves stays deleted.
            if snap.was_skipped && !exists {
                to_materialize.push(snap.path.clone());
            }
        } else if snap.was_skipped {
            // ```c
            // if (was_skip_worktree && ce_skip_worktree(ce)) {
            //         ce->ce_flags &= ~CE_UPDATE;
            //         [...]
            //                 ce->ce_flags &= ~CE_WT_REMOVE;
            // }
            // ```
            //
            // (`apply_sparse_checkout()`, unpack-trees.c:553-564.) An entry that was
            // already outside the worktree area is left alone, and so is whatever file
            // sits at its path.
            entry.flags.insert(Flags::SKIP_WORKTREE | Flags::EXTENDED);
        } else {
            // git refuses to sparsify a path `verify_uptodate_sparse()` rejects: the
            // file stays, and so does its cleared skip bit (unpack-trees.c:568-572).
            if rejected {
                entry.flags.remove(Flags::SKIP_WORKTREE);
                if !entry.flags.contains(Flags::INTENT_TO_ADD) {
                    entry.flags.remove(Flags::EXTENDED);
                }
                not_uptodate.push(snap.path.clone());
            } else {
                // EXTENDED is what makes the skip bit survive serialization
                // (and forces index version 3, exactly as git does).
                entry.flags.insert(Flags::SKIP_WORKTREE | Flags::EXTENDED);
                // Queued whether or not the file is still there: git's
                // `unlink_entry()` prunes the entry's now-empty parent
                // directories even when the unlink itself finds nothing, so a
                // path the user had already deleted still takes its directory
                // with it.
                to_remove.push(snap.path.clone());
            }
        }
    }

    // `update_sparsity()` (unpack-trees.c:2132-2196) is not `unpack_trees()`: it marks the
    // source index in place and never calls `cache_tree_update()`, and
    // `update_working_directory()` writes that index as it stands
    // (builtin/sparse-checkout.c:246-247). The cache-tree read with the index is what gets
    // written, nodes an earlier `update-index` invalidated included. A collapsed index had its
    // cache-tree recomputed when `expand_index()` expanded it (sparse-index.c:463-464), which
    // `ensure_full_index_with_advice()` above did.
    //
    // `check_updates()` (unpack-trees.c:2185) writes the files `apply_sparse_checkout()`
    // marked `CE_UPDATE` before `update_working_directory()` writes the index
    // (builtin/sparse-checkout.c:246-247), and `update_ce_after_write()` fills each entry's
    // stat data from the file it wrote (entry.c:269-279), so the index records the materialized
    // files as they are on disk rather than the stat data of the entry it was before.
    if !to_materialize.is_empty() {
        let mut subset = index.clone();
        subset.remove_entries(|_, path, _| !to_materialize.iter().any(|k| k.as_bstr() == path));
        checkout_subset(repo, &mut subset)?;
        let fresh: std::collections::HashMap<BString, gix::index::entry::Stat> = {
            let backing = subset.path_backing();
            subset.entries().iter().map(|e| (e.path_in(backing).to_owned(), e.stat)).collect()
        };
        let backing = index.path_backing().to_owned();
        for e in index.entries_mut() {
            if let Some(stat) = fresh.get(e.path_in(&backing)) {
                e.stat = *stat;
            }
        }
    }

    super::write_tree::prepare_offset_table(repo, &mut index);
    crate::index_racy::write(repo, &mut index)?;

    // Cone mode alone refreshes the cache tree: git finishes
    // `update_working_directory()` with `clean_tracked_sparse_directories()`,
    // which bails immediately on a non-cone pattern list and otherwise builds an
    // in-memory sparse index — and that conversion runs `cache_tree_update()`,
    // depositing the index's root tree and every sub-tree in the odb. `disable`
    // installs a non-cone `/*` list precisely so it does not reach this.
    if matches!(sparsity, Sparsity::Cone(_)) {
        write_cache_tree(repo, &index)?;
    }

    for path in &to_remove {
        let Some(full) = repo.workdir_path(path.as_bstr()) else {
            continue;
        };
        let _ = std::fs::remove_file(&full);
        crate::worktree::prune_empty_dirs(&workdir, &full);
    }

    // `display_warning_msgs()` (unpack-trees.c:290-315) walks the warning types in
    // enum order — `WARNING_SPARSE_NOT_UPTODATE_FILE` then
    // `WARNING_SPARSE_UNMERGED_FILE` (unpack-trees.h:31-33) — prints one
    // `warning:` block per non-empty list, and closes with a single
    // `After fixing the above paths` line however many blocks there were.
    let mut msg = String::new();
    for (paths, headline) in [
        (&not_uptodate, "The following paths are not up to date and were left despite sparse patterns:"),
        (&unmerged, "The following paths are unmerged and were left despite sparse patterns:"),
    ] {
        if paths.is_empty() {
            continue;
        }
        msg.push_str("warning: ");
        msg.push_str(headline);
        msg.push('\n');
        for p in paths.iter() {
            msg.push('\t');
            msg.push_str(&p.to_str_lossy());
            msg.push('\n');
        }
        // `warning()` ends its own line, and the message already ended in one.
        msg.push('\n');
    }
    if !msg.is_empty() {
        msg.push_str("After fixing the above paths, you may want to run `git sparse-checkout reapply`.\n");
        eprint!("{msg}");
    }

    // `clean_tracked_sparse_directories()` runs *after* the working directory has
    // been updated (builtin/sparse-checkout.c:251), so every tracked file it would
    // have removed is already gone.
    eprint!("{load_warnings}");
    if let Sparsity::Cone(cone) = sparsity {
        clean_tracked_sparse_directories(&workdir, &index, cone);
    }

    Ok(())
}

/// Port of `clean_tracked_sparse_directories()` (builtin/sparse-checkout.c:115-204).
///
/// git builds an in-memory sparse index and then, for each sparse-directory entry
/// that still exists on disk, asks `fill_directory` (with `DIR_SHOW_IGNORED_TOO`)
/// whether anything is left inside:
///
/// ```c
///                 if (dir.nr) {
///                         warning(_("directory '%s' contains untracked files,"
///                                   " but is not in the sparse-checkout cone"),
///                                 item->string);
///                 } else if (remove_dir_recursively(&path, 0)) {
///                         warning(_("failed to remove directory '%s'"), item->string);
///                 }
/// ```
///
/// The entry's name carries a trailing slash, and the warning prints it that way.
/// Non-cone pattern lists never get here (:129-131) — `convert_to_sparse` needs
/// cone patterns — which is why `disable`, whose `/*` list is deliberately
/// non-cone, cleans nothing.
fn clean_tracked_sparse_directories(workdir: &Path, index: &gix::index::File, cone: &Cone) {
    for dir in sparse_dirs(index, cone) {
        let full = workdir.join(dir.trim_end_matches('/'));
        if !full.exists() {
            continue;
        }
        if dir_holds_a_file(&full) {
            eprintln!(
                "warning: directory '{dir}' contains untracked files, but is not in the sparse-checkout cone"
            );
        } else if std::fs::remove_dir_all(&full).is_err() {
            eprintln!("warning: failed to remove directory '{dir}'");
        }
    }
}

/// Whether anything `fill_directory` would count sits under `full`. Empty
/// directories are not entries, so a tree of them still counts as removable.
fn dir_holds_a_file(full: &Path) -> bool {
    let Ok(entries) = std::fs::read_dir(full) else {
        return true;
    };
    for entry in entries.flatten() {
        match entry.file_type() {
            Ok(t) if t.is_dir() => {
                if dir_holds_a_file(&entry.path()) {
                    return true;
                }
            }
            _ => return true,
        }
    }
    false
}

/// The directory entries `convert_to_sparse_rec()` (sparse-index.c) would collapse:
/// the outermost directories that the cone excludes and whose every index entry is
/// skipped, unmerged-free and not a gitlink.
///
/// ```c
///         if (path_in_sparse_checkout(ct_path, istate))
///                 can_convert = 0;
///         for (i = start; can_convert && i < end; i++) {
///                 struct cache_entry *ce = istate->cache[i];
///                 if (ce_stage(ce) || S_ISGITLINK(ce->ce_mode) ||
///                     !(ce->ce_flags & CE_SKIP_WORKTREE))
///                         can_convert = 0;
///         }
/// ```
///
/// `ct_path` carries a trailing slash and the root is `""`, which
/// `path_in_sparse_checkout` always answers "in" — so the root is never collapsed
/// and the walk always descends at least one level.
///
/// A sub-directory whose cache-tree node is invalidated is neither collapsed nor
/// descended into; its entries stay as they are (2.56, sparse-index.c:116-121):
///
/// ```c
///         span = ct->down[pos]->cache_tree->entry_count;
///         if (span < 0) {
///                 /* cache-tree entry is invalidated, cannot collapse. */
///                 istate->cache[num_converted++] = ce;
///                 i++;
///                 continue;
///         }
/// ```
///
/// `convert_to_sparse()` rebuilds the cache-tree from the entries just before
/// (sparse-index.c:231-237), and what that rebuild leaves invalidated is the chain
/// of directories above an intent-to-add entry: `update_one()` skips the entry and
/// sets `to_invalidate`, and a parent that finds a sub-tree with `entry_count < 0`
/// does the same, up to the root (cache-tree.c:436-441, :472-481, :517). So a
/// directory with an intent-to-add path anywhere below it is left alone.
fn sparse_dirs(index: &gix::index::File, cone: &Cone) -> Vec<String> {
    let backing = index.path_backing();
    let entries: Vec<SparseCandidate> = index
        .entries()
        .iter()
        .map(|e| SparseCandidate {
            path: e.path_in(backing).to_str_lossy().into_owned(),
            collapsible: e.stage_raw() == 0
                && e.mode != Mode::COMMIT
                && e.flags.contains(Flags::SKIP_WORKTREE),
            intent_to_add: e.flags.contains(Flags::INTENT_TO_ADD),
        })
        .collect();
    let mut out = Vec::new();
    collapse(&entries, 0, entries.len(), "", cone, &mut out);
    out
}

/// One index entry as `convert_to_sparse_rec()` weighs it.
struct SparseCandidate {
    path: String,
    /// Merged, not a gitlink, and `CE_SKIP_WORKTREE` (sparse-index.c:78-85).
    collapsible: bool,
    /// `CE_INTENT_TO_ADD`, which invalidates every cache-tree node above it.
    intent_to_add: bool,
}

fn collapse(
    entries: &[SparseCandidate],
    start: usize,
    end: usize,
    dir: &str,
    cone: &Cone,
    out: &mut Vec<String>,
) {
    if !dir.is_empty()
        && !cone.matches(dir)
        && entries[start..end].iter().all(|e| e.collapsible)
    {
        out.push(dir.to_owned());
        return;
    }
    let mut i = start;
    while i < end {
        let rest = &entries[i].path[dir.len()..];
        let Some(slash) = rest.find('/') else {
            i += 1;
            continue;
        };
        let child = entries[i].path[..dir.len() + slash + 1].to_owned();
        let mut j = i;
        while j < end && entries[j].path.starts_with(&child) {
            j += 1;
        }
        // `span < 0`: the child's cache-tree node is invalidated.
        if !entries[i..j].iter().any(|e| e.intent_to_add) {
            collapse(entries, i, j, &child, cone, out);
        }
        i = j;
    }
}

/// Write the index's cache tree — its root tree and every sub-tree — into the
/// odb, the side effect git's `cache_tree_update(WRITE_TREE_MISSING_OK)` has.
///
/// `MISSING_OK` means no odb presence check on the entries, so an index naming
/// an absent blob still produces its trees. `WRITE_TREE_SILENT` means the
/// refresh gives up without a word when the index cannot be turned into a tree
/// — an unmerged entry, or a mode the tree format cannot express — leaving the
/// odb untouched, which is why a conflicted worktree gains no objects here.
fn write_cache_tree(repo: &gix::Repository, index: &gix::index::File) -> Result<()> {
    let entries = index.entries();
    if entries.is_empty() || entries.iter().any(|e| e.stage_raw() != 0) {
        return Ok(());
    }
    let backing = index.path_backing();
    let mut editor =
        gix::objs::tree::Editor::new(gix::objs::Tree::empty(), &repo.objects, repo.object_hash());
    for entry in entries {
        let Some(mode) = entry.mode.to_tree_entry_mode() else {
            return Ok(());
        };
        editor.upsert(
            entry.path_in(backing).split(|&b| b == b'/').map(|c| c.as_bstr()),
            mode.kind(),
            entry.id,
        )?;
    }
    editor.write(|tree| repo.write_object(tree).map(|id| id.detach()))?;
    Ok(())
}

/// The configuration `ie_match_stat()` consults: `core.trustctime` / `core.checkStat`,
/// `core.fileMode` and `core.symlinks`.
struct StatCheck {
    opts: gix::index::entry::stat::Options,
    trust_executable_bit: bool,
    has_symlinks: bool,
}

impl StatCheck {
    fn new(repo: &gix::Repository) -> Result<Self> {
        let snapshot = repo.config_snapshot();
        Ok(StatCheck {
            opts: repo.stat_options()?,
            trust_executable_bit: snapshot.boolean("core.fileMode").unwrap_or(true),
            has_symlinks: snapshot.boolean("core.symlinks").unwrap_or(true),
        })
    }
}

/// `verify_uptodate_sparse()`, which is `verify_uptodate_1()` (unpack-trees.c:2224-2270)
/// with `WARNING_SPARSE_NOT_UPTODATE_FILE`: true when the entry at `idx` must stay in the
/// worktree. `ce_uptodate()` is an in-core mark a freshly read index never carries, so the
/// answer is the `lstat()` and `ie_match_stat()` under `CE_MATCH_IGNORE_VALID |
/// CE_MATCH_IGNORE_SKIP_WORKTREE`: a stat difference rejects the entry whatever the file
/// holds, and the content is compared only for a racily clean entry. A missing file is fine
/// (`errno == ENOENT`), any other `lstat()` failure is a rejection, and a gitlink never is
/// (`submodule_from_ce()` is NULL without submodule recursion, then the `S_ISGITLINK`
/// return).
fn verify_uptodate_sparse(index: &gix::index::File, idx: usize, full: &Path, check: &StatCheck) -> bool {
    match std::fs::symlink_metadata(full) {
        Ok(_) => {}
        Err(e) => return e.kind() != std::io::ErrorKind::NotFound,
    }
    let entry = &index.entries()[idx];
    if entry.mode == Mode::COMMIT {
        return false;
    }
    !crate::index_racy::ie_match_stat_unchanged_ignoring_valid(
        index,
        entry,
        full,
        check.opts,
        check.trust_executable_bit,
        check.has_symlinks,
    )
}

/// Write every entry of `index` into the worktree (same helper shape the other
/// worktree-mutating porcelain uses).
pub(super) fn checkout_subset(repo: &gix::Repository, index: &mut gix::index::File) -> Result<()> {
    let workdir = repo
        .workdir()
        .ok_or_else(|| anyhow!("bare repository has no worktree to update"))?
        .to_owned();
    let mut opts =
        repo.checkout_options(gix::worktree::stack::state::attributes::Source::IdMapping)?;
    opts.destination_is_initially_empty = false;
    opts.overwrite_existing = true;
    let odb = repo.objects.clone().into_arc()?;
    let should_interrupt = AtomicBool::new(false);
    crate::worktree::checkout_subset(
        index,
        workdir.as_path(),
        odb,
        &gix::progress::Discard,
        &gix::progress::Discard,
        &should_interrupt,
        opts,
    )?;
    Ok(())
}

// --- path quoting ----------------------------------------------------------

/// Undo git's C-style quoting when `input` is a quoted path; otherwise return
/// it unchanged.
fn unquote_c(input: &[u8]) -> Result<BString> {
    if !input.starts_with(b"\"") {
        return Ok(BString::from(input));
    }
    let body = input
        .strip_prefix(b"\"")
        .and_then(|r| r.strip_suffix(b"\""))
        .ok_or_else(|| anyhow!("unterminated quoted path"))?;

    let mut out: Vec<u8> = Vec::with_capacity(body.len());
    let mut it = body.iter().copied();
    while let Some(b) = it.next() {
        if b != b'\\' {
            out.push(b);
            continue;
        }
        let e = it.next().ok_or_else(|| anyhow!("trailing backslash in quoted path"))?;
        match e {
            b'a' => out.push(0x07),
            b'b' => out.push(0x08),
            b't' => out.push(b'\t'),
            b'n' => out.push(b'\n'),
            b'v' => out.push(0x0b),
            b'f' => out.push(0x0c),
            b'r' => out.push(b'\r'),
            b'"' => out.push(b'"'),
            b'\\' => out.push(b'\\'),
            d if d.is_ascii_digit() => {
                // Three-digit octal escape, the first digit already consumed.
                let d2 = it.next().ok_or_else(|| anyhow!("truncated octal escape"))?;
                let d3 = it.next().ok_or_else(|| anyhow!("truncated octal escape"))?;
                let val = u32::from(d - b'0') * 64 + u32::from(d2 - b'0') * 8 + u32::from(d3 - b'0');
                out.push(u8::try_from(val).map_err(|_| anyhow!("octal escape out of range"))?);
            }
            other => return Err(anyhow!("unknown escape \\{} in quoted path", other as char)),
        }
    }
    Ok(BString::from(out))
}

/// `quote_c_style()`: the name verbatim unless some byte needs escaping, in which
/// case the whole name double-quoted with C escapes. The table and the
/// `core.quotePath` flag it reads live in [`crate::quote`], shared with every
/// other verb that prints a path.
fn quote_path(path: impl AsRef<[u8]>) -> String {
    crate::quote::quoted_name_string(path.as_ref())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn set(items: &[&str]) -> BTreeSet<String> {
        items.iter().map(|s| (*s).to_owned()).collect()
    }

    /// The generated file must match stock git's cone layout exactly: root pair,
    /// then sorted parent pairs, then sorted recursive lines.
    #[test]
    fn cone_file_layout_round_trips() {
        let cone = Cone::new(set(&["a/b/c", "q", "z/y"]));
        let lines = cone_lines(&cone);
        assert_eq!(
            lines.join("\n") + "\n",
            "/*\n!/*/\n/a/\n!/a/*/\n/a/b/\n!/a/b/*/\n/z/\n!/z/*/\n/a/b/c/\n/q/\n/z/y/\n"
        );
        assert_eq!(cone_dirs(&lines), set(&["a/b/c", "q", "z/y"]));
    }

    /// A directory name carrying glob metacharacters is escaped on the way into
    /// the pattern file and recovered on the way out, so `list` still round-trips.
    #[test]
    fn cone_escapes_glob_metacharacters() {
        let lines = cone_lines(&Cone::new(set(&["a*b"])));
        assert_eq!(lines.last().unwrap(), "/a\\*b/");
        assert_eq!(cone_dirs(&lines), set(&["a*b"]));
    }

    /// Cone membership: files directly in an ancestor are in, files at any depth
    /// under a recursive directory are in, everything else is out.
    #[test]
    fn cone_membership() {
        let cone = Cone::new(set(&["a/b"]));
        for inside in ["top", "a/5", "a/b/4", "a/b/c/3"] {
            assert!(cone.matches(inside), "{inside} should be inside");
        }
        for outside in ["q/6", "z/2", "z/y/1", "ab/1"] {
            assert!(!cone.matches(outside), "{outside} should be outside");
        }
    }

    /// Non-cone patterns decide a path through its directory prefixes, so a bare
    /// directory name covers everything beneath it and a later negation wins.
    #[test]
    fn non_cone_membership() {
        let pats = parse_patterns(&["keep".to_owned(), "!keep/skip".to_owned()]);
        assert!(patterns_include(&pats, "keep/k"));
        assert!(patterns_include(&pats, "keep/deep/k"));
        assert!(!patterns_include(&pats, "keep/skip/k"));
        assert!(!patterns_include(&pats, "top"));
    }

    /// A directory already covered by a recursive ancestor is dropped, matching
    /// git collapsing `set a a/b` down to `/a/`.
    #[test]
    fn nested_directories_collapse() {
        assert_eq!(dedup_nested(set(&["a", "a/b", "ab"])), set(&["a", "ab"]));
    }

    /// git resolves `.` and a `prefix`, and refuses a path that climbs out.
    /// The three shape refusals `sanitize_paths()` applies in cone mode, and the
    /// two ways out of them. Measured against stock 2.55.0, each of these dies at
    /// 128: `specify directories rather than patterns (no leading slash)`, and the
    /// `'!'` and `'*?[]'` variants that name `--skip-checks`.
    #[test]
    fn cone_shapes_are_refused_by_sanitize_paths() {
        let dir = std::env::temp_dir().join(format!("zvcs-sparse-vet-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("temp dir");
        let repo = gix::init(&dir).expect("init a repository to vet against");
        for rejected in ["/a", "!a", "a*b"] {
            let got = sanitize_paths(&repo, &[rejected.to_owned()], "", true, false).unwrap();
            assert!(got.is_some(), "{rejected} should be refused in cone mode");
        }
        // `if (skip_checks) return;` sits above the refusals, and `if (!args->nr)
        // return;` above that, so neither shape is reached.
        let bypassed = sanitize_paths(&repo, &["a*b".to_owned()], "", true, true).unwrap();
        assert!(bypassed.is_none(), "--skip-checks must bypass the shape refusals");
        let empty = sanitize_paths(&repo, &[], "", true, false).unwrap();
        assert!(empty.is_none(), "an empty argument list is not checked at all");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn paths_normalize_against_the_prefix() {
        assert_eq!(normalize_dir("", "./a/"), Some("a".to_owned()));
        assert_eq!(normalize_dir("sub", "a"), Some("sub/a".to_owned()));
        assert_eq!(normalize_dir("sub", "../a"), Some("a".to_owned()));
        assert_eq!(normalize_dir("", "../a"), None);
    }

    /// git vets a cone argument in `sanitize_paths()` — a pass over the whole
    /// argument list that runs before any argument becomes a pattern — not in the
    /// per-argument conversion, so `cone_argument()` only normalizes and unquotes.
    /// The refusals themselves are covered by
    /// [`cone_shapes_are_refused_by_sanitize_paths`].
    /// `ExitCode` is not comparable, so accepted arguments are matched by shape.
    #[test]
    fn cone_arguments_normalize_and_unquote() {
        for (raw, skip_checks, want) in [("a*b", true, "a*b"), ("\"w x\"", false, "w x")] {
            match cone_argument(raw, "", skip_checks).unwrap() {
                Ok(Some(dir)) => assert_eq!(dir, want),
                _ => panic!("{raw} should normalize to {want}"),
            }
        }
    }

    #[test]
    fn quoted_paths_round_trip() {
        assert_eq!(unquote_c(b"\"a\\tb\"").unwrap(), BString::from("a\tb"));
        assert_eq!(unquote_c(b"w x").unwrap(), BString::from("w x"));
        assert_eq!(quote_path(b"w x"), "w x");
        assert_eq!(quote_path(b"a\tb"), "\"a\\tb\"");
    }
}

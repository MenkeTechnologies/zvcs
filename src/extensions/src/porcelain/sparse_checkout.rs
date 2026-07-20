use anyhow::{anyhow, Result};
use std::collections::BTreeSet;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::atomic::AtomicBool;

use gix::bstr::{BString, ByteSlice};
use gix::config::{File as ConfigFile, Source};
use gix::glob::{pattern::Case, wildmatch::Mode as WildMode, Pattern};
use gix::hash::ObjectId;
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
/// Applying sparsity walks the index: entries outside the sparsity get the
/// `SKIP_WORKTREE` bit (and are deleted from disk, pruning directories that
/// become empty), entries inside get it cleared and — only if they were skipped
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
/// `-h` prints the usage block on stdout, anything else names the offending
/// option or switch on stderr above the block. Both exit 129.
fn opt_error(arg: &str, usage: &str) -> ExitCode {
    if arg == "-h" {
        print!("{usage}");
    } else if let Some(long) = arg.strip_prefix("--") {
        eprint!("error: unknown option `{long}'\n{usage}");
    } else {
        let switch = arg.chars().nth(1).unwrap_or('-');
        eprint!("error: unknown switch `{switch}'\n{usage}");
    }
    ExitCode::from(129)
}

// --- subcommands -----------------------------------------------------------

fn cmd_list(args: &[String]) -> Result<ExitCode> {
    let repo = gix::discover(".")?;
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

    let lines = read_pattern_file(&repo)?;
    let mut out = String::new();
    if is_cone(&repo)? {
        for d in cone_dirs(&lines) {
            out.push_str(&quote_path(d.as_bytes()));
            out.push('\n');
        }
    } else {
        // Non-cone worktrees list the raw patterns verbatim.
        for l in &lines {
            out.push_str(l);
            out.push('\n');
        }
    }
    print!("{out}");
    Ok(ExitCode::SUCCESS)
}

/// `set` (`add == true` merges into the existing sparsity instead of replacing
/// it, and accepts the smaller option set git gives `add`).
fn cmd_set(args: &[String], add: bool) -> Result<ExitCode> {
    let repo = gix::discover(".")?;
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
    for a in args {
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

    let mut inputs: Vec<String> = Vec::new();
    if stdin {
        let mut buf = String::new();
        std::io::stdin().read_to_string(&mut buf)?;
        inputs.extend(buf.lines().map(str::to_owned));
    }
    inputs.extend(positional.into_iter().map(str::to_owned));

    // `add` never switches dialects; `set` honours an explicit flag and
    // otherwise keeps whatever the worktree is already configured for.
    let cone = if add { is_cone(&repo)? } else { cone.unwrap_or(is_cone(&repo)?) };
    let prefix = worktree_prefix(&repo);

    let sparsity = if cone {
        let mut dirs: BTreeSet<String> = if add {
            cone_dirs(&read_pattern_file(&repo)?)
        } else {
            BTreeSet::new()
        };
        for raw in &inputs {
            match cone_argument(raw, &prefix, skip_checks)? {
                Ok(Some(d)) => {
                    dirs.insert(d);
                }
                // An empty argument (e.g. a blank `--stdin` line) names the root.
                Ok(None) => {}
                Err(code) => return Ok(code),
            }
        }
        let cone = Cone::new(dedup_nested(dirs));
        write_pattern_file(&repo, &cone_lines(&cone))?;
        Sparsity::Cone(cone)
    } else {
        if !prefix.is_empty() {
            eprintln!("fatal: please run from the toplevel directory in non-cone mode");
            return Ok(ExitCode::from(128));
        }
        // Non-cone patterns are stored exactly as typed, appended in order.
        let mut lines: Vec<String> = if add { read_pattern_file(&repo)? } else { Vec::new() };
        lines.extend(inputs.iter().filter(|l| !l.is_empty()).cloned());
        write_pattern_file(&repo, &lines)?;
        Sparsity::Patterns(parse_patterns(&lines))
    };

    enable_config(&repo, cone, sparse_index)?;
    apply(&repo, &sparsity)?;
    Ok(ExitCode::SUCCESS)
}

fn cmd_init(args: &[String]) -> Result<ExitCode> {
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
    let repo = gix::discover(".")?;
    let cone = cone.unwrap_or(is_cone(&repo)?);

    // `init` keeps an existing pattern file (that is how a `disable`d sparsity
    // is restored); only a missing one is seeded with the empty cone.
    if !pattern_path(&repo).exists() {
        write_pattern_file(&repo, &cone_lines(&Cone::new(BTreeSet::new())))?;
    }
    let lines = read_pattern_file(&repo)?;
    let sparsity = if cone {
        Sparsity::Cone(Cone::new(cone_dirs(&lines)))
    } else {
        Sparsity::Patterns(parse_patterns(&lines))
    };

    enable_config(&repo, cone, sparse_index)?;
    apply(&repo, &sparsity)?;
    Ok(ExitCode::SUCCESS)
}

fn cmd_reapply(args: &[String]) -> Result<ExitCode> {
    let repo = gix::discover(".")?;
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
    let cone = cone.unwrap_or(is_cone(&repo)?);

    let lines = read_pattern_file(&repo)?;
    let sparsity = if cone {
        Sparsity::Cone(Cone::new(cone_dirs(&lines)))
    } else {
        Sparsity::Patterns(parse_patterns(&lines))
    };
    enable_config(&repo, cone, sparse_index)?;
    apply(&repo, &sparsity)?;
    Ok(ExitCode::SUCCESS)
}

fn cmd_disable(args: &[String]) -> Result<ExitCode> {
    for a in args {
        if a.starts_with('-') {
            return Ok(opt_error(a, USAGE_DISABLE));
        }
    }
    let repo = gix::discover(".")?;
    // git leaves the pattern file in place so a later `init` can restore it.
    apply(&repo, &Sparsity::Full)?;
    disable_config(&repo)?;
    Ok(ExitCode::SUCCESS)
}

fn cmd_check_rules(args: &[String]) -> Result<ExitCode> {
    let mut nul = false;
    let mut cone = true;
    let mut rules_file: Option<PathBuf> = None;
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "-z" => nul = true,
            "--cone" => cone = true,
            "--no-cone" => cone = false,
            "--skip-checks" | "--no-skip-checks" => {}
            "--no-rules-file" => rules_file = None,
            "--rules-file" => match it.next() {
                Some(v) => rules_file = Some(PathBuf::from(v)),
                None => {
                    // git reports a missing value without the usage block.
                    eprintln!("error: option `rules-file' requires a value");
                    return Ok(ExitCode::from(129));
                }
            },
            _ if a.starts_with("--rules-file=") => {
                rules_file = Some(PathBuf::from(&a["--rules-file=".len()..]));
            }
            _ if a.starts_with('-') => return Ok(opt_error(a, USAGE_CHECK_RULES)),
            _ => {}
        }
    }

    let sparsity = match &rules_file {
        // `--rules-file` holds a directory list in cone mode, patterns otherwise.
        Some(p) => {
            let Ok(text) = std::fs::read_to_string(p) else {
                eprintln!("fatal: unable to load existing sparse-checkout patterns");
                return Ok(ExitCode::from(128));
            };
            let lines: Vec<String> = text.lines().map(str::to_owned).collect();
            if cone {
                let mut dirs = BTreeSet::new();
                for line in &lines {
                    if let Ok(Some(d)) = cone_argument(line, "", true)? {
                        dirs.insert(d);
                    }
                }
                Sparsity::Cone(Cone::new(dedup_nested(dirs)))
            } else {
                Sparsity::Patterns(parse_patterns(&lines))
            }
        }
        None => {
            let repo = gix::discover(".")?;
            if !pattern_path(&repo).exists() {
                eprintln!("fatal: unable to load existing sparse-checkout patterns");
                return Ok(ExitCode::from(128));
            }
            load_sparsity(&repo)?
        }
    };

    let mut input = Vec::new();
    std::io::stdin().read_to_end(&mut input)?;
    let sep = if nul { b'\0' } else { b'\n' };

    let mut out = Vec::new();
    for raw in input.split(|&b| b == sep) {
        if raw.is_empty() {
            continue;
        }
        // Without -z, a leading double quote marks a C-style quoted path.
        let path = if nul { BString::from(raw) } else { unquote_c(raw)? };
        if sparsity.includes(&path.to_str_lossy()) {
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
fn cmd_clean(args: &[String]) -> Result<ExitCode> {
    let repo = gix::discover(".")?;
    if !is_sparse(&repo)? {
        eprintln!("fatal: must be in a sparse-checkout to clean directories");
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
        .ok_or_else(|| anyhow!("this operation must be run in a work tree"))?
        .to_owned();
    let sparsity = load_sparsity(&repo)?;
    let index = repo.open_index()?;
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
enum Sparsity {
    /// No restriction at all — what `disable` applies.
    Full,
    Cone(Cone),
    /// Non-cone gitignore-syntax patterns, in file order.
    Patterns(Vec<Pattern>),
}

impl Sparsity {
    fn includes(&self, path: &str) -> bool {
        match self {
            Sparsity::Full => true,
            Sparsity::Cone(c) => c.matches(path),
            Sparsity::Patterns(p) => patterns_include(p, path),
        }
    }
}

fn load_sparsity(repo: &gix::Repository) -> Result<Sparsity> {
    let lines = read_pattern_file(repo)?;
    Ok(if is_cone(repo)? {
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

// --- cone model ------------------------------------------------------------

/// A cone-mode sparsity definition: the recursive directories plus every
/// ancestor of them (the "parent" directories, which contribute only their
/// immediate files).
struct Cone {
    /// Recursive directories, repo-relative, no leading or trailing slash.
    recursive: BTreeSet<String>,
    /// Strict ancestors of `recursive`, plus the repository root as `""`.
    parents: BTreeSet<String>,
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
        Cone { recursive, parents }
    }

    /// Whether the tracked file `path` is inside the cone: it either sits
    /// directly in a parent directory (or the root), or lives at any depth
    /// under a recursive directory.
    fn matches(&self, path: &str) -> bool {
        let dir = match path.rfind('/') {
            Some(i) => &path[..i],
            None => "",
        };
        if self.parents.contains(dir) {
            return true;
        }
        self.recursive
            .iter()
            .any(|d| dir == d || dir.starts_with(&format!("{d}/")))
    }
}

/// Vet and normalize one cone-mode `set`/`add` argument.
///
/// The outer `Result` is for I/O-shaped failures; the inner one carries git's
/// own fatal (already reported) as the exit code to return. `Ok(Ok(None))`
/// means the argument named the repository root and contributes nothing.
fn cone_argument(raw: &str, prefix: &str, skip_checks: bool) -> Result<Result<Option<String>, ExitCode>> {
    if !skip_checks {
        if raw.starts_with('/') {
            eprintln!("fatal: specify directories rather than patterns (no leading slash)");
            return Ok(Err(ExitCode::from(128)));
        }
        if raw.starts_with('!') {
            eprintln!("fatal: specify directories rather than patterns.  If your directory starts with a '!', pass --skip-checks");
            return Ok(Err(ExitCode::from(128)));
        }
        if raw.contains(|c| matches!(c, '*' | '?' | '[' | ']' | '\\')) {
            eprintln!("fatal: specify directories rather than patterns.  If your directory really has any of '*?[]\\' in it, pass --skip-checks");
            return Ok(Err(ExitCode::from(128)));
        }
    }
    // A leading double quote marks a C-style quoted path.
    let unquoted = unquote_c(raw.as_bytes())?;
    let s = unquoted.to_str_lossy().into_owned();
    match normalize_dir(prefix, &s) {
        Some(d) if d.is_empty() => Ok(Ok(None)),
        Some(d) => Ok(Ok(Some(d))),
        None => {
            eprintln!("fatal: could not normalize path {raw}");
            Ok(Err(ExitCode::from(128)))
        }
    }
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
fn config_bool(repo: &gix::Repository, section: &str, key: &str) -> Result<Option<bool>> {
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

/// Cone mode is git's default whenever sparsity is on and nothing says otherwise.
fn is_cone(repo: &gix::Repository) -> Result<bool> {
    Ok(config_bool(repo, "core", "sparseCheckoutCone")?.unwrap_or(true))
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
fn enable_config(repo: &gix::Repository, cone: bool, sparse_index: Option<bool>) -> Result<()> {
    let _lock = crate::lock::RepoLock::acquire(repo.git_dir());
    edit_config(
        &repo.common_dir().join("config"),
        &[("extensions", "worktreeConfig", "true")],
    )?;
    let mut edits: Vec<(&str, &str, &str)> = vec![
        ("core", "sparseCheckout", "true"),
        ("core", "sparseCheckoutCone", if cone { "true" } else { "false" }),
    ];
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
    id: ObjectId,
    mode: Mode,
    unmerged: bool,
    was_skipped: bool,
}

/// Reconcile the index `SKIP_WORKTREE` bits and the worktree files with
/// `sparsity`.
fn apply(repo: &gix::Repository, sparsity: &Sparsity) -> Result<()> {
    let workdir = repo
        .workdir()
        .ok_or_else(|| anyhow!("this operation must be run in a work tree"))?
        .to_owned();

    let _lock = crate::lock::RepoLock::acquire(repo.git_dir());
    let mut index = repo.open_index()?;

    let snapshot: Vec<Snapshot> = {
        let backing = index.path_backing();
        index
            .entries()
            .iter()
            .map(|e| Snapshot {
                path: e.path_in(backing).to_owned(),
                id: e.id,
                mode: e.mode,
                unmerged: e.stage_raw() != 0,
                was_skipped: e.flags.contains(Flags::SKIP_WORKTREE),
            })
            .collect()
    };

    let mut to_materialize: Vec<BString> = Vec::new();
    let mut to_remove: Vec<BString> = Vec::new();
    let mut unmerged: Vec<BString> = Vec::new();

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
        } else {
            // git refuses to sparsify a path with local modifications: the file
            // stays, and so does its cleared skip bit.
            let dirty = exists
                && disk
                    .as_ref()
                    .map(|p| is_modified(repo, p, snap.id, snap.mode))
                    .unwrap_or(false);
            if dirty {
                entry.flags.remove(Flags::SKIP_WORKTREE);
                if !entry.flags.contains(Flags::INTENT_TO_ADD) {
                    entry.flags.remove(Flags::EXTENDED);
                }
            } else {
                // EXTENDED is what makes the skip bit survive serialization
                // (and forces index version 3, exactly as git does).
                entry.flags.insert(Flags::SKIP_WORKTREE | Flags::EXTENDED);
                if exists {
                    to_remove.push(snap.path.clone());
                }
            }
        }
    }

    index.remove_tree();
    index.write(Default::default())?;

    if !to_materialize.is_empty() {
        // Re-open so the checkout sees the freshly cleared skip bits (entries
        // carrying SKIP_WORKTREE are ignored by the worktree writer).
        let mut subset = repo.open_index()?;
        subset.remove_entries(|_, path, _| !to_materialize.iter().any(|k| k.as_bstr() == path));
        checkout_subset(repo, &mut subset)?;
    }

    for path in &to_remove {
        let Some(full) = repo.workdir_path(path.as_bstr()) else {
            continue;
        };
        let _ = std::fs::remove_file(&full);
        prune_empty_dirs(&workdir, &full);
    }

    if !unmerged.is_empty() {
        let mut msg =
            String::from("warning: The following paths are unmerged and were left despite sparse patterns:\n");
        for p in &unmerged {
            msg.push('\t');
            msg.push_str(&p.to_str_lossy());
            msg.push('\n');
        }
        msg.push_str("\nAfter fixing the above paths, you may want to run `git sparse-checkout reapply`.\n");
        eprint!("{msg}");
    }

    Ok(())
}

/// Whether the worktree file at `full` differs from the index blob `id`.
fn is_modified(repo: &gix::Repository, full: &Path, id: ObjectId, mode: Mode) -> bool {
    let content = if mode.to_tree_entry_mode().is_some_and(|m| m.is_link()) {
        match std::fs::read_link(full) {
            Ok(t) => gix::path::into_bstr(t).into_owned().into(),
            Err(_) => return false,
        }
    } else {
        match std::fs::read(full) {
            Ok(c) => c,
            Err(_) => return false,
        }
    };
    match gix::objs::compute_hash(repo.object_hash(), gix::objs::Kind::Blob, &content) {
        Ok(actual) => actual != id,
        // If we cannot hash it we must not delete it.
        Err(_) => true,
    }
}

/// Remove the now-empty ancestor directories of `full`, stopping at `workdir`
/// or at the first directory that still holds something.
fn prune_empty_dirs(workdir: &Path, full: &Path) {
    let mut cur = full.parent();
    while let Some(dir) = cur {
        if dir == workdir || !dir.starts_with(workdir) {
            break;
        }
        if std::fs::remove_dir(dir).is_err() {
            break;
        }
        cur = dir.parent();
    }
}

/// Write every entry of `index` into the worktree (same helper shape the other
/// worktree-mutating porcelain uses).
fn checkout_subset(repo: &gix::Repository, index: &mut gix::index::File) -> Result<()> {
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

/// C-style path quoting matching git's default `core.quotePath=true`: a path is
/// wrapped in double quotes and escaped when it contains control bytes, a quote,
/// a backslash, or any byte >= 0x80; otherwise it is emitted verbatim.
fn quote_path(path: impl AsRef<[u8]>) -> String {
    let bytes = path.as_ref();
    let needs = bytes
        .iter()
        .any(|&b| b < 0x20 || b == 0x7f || b == b'"' || b == b'\\' || b >= 0x80);
    if !needs {
        return String::from_utf8_lossy(bytes).into_owned();
    }
    let mut out = String::from("\"");
    for &b in bytes {
        match b {
            b'"' => out.push_str("\\\""),
            b'\\' => out.push_str("\\\\"),
            0x07 => out.push_str("\\a"),
            0x08 => out.push_str("\\b"),
            0x09 => out.push_str("\\t"),
            0x0a => out.push_str("\\n"),
            0x0b => out.push_str("\\v"),
            0x0c => out.push_str("\\f"),
            0x0d => out.push_str("\\r"),
            b if b < 0x20 || b == 0x7f || b >= 0x80 => out.push_str(&format!("\\{b:03o}")),
            b => out.push(b as char),
        }
    }
    out.push('"');
    out
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
    #[test]
    fn paths_normalize_against_the_prefix() {
        assert_eq!(normalize_dir("", "./a/"), Some("a".to_owned()));
        assert_eq!(normalize_dir("sub", "a"), Some("sub/a".to_owned()));
        assert_eq!(normalize_dir("sub", "../a"), Some("a".to_owned()));
        assert_eq!(normalize_dir("", "../a"), None);
    }

    /// The sanity checks git applies to cone arguments, and their bypass.
    /// `ExitCode` is not comparable, so accepted arguments are matched by shape.
    #[test]
    fn cone_arguments_are_vetted() {
        for rejected in ["/a", "!a", "a*b"] {
            assert!(
                cone_argument(rejected, "", false).unwrap().is_err(),
                "{rejected} should be rejected"
            );
        }
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

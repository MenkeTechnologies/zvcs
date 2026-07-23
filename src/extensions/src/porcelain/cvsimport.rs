//! `git cvsimport` — import a CVS repository into git. **The import itself is
//! not ported**; only the pre-flight that stock git runs before it spawns any
//! foreign tooling is reproduced here.
//!
//! Stock `git-cvsimport` is a Perl script (`git-cvsimport.perl`) that drives
//! two external programs it does not contain: the `cvs` client (it speaks the
//! CVS client protocol over a pipe to `cvs server` for every file revision) and
//! `cvsps` version 2 (which turns the CVS log into patch sets). Neither has any
//! substrate in the vendored gitoxide crates under `src/ported` — there is no
//! CVS protocol implementation, no RCS/`,v` reader, and no cvsps replacement —
//! so the fetch/commit half of the command cannot be reproduced, only faked.
//! It is therefore rejected outright rather than approximated: cvsimport's
//! whole observable result is the refs and objects it writes, which is exactly
//! the post-command state a differential harness inspects.
//!
//! ### Covered (byte-identical stderr and exit code against git 2.55.0)
//!
//! Everything up to and including the point where the script would exec `cvs`,
//! all of which is pure argument/config/file handling:
//!
//! * The `Getopt::Long` scan the script configures with `bundling` and
//!   `no_ignore_case` over the spec `haivmkuo:d:p:r:C:z:s:M:P:A:S:L:R`:
//!   bundled shorts (`-vk`), attached values (`-zz`, `-d=x` — the `=` is part
//!   of the value in bundling mode), detached values (`-d CVSROOT`), the
//!   single-character long forms (`--d=x`, `--o name`), `--` as a terminator,
//!   and permuted positionals (`module -v`). Repeatable `-M`.
//! * Its two error shapes, emitted in encounter order and *all* of them before
//!   the usage block: `Unknown option: X` (the bundle character, or the long
//!   name as spelled) and `Option d requires an argument`. Exit 1.
//! * `-h`, and the three `usage("…")` call sites: `Error: You can't specify
//!   more than one CVS module`, `Error: CVSROOT needs to be set`, and
//!   `Error: CVS module has to be specified`. The usage block goes to stderr in
//!   every case, exit 1 always — there is no exit-0 path.
//! * `read_repo_config()`: `cvsimport.<key>` defaults applied *before* the
//!   command line, `--bool` for the flag options with `false` treated as unset,
//!   the uppercase remaps (`-A` → `authorsfile`, `-S` → `ignorepaths`, `-R` →
//!   `trackrevisions`, `-M` → `mergeregex`) and `-P` having no config key at
//!   all. Config is read from the current directory's repository (plus global
//!   and system), before any `-C` directory change, as the script does.
//! * CVSROOT resolution order: `-d`, else the first line of `./CVS/Root`, else
//!   `$CVSROOT`, else the usage error. Module resolution order: the positional,
//!   else `cvsimport.module`, else the first line of `./CVS/Repository`, else
//!   the usage error.
//!
//! ### Not covered
//!
//! Once a CVSROOT and a module are in hand the script chdirs to `-C`, runs
//! `git init` if needed, forks `cvsps`, opens a `cvs server` pipe and writes
//! commits; that entire path bails. `cvsimport.mergeregex` is read and
//! discarded, mirroring the script's own bug (it assigns the scalar `$opt_M`,
//! which nothing reads, not the `@opt_M` array). Perl runtime diagnostics
//! (`Can't exec "cvs" … at line 405`) are not reproduced.

use anyhow::{bail, Result};
use std::process::ExitCode;

use gix::config::File as ConfigFile;

/// The usage block from `git-cvsimport.perl`'s `usage()`, verbatim.
const USAGE: &str = "\
usage: git cvsimport     # fetch/update GIT from CVS
       [-o branch-for-HEAD] [-h] [-v] [-d CVSROOT] [-A author-conv-file]
       [-p opts-for-cvsps] [-P file] [-C GIT_repository] [-z fuzz] [-i] [-k]
       [-u] [-s subst] [-a] [-m] [-M regex] [-S regex] [-L commitlimit]
       [-r remote] [-R] [CVS_module]
";

/// One entry of the Getopt spec: the option letter, whether it takes a value,
/// and the `cvsimport.<key>` name `read_repo_config` uses (`None` for `-P`,
/// which `%longmap` maps to `undef`).
struct Spec {
    letter: char,
    takes_value: bool,
    config_key: Option<&'static str>,
}

/// `my $opts = "haivmkuo:d:p:r:C:z:s:M:P:A:S:L:R";` plus the `%longmap` remaps.
/// Config keys are the option letter downcased, except where `%longmap`
/// overrides it (and then has its dashes stripped).
const SPECS: &[Spec] = &[
    Spec { letter: 'h', takes_value: false, config_key: Some("h") },
    Spec { letter: 'a', takes_value: false, config_key: Some("a") },
    Spec { letter: 'i', takes_value: false, config_key: Some("i") },
    Spec { letter: 'v', takes_value: false, config_key: Some("v") },
    Spec { letter: 'm', takes_value: false, config_key: Some("m") },
    Spec { letter: 'k', takes_value: false, config_key: Some("k") },
    Spec { letter: 'u', takes_value: false, config_key: Some("u") },
    Spec { letter: 'o', takes_value: true, config_key: Some("o") },
    Spec { letter: 'd', takes_value: true, config_key: Some("d") },
    Spec { letter: 'p', takes_value: true, config_key: Some("p") },
    Spec { letter: 'r', takes_value: true, config_key: Some("r") },
    Spec { letter: 'C', takes_value: true, config_key: Some("c") },
    Spec { letter: 'z', takes_value: true, config_key: Some("z") },
    Spec { letter: 's', takes_value: true, config_key: Some("s") },
    Spec { letter: 'M', takes_value: true, config_key: Some("mergeregex") },
    Spec { letter: 'P', takes_value: true, config_key: None },
    Spec { letter: 'A', takes_value: true, config_key: Some("authorsfile") },
    Spec { letter: 'S', takes_value: true, config_key: Some("ignorepaths") },
    Spec { letter: 'L', takes_value: true, config_key: Some("l") },
    Spec { letter: 'R', takes_value: false, config_key: Some("trackrevisions") },
];

/// The parsed command line, in the shape of the script's `$opt_*` globals.
#[derive(Default)]
struct Opts {
    h: bool,
    a: bool,
    i: bool,
    v: bool,
    m: bool,
    k: bool,
    u: bool,
    r_track: bool,
    o: Option<String>,
    d: Option<String>,
    p: Option<String>,
    r: Option<String>,
    c: Option<String>,
    z: Option<String>,
    s: Option<String>,
    p_file: Option<String>,
    a_file: Option<String>,
    s_regex: Option<String>,
    l: Option<String>,
    merge_rx: Vec<String>,
}

impl Opts {
    /// Assign one parsed option, exactly as `GetOptions` would store it.
    ///
    /// A flag set from config carries the string `git config --bool` printed;
    /// only its truthiness matters, so flags collapse to `bool` here.
    fn set(&mut self, letter: char, value: Option<String>) {
        match letter {
            'h' => self.h = true,
            'a' => self.a = true,
            'i' => self.i = true,
            'v' => self.v = true,
            'm' => self.m = true,
            'k' => self.k = true,
            'u' => self.u = true,
            'R' => self.r_track = true,
            'o' => self.o = value,
            'd' => self.d = value,
            'p' => self.p = value,
            'r' => self.r = value,
            'C' => self.c = value,
            'z' => self.z = value,
            's' => self.s = value,
            'P' => self.p_file = value,
            'A' => self.a_file = value,
            'S' => self.s_regex = value,
            'L' => self.l = value,
            'M' => self.merge_rx.push(value.unwrap_or_default()),
            _ => unreachable!("letters come from SPECS"),
        }
    }

    /// Whether an option already has a value, for `read_repo_config`'s
    /// `if (!$$opt_name)` guard — config never overrides the command line, and
    /// here it is applied first, so this guards config against itself.
    fn is_set(&self, letter: char) -> bool {
        match letter {
            'h' => self.h,
            'a' => self.a,
            'i' => self.i,
            'v' => self.v,
            'm' => self.m,
            'k' => self.k,
            'u' => self.u,
            'R' => self.r_track,
            'o' => self.o.is_some(),
            'd' => self.d.is_some(),
            'p' => self.p.is_some(),
            'r' => self.r.is_some(),
            'C' => self.c.is_some(),
            'z' => self.z.is_some(),
            's' => self.s.is_some(),
            'P' => self.p_file.is_some(),
            'A' => self.a_file.is_some(),
            'S' => self.s_regex.is_some(),
            'L' => self.l.is_some(),
            // `$opt_M` the scalar, which nothing else touches; see the module
            // docs. Always unset, so config always "applies" and is discarded.
            'M' => false,
            _ => unreachable!("letters come from SPECS"),
        }
    }
}

/// `usage(;$)` — an optional `Error: …` line, then the usage block, then
/// `exit(1)`. Everything goes to stderr.
fn usage(msg: Option<&str>) -> ExitCode {
    if let Some(msg) = msg {
        eprintln!("Error: {msg}");
    }
    eprint!("{USAGE}");
    ExitCode::from(1)
}

/// The config git would see: the repository in the current directory when
/// there is one (system + global + local + environment), otherwise the global
/// and system files alone, which is what `git config --get` falls back to.
fn load_config() -> Option<ConfigFile> {
    match gix::discover(".") {
        Ok(repo) => Some(repo.config_snapshot().plumbing().clone()),
        Err(_) => {
            let mut file = ConfigFile::from_globals().ok()?;
            file.append(ConfigFile::from_environment_overrides().ok()?).ok()?;
            Some(file)
        }
    }
}

/// `read_repo_config($opts)` — seed `$opt_*` from `cvsimport.*` before the
/// command line is parsed.
///
/// Flags are read with `--bool`, and a `false` counts as unset. A value that
/// git would reject as a boolean is treated as unset here; git prints a `fatal:`
/// from the inner `git config` and leaves the option unset too, but without the
/// diagnostic.
fn read_repo_config(opts: &mut Opts, cfg: &ConfigFile) {
    for spec in SPECS {
        let Some(key) = spec.config_key else { continue };
        if opts.is_set(spec.letter) {
            continue;
        }
        let full = format!("cvsimport.{key}");
        if spec.takes_value {
            match cfg.string(full.as_str()) {
                // A set-but-empty value is false in Perl, so it is skipped.
                Some(v) if truthy(&v.to_string()) => opts.set(spec.letter, Some(v.to_string())),
                _ => {}
            }
        } else if cfg.boolean(full.as_str()).ok().flatten() == Some(true) {
            opts.set(spec.letter, None);
        }
    }
}

/// The outcome of the option scan: parsed options, positionals, and the error
/// lines `Getopt::Long` printed. Any error means `usage()` regardless of what
/// else parsed.
struct Parsed {
    opts: Opts,
    positional: Vec<String>,
    errors: Vec<String>,
}

/// Reproduce `Getopt::Long::Configure('no_ignore_case', 'bundling')` over
/// [`SPECS`]: single-character options only, so no abbreviation matching
/// applies to the `--name` forms, and case is significant (`-C` and `-c` are
/// not the same option; `-c` does not exist).
fn parse(args: &[String], mut opts: Opts) -> Parsed {
    let lookup = |c: char| SPECS.iter().find(|s| s.letter == c);
    let mut positional = Vec::new();
    let mut errors = Vec::new();
    let mut it = args.iter().peekable();

    while let Some(arg) = it.next() {
        if arg == "--" {
            positional.extend(it.map(String::clone));
            break;
        }
        if let Some(name) = arg.strip_prefix("--") {
            let (name, attached) = match name.split_once('=') {
                Some((n, v)) => (n, Some(v.to_string())),
                None => (name, None),
            };
            let mut chars = name.chars();
            match (chars.next(), chars.next()) {
                (Some(c), None) if lookup(c).is_some() => {
                    let spec = lookup(c).expect("checked above");
                    if !spec.takes_value {
                        opts.set(c, None);
                    } else if let Some(v) = attached.or_else(|| it.next().cloned()) {
                        opts.set(c, Some(v));
                    } else {
                        errors.push(format!("Option {c} requires an argument"));
                    }
                }
                _ => errors.push(format!("Unknown option: {name}")),
            }
            continue;
        }
        // A lone `-` is a positional, as is anything not starting with `-`.
        let bundle = match arg.strip_prefix('-') {
            Some(b) if !b.is_empty() => b,
            _ => {
                positional.push(arg.clone());
                continue;
            }
        };
        let mut rest = bundle;
        while let Some(c) = rest.chars().next() {
            rest = &rest[c.len_utf8()..];
            match lookup(c) {
                None => errors.push(format!("Unknown option: {c}")),
                Some(spec) if !spec.takes_value => opts.set(c, None),
                Some(_) => {
                    // In bundling mode the remainder of the bundle is the
                    // value verbatim — `-d=x` yields `=x` — and an empty
                    // remainder takes the next argument.
                    let value = if rest.is_empty() {
                        it.next().cloned()
                    } else {
                        Some(std::mem::take(&mut rest).to_string())
                    };
                    match value {
                        Some(v) => opts.set(c, Some(v)),
                        None => errors.push(format!("Option {c} requires an argument")),
                    }
                    break;
                }
            }
        }
    }

    Parsed { opts, positional, errors }
}

/// Perl's notion of a true string: everything except `""` and `"0"`.
fn truthy(s: &str) -> bool {
    !s.is_empty() && s != "0"
}

/// The first line of a file with its trailing newline removed, as Perl's
/// `<$f>` plus `chomp` produces.
fn first_line(path: &str) -> Result<String> {
    let bytes = std::fs::read(path).map_err(|e| anyhow::anyhow!("Failed to open {path}: {e}"))?;
    let text = String::from_utf8_lossy(&bytes);
    let line = text.split_inclusive('\n').next().unwrap_or("");
    Ok(line.strip_suffix('\n').unwrap_or(line).to_string())
}

pub fn cvsimport(args: &[String]) -> Result<ExitCode> {
    let mut opts = Opts::default();
    if let Some(cfg) = load_config() {
        read_repo_config(&mut opts, &cfg);
    }
    let Parsed { opts, mut positional, errors } = parse(args, opts);

    if !errors.is_empty() {
        for line in &errors {
            eprintln!("{line}");
        }
        return Ok(usage(None));
    }
    if opts.h {
        return Ok(usage(None));
    }

    if positional.is_empty() {
        if let Some(module) = load_config().and_then(|cfg| cfg.string("cvsimport.module")) {
            positional.push(module.to_string());
        }
    }
    if positional.len() > 1 {
        return Ok(usage(Some("You can't specify more than one CVS module")));
    }

    // CVSROOT: -d, then ./CVS/Root, then the environment. Each test is Perl's
    // truthiness, so `""` and `"0"` count as unset at every step.
    let cvsroot = match opts.d.as_deref().filter(|d| truthy(d)) {
        Some(d) => d.to_string(),
        None if std::path::Path::new("CVS/Root").is_file() => first_line("CVS/Root")?,
        None => match std::env::var("CVSROOT") {
            Ok(v) if truthy(&v) => v,
            _ => return Ok(usage(Some("CVSROOT needs to be set"))),
        },
    };

    // Module: the positional, then ./CVS/Repository.
    let module = match positional.first() {
        Some(m) => m.clone(),
        None if std::path::Path::new("CVS/Repository").is_file() => first_line("CVS/Repository")?,
        None => return Ok(usage(Some("CVS module has to be specified"))),
    };

    bail!(
        "unsupported: importing {module:?} from {cvsroot:?} needs the external `cvs` client and \
         `cvsps` v2, neither of which has a substrate in the vendored gitoxide crates \
         (ported: option and config parsing, -h, CVSROOT/module resolution)"
    );
}

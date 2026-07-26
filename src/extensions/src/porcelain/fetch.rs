use anyhow::Result;
use prodash::Root as _;
use std::collections::HashSet;
use std::io::{IsTerminal, Read, Write};
use std::num::NonZeroU32;
use std::process::ExitCode;
use std::sync::atomic::AtomicBool;

use gix::bstr::{BStr, BString, ByteSlice};
use gix::refs::transaction::{Change, LogChange, PreviousValue, RefEdit, RefLog};
use gix::refs::{Category, FullName, Target, TargetRef};

use gix::remote::fetch::refs::update::Mode;
use gix::remote::fetch::{RefLogMessage, Shallow, Status, Tags};

/// `git fetch [<options>] [<remote> [<refspec>...]]` — download objects and
/// update the remote-tracking refs, backed by gitoxide's blocking fetch.
///
/// Supported forms:
///   * `git fetch`                    → fetch the branch's remote, else the default remote
///   * `git fetch <remote>`           → fetch a named remote (or a bare URL)
///   * `git fetch <remote> <refspec>…`→ fetch explicit refspecs (override configured)
///   * `--all`                        → fetch every configured remote
///   * `-m`/`--multiple`              → treat all positionals as remotes and fetch each
///   * `-t`/`--tags`                  → also fetch all tags (`refs/tags/*:refs/tags/*`)
///   * `-n`/`--no-tags`               → disable automatic tag following
///   * `-p`/`--prune`                 → delete tracking refs no longer on the remote
///   * `-P`/`--prune-tags`            → add the tags refspec and (with `-p`) prune stale tags
///   * `-f`/`--force`                 → force updates (treat every refspec as `+`)
///   * `--depth <n>`/`--deepen <n>`/`--unshallow` → shallow-clone history controls
///   * `--shallow-since <time>`       → set the shallow boundary at a cutoff date
///   * `--shallow-exclude <ref>`      → exclude history reachable from a ref (repeatable)
///   * `-v`/`--verbose`, `-q`/`--quiet`, `--dry-run` (and their `--no-…` negations)
///   * `--porcelain`                  → machine-readable `<flag> <old> <new> <ref>` on stdout
///   * `--write-fetch-head`/`--no-write-fetch-head`, `-a`/`--append` → `.git/FETCH_HEAD`
///   * `--progress`/`--no-progress`   → force/suppress the stderr progress meter
///   * `--show-forced-updates`/`--no-show-forced-updates` → the `(forced update)` note
///   * `--prefetch`                   → rewrite every refspec into `refs/prefetch/…`
///   * `--stdin`                      → read additional refspecs from standard input
///   * `-u`/`--update-head-ok`        → allow updating the ref `HEAD` points at
///   * `-k`/`--keep`                  → keep the downloaded pack (always the case here)
///   * `--write-commit-graph`         → write the commit-graph after fetching
///   * `--recurse-submodules[=yes|no]`, `-j`/`--jobs <n>` → fetch in populated submodules
///   * `--upload-pack <path>`         → run `<path>` instead of `git-upload-pack` on the other end
///   * `-o`/`--server-option <opt>`   → protocol-v2 `server-option=<opt>` line (repeatable)
///   * `--refmap <refspec>`           → map the command-line refspecs' results with `<refspec>`
///     instead of the remote's configured ones (repeatable; `--refmap=''` stores nowhere)
///
/// Config-supplied defaults (overridden by the matching flag, git precedence
/// CLI > config > built-in default):
///   * `fetch.prune`              → behave as `--prune`
///   * `fetch.pruneTags`          → behave as `--prune-tags`
///   * `fetch.all`                → behave as `--all` when no remote is named
///   * `fetch.showForcedUpdates`  → default for `--show-forced-updates`
///   * `fetch.writeCommitGraph`   → default for `--write-commit-graph`
///   * `fetch.recurseSubmodules`  → default for `--recurse-submodules`
///   * `fetch.parallel`           → default for `-j`/`--jobs`
///   * `fetch.output`             → `compact` abbreviates the `<from> -> <to>` columns
///   * `remote.<name>.uploadpack` → default for `--upload-pack`
///   * `remote.<name>.serverOption` → default set of `-o`/`--server-option` values
///
/// Command-line refspecs go through git's two-stage match (`get_ref_map` in
/// `builtin/fetch.c`): the refspecs on the command line select the refs, and
/// the remote's configured refspecs — or `--refmap` — then map *only those*
/// onto local tracking refs. That second stage is why `git fetch origin main`
/// still updates `refs/remotes/origin/main`; those opportunistic updates are
/// reported in the summary but contribute no `FETCH_HEAD` row, exactly as
/// git's `FETCH_HEAD_IGNORE` does.
///
/// The per-ref summary is written to stderr in `git fetch` layout (`From <url>`
/// header plus one aligned line per changed or pruned ref), or to stdout in the
/// machine-readable layout under `--porcelain`. Options that require substrate
/// gitoxide's high-level fetch does not expose are rejected rather than silently
/// ignored: `--filter`, `--set-upstream`, `--atomic`, `--refetch`,
/// `--update-shallow`, `--ipv4`/`--ipv6`, the `--negotiation-*` family and
/// `--auto-maintenance`/`--auto-gc`.
// The final `take_value!` expansion bumps the `i` cursor that no later arm reads;
// the write is needed by every other expansion, so it can't be removed.
#[allow(unused_assignments)]
pub fn fetch(args: &[String]) -> Result<ExitCode> {
    let mut repo = gix::discover(".")?;

    // Remote-tracking ref updates write reflogs; without a configured identity, seed
    // a synthesized system default so the reflog write can't fail (git does the same).
    crate::ensure_reflog_identity(&mut repo);

    // --- argument parsing -------------------------------------------------
    let mut opts = FetchOpts::default();
    // Tri-state so `fetch.all` can supply the default: `Some(true/false)` is an
    // explicit `--all`/`--no-all`, `None` defers to config (git precedence:
    // CLI > config > built-in default).
    let mut all_flag: Option<bool> = None;
    let mut multiple = false;
    let mut positionals: Vec<&str> = Vec::new();

    // Shallow-boundary selectors that combine (git's `--shallow-exclude` is a
    // repeatable OPT_STRING_LIST → `deepen_not`, `--shallow-since` → `deepen_since`,
    // and the two may be given together). Accumulated here and resolved into a
    // single `Shallow` value after parsing.
    let mut shallow_exclude: Vec<gix::refs::PartialName> = Vec::new();
    let mut shallow_since: Option<gix::date::Time> = None;

    // `--stdin`: git appends the refspecs read from standard input to the ones
    // named on the command line, so the read is deferred until parsing is done.
    let mut read_stdin = false;
    // Tri-states resolved against config after parsing.
    let mut show_forced_updates: Option<bool> = None;
    let mut write_commit_graph: Option<bool> = None;
    let mut recurse_submodules: Option<Recurse> = None;
    let mut jobs: Option<usize> = None;
    // `--refmap` (repeatable). Kept as raw strings because an empty one is legal and doesn't parse.
    let mut refmap: Vec<String> = Vec::new();

    let mut i = 0;
    while i < args.len() {
        let a = args[i].as_str();
        i += 1;

        // Split `--opt=value` for the value-taking long options.
        let (key, inline_val) = match (a.starts_with("--"), a.split_once('=')) {
            (true, Some((k, v))) => (k, Some(v.to_string())),
            _ => (a, None),
        };

        // Fetch the value for a value-taking option (inline `=v` or next arg).
        // Kept as a plain expression (not a closure) so the `i` cursor stays
        // freely borrowable in the other match arms.
        macro_rules! take_value {
            ($name:literal) => {
                match inline_val.clone() {
                    Some(v) => v,
                    None => {
                        let v = args
                            .get(i)
                            .cloned()
                            .ok_or_else(|| anyhow::anyhow!(concat!($name, " requires a value")))?;
                        i += 1;
                        v
                    }
                }
            };
        }

        match key {
            "-v" | "--verbose" => opts.verbose = true,
            "-q" | "--quiet" => opts.quiet = true,
            "--dry-run" => opts.dry_run = true,
            "--all" => all_flag = Some(true),
            "-m" | "--multiple" => multiple = true,
            "-t" | "--tags" => opts.tags = Some(Tags::All),
            // git: `-n` is the short form of `--no-tags`, not `--dry-run`.
            "-n" | "--no-tags" => opts.tags = Some(Tags::None),
            "-p" | "--prune" => opts.prune = Some(true),
            "-P" | "--prune-tags" => opts.prune_tags = Some(true),
            "-f" | "--force" => opts.force = true,
            // Negations git's parse-options accepts for the `--[no-]…` booleans:
            // resetting each flag to its default (git clears the corresponding bit).
            "--no-verbose" => opts.verbose = false,
            "--no-quiet" => opts.quiet = false,
            "--no-dry-run" => opts.dry_run = false,
            "--no-all" => all_flag = Some(false),
            "--no-multiple" => multiple = false,
            "--no-prune" => opts.prune = Some(false),
            "--no-prune-tags" => opts.prune_tags = Some(false),
            "--no-force" => opts.force = false,
            "--unshallow" => opts.shallow = Some(Shallow::undo()),

            // Machine-readable output: the per-ref rows go to stdout as
            // `<flag> <old-object-id> <new-object-id> <local-reference>` and the
            // `From <url>` header is not printed.
            "--porcelain" => opts.porcelain = true,
            "--no-porcelain" => opts.porcelain = false,

            // FETCH_HEAD control. `--write-fetch-head` is git's default; `-a`
            // appends to the existing file instead of truncating it.
            "--write-fetch-head" => opts.write_fetch_head = true,
            "--no-write-fetch-head" => opts.write_fetch_head = false,
            "-a" | "--append" => opts.append = true,
            "--no-append" => opts.append = false,

            // Progress meter: forced on, forced off, or (unset) shown when stderr
            // is a terminal, exactly as git decides it.
            "--progress" => opts.progress = Some(true),
            "--no-progress" => opts.progress = Some(false),

            // Whether to annotate non-fast-forward updates with `(forced update)`.
            "--show-forced-updates" => show_forced_updates = Some(true),
            "--no-show-forced-updates" => show_forced_updates = Some(false),

            // Place every fetched ref under `refs/prefetch/` instead of its
            // configured destination (git's `filter_prefetch_refspec`).
            "--prefetch" => opts.prefetch = true,
            "--no-prefetch" => opts.prefetch = false,

            // Additional refspecs from standard input, appended to the ones on
            // the command line.
            "--stdin" => read_stdin = true,
            "--no-stdin" => read_stdin = false,

            // Permit updating the ref `HEAD` resolves to in a worktree, which is
            // otherwise refused to keep the index and worktree consistent.
            "-u" | "--update-head-ok" => opts.update_head_ok = true,
            "--no-update-head-ok" => opts.update_head_ok = false,

            // `-k`/`--keep` asks for the received pack to be kept rather than
            // exploded into loose objects. This build never runs the equivalent
            // of `unpack-objects` — gitoxide always writes the pack and its index
            // into `objects/pack` — so the flag names the behaviour that is
            // already in force. `--no-keep` would have to explode the pack, which
            // has no implementation here, so it is refused instead of ignored.
            "-k" | "--keep" => {}
            "--no-keep" => anyhow::bail!(
                "unsupported option \"--no-keep\" (the received pack is always kept; \
                 there is no unpack-objects path)"
            ),

            // Post-fetch commit-graph write (git's `--write-commit-graph`).
            "--write-commit-graph" => write_commit_graph = Some(true),
            "--no-write-commit-graph" => write_commit_graph = Some(false),

            // Submodule recursion and its parallelism.
            "--recurse-submodules" => {
                recurse_submodules = Some(match inline_val.as_deref() {
                    None | Some("yes") | Some("true") => Recurse::Yes,
                    Some("no") | Some("false") => Recurse::No,
                    Some("on-demand") => anyhow::bail!(
                        "unsupported option \"--recurse-submodules=on-demand\" (it needs the \
                         superproject's old/new submodule gitlinks to decide what to fetch)"
                    ),
                    Some(other) => {
                        anyhow::bail!("--recurse-submodules expects yes/on-demand/no, got {other:?}")
                    }
                });
            }
            "--no-recurse-submodules" => recurse_submodules = Some(Recurse::No),
            "-j" | "--jobs" => {
                let v = take_value!("--jobs");
                let n: usize = v
                    .parse()
                    .map_err(|_| anyhow::anyhow!("--jobs expects a positive integer, got {v:?}"))?;
                // `0` is git's "pick a reasonable number", resolved below.
                jobs = Some(n);
            }

            "--depth" => {
                let v = take_value!("--depth");
                let n: u32 = v
                    .parse()
                    .map_err(|_| anyhow::anyhow!("--depth expects a positive integer, got {v:?}"))?;
                let n = NonZeroU32::new(n)
                    .ok_or_else(|| anyhow::anyhow!("--depth expects a positive integer"))?;
                opts.shallow = Some(Shallow::DepthAtRemote(n));
            }
            "--deepen" => {
                let v = take_value!("--deepen");
                let n: u32 = v
                    .parse()
                    .map_err(|_| anyhow::anyhow!("--deepen expects an integer, got {v:?}"))?;
                opts.shallow = Some(Shallow::Deepen(n));
            }
            // Shallow boundary at a cutoff date (git's `deepen_since`). Parsed with
            // gitoxide's git-compatible date parser, relative to the current time.
            "--shallow-since" => {
                let v = take_value!("--shallow-since");
                let t = gix::date::parse(&v, Some(std::time::SystemTime::now()))
                    .map_err(|_| anyhow::anyhow!("--shallow-since expects a valid date, got {v:?}"))?;
                shallow_since = Some(t);
            }
            // Exclude history reachable from a ref (git's repeatable `deepen_not`).
            "--shallow-exclude" => {
                let v = take_value!("--shallow-exclude");
                let name = gix::refs::PartialName::try_from(v.as_str())
                    .map_err(|_| anyhow::anyhow!("--shallow-exclude expects a valid ref, got {v:?}"))?;
                shallow_exclude.push(name);
            }
            // The program to run instead of `git-upload-pack` on the other end. git passes it verbatim to
            // whatever spawns the service, so it can be a path or (over ssh) a whole command line.
            "--upload-pack" => opts.upload_pack = Some(take_value!("--upload-pack")),

            // Protocol-v2 server options, repeatable, transmitted as `server-option=<value>` lines.
            "-o" | "--server-option" => opts.server_options.push(take_value!("--server-option").into()),

            // git's `parse_refmap_arg`: repeatable, no negation, and an empty value is the documented way to
            // say "don't store anywhere" — it appends a refspec that matches nothing rather than clearing the
            // list, which still counts as "a refmap was given".
            "--refmap" => {
                let v = take_value!("--refmap");
                refmap.push(v);
            }

            // Options requiring substrate the high-level fetch does not expose.
            "--filter" => {
                let _ = take_value!("--filter");
                anyhow::bail!("--filter (partial clone) is not supported");
            }
            "--set-upstream" => {
                anyhow::bail!("--set-upstream is not supported");
            }
            "--" => {
                positionals.extend(args[i..].iter().map(String::as_str));
                break;
            }
            s if s.starts_with('-') && s.len() > 1 => anyhow::bail!("unsupported option {s:?}"),
            s => positionals.push(s),
        }
    }

    // `--stdin` refspecs are appended after everything named on the command line,
    // as git's `add_refspec` on the stdin lines does.
    let stdin_specs: Vec<String> = if read_stdin {
        let mut buf = String::new();
        std::io::stdin().read_to_string(&mut buf)?;
        buf.lines()
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .map(str::to_string)
            .collect()
    } else {
        Vec::new()
    };

    // Resolve the accumulated shallow-boundary selectors. `--shallow-exclude`
    // (repeatable) may be combined with `--shallow-since`, mirroring git's
    // `deepen_not` + `deepen_since`; a lone `--shallow-since` sets only the cutoff.
    // Either form supersedes an earlier `--depth`/`--deepen`/`--unshallow`, as git
    // treats the shallow selectors as one group.
    if !shallow_exclude.is_empty() {
        opts.shallow = Some(Shallow::Exclude {
            remote_refs: shallow_exclude,
            since_cutoff: shallow_since,
        });
    } else if let Some(cutoff) = shallow_since {
        opts.shallow = Some(Shallow::Since { cutoff });
    }

    // --- config-supplied defaults -----------------------------------------
    // git resolves each of these with CLI > config > built-in default (see
    // builtin/fetch.c `cmd_fetch`): a bare `git fetch` behaves as if the
    // corresponding flag were given when the config is set, but an explicit
    // flag always wins. `-c`/`--config` overrides land here via gix's snapshot
    // (they are injected as `GIT_CONFIG_*` before the repo is opened).
    //   * `fetch.prune`     → default for `--prune`
    //   * `fetch.pruneTags` → default for `--prune-tags`
    //   * `fetch.all`       → default for `--all` (only with no explicit remote,
    //                          matching git: a positional remote suppresses it)
    let recurse;
    {
        let snap = repo.config_snapshot();
        if opts.prune.is_none() {
            opts.prune = snap.boolean("fetch.prune");
        }
        if opts.prune_tags.is_none() {
            opts.prune_tags = snap.boolean("fetch.pruneTags");
        }
        if all_flag.is_none()
            && positionals.is_empty()
            && snap.boolean("fetch.all") == Some(true)
        {
            all_flag = Some(true);
        }
        // git's `fetch.showForcedUpdates` defaults to true.
        opts.show_forced_updates = show_forced_updates
            .or_else(|| snap.boolean("fetch.showForcedUpdates"))
            .unwrap_or(true);
        opts.write_commit_graph = write_commit_graph
            .or_else(|| snap.boolean("fetch.writeCommitGraph"))
            .unwrap_or(false);
        // `fetch.output` selects between the default `full` layout and `compact`,
        // which folds a `<from>`/`<to>` pair that contains the other into a `*`.
        opts.compact = snap
            .string("fetch.output")
            .is_some_and(|v| v == "compact");
        // `fetch.recurseSubmodules` supplies the default; `on-demand` (git's own
        // default) is not implementable here and is treated as "off" rather than
        // guessed at, which is what a bare `git fetch` does in this build today.
        recurse = match recurse_submodules {
            Some(r) => r,
            None => match snap
                .string("fetch.recurseSubmodules")
                .map(|v| v.to_string())
                .as_deref()
            {
                Some("yes" | "true" | "on" | "1") => Recurse::Yes,
                _ => Recurse::No,
            },
        };
        // `fetch.parallel` is git's default for `-j`, and is itself 1 when unset;
        // an explicit `0` on either means "pick a reasonable number", which here
        // is the machine's available parallelism.
        let parallel = jobs.or_else(|| {
            snap.integer("fetch.parallel")
                .and_then(|n: i64| usize::try_from(n).ok())
        });
        opts.jobs = match parallel {
            Some(0) => std::thread::available_parallelism().map_or(1, |n| n.get()),
            Some(n) => n,
            None => 1,
        };
    }
    let all = all_flag.unwrap_or(false);

    // Every refspec git accepts on the command line, from `--stdin` or via `--refmap` goes through
    // `refspec_append()`, which dies on a malformed one before anything is fetched.
    // Under `--all`/`--multiple` every positional is a remote name, so there are no refspecs to expand
    // or check. Otherwise `tag <name>` is git's shorthand for `refs/tags/<name>:refs/tags/<name>`.
    let mut positional_specs: Vec<String> = Vec::new();
    if !all && !multiple {
        let mut rest = positionals.iter().skip(1);
        while let Some(arg) = rest.next() {
            if *arg == "tag" {
                let name = rest
                    .next()
                    .ok_or_else(|| anyhow::anyhow!("you need to specify a tag name"))?;
                positional_specs.push(format!("refs/tags/{name}:refs/tags/{name}"));
            } else {
                positional_specs.push((*arg).to_string());
            }
        }
    }
    for spec in positional_specs
        .iter()
        .map(String::as_str)
        .chain(stdin_specs.iter().map(String::as_str))
        .chain(refmap.iter().map(String::as_str))
        .filter(|s| !s.is_empty())
    {
        if !refspec_globs_agree(spec) {
            eprintln!("fatal: invalid refspec '{spec}'");
            return Ok(ExitCode::from(128));
        }
    }

    // `--refmap` is the second half of git's two-stage match, so it only means anything once the first stage
    // has command-line refspecs to select refs with. `--all`/`--multiple` read every positional as a remote,
    // leaving no refspecs at all.
    if !refmap.is_empty() {
        let has_refspecs = !positional_specs.is_empty() || (!all && !multiple && !stdin_specs.is_empty());
        if !has_refspecs {
            eprintln!("fatal: --refmap option is only meaningful with command-line refspec(s)");
            return Ok(ExitCode::from(128));
        }
        opts.refmap = Some(
            refmap
                .iter()
                // git's documented `--refmap=''`: it appends a refspec that matches nothing, which is how the
                // fetch is told to store nowhere while still counting as a refmap.
                .filter(|s| !s.is_empty())
                .map(|s| {
                    gix::refspec::parse(s.as_str().into(), gix::refspec::parse::Operation::Fetch)
                        .map(|s| s.to_owned())
                })
                .collect::<Result<_, _>>()?,
        );
    }

    // Turning the forced-update check off makes the summary silently misreport
    // rewritten branches as fast-forwards, so git says so once per invocation —
    // before any fetching, and regardless of `-q` or of whether anything is
    // fetched at all.
    if !opts.show_forced_updates {
        eprintln!(
            "warning: fetch normally indicates which branches had a forced update,\n\
             but that check has been disabled; to re-enable, use '--show-forced-updates'\n\
             flag or run 'git config fetch.showForcedUpdates true'"
        );
    }

    // Serialize ref mutations through the repo coordinator, as the write
    // commands do; a no-op guard if no daemon is running.
    let _lock = crate::lock::RepoLock::acquire(repo.git_dir());

    // The upstream of the current branch decides which FETCH_HEAD row is the
    // merge candidate (git's `FETCH_HEAD_MERGE`) when the configured refspecs
    // are used; explicit command-line refspecs mark every row for merge instead.
    let head_name = repo.head_name()?;
    let upstream = head_name.as_ref().and_then(|h| {
        let short = h.shorten().to_string();
        let remote = repo
            .config_snapshot()
            .string(&format!("branch.{short}.remote"))
            .map(|v| v.to_string())?;
        let merge = repo
            .branch_remote_ref_name(h.as_ref(), gix::remote::Direction::Fetch)
            .and_then(Result::ok)?;
        Some((remote, merge.as_bstr().to_string()))
    });

    // The progress tree is always built; only the renderer is conditional, so
    // gitoxide's counters go nowhere when progress is suppressed (as under a
    // non-terminal stderr) and to the line renderer otherwise.
    let show_progress =
        opts.progress.unwrap_or_else(|| std::io::stderr().is_terminal()) && !opts.quiet;
    let root = prodash::tree::Root::new();
    let mut op = root.add_child("fetch");
    let render = show_progress.then(|| {
        let mut o = prodash::render::line::Options {
            throughput: true,
            ..Default::default()
        }
        .auto_configure(prodash::render::line::StreamKind::Stderr);
        // `--progress` forces the live display even when stderr is not a terminal,
        // matching git; auto_configure would otherwise disable it in that case.
        if opts.progress == Some(true) {
            o.output_is_terminal = true;
        }
        o.hide_cursor = false;
        // git colors progress only on a real terminal, so `--progress` into a
        // pipe stays plain even though the meter is forced on.
        o.colored = std::io::stderr().is_terminal() && std::env::var_os("NO_COLOR").is_none();
        prodash::render::line::render(std::io::stderr(), root.downgrade(), o)
    });

    // --- dispatch by mode -------------------------------------------------
    let mut failure = false;
    let mut fatal = false;
    let mut fetch_head = FetchHead {
        path: repo.git_dir().join("FETCH_HEAD"),
        enabled: opts.write_fetch_head && !opts.dry_run,
        truncate: !opts.append,
    };

    let result = (|| -> Result<()> {
        if all {
            if !positionals.is_empty() {
                anyhow::bail!("fetch --all does not take a repository argument");
            }
            // git announces each remote on stdout while fanning out, but only on
            // the genuinely multi-remote path: `cmd_fetch` short-circuits
            // `--all` over a single remote into the ordinary one-remote fetch,
            // which prints nothing. `-q` silences the announcement either way.
            let names = repo.remote_names();
            let announce = names.len() > 1 && !opts.quiet;
            for name in names {
                let n = name.as_bstr();
                if announce {
                    println!("Fetching {n}");
                }
                match fetch_one(
                    &repo,
                    Some(n),
                    &stdin_specs.iter().map(String::as_str).collect::<Vec<_>>(),
                    &opts,
                    upstream.as_ref(),
                    &mut fetch_head,
                    &mut op,
                ) {
                    Ok(Verdict::Ok) => {}
                    Ok(Verdict::Rejected) => failure = true,
                    Ok(Verdict::Fatal) => {
                        fatal = true;
                        break;
                    }
                    Err(e) => {
                        eprintln!("error: could not fetch {n}: {e}");
                        failure = true;
                    }
                }
            }
        } else if multiple {
            // `--multiple` always takes the fan-out path, so even one remote is
            // announced.
            for name in &positionals {
                if !opts.quiet {
                    println!("Fetching {name}");
                }
                match fetch_one(
                    &repo,
                    Some(BStr::new(*name)),
                    &stdin_specs.iter().map(String::as_str).collect::<Vec<_>>(),
                    &opts,
                    upstream.as_ref(),
                    &mut fetch_head,
                    &mut op,
                ) {
                    Ok(Verdict::Ok) => {}
                    Ok(Verdict::Rejected) => failure = true,
                    Ok(Verdict::Fatal) => {
                        fatal = true;
                        break;
                    }
                    Err(e) => {
                        eprintln!("error: could not fetch {name}: {e}");
                        failure = true;
                    }
                }
            }
        } else {
            let name = positionals.first().map(|s| BStr::new(*s));
            let mut refspecs: Vec<&str> = positional_specs.iter().map(String::as_str).collect();
            refspecs.extend(stdin_specs.iter().map(String::as_str));
            match fetch_one(
                &repo,
                name,
                &refspecs,
                &opts,
                upstream.as_ref(),
                &mut fetch_head,
                &mut op,
            )? {
                Verdict::Ok => {}
                Verdict::Rejected => failure = true,
                Verdict::Fatal => fatal = true,
            }
        }
        Ok(())
    })();

    if let Some(handle) = render {
        handle.shutdown_and_wait();
    }
    result?;

    // `transfer.credentialsInUrl=die` is git's `fatal:` exit, taken before any
    // post-fetch work runs.
    if fatal {
        return Ok(ExitCode::from(128));
    }

    // `--write-commit-graph` / `fetch.writeCommitGraph`: rebuild the commit-graph
    // over everything now reachable, which is what git does at the end of a
    // fetch. git writes it as an incremental split chain
    // (`objects/info/commit-graphs/`); the commit-graph port has no chain
    // protocol, so this is the single-file form at `objects/info/commit-graph`.
    if opts.write_commit_graph && !opts.dry_run {
        let code = super::commit_graph(&[
            "write".to_string(),
            "--reachable".to_string(),
            "--no-progress".to_string(),
        ])?;
        if code != ExitCode::SUCCESS {
            failure = true;
        }
    }

    // `--recurse-submodules[=yes]` / `fetch.recurseSubmodules=yes`: run the same
    // fetch inside every populated submodule, up to `--jobs` at a time.
    if recurse == Recurse::Yes && !opts.dry_run && fetch_submodules(&repo, &opts)? {
        failure = true;
    }

    if failure {
        return Ok(ExitCode::FAILURE);
    }
    Ok(ExitCode::SUCCESS)
}

/// What one remote's fetch produced, beyond the objects themselves.
#[derive(PartialEq, Eq)]
pub(super) enum Verdict {
    /// Everything the refspecs asked for was applied.
    Ok,
    /// At least one ref update was rejected; the command exits non-zero.
    Rejected,
    /// `transfer.credentialsInUrl=die` matched, which git reports as a `fatal:`
    /// and exit 128 before any network traffic.
    Fatal,
}

/// git's `transfer.credentialsInUrl`, applied before a connection is opened.
///
/// A fetch URL that carries a plaintext password is accepted silently under the
/// default `allow`, reported as `warning: URL '<url>' uses plaintext
/// credentials` under `warn`, and refused with the same sentence as a `fatal:`
/// under `die`. The password is replaced with `<redacted>` in the message, as
/// git's `transport_anonymize_url` does.
///
/// git emits the warning once per transport it constructs for the URL (three
/// times for a fetch, twice for `ls-remote`); this build reports it once.
pub(super) fn credentials_in_url(repo: &gix::Repository, url: Option<&gix::url::Url>) -> Verdict {
    let Some(url) = url.filter(|u| u.password().is_some()) else {
        return Verdict::Ok;
    };
    let policy = repo
        .config_snapshot()
        .string("transfer.credentialsInUrl")
        .map(|v| v.to_string());
    // gix percent-encodes whatever the password field holds, so the placeholder
    // is a plain token during serialization and swapped for git's literal
    // `<redacted>` afterwards.
    const TOKEN: &str = "zvcsRedactedPasswordPlaceholder";
    let mut redacted = url.clone();
    redacted.set_password(Some(TOKEN.into()));
    let redacted = redacted.to_bstring().to_string().replace(TOKEN, "<redacted>");
    match policy.as_deref() {
        Some("die") => {
            eprintln!("fatal: URL '{redacted}' uses plaintext credentials");
            Verdict::Fatal
        }
        Some("warn") => {
            eprintln!("warning: URL '{redacted}' uses plaintext credentials");
            Verdict::Ok
        }
        _ => Verdict::Ok,
    }
}

/// `--recurse-submodules`' tri-state, minus git's `on-demand` which needs the
/// superproject's old/new gitlinks to decide and is refused at parse time.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Recurse {
    Yes,
    No,
}

/// Parsed command-line options shared across every remote a single invocation
/// touches (`--all`/`--multiple` fan out but carry the same flags).
struct FetchOpts {
    dry_run: bool,
    verbose: bool,
    quiet: bool,
    force: bool,
    // `None` = neither the flag nor the config set it (git's "unspecified");
    // resolved to a concrete value from `fetch.prune`/`fetch.pruneTags` before
    // dispatch, so `Some(true)` here means "prune" regardless of origin.
    prune: Option<bool>,
    prune_tags: Option<bool>,
    tags: Option<Tags>,
    shallow: Option<Shallow>,
    /// `--porcelain`: rows go to stdout in the machine-readable layout.
    porcelain: bool,
    /// `--write-fetch-head` (git's default) / `--no-write-fetch-head`.
    write_fetch_head: bool,
    /// `-a`/`--append`: add to the existing FETCH_HEAD instead of truncating it.
    append: bool,
    /// `--progress` forced on / `--no-progress` forced off / unset = auto.
    progress: Option<bool>,
    /// Resolved `--show-forced-updates` / `fetch.showForcedUpdates`.
    show_forced_updates: bool,
    /// `--prefetch`: every destination moves under `refs/prefetch/`.
    prefetch: bool,
    /// `-u`/`--update-head-ok`.
    update_head_ok: bool,
    /// Resolved `--write-commit-graph` / `fetch.writeCommitGraph`.
    write_commit_graph: bool,
    /// `fetch.output=compact`.
    compact: bool,
    /// Resolved `-j`/`--jobs` / `fetch.parallel`, always at least 1.
    jobs: usize,
    /// `--upload-pack <path>`; `remote.<name>.uploadpack` supplies the per-remote default.
    upload_pack: Option<String>,
    /// `-o`/`--server-option`, repeatable; `remote.<name>.serverOption` supplies the default.
    server_options: Vec<BString>,
    /// The refspecs `--refmap` supplied, already parsed. `None` means no `--refmap` was given at all,
    /// which is what decides whether the configured refspecs act as the opportunistic ones.
    refmap: Option<Vec<gix::refspec::RefSpec>>,
}

impl Default for FetchOpts {
    fn default() -> Self {
        FetchOpts {
            dry_run: false,
            verbose: false,
            quiet: false,
            force: false,
            prune: None,
            prune_tags: None,
            tags: None,
            shallow: None,
            porcelain: false,
            // git writes FETCH_HEAD unless `--no-write-fetch-head` is given.
            write_fetch_head: true,
            append: false,
            progress: None,
            show_forced_updates: true,
            prefetch: false,
            update_head_ok: false,
            write_commit_graph: false,
            compact: false,
            jobs: 1,
            upload_pack: None,
            server_options: Vec::new(),
            refmap: None,
        }
    }
}

/// One line of the git-style per-ref summary.
struct Line {
    flag: char,
    summary: String,
    from: String,
    to: String,
    reason: &'static str,
    /// Value the ref held before the fetch, for `--porcelain`'s second column.
    old: gix::ObjectId,
    /// Value it holds afterwards, for `--porcelain`'s third column.
    new: gix::ObjectId,
    /// Full local ref name for `--porcelain`'s fourth column (`FETCH_HEAD` for
    /// the rows that have no tracking ref).
    full: String,
}

/// The `.git/FETCH_HEAD` sink for one `git fetch` invocation.
///
/// git opens the file once per command and appends for every remote after the
/// first, so `--all`/`--multiple` accumulate rather than overwrite; `-a` starts
/// in append mode from the outset. Under `--dry-run` or `--no-write-fetch-head`
/// nothing is opened at all.
struct FetchHead {
    path: std::path::PathBuf,
    enabled: bool,
    truncate: bool,
}

impl FetchHead {
    /// Write one remote's rows, merge candidates first, exactly as git's two
    /// passes over `store_updated_refs` do.
    fn write(&mut self, rows: &[(String, bool)]) -> Result<()> {
        if !self.enabled {
            return Ok(());
        }
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(self.truncate)
            .append(!self.truncate)
            .open(&self.path)?;
        self.truncate = false;
        for for_merge in [true, false] {
            for (note, _) in rows.iter().filter(|(_, m)| *m == for_merge) {
                writeln!(f, "{note}")?;
            }
        }
        Ok(())
    }
}

/// Prepend `+` (force) to a refspec string unless it is already forced or a
/// negative/exclude spec (`^`).
fn forced(spec: BString) -> BString {
    match spec.first() {
        Some(b'+') | Some(b'^') => spec,
        _ => {
            let mut out = BString::from("+");
            out.extend_from_slice(&spec);
            out
        }
    }
}

/// git's `filter_prefetch_refspec`: move every destination under
/// `refs/prefetch/`, drop the specs that target `refs/tags/` or have no
/// destination at all, and force the rest.
///
/// A destination that already starts with `refs/` keeps the remainder
/// (`refs/remotes/origin/*` → `refs/prefetch/remotes/origin/*`); anything else is
/// appended whole.
fn prefetch_spec(spec: &BStr) -> Option<BString> {
    let s = spec.to_str().ok()?;
    let s = s.strip_prefix('+').unwrap_or(s);
    if s.starts_with('^') {
        return Some(spec.to_owned());
    }
    let (src, dst) = s.split_once(':')?;
    if dst.is_empty() || dst.starts_with("refs/tags/") {
        return None;
    }
    let tail = dst.strip_prefix("refs/").unwrap_or(dst);
    Some(BString::from(format!("+{src}:refs/prefetch/{tail}")))
}

/// git's minimum width for the `<from>` column (`refcol_width` in
/// `builtin/fetch.c`, which starts at 10 and only grows).
const REFCOL_WIDTH: usize = 10;

/// The URL as `git fetch` shows it in the `From …` header and in every
/// FETCH_HEAD row: trailing slashes are dropped, and a trailing `.git` with it
/// (`store_updated_refs` computes `url_len` exactly this way).
fn display_url(url: &str) -> &str {
    let trimmed = url.trim_end_matches('/');
    match trimmed.strip_suffix(".git") {
        // git requires more than four characters before the suffix, so a bare
        // `.git` (or `x.git`) keeps its name.
        Some(head) if head.len() > 1 => head,
        _ => trimmed,
    }
}

/// The number of hex characters `git fetch` abbreviates object ids to in its
/// summary, which also fixes the summary column width
/// (`TRANSPORT_SUMMARY_WIDTH` is `2 * DEFAULT_ABBREV + 3`).
///
/// `core.abbrev` overrides git's built-in 7; `auto` and out-of-range values fall
/// back to it. git additionally lengthens an abbreviation that would be
/// ambiguous in the local object database, which this port does not do.
fn abbrev_len(repo: &gix::Repository) -> usize {
    const FALLBACK: usize = 7;
    let max = repo.object_hash().len_in_hex();
    repo.config_snapshot()
        .string("core.abbrev")
        .map(|v| v.to_string())
        .and_then(|v| v.parse::<usize>().ok())
        .filter(|n| (4..=max).contains(n))
        .unwrap_or(FALLBACK)
}

/// git's compact `fetch.output`: when one of `<from>`/`<to>` contains the other
/// as a substring, the containing side shows `*` in place of it.
fn compact(from: &str, to: &str) -> (String, String) {
    if !from.is_empty() && to.contains(from) {
        (from.to_string(), to.replace(from, "*"))
    } else if !to.is_empty() && from.contains(to) {
        (from.replace(to, "*"), to.to_string())
    } else {
        (from.to_string(), to.to_string())
    }
}

/// The `<kind> '<what>' of <url>` tail of a FETCH_HEAD row, from the full remote
/// ref name (git's `store_updated_refs`).
fn fetch_head_note(id: gix::ObjectId, for_merge: bool, remote_ref: &str, url: &str) -> String {
    let (kind, what) = match remote_ref {
        "HEAD" => ("", ""),
        r if r.starts_with("refs/heads/") => ("branch", &r["refs/heads/".len()..]),
        r if r.starts_with("refs/tags/") => ("tag", &r["refs/tags/".len()..]),
        r if r.starts_with("refs/remotes/") => ("remote-tracking branch", &r["refs/remotes/".len()..]),
        r => ("", r),
    };
    let mut note = format!(
        "{}\t{}\t",
        id.to_hex(),
        if for_merge { "" } else { "not-for-merge" }
    );
    if !what.is_empty() {
        if !kind.is_empty() {
            note.push_str(kind);
            note.push(' ');
        }
        note.push_str(&format!("'{what}' of "));
    }
    note.push_str(url);
    note
}

/// git's glob rules for a *fetch* refspec (`parse_refspec()` in `refspec.c`).
///
/// A `*` on one side must be matched by a `*` on the other, and a pattern source with no destination at all
/// is refused — `refs/heads/*` alone would name a set of refs with nowhere to put them. Negative (`^`) specs
/// carry only a left-hand side and are exempt from the second rule.
fn refspec_globs_agree(spec: &str) -> bool {
    let (negative, body) = match spec.strip_prefix('^') {
        Some(rest) => (true, rest),
        None => (false, spec.strip_prefix('+').unwrap_or(spec)),
    };
    // git splits on the *last* colon.
    let (src, dst) = match body.rfind(':') {
        Some(at) => (&body[..at], Some(&body[at + 1..])),
        None => (body, None),
    };
    let dst_is_glob = dst.is_some_and(|d| d.contains('*'));
    if !src.is_empty() && src.contains('*') {
        if dst.is_some() && !dst_is_glob {
            return false;
        }
        if dst.is_none() && !negative {
            return false;
        }
    } else if dst_is_glob {
        return false;
    }
    true
}

/// The program to run instead of `git-upload-pack` on the other end.
///
/// `--upload-pack` wins over `remote.<name>.uploadpack`, which git reads in `get_upload_pack()`.
pub(super) fn upload_pack_program(
    repo: &gix::Repository,
    remote_name: Option<&str>,
    opts_upload_pack: Option<&str>,
) -> Option<BString> {
    if let Some(program) = opts_upload_pack {
        return Some(program.into());
    }
    let name = remote_name?;
    repo.config_snapshot()
        .string(&format!("remote.{name}.uploadpack"))
        .map(|v| v.to_owned())
}

/// The protocol-v2 server options to transmit.
///
/// `--server-option` replaces `remote.<name>.serverOption` rather than adding to it, as documented: "These
/// server options can be overridden by the `--server-option=` command line arguments."
pub(super) fn server_options_for(
    repo: &gix::Repository,
    remote_name: Option<&str>,
    from_command_line: &[BString],
) -> Vec<BString> {
    if !from_command_line.is_empty() {
        return from_command_line.to_vec();
    }
    let Some(name) = remote_name else {
        return Vec::new();
    };
    repo.config_snapshot()
        .strings(&format!("remote.{name}.serverOption"))
        .map(|values| {
            values
                .into_iter()
                // An empty value in a higher-priority file clears everything inherited so far.
                .fold(Vec::new(), |mut acc, v| {
                    if v.is_empty() {
                        acc.clear();
                    } else {
                        acc.push(v.to_owned());
                    }
                    acc
                })
        })
        .unwrap_or_default()
}

/// Run the fetch pipeline for a single remote and print its summary.
#[allow(clippy::too_many_arguments)]
fn fetch_one(
    repo: &gix::Repository,
    name_or_url: Option<&BStr>,
    refspecs: &[&str],
    opts: &FetchOpts,
    upstream: Option<&(String, String)>,
    fetch_head: &mut FetchHead,
    progress: &mut prodash::tree::Item,
) -> Result<Verdict> {
    let mut remote = repo.find_fetch_remote(name_or_url)?;
    let remote_name = remote.name().map(|n| n.as_bstr().to_string());

    // `transfer.credentialsInUrl` is checked before any connection is opened,
    // where git checks it.
    if credentials_in_url(repo, remote.url(gix::remote::Direction::Fetch)) == Verdict::Fatal {
        return Ok(Verdict::Fatal);
    }

    // The configured fetch refspecs, captured before command-line refspecs replace them: with explicit
    // refspecs they become git's *opportunistic* second stage, mapping the refs the command line selected onto
    // the tracking refs they would normally land in (`get_ref_map` in `builtin/fetch.c`).
    let configured_refspecs: Vec<gix::refspec::RefSpec> = remote
        .refspecs(gix::remote::Direction::Fetch)
        .iter()
        .map(|s| s.to_ref().to_owned())
        .collect();

    // Tag handling: `-t` → all tags, `-n` → none. Injected as an implicit
    // `refs/tags/*:refs/tags/*` refspec by the ref-map builder.
    if let Some(tags) = opts.tags {
        remote = remote.with_fetch_tags(tags);
    }

    // Refspec selection. Explicit command-line refspecs replace the configured
    // set and additionally make every FETCH_HEAD row a merge candidate, as git's
    // `get_ref_map` does when `refspec_count > 0`.
    let explicit_refspecs = !refspecs.is_empty();
    if explicit_refspecs {
        let specs: Vec<BString> = refspecs
            .iter()
            .map(|r| {
                let s = BString::from(*r);
                if opts.force {
                    forced(s)
                } else {
                    s
                }
            })
            .collect();
        remote.replace_refspecs(specs, gix::remote::Direction::Fetch)?;
    } else if opts.force {
        let specs: Vec<BString> = remote
            .refspecs(gix::remote::Direction::Fetch)
            .iter()
            .map(|s| forced(s.to_ref().to_bstring()))
            .collect();
        remote.replace_refspecs(specs, gix::remote::Direction::Fetch)?;
    }

    // git's `*autotags`: automatic tag following is armed by a command-line refspec only if that refspec has a
    // destination. `git fetch <remote> <branch>` therefore fetches no tags at all, while
    // `git fetch <remote> <branch>:<dst>` does. An explicit `--tags`/`--no-tags` decides on its own.
    if explicit_refspecs && opts.tags.is_none() {
        let any_destination = remote
            .refspecs(gix::remote::Direction::Fetch)
            .iter()
            .any(|s| s.to_ref().destination().is_some());
        if !any_destination {
            remote = remote.with_fetch_tags(Tags::None);
        }
    }

    // `--prefetch` rewrites every destination under `refs/prefetch/` and forces
    // it; specs that would land in `refs/tags/` are dropped entirely.
    if opts.prefetch {
        let specs: Vec<BString> = remote
            .refspecs(gix::remote::Direction::Fetch)
            .iter()
            .filter_map(|s| prefetch_spec(s.to_ref().to_bstring().as_bstr()))
            .collect();
        remote.replace_refspecs(specs, gix::remote::Direction::Fetch)?;
        // Tag following would reintroduce `refs/tags/*`, which git's prefetch
        // filter removes, so it is switched off for the duration.
        remote = remote.with_fetch_tags(Tags::None);
    }

    // Destination prefixes to prune (glob refspec destinations only), captured
    // before the remote is consumed by `connect`.
    let mut prune_prefixes: Vec<Vec<u8>> = Vec::new();
    if opts.prune == Some(true) {
        for s in remote.refspecs(gix::remote::Direction::Fetch) {
            if let Some(dst) = s.to_ref().destination() {
                let dst: &[u8] = dst.as_ref();
                if let Some(star) = dst.iter().position(|&b| b == b'*') {
                    prune_prefixes.push(dst[..star].to_vec());
                }
            }
        }
        // `-P` adds the tags refspec, so its destination joins the prune set.
        if opts.prune_tags == Some(true) {
            prune_prefixes.push(b"refs/tags/".to_vec());
        }
        prune_prefixes.sort();
        prune_prefixes.dedup();
    }

    // `-P` fetches all tags via an implicit refspec so pruning has the full
    // remote tag set to diff against, without persisting the spec to config.
    let mut extra_refspecs = Vec::new();
    if opts.prune_tags == Some(true) {
        extra_refspecs.push(
            gix::refspec::parse(
                "refs/tags/*:refs/tags/*".into(),
                gix::refspec::parse::Operation::Fetch,
            )?
            .to_owned(),
        );
    }
    // git's second matching stage. With command-line refspecs the configured refspecs no longer select refs;
    // they map the refs that were selected onto their tracking refs, so `git fetch origin main` still moves
    // `refs/remotes/origin/main`. `--refmap` replaces them for that purpose only.
    let mut opportunistic_refspecs = if explicit_refspecs {
        opts.refmap.clone().unwrap_or(configured_refspecs)
    } else {
        Vec::new()
    };
    if opts.prefetch {
        // `filter_prefetch_refspec` rewrites `remote->fetch` as well as the command-line refspecs.
        opportunistic_refspecs = opportunistic_refspecs
            .iter()
            .filter_map(|s| prefetch_spec(s.to_ref().to_bstring().as_bstr()))
            .filter_map(|s| {
                gix::refspec::parse(s.as_ref(), gix::refspec::parse::Operation::Fetch)
                    .ok()
                    .map(|s| s.to_owned())
            })
            .collect();
    } else if opts.force {
        opportunistic_refspecs = opportunistic_refspecs
            .iter()
            .filter_map(|s| {
                gix::refspec::parse(
                    forced(s.to_ref().to_bstring()).as_ref(),
                    gix::refspec::parse::Operation::Fetch,
                )
                .ok()
                .map(|s| s.to_owned())
            })
            .collect();
    }

    let map_options = gix::remote::ref_map::Options {
        extra_refspecs,
        opportunistic_refspecs,
        ..Default::default()
    };

    let raw_url = remote
        .url(gix::remote::Direction::Fetch)
        .map(ToString::to_string)
        .or_else(|| remote.name().map(|n| n.as_bstr().to_string()))
        .unwrap_or_default();
    let url = display_url(&raw_url).to_string();
    let abbrev = abbrev_len(repo);

    let connect_options = gix::remote::connect::Options {
        upload_pack: upload_pack_program(repo, remote_name.as_deref(), opts.upload_pack.as_deref()),
    };
    let server_options = server_options_for(repo, remote_name.as_deref(), &opts.server_options);

    let should_interrupt = AtomicBool::new(false);
    let prepared = match remote
        .connect_with_options(gix::remote::Direction::Fetch, connect_options)?
        .with_server_options(server_options)
        .prepare_fetch(&mut *progress, map_options)
    {
        Ok(p) => p,
        // git's `die_if_server_options()` also prints the advice line, and both it and the
        // "server doesn't support" case are `fatal:` exits rather than per-remote failures.
        Err(gix::remote::fetch::prepare::Error::RefMap(
            e @ gix::remote::ref_map::Error::ServerOptionsRequireV2,
        )) => {
            eprintln!("hint: see protocol.version in 'git help config' for more details");
            eprintln!("fatal: {e}");
            return Ok(Verdict::Fatal);
        }
        Err(gix::remote::fetch::prepare::Error::RefMap(
            e @ gix::remote::ref_map::Error::ServerOptionsUnsupported,
        )) => {
            eprintln!("fatal: {e}");
            return Ok(Verdict::Fatal);
        }
        Err(e) => return Err(e.into()),
    };
    let outcome = prepared
        .with_dry_run(opts.dry_run)
        .with_shallow(opts.shallow.clone().unwrap_or_default())
        .with_reflog_message(RefLogMessage::Prefixed {
            action: "fetch".into(),
        })
        .receive(&mut *progress, &should_interrupt)?;

    // Both status variants carry the ref-update outcome; the ref_map ties each
    // update back to its remote/local mapping.
    let ref_map = &outcome.ref_map;
    let update_refs = match &outcome.status {
        Status::NoPackReceived { update_refs, .. } => update_refs,
        Status::Change { update_refs, .. } => update_refs,
    };

    let null = gix::ObjectId::null(repo.object_hash());

    // --- build the git-style per-ref summary ------------------------------
    let mut update_lines: Vec<Line> = Vec::new();
    let mut fetch_head_rows: Vec<(String, bool)> = Vec::new();
    let mut rejected = false;
    // Set when a refspec would overwrite a ref some worktree has checked out and
    // `--update-head-ok` was not given: git turns that into a fatal for the whole
    // command rather than a per-ref rejection.
    let mut checked_out: Option<(String, std::path::PathBuf)> = None;

    for (update, mapping, spec, edit) in update_refs.iter_mapping_updates(
        &ref_map.mappings,
        &ref_map.refspecs,
        &ref_map.extra_refspecs,
    ) {
        let remote_full = mapping
            .remote
            .as_name()
            .map(|n| n.to_string())
            .unwrap_or_default();
        let remote_id = mapping.remote.as_id().map(ToOwned::to_owned);

        // Opportunistic mappings exist only to move the tracking ref. git marks them `FETCH_HEAD_IGNORE`
        // because their row would duplicate the one the command-line refspec already contributed.
        let opportunistic = ref_map.is_opportunistic(mapping);
        // git marks every entry a *command-line* refspec produced `FETCH_HEAD_MERGE`. Refs that only
        // automatic tag following pulled in are added afterwards and keep the default `not-for-merge`.
        let from_command_line = explicit_refspecs
            && matches!(
                mapping.spec_index,
                gix::protocol::fetch::refmap::SpecIndex::ExplicitInRemote(_)
            );

        let from = mapping
            .remote
            .as_name()
            .and_then(|n| FullName::try_from(n).ok())
            .map(|f| f.shorten().to_string())
            .or_else(|| mapping.remote.as_id().map(|id| id.to_hex_with_len(abbrev).to_string()))
            .unwrap_or_default();

        // A mapping with no local destination lands in FETCH_HEAD only, which git
        // reports as a `* <kind> <from> -> FETCH_HEAD` row.
        let local_full = match mapping.local.as_ref() {
            Some(name) => match FullName::try_from(BStr::new(name)) {
                Ok(f) => f,
                Err(_) => continue,
            },
            None => {
                // `--no-write-fetch-head` drops the row as well as the file;
                // `--dry-run` keeps the row and skips only the file.
                if !opts.write_fetch_head {
                    continue;
                }
                if let Some(id) = remote_id {
                    let for_merge = from_command_line
                        || (!explicit_refspecs
                            && upstream.is_some_and(|(r, m)| {
                                Some(r.as_str()) == remote_name.as_deref() && *m == remote_full
                            }));
                    fetch_head_rows.push((
                        fetch_head_note(id, for_merge, &remote_full, &url),
                        for_merge,
                    ));
                    let kind = match remote_full.as_str() {
                        r if r.starts_with("refs/heads/") => "branch",
                        r if r.starts_with("refs/tags/") => "tag",
                        r if r.starts_with("refs/remotes/") => "remote-tracking branch",
                        _ => "",
                    };
                    update_lines.push(Line {
                        flag: '*',
                        summary: kind.to_string(),
                        from,
                        to: "FETCH_HEAD".to_string(),
                        reason: "",
                        old: null,
                        new: id,
                        full: "FETCH_HEAD".to_string(),
                    });
                }
                continue;
            }
        };
        let to = local_full.shorten().to_string();
        let is_tag = matches!(local_full.category(), Some(Category::Tag));

        // A tag the repository already has is invisible under automatic tag
        // following: git's `find_non_local_tags` only proposes tags that are
        // missing locally, so such a tag never enters the ref map and appears in
        // neither the summary (not even under `-v`) nor FETCH_HEAD. gitoxide's
        // implicit tag refspec maps it regardless, so it is dropped here. An
        // explicit `--tags` fetches the whole namespace and does list them.
        if is_tag && !matches!(opts.tags, Some(Tags::All)) && update.mode == Mode::NoChangeNeeded {
            continue;
        }

        // Every mapping with a local destination contributes a FETCH_HEAD row,
        // whether or not the tracking ref actually moved.
        if let (Some(id), false) = (remote_id, opportunistic) {
            let for_merge = from_command_line
                || (!explicit_refspecs
                    && upstream.is_some_and(|(r, m)| {
                        Some(r.as_str()) == remote_name.as_deref() && *m == remote_full
                    }));
            fetch_head_rows.push((
                fetch_head_note(id, for_merge, &remote_full, &url),
                for_merge,
            ));
        }

        // Old/new ids for range summaries, extracted from the applied edit.
        let (old_id, new_id) = match edit.map(|e| &e.change) {
            Some(Change::Update { expected, new, .. }) => {
                let old = match expected {
                    PreviousValue::MustExistAndMatch(Target::Object(id)) => Some(*id),
                    _ => None,
                };
                let new = match new {
                    Target::Object(id) => Some(*id),
                    _ => None,
                };
                (old, new)
            }
            _ => (None, None),
        };
        let range = |sep: &str| match (old_id, new_id) {
            (Some(o), Some(n)) => {
                format!("{}{sep}{}", o.to_hex_with_len(abbrev), n.to_hex_with_len(abbrev))
            }
            _ => String::new(),
        };

        let (flag, summary, reason): (char, String, &'static str) = match &update.mode {
            Mode::New => {
                let s = if is_tag { "[new tag]" } else { "[new branch]" };
                ('*', s.to_string(), "")
            }
            Mode::FastForward => (' ', range(".."), ""),
            // `--no-show-forced-updates` / `fetch.showForcedUpdates=false` skips
            // the forced-update check outright, so git reports the ref as an
            // ordinary fast-forward: a blank flag, a `..` range and no note.
            Mode::Forced if !opts.show_forced_updates => (' ', range(".."), ""),
            Mode::Forced => ('+', range("..."), "  (forced update)"),
            Mode::NoChangeNeeded => {
                if !opts.verbose {
                    continue;
                }
                ('=', "[up to date]".to_string(), "")
            }
            Mode::ImplicitTagNotSentByRemote => continue,
            Mode::RejectedNonFastForward => {
                rejected = true;
                ('!', "[rejected]".to_string(), "  (non-fast-forward)")
            }
            Mode::RejectedTagUpdate => {
                rejected = true;
                ('!', "[rejected]".to_string(), "  (would clobber existing tag)")
            }
            Mode::RejectedCurrentlyCheckedOut { worktree_dirs } => {
                // `-u`/`--update-head-ok` lifts the guard gitoxide applies to the
                // ref a worktree has checked out. The pack is already local at
                // this point, so the update is applied here with the same
                // fast-forward rule the refspec carries, and reported like any
                // other update rather than as a rejection.
                match (opts.update_head_ok, remote_id) {
                    (true, Some(id)) => {
                        match update_checked_out_ref(
                            repo,
                            &local_full,
                            id,
                            opts,
                            spec.is_some_and(|s| s.allow_non_fast_forward()),
                        )? {
                            Some((f, s, r)) => (f, s, r),
                            None => continue,
                        }
                    }
                    // Without it git refuses the whole command up front, naming
                    // the ref and the worktree that holds it, and exits 128
                    // without a summary or a FETCH_HEAD.
                    _ => {
                        checked_out = Some((
                            local_full.as_bstr().to_string(),
                            worktree_dirs.first().cloned().unwrap_or_default(),
                        ));
                        break;
                    }
                }
            }
            Mode::RejectedToReplaceWithUnborn => {
                rejected = true;
                ('!', "[rejected]".to_string(), "  (would replace with unborn)")
            }
            Mode::RejectedSourceObjectNotFound { .. } => {
                rejected = true;
                ('!', "[rejected]".to_string(), "  (source object not found)")
            }
        };
        // `--porcelain`'s two id columns: a ref that did not exist before shows
        // the null id on the left, and one that stayed put repeats its own id on
        // both sides (git prints `<old-object-id> <new-object-id>` either way).
        let (porcelain_old, porcelain_new) = match &update.mode {
            Mode::New => (null, remote_id.unwrap_or(null)),
            _ => (
                old_id.or(remote_id).unwrap_or(null),
                new_id.or(remote_id).unwrap_or(null),
            ),
        };
        update_lines.push(Line {
            flag,
            summary,
            from,
            to,
            reason,
            old: porcelain_old,
            new: porcelain_new,
            full: local_full.as_bstr().to_string(),
        });
    }

    if let Some((name, worktree)) = checked_out {
        // gitoxide reports the worktree as the repository was discovered (often
        // `.`); git names it absolutely, so the path is anchored on the current
        // directory and lexically normalized — no symlink resolution, which git
        // does not do either.
        let cwd = std::env::current_dir().unwrap_or_default();
        let absolute: std::path::PathBuf = if worktree.is_absolute() {
            worktree.clone()
        } else {
            cwd.join(&worktree)
        }
        .components()
        .filter(|c| !matches!(c, std::path::Component::CurDir))
        .collect();
        let worktree = gix::path::normalize(std::borrow::Cow::Owned(absolute.clone()), &cwd)
            .map(std::borrow::Cow::into_owned)
            .unwrap_or(absolute);
        eprintln!(
            "fatal: refusing to fetch into branch '{name}' checked out at '{}'",
            worktree.display()
        );
        return Ok(Verdict::Fatal);
    }

    // --- prune stale tracking refs ----------------------------------------
    let mut prune_lines: Vec<Line> = Vec::new();
    if !prune_prefixes.is_empty() {
        // Every local ref the remote still advertises is kept; the rest under a
        // pruned prefix are deleted (git's `prune_refs`).
        let kept: HashSet<BString> = ref_map
            .mappings
            .iter()
            .filter_map(|m| m.local.clone())
            .collect();
        let mut pruned: HashSet<BString> = HashSet::new();

        // Collect candidates first, then delete: mutating refs while the ref
        // iterator still borrows the store would be unsound.
        let mut to_delete: Vec<(FullName, String, gix::ObjectId)> = Vec::new();
        for prefix in &prune_prefixes {
            for r in repo.references()?.prefixed(&prefix[..])? {
                let r = r.map_err(anyhow::Error::msg)?;
                // Never prune symbolic tracking refs like `refs/remotes/*/HEAD`.
                if matches!(r.target(), TargetRef::Symbolic(_)) {
                    continue;
                }
                let full = r.name().as_bstr().to_owned();
                if kept.contains(&full) || !pruned.insert(full.clone()) {
                    continue;
                }
                let id = match r.target() {
                    TargetRef::Object(id) => id.to_owned(),
                    TargetRef::Symbolic(_) => continue,
                };
                to_delete.push((
                    FullName::try_from(full.as_bstr())?,
                    r.name().shorten().to_string(),
                    id,
                ));
            }
        }

        for (name, short, id) in to_delete {
            if !opts.dry_run {
                repo.edit_reference(RefEdit {
                    change: Change::Delete {
                        expected: PreviousValue::Any,
                        log: RefLog::AndReference,
                    },
                    name: name.clone(),
                    deref: false,
                })?;
            }
            prune_lines.push(Line {
                flag: '-',
                summary: "[deleted]".to_string(),
                from: "(none)".to_string(),
                to: short,
                reason: "",
                old: id,
                new: null,
                full: name.as_bstr().to_string(),
            });
        }
    }

    fetch_head.write(&fetch_head_rows)?;

    // --- print the summary ------------------------------------------------
    // Pruned refs are reported first, mirroring git's prune-before-fetch order.
    let mut lines = prune_lines;
    lines.extend(update_lines);

    if !opts.quiet && !lines.is_empty() {
        if opts.porcelain {
            // Machine-readable: `<flag> <old> <new> <local-ref>` on stdout, with
            // no `From <url>` header — git documents this as the parseable form.
            let mut out = String::new();
            for l in &lines {
                out.push_str(&format!(
                    "{} {} {} {}\n",
                    l.flag,
                    l.old.to_hex(),
                    l.new.to_hex(),
                    l.full
                ));
            }
            print!("{out}");
        } else {
            let rendered: Vec<(String, String)> = lines
                .iter()
                .map(|l| {
                    if opts.compact {
                        compact(&l.from, &l.to)
                    } else {
                        (l.from.clone(), l.to.clone())
                    }
                })
                .collect();
            // git's columns are fixed, not fitted: the summary is padded to
            // `TRANSPORT_SUMMARY_WIDTH` (`2 * <abbrev> + 3`, wide enough for an
            // `<old>...<new>` range) and the `<from>` column starts at
            // `REFCOL_WIDTH` and only grows past it for a longer name.
            let sw = 2 * abbrev + 3;
            let fw = rendered
                .iter()
                .map(|(f, _)| f.chars().count())
                .max()
                .unwrap_or(0)
                .max(REFCOL_WIDTH);
            eprintln!("From {url}");
            for (l, (from, to)) in lines.iter().zip(&rendered) {
                eprintln!(
                    " {} {:<sw$} {:<fw$} -> {}{}",
                    l.flag, l.summary, from, to, l.reason,
                );
            }
        }
    }

    Ok(if rejected { Verdict::Rejected } else { Verdict::Ok })
}

/// Apply the update gitoxide refused because the destination is checked out in a
/// worktree, which is what `-u`/`--update-head-ok` asks for.
///
/// The refspec's own force bit still decides whether a non-fast-forward is
/// allowed, so the outcome is one of git's ordinary summary rows: a fast-forward,
/// a forced update, "up to date", or a non-fast-forward rejection. `None` means
/// there is nothing to report (the ref already pointed at `new_id` and the
/// summary is not verbose).
fn update_checked_out_ref(
    repo: &gix::Repository,
    name: &FullName,
    new_id: gix::ObjectId,
    opts: &FetchOpts,
    allow_non_fast_forward: bool,
) -> Result<Option<(char, String, &'static str)>> {
    let existing = repo.find_reference(name.as_bstr())?;
    let old_id = existing.clone().peel_to_id()?.detach();
    if old_id == new_id {
        return Ok(if opts.verbose {
            Some(('=', "[up to date]".to_string(), ""))
        } else {
            None
        });
    }
    // A fast-forward is an update whose old value is an ancestor of the new one.
    let fast_forward = repo
        .merge_base(old_id, new_id)
        .map(|base| base.detach() == old_id)
        .unwrap_or(false);
    if !fast_forward && !allow_non_fast_forward {
        return Ok(Some((
            '!',
            "[rejected]".to_string(),
            "  (non-fast-forward)",
        )));
    }
    if !opts.dry_run {
        repo.edit_reference(RefEdit {
            change: Change::Update {
                log: LogChange {
                    mode: RefLog::AndReference,
                    force_create_reflog: false,
                    message: format!("fetch: {}..{}", old_id.to_hex(), new_id.to_hex()).into(),
                },
                expected: PreviousValue::MustExistAndMatch(Target::Object(old_id)),
                new: Target::Object(new_id),
            },
            name: name.clone(),
            deref: false,
        })?;
    }
    let abbrev = abbrev_len(repo);
    let range = |sep: &str| {
        format!(
            "{}{sep}{}",
            old_id.to_hex_with_len(abbrev),
            new_id.to_hex_with_len(abbrev)
        )
    };
    Ok(Some(if fast_forward || !opts.show_forced_updates {
        (' ', range(".."), "")
    } else {
        ('+', range("..."), "  (forced update)")
    }))
}

/// `--recurse-submodules[=yes]`: run this binary's own `fetch` inside every
/// populated submodule, `--jobs` at a time.
///
/// git fetches in submodules with the superproject's flags; only the ones that
/// make sense below the top level are forwarded here (verbosity, prune, tags and
/// the recursion itself), since the superproject's refspecs and remote names do
/// not apply to a submodule. Returns `true` if any submodule fetch failed.
fn fetch_submodules(repo: &gix::Repository, opts: &FetchOpts) -> Result<bool> {
    let Some(modules) = repo.submodules()? else {
        return Ok(false);
    };
    let mut dirs: Vec<std::path::PathBuf> = Vec::new();
    for sm in modules {
        if !sm.is_active().unwrap_or(false) {
            continue;
        }
        // An unpopulated submodule has no repository to fetch into; git skips it.
        if matches!(sm.open(), Ok(Some(_))) {
            if let Ok(dir) = sm.work_dir() {
                dirs.push(dir);
            }
        }
    }
    if dirs.is_empty() {
        return Ok(false);
    }

    let exe = std::env::current_exe()?;
    let mut forwarded: Vec<String> = vec!["fetch".into(), "--recurse-submodules".into()];
    if opts.quiet {
        forwarded.push("--quiet".into());
    }
    if opts.verbose {
        forwarded.push("--verbose".into());
    }
    if opts.prune == Some(true) {
        forwarded.push("--prune".into());
    }
    if matches!(opts.tags, Some(Tags::All)) {
        forwarded.push("--tags".into());
    }
    if matches!(opts.tags, Some(Tags::None)) {
        forwarded.push("--no-tags".into());
    }
    forwarded.push(format!("--jobs={}", opts.jobs));

    let failed = AtomicBool::new(false);
    let next = std::sync::atomic::AtomicUsize::new(0);
    let workers = opts.jobs.min(dirs.len()).max(1);
    std::thread::scope(|scope| {
        for _ in 0..workers {
            let (next, failed, dirs, exe, forwarded) =
                (&next, &failed, &dirs, &exe, &forwarded);
            scope.spawn(move || loop {
                let i = next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                let Some(dir) = dirs.get(i) else { break };
                let status = std::process::Command::new(exe)
                    .arg("-C")
                    .arg(dir)
                    .args(forwarded)
                    .status();
                if !status.map(|s| s.success()).unwrap_or(false) {
                    failed.store(true, std::sync::atomic::Ordering::Relaxed);
                }
            });
        }
    });
    Ok(failed.load(std::sync::atomic::Ordering::Relaxed))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// git's prefetch filter moves the destination under `refs/prefetch/`,
    /// forces it, and drops the tag refspec entirely.
    #[test]
    fn prefetch_rewrites_destinations_and_drops_tags() {
        assert_eq!(
            prefetch_spec(BStr::new("+refs/heads/*:refs/remotes/origin/*"))
                .unwrap()
                .to_string(),
            "+refs/heads/*:refs/prefetch/remotes/origin/*"
        );
        assert_eq!(
            prefetch_spec(BStr::new("refs/heads/main:refs/heads/main"))
                .unwrap()
                .to_string(),
            "+refs/heads/main:refs/prefetch/heads/main"
        );
        assert!(prefetch_spec(BStr::new("refs/tags/*:refs/tags/*")).is_none());
        assert!(prefetch_spec(BStr::new("refs/heads/main")).is_none());
    }

    /// The compact `fetch.output` layout substitutes `*` for whichever of the
    /// two names is contained in the other.
    #[test]
    fn compact_substitutes_the_contained_name() {
        assert_eq!(
            compact("main", "origin/main"),
            ("main".to_string(), "origin/*".to_string())
        );
        assert_eq!(
            compact("origin/main", "main"),
            ("origin/*".to_string(), "main".to_string())
        );
        assert_eq!(
            compact("main", "other"),
            ("main".to_string(), "other".to_string())
        );
    }

    /// The FETCH_HEAD row is `<oid> TAB <not-for-merge|> TAB <kind> '<what>' of <url>`.
    #[test]
    fn fetch_head_rows_match_gits_layout() {
        let id = gix::ObjectId::null(gix::hash::Kind::Sha1);
        assert_eq!(
            fetch_head_note(id, true, "refs/heads/main", "/tmp/o"),
            format!("{}\t\tbranch 'main' of /tmp/o", id.to_hex())
        );
        assert_eq!(
            fetch_head_note(id, false, "refs/tags/v1", "/tmp/o"),
            format!("{}\tnot-for-merge\ttag 'v1' of /tmp/o", id.to_hex())
        );
        assert_eq!(
            fetch_head_note(id, false, "HEAD", "/tmp/o"),
            format!("{}\tnot-for-merge\t/tmp/o", id.to_hex())
        );
    }
}

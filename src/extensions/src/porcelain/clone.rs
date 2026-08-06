use anyhow::{bail, Context, Result};
use prodash::Root as _;
use std::io::IsTerminal;
use std::num::NonZeroU32;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::atomic::AtomicBool;
use std::time::SystemTime;

use gix::bstr::ByteSlice;
use gix::protocol::handshake::Ref;
use gix::remote::fetch::{Shallow, Tags};

/// `git clone <url> [<directory>]` — clone a repository into a new directory.
///
/// Backed by gitoxide's clone builder (`gix::prepare_clone` /
/// `gix::prepare_clone_bare`): a fresh repository is initialized at the
/// destination, a pack is fetched over the compiled-in blocking transport,
/// `HEAD`/remote-tracking refs are written, and — for a non-bare clone — the
/// main worktree is checked out.
///
/// A *local* source then gets git's `clone_local()` treatment: the source object store
/// is adopted wholesale, hardlinked file by file (copied under `--no-hardlinks`, and
/// skipped entirely under `--no-local` or with a `--filter`), replacing what the fetch
/// packed. The clone therefore ends up with the object layout git leaves — loose
/// objects stay loose, packs stay packs — rather than a re-packed equivalent. gitoxide
/// has no local-clone shortcut, so the pack is still built and then discarded; the
/// end state matches git, the work to get there does not.
///
/// Supported forms:
///   * `git clone <url>`             → clone into a directory named after the URL
///   * `git clone <url> <directory>` → clone into an explicit directory
///   * `git clone --bare <url> [<directory>]`
///   * `--mirror`                    → bare + `+refs/*:refs/*` + `remote.<name>.mirror`
///   * `-o`/`--origin <name>`        → track upstream under `<name>` instead of `origin`
///   * `-b`/`--branch <branch>`      → check out `<branch>` instead of the remote's `HEAD`
///   * `-n`/`--no-checkout`          → set up refs/`HEAD` but do not populate a worktree
///   * `-c`/`--config <key>=<value>` → seed the new repository's configuration (repeatable)
///   * `--single-branch`/`--no-single-branch` → clone one branch, or force the wildcard
///   * `--depth <n>`                 → shallow clone truncated to `<n>` commits
///   * `--shallow-since <time>`      → shallow boundary at a cutoff date
///   * `--shallow-exclude <ref>`     → exclude history reachable from a ref (repeatable)
///   * `--no-tags`                   → do not fetch tags
///   * `-u`/`--upload-pack <path>`   → run `<path>` instead of `git-upload-pack` on the other end
///   * `--server-option <opt>`       → protocol-v2 `server-option=<opt>` line (repeatable)
///   * `--reject-shallow`            → refuse to clone from a shallow remote
///   * `--template <dir>`            → seed the new git dir from a template directory
///   * `--separate-git-dir <dir>`    → real git dir elsewhere + a `gitdir:` link file
///   * `-s`/`--shared`, `--reference <repo>`, `--reference-if-able <repo>`,
///     `--dissociate`               → `objects/info/alternates` borrowing
///   * `--sparse`                    → cone-mode sparse checkout of the top level only
///   * `--ref-format <format>`       → `files` accepted; `reftable` rejected (see below)
///   * `--recursive`/`--recurse-submodules[=<pathspec>]` → update submodules after
///     clone and record `submodule.active`
///   * `-j`/`--jobs <n>`             → clone that many submodules at once (forwarded to
///     the recursive `submodule update`, which otherwise reads `submodule.fetchJobs`)
///   * `-4`/`--ipv4`, `-6`/`--ipv6`  → restrict the transport to one address family
///   * `--remote-submodules`         → submodules track their remote branch
///   * `--shallow-submodules`        → each submodule is cloned with `--depth=1`
///
/// `init.defaultSubmodulePathConfig` is honored here as it is in `git init`.
///
/// Progress (remote sideband + local receive/resolve/checkout) is rendered to
/// stderr like git: shown when stderr is a terminal, forced on with `--progress`,
/// and suppressed by `-q`/`--quiet` or `--no-progress`.
/// Any other option is rejected explicitly rather than silently mis-handled.
///
/// # Refspec and `HEAD` setup
///
/// gitoxide's clone always installs `+refs/heads/*:refs/remotes/<name>/*` (or,
/// for a shallow clone, the single-branch spec it derives itself) and cannot have
/// that spec cleared — `Remote::with_refspecs` only appends. The
/// `configure_remote` hook does hand out the owning `Repository`, though, so the
/// option combinations that need a *different* refspec build a brand new remote
/// from that repository and return it in place of gitoxide's. Whatever remote is
/// returned is what gets fetched with *and* what gets written to
/// `remote.<name>.*`, which is exactly git's model:
/// ```text
///   --mirror              +refs/*:refs/*            (+ `mirror = true`)
///   --bare                +refs/heads/*:refs/heads/*
///   --single-branch       +refs/heads/<b>:refs/remotes/<name>/<b>
///   --no-single-branch    +refs/heads/*:refs/remotes/<name>/*
/// ```
/// `--single-branch` resolves `<b>` from the remote's own advertisement (the
/// branch `HEAD` points at, or the `--branch` argument, which may name a tag and
/// then maps onto itself) — the same ls-refs round trip git makes.
///
/// Two leftovers of gitoxide's clone are cleaned up afterwards so a bare or
/// mirrored clone matches stock git byte-for-byte: the `HEAD:refs/remotes/<name>/HEAD`
/// refspec gitoxide always adds implicitly (removed unless the remote really
/// advertised that ref), and the `remote.<name>.fetch` / `remote.<name>.tagOpt` /
/// `branch.<name>.*` entries git does not persist for those modes.
///
/// # Deviations (surfaced honestly, never faked)
/// ```text
///   * `-s`/`--shared` and `--reference` record the alternate in
///     `objects/info/alternates` before the fetch, so the borrow is real and
///     subsequent object lookups resolve through it; whether the negotiation
///     actually skips the borrowed objects is up to gitoxide's object database,
///     so the disk-space saving git guarantees is not guaranteed here.
///   * `--dissociate` writes no alternate at all. The pack is fetched in full,
///     which is the state git reaches by copying the borrowed objects in, so the
///     resulting repository is self-contained either way.
///   * `--ref-format=reftable` is rejected with an honest "not supported" error
///     (no vendored reftable backend). `--ref-format=files` is the format gix
///     writes and is honored; any other value reproduces git's exact
///     `unknown ref storage format '<v>'`.
///   * `--depth` combined with `--no-single-branch` costs one extra ls-refs round
///     trip, because gitoxide has already resolved its implicit single-branch
///     refspec by the time the replacement wildcard remote is installed.
///   * `--single-branch` fetches every tag, not only the ones reachable from the
///     branch. git auto-follows tags during negotiation; the vendored
///     `Tags::Included` maps `refs/tags/*:refs/tags/*` exactly like `Tags::All`
///     (`gix-protocol/src/fetch/types.rs:241`) and no reachability filter exists
///     anywhere in the vendored crates, so an unreachable tag — and the history
///     it pins — is transferred too. Pruning the extra tag refs afterwards would
///     hide the over-fetch rather than fix it, so they are left visible.
///   * Fetched refs are written packed (gitoxide's clone sets
///     `with_write_packed_refs_only`), where stock git leaves some of them loose.
///     The ref set is identical; only the loose/packed split differs.
/// ```
/// `git clone`'s usage block, byte-for-byte from stock git 2.55.0. Printed on
/// stdout for `-h` and on stderr for a usage error, both exiting 129.
const USAGE: &str = "usage: git clone [<options>] [--] <repo> [<dir>]\n\n    -v, --[no-]verbose    be more verbose\n    -q, --[no-]quiet      be more quiet\n    --[no-]progress       force progress reporting\n    --[no-]reject-shallow don't clone shallow repository\n    -n, --no-checkout     don't create a checkout\n    --checkout            opposite of --no-checkout\n    --[no-]bare           create a bare repository\n    --[no-]mirror         create a mirror repository (implies --bare)\n    -l, --[no-]local      to clone from a local repository\n    --no-hardlinks        don't use local hardlinks, always copy\n    --hardlinks           opposite of --no-hardlinks\n    -s, --[no-]shared     setup as shared repository\n    --[no-]recurse-submodules[=<pathspec>]\n                          initialize submodules in the clone\n    --[no-]recursive[=<pathspec>]\n                          alias of --recurse-submodules\n    -j, --[no-]jobs <n>   number of submodules cloned in parallel\n    --[no-]template <template-directory>\n                          directory from which templates will be used\n    --[no-]reference <repo>\n                          reference repository\n    --[no-]reference-if-able <repo>\n                          reference repository\n    --[no-]dissociate     use --reference only while cloning\n    -o, --[no-]origin <name>\n                          use <name> instead of 'origin' to track upstream\n    -b, --[no-]branch <branch>\n                          checkout <branch> instead of the remote's HEAD\n    --[no-]revision <rev> clone single revision <rev> and check out\n    -u, --[no-]upload-pack <path>\n                          path to git-upload-pack on the remote\n    --[no-]depth <depth>  create a shallow clone of that depth\n    --[no-]shallow-since <time>\n                          create a shallow clone since a specific time\n    --[no-]shallow-exclude <ref>\n                          deepen history of shallow clone, excluding ref\n    --[no-]single-branch  clone only one branch, HEAD or --branch\n    --[no-]tags           clone tags, and make later fetches not to follow them\n    --[no-]shallow-submodules\n                          any cloned submodules will be shallow\n    --[no-]separate-git-dir <gitdir>\n                          separate git dir from working tree\n    --[no-]ref-format <format>\n                          specify the reference format to use\n    -c, --[no-]config <key=value>\n                          set config inside the new repository\n    --[no-]server-option <server-specific>\n                          option to transmit\n    -4, --ipv4            use IPv4 addresses only\n    -6, --ipv6            use IPv6 addresses only\n    --[no-]filter <args>  object filtering\n    --[no-]also-filter-submodules\n                          apply partial clone filters to submodules\n    --[no-]remote-submodules\n                          any cloned submodules will use their remote-tracking branch\n    --[no-]sparse         initialize sparse-checkout file to include only files at root\n    --[no-]bundle-uri <uri>\n                          a URI for downloading bundles before fetching from origin remote\n\n";

/// `die()`: the `fatal: ` prefix and exit 128, which is what every clone refusal but the
/// usage errors exits with.
fn fatal(msg: &str) -> ExitCode {
    eprintln!("fatal: {msg}");
    ExitCode::from(128)
}

/// A `parse-options` usage error: the message, a blank line, then the usage block on
/// stderr, exit 129.
fn usage_error(msg: &str) -> ExitCode {
    eprintln!("fatal: {msg}\n");
    eprint!("{USAGE}");
    ExitCode::from(129)
}

pub fn clone(args: &[String]) -> Result<ExitCode> {
    // `-h` is answered by `parse-options` before anything else, on stdout.
    if args.iter().any(|a| a == "-h") {
        print!("{USAGE}");
        return Ok(ExitCode::from(129));
    }
    let mut bare = false;
    let mut mirror = false;
    let mut quiet = false;
    // `Some(true)` = `--progress` forced, `Some(false)` = `--no-progress`,
    // `None` = default (progress iff stderr is a terminal), matching git.
    let mut force_progress: Option<bool> = None;
    // `--no-local` / `--no-hardlinks`, which decide whether a local source's object
    // store is adopted directly and whether that adoption may hardlink.
    let mut no_local = false;
    let mut hardlinks = true;
    let mut recurse_submodules = false;
    // The `<pathspec>` arguments of `--recurse-submodules[=<pathspec>]`, recorded
    // as `submodule.active` in the new repository the way git does.
    let mut submodule_pathspecs: Vec<String> = Vec::new();
    let mut remote_submodules = false;
    // `--shallow-submodules` / `--no-shallow-submodules`, git's `OPT_BOOL`
    // `option_shallow_submodules`. Only the `== 1` case adds `--depth=1` to the
    // recursive `submodule update`; the negation just cancels a prior one.
    let mut shallow_submodules = false;
    let mut no_checkout = false;
    let mut origin: Option<String> = None;
    let mut branch: Option<String> = None;
    // `--no-tags` → `Tags::None`. Left `None` for git's default (fetch all tags).
    let mut tags: Option<Tags> = None;
    // `--reject-shallow` (`true`) / `--no-reject-shallow` (`false`); `None` leaves
    // it unspecified so the `clone.rejectShallow` config value, if any, decides.
    let mut reject_shallow: Option<bool> = None;
    // `-u`/`--upload-pack <path>` and the repeatable `--server-option`, both handed to the transport.
    let mut upload_pack: Option<String> = None;
    let mut server_options: Vec<gix::bstr::BString> = Vec::new();
    // `-j`/`--jobs`: how many submodules the recursive `submodule update` clones at once.
    // git's `max_jobs` starts at `-1` meaning "not given", in which case nothing is forwarded
    // and `submodule update` falls back to `submodule.fetchJobs` itself.
    let mut jobs: Option<String> = None;
    // `-4`/`--ipv4` and `-6`/`--ipv6`, git's `family`. `None` is `TRANSPORT_FAMILY_ALL`.
    let mut address_family: Option<gix::protocol::transport::AddressFamily> = None;
    // `--single-branch` (`true`) / `--no-single-branch` (`false`); `None` leaves the
    // choice to gitoxide, which single-branches a shallow clone like git does.
    let mut single_branch: Option<bool> = None;
    // Shallow-boundary selectors that combine (git's `--shallow-exclude` is a
    // repeatable list → `deepen_not`, `--shallow-since` → `deepen_since`, and the
    // two may be given together). Resolved into a single `Shallow` after parsing.
    let mut depth: Option<NonZeroU32> = None;
    let mut shallow_exclude: Vec<gix::refs::PartialName> = Vec::new();
    let mut shallow_since: Option<gix::date::Time> = None;
    // `-c <key>=<value>`, in command-line order; repeats of a key are kept so each
    // value is written, as git documents.
    let mut config_pairs: Vec<(String, String)> = Vec::new();
    let mut template: Option<String> = None;
    let mut separate_git_dir: Option<String> = None;
    let mut ref_format: Option<String> = None;
    // `--reference <repo>` / `--reference-if-able <repo>`: the bool records
    // whether a missing repository is a warning (`if-able`) or fatal.
    let mut references: Vec<(String, bool)> = Vec::new();
    let mut shared = false;
    let mut dissociate = false;
    let mut sparse = false;
    // `--filter=<spec>`: the object filter of a partial clone, validated up front so a bad spec is
    // rejected with git's own message before the destination directory is created.
    let mut filter: Option<gix::protocol::fetch::filter::Filter> = None;
    // `--also-filter-submodules` / `--no-also-filter-submodules`; `None` leaves the choice to
    // `clone.filterSubmodules`.
    let mut also_filter_submodules: Option<bool> = None;
    // `--bundle-uri=<uri>`: a bundle (or bundle list) to bootstrap the object
    // database from before the fetch. git's `OPT_STRING`, so `--no-bundle-uri`
    // clears it.
    let mut bundle_uri: Option<String> = None;
    let mut positionals: Vec<&str> = Vec::new();

    let mut i = 0;
    let mut end_of_options = false;
    while i < args.len() {
        let a = args[i].as_str();
        i += 1;

        if end_of_options {
            positionals.push(a);
            continue;
        }

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
            "--" => end_of_options = true,
            "--bare" => bare = true,
            "--no-bare" => bare = false,
            // `--mirror` implies `--bare` (git-clone(1)); the implication is applied
            // after parsing so a later `--no-mirror` can still take it back.
            "--mirror" => mirror = true,
            "--no-mirror" => mirror = false,
            "-q" | "--quiet" => quiet = true,
            "--no-quiet" => quiet = false,
            // `-v`/`--verbose` only adds output detail in git; the clone result
            // is identical, so it is accepted without changing behavior.
            "-v" | "--verbose" | "--no-verbose" => {}
            "--progress" => force_progress = Some(true),
            "--no-progress" => force_progress = Some(false),
            // `clone_local()`: a local source has its object store copied (hardlinked
            // where the filesystem allows) instead of being packed and unpacked.
            // `--no-local` forces the transport, `--no-hardlinks` copies the bytes.
            "-l" | "--local" => no_local = false,
            "--no-local" => no_local = true,
            "--no-hardlinks" => hardlinks = false,
            "--hardlinks" => hardlinks = true,
            // `-n`/`--no-checkout`: fetch refs and set `HEAD`, but do not populate
            // a worktree (leaves an empty index, exactly like git).
            "-n" | "--no-checkout" => no_checkout = true,
            "--checkout" => no_checkout = false,
            "-o" | "--origin" => origin = Some(take_value!("--origin")),
            "--no-origin" => origin = None,
            "-b" | "--branch" => branch = Some(take_value!("--branch")),
            "--no-branch" => branch = None,
            "-c" | "--config" => {
                let raw = take_value!("--config");
                config_pairs.push(parse_config_pair(&raw)?);
            }
            "--no-tags" => tags = Some(Tags::None),
            // `--tags` resets to git's default (all tags fetched).
            "--tags" => tags = None,
            // `-u`/`--upload-pack <path>`: the program to run in place of `git-upload-pack`.
            "-u" | "--upload-pack" => upload_pack = Some(take_value!("--upload-pack")),
            "--no-upload-pack" => upload_pack = None,
            // Protocol-v2 server options, repeatable. `-o` is `--origin` for clone, so only the long form.
            "--server-option" => server_options.push(take_value!("--server-option").into()),
            "--no-server-option" => server_options.clear(),
            "-j" | "--jobs" => jobs = Some(take_value!("--jobs")),
            other if other.starts_with("-j") && other.len() > 2 => {
                jobs = Some(other[2..].to_string())
            }
            // git's `OPT_SET_INT` pair writing one `family` slot: last wins, no `--no-` form.
            "-4" | "--ipv4" => address_family = Some(gix::protocol::transport::AddressFamily::V4),
            "-6" | "--ipv6" => address_family = Some(gix::protocol::transport::AddressFamily::V6),
            "--reject-shallow" => reject_shallow = Some(true),
            "--no-reject-shallow" => reject_shallow = Some(false),
            "--single-branch" => single_branch = Some(true),
            "--no-single-branch" => single_branch = Some(false),
            "--template" => template = Some(take_value!("--template")),
            "--no-template" => template = None,
            "--separate-git-dir" => separate_git_dir = Some(take_value!("--separate-git-dir")),
            "--no-separate-git-dir" => separate_git_dir = None,
            "--ref-format" => ref_format = Some(take_value!("--ref-format")),
            "--no-ref-format" => ref_format = None,
            "--reference" => references.push((take_value!("--reference"), false)),
            "--reference-if-able" => references.push((take_value!("--reference-if-able"), true)),
            "-s" | "--shared" => shared = true,
            "--no-shared" => shared = false,
            "--dissociate" => dissociate = true,
            "--no-dissociate" => dissociate = false,
            "--sparse" => sparse = true,
            "--no-sparse" => sparse = false,
            // `--filter=<spec>`: ask the remote to withhold the objects `<spec>` selects against.
            // git parses the spec in `cmd_clone`'s option table, so an invalid one never reaches the
            // network; `Filter::from_str` reproduces both the grammar and the messages.
            "--filter" => {
                let raw = take_value!("--filter");
                filter = Some(raw.parse().map_err(|e| anyhow::anyhow!("{e}"))?);
            }
            "--no-filter" => filter = None,
            // `--also-filter-submodules`: apply the same filter to the submodules the clone recurses
            // into (git's `option_also_filter_submodules`).
            "--also-filter-submodules" => also_filter_submodules = Some(true),
            "--no-also-filter-submodules" => also_filter_submodules = Some(false),
            "--bundle-uri" => bundle_uri = Some(take_value!("--bundle-uri")),
            "--no-bundle-uri" => bundle_uri = None,
            "--depth" => {
                let v = take_value!("--depth");
                let n: u32 = v
                    .parse()
                    .map_err(|_| anyhow::anyhow!("depth {v:?} is not a positive number"))?;
                depth = Some(
                    NonZeroU32::new(n)
                        .ok_or_else(|| anyhow::anyhow!("depth {v:?} is not a positive number"))?,
                );
            }
            // Shallow boundary at a cutoff date (git's `deepen_since`). Parsed with
            // gitoxide's git-compatible date parser, relative to the current time.
            "--shallow-since" => {
                let v = take_value!("--shallow-since");
                let t = gix::date::parse(&v, Some(SystemTime::now()))
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
            // `--recursive` / `--recurse-submodules[=<pathspec>]`: after the clone,
            // initialize and update submodules recursively. A pathspec-limited
            // recurse is honored as a full recursive update.
            "--recursive" | "--recurse-submodules" => {
                recurse_submodules = true;
                if let Some(pathspec) = inline_val.clone() {
                    submodule_pathspecs.push(pathspec);
                }
            }
            "--no-recursive" | "--no-recurse-submodules" => {
                recurse_submodules = false;
                submodule_pathspecs.clear();
            }
            // `--remote-submodules` is `git submodule update --remote`, which this
            // port implements, so it is passed straight through to that update.
            "--remote-submodules" => remote_submodules = true,
            "--no-remote-submodules" => remote_submodules = false,
            // `--shallow-submodules`: `git submodule update` gets `--depth=1`, so
            // every submodule this clone recurses into is truncated to one commit.
            "--shallow-submodules" => shallow_submodules = true,
            "--no-shallow-submodules" => shallow_submodules = false,
            // git's attached short-option forms (`-o<name>`, `-b<name>`,
            // `-c<key>=<value>` in the synopsis). Checked after the exact matches
            // so the detached forms above still win.
            other if other.starts_with("-o") && other.len() > 2 => {
                origin = Some(other[2..].to_string());
            }
            other if other.starts_with("-b") && other.len() > 2 => {
                branch = Some(other[2..].to_string());
            }
            other if other.starts_with("-c") && other.len() > 2 => {
                config_pairs.push(parse_config_pair(&other[2..])?);
            }
            other if other.starts_with('-') && other.len() > 1 => {
                bail!("unsupported option {other:?}");
            }
            other => positionals.push(other),
        }
    }

    // `--mirror` implies `--bare` (git-clone(1): "This implies --bare").
    if mirror {
        bare = true;
    }

    // git's ref storage formats are exactly `files` and `reftable`. `files` is the
    // backend gix writes, so it is a no-op that matches stock git; `reftable` has
    // no vendored backend and is rejected honestly rather than silently producing
    // a files-backed repository.
    if let Some(fmt) = ref_format.as_deref() {
        match fmt {
            "files" => {}
            "reftable" => bail!(
                "the reftable ref storage format is not supported: no vendored \
                 reftable backend"
            ),
            other => crate::git_fatal!("unknown ref storage format '{other}'"),
        }
    }

    // `--also-filter-submodules` is meaningless without something to pass down and something to pass
    // it to. Verified against stock git 2.55.0, which reports the missing option and exits 128 — and
    // which raises this only for the explicit flag, never for a `clone.filterSubmodules=true` that
    // happens to be configured.
    if also_filter_submodules == Some(true) {
        if filter.is_none() {
            crate::git_fatal!("the option '--also-filter-submodules' requires '--filter'");
        }
        if !recurse_submodules {
            crate::git_fatal!("the option '--also-filter-submodules' requires '--recurse-submodules'");
        }
    }

    // `builtin/clone.c`: bundles carry whole history, so they cannot be combined
    // with a shallow request.
    if bundle_uri.is_some() && (depth.is_some() || shallow_since.is_some() || !shallow_exclude.is_empty())
    {
        crate::git_fatal!(
            "options '--bundle-uri' and '--depth/--shallow-since/--shallow-exclude' cannot be used together"
        );
    }

    // git refuses to combine these (`builtin/clone.c`, verified against stock git:
    // `fatal: options '--bare' and '--separate-git-dir' cannot be used together`).
    if bare && separate_git_dir.is_some() {
        crate::git_fatal!("options '--bare' and '--separate-git-dir' cannot be used together");
    }
    // A sparse checkout needs a worktree; stock git fails the clone here with
    // `fatal: this operation must be run in a work tree` /
    // `error: failed to initialize sparse-checkout`.
    if sparse && bare {
        crate::git_fatal!("failed to initialize sparse-checkout: this operation must be run in a work tree");
    }

    if positionals.len() > 2 {
        return Ok(usage_error("Too many arguments."));
    }
    let url_str = match positionals.first() {
        Some(u) => *u,
        None => return Ok(usage_error("You must specify a repository to clone.")),
    };
    // `cmd_clone` resolves a local path before anything is created or announced, so a
    // source that is not there is reported on its own.
    if !url_str.contains("://")
        && !url_str.contains('@')
        && !Path::new(url_str).exists()
    {
        return Ok(fatal(&format!("repository '{url_str}' does not exist")));
    }

    // Parse the URL up front so a malformed one is reported before touching disk.
    let url = gix::url::parse(url_str.into())?;

    // Resolve the shallow-boundary selectors into a single `Shallow` value. The
    // exclude form supersedes a lone `--shallow-since`, which supersedes
    // `--depth`, matching git's treatment of them as one deepen group.
    // Remembered before the selectors are folded together, so the local-clone
    // warning below can name the flag the caller actually typed.
    let shallow_depth_given = depth.is_some();
    let shallow_since_given = shallow_since.is_some();
    let shallow_exclude_given = !shallow_exclude.is_empty();
    let local_path_source = !url_str.contains("://") && Path::new(url_str).is_dir();
    let shallow = if local_path_source {
        // Ignored for a local path clone — see the warning at the banner.
        Shallow::NoChange
    } else if !shallow_exclude.is_empty() {
        Shallow::Exclude {
            remote_refs: shallow_exclude,
            since_cutoff: shallow_since,
        }
    } else if let Some(cutoff) = shallow_since {
        Shallow::Since { cutoff }
    } else if let Some(n) = depth {
        Shallow::DepthAtRemote(n)
    } else {
        Shallow::NoChange
    };

    // Destination directory: explicit second positional, else derived from the
    // URL the way git's `guess_dir_name` does (humanish last component).
    let dir = match positionals.get(1) {
        Some(d) => (*d).to_string(),
        // `guess_dir_name()` falls back to the argument itself, which is how
        // `git clone .` ends up refusing `.` as a non-empty destination.
        None => derive_dir_name(url_str, bare).unwrap_or_else(|| url_str.to_string()),
    };
    let dst = Path::new(&dir);

    // git refuses to clone onto a non-empty existing path; gix's create options
    // enforce the same (destination_must_be_empty), so surface that early with a
    // git-matching message rather than a lower-level init error.
    if dst.exists()
        && dst
            .read_dir()
            .map(|mut e| e.next().is_some())
            .unwrap_or(true)
    {
        return Ok(fatal(&format!(
            "destination path '{dir}' already exists and is not an empty directory."
        )));
    }
    std::fs::create_dir_all(dst)?;

    // Serialize the ref/index writes through the repo coordinator, matching the
    // other write commands. On a freshly created clone no daemon is listening, so
    // this degrades to a no-op guard; it is held for the whole fetch+checkout.
    let git_dir = if bare {
        dst.to_path_buf()
    } else {
        dst.join(".git")
    };
    let _lock = crate::lock::RepoLock::acquire(&git_dir);

    let should_interrupt = AtomicBool::new(false);

    // git prints the "Cloning into ..." banner before any progress; keep it above
    // the live renderer so the two don't overdraw each other. `0 <= option_verbosity`
    // (clone.c:1134) is what `-q` switches off.
    if !quiet {
        if bare {
            eprintln!("Cloning into bare repository '{dir}'...");
        } else {
            eprintln!("Cloning into '{dir}'...");
        }
    }

    // `cmd_clone`'s local-clone block: when the source is a *path* on this
    // machine, git copies the object store instead of running the transport, so
    // every shallow selector is ignored — with a warning naming the `file://`
    // spelling that does go through the transport. Erroring instead (the server
    // "lacks" the `shallow` capability because there is no server) turns a
    // working `git clone --depth 1 ../repo` into a failure.
    let local_path_clone = !url_str.contains("://") && Path::new(url_str).is_dir();
    if local_path_clone && !quiet {
        for (given, flag) in [
            (shallow_depth_given, "--depth"),
            (shallow_since_given, "--shallow-since"),
            (shallow_exclude_given, "--shallow-exclude"),
        ] {
            if given {
                eprintln!("warning: {flag} is ignored in local clones; use file:// instead.");
            }
        }
    }

    // git's `transport_check_allowed()`, reached from `git_connect()`
    // (`connect.c:1480`) right after the banner and before a single byte moves.
    // A transport the policy forbids is fatal here, which is what stops a
    // submodule URL from choosing the transport (CVE-2022-39253).
    if let Err(e) = check_transport_allowed(&url) {
        eprintln!("fatal: {e}");
        return Ok(ExitCode::from(128));
    }

    // Build the clone platform. This already lays the repository down on disk, so
    // the template and alternates below land in the very git dir the fetch will
    // populate — the window git uses for the same work between `init_db()` and
    // the first fetch.
    let mut prepare = if bare {
        gix::prepare_clone_bare(url.clone(), dst)?
    } else {
        gix::prepare_clone(url.clone(), dst)?
    };

    // `--template=<dir>`: git runs its init step with the template, so the sample
    // hooks, `description` and `info/exclude` come from there instead of the
    // built-in default. Same code path `git init --template` uses.
    if let Some(tpl) = template.as_deref().filter(|t| !t.is_empty()) {
        super::init::apply_template(tpl, &git_dir)?;
    }

    // `-s`/`--shared` and `--reference`/`--reference-if-able`: record the object
    // stores to borrow from in `objects/info/alternates` *before* the fetch, which
    // is where git writes them. `--dissociate` asks for a self-contained result, so
    // nothing is recorded and the full pack is fetched instead.
    if !dissociate {
        let mut alternates: Vec<PathBuf> = Vec::new();
        // git only shares "when the repository to clone is on the local machine",
        // so a URL that names no openable local repository leaves `-s` a no-op.
        if shared {
            if let Some(objects) = local_path_of(&url).and_then(|p| objects_dir_of(&p).ok()) {
                alternates.push(objects);
            }
        }
        for (path, if_able) in &references {
            match objects_dir_of(Path::new(path)) {
                Ok(objects) => alternates.push(objects),
                Err(e) if *if_able => {
                    eprintln!("info: Could not add alternate for '{path}': {e}");
                }
                Err(e) => crate::git_fatal!("{e}"),
            }
        }
        if !alternates.is_empty() {
            write_alternates(&git_dir, &alternates)?;
        }
    }

    // Apply the parsed options. `--origin` renames the remote (and its tracking
    // refspec), `--branch` selects the ref to fetch and check out,
    // `--depth`/`--shallow-*` set the shallow boundary, and `--reject-shallow` /
    // `-c <key>=<value>` are injected as in-memory config so the fetch and the
    // checkout that follows it see them — git likewise promises `-c` takes effect
    // "before the remote history is fetched or any files checked out".
    if let Some(name) = &origin {
        prepare = prepare
            .with_remote_name(name.as_str())
            .map_err(|_| anyhow::anyhow!("{name:?} is not a valid remote name"))?;
    }
    if let Some(name) = &branch {
        prepare = prepare
            .with_ref_name(Some(name.as_str()))
            .map_err(|_| anyhow::anyhow!("--branch expects a valid branch name, got {name:?}"))?;
    }
    prepare = prepare.with_shallow(shallow);
    prepare = prepare.with_filter(filter.clone());

    let mut overrides: Vec<String> = config_pairs
        .iter()
        .map(|(k, v)| format!("{k}={v}"))
        .collect();
    if let Some(reject) = reject_shallow {
        overrides.push(format!("clone.rejectShallow={reject}"));
    }
    if !overrides.is_empty() {
        prepare = prepare.with_in_memory_config_overrides(overrides);
    }

    // git-clone(1) on `--depth`: "Implies `--single-branch` unless
    // `--no-single-branch` is given" — `option_single_branch` defaults to whether
    // any deepen selector was used, which is the `--depth` / `--shallow-since` /
    // `--shallow-exclude` group as a whole (clone.c:1370). A local-path clone
    // ignores the selectors entirely, so it must not inherit the implication.
    let single_branch = single_branch.or_else(|| {
        (!local_path_source
            && (shallow_depth_given || shallow_since_given || shallow_exclude_given))
            .then_some(true)
    });

    // Decide which refspec set this clone needs and, with it, which tag mode.
    // Supplying `configure_remote` at all suppresses gitoxide's own clone default
    // of `Tags::All`, so every plan that is not a single-branch clone has to ask
    // for it back explicitly. `Tags::All` is written to `remote.<name>.tagOpt`,
    // which git does not do, so those cases strip the key again afterwards.
    let plan = if mirror {
        Some(Refspecs::Mirror)
    } else if single_branch == Some(true) {
        Some(Refspecs::Single {
            branch: branch.clone(),
            bare,
        })
    } else if bare {
        Some(Refspecs::Bare)
    } else if single_branch == Some(false) {
        Some(Refspecs::Wildcard)
    } else {
        None
    };
    // `clone.c:1414`: `if (option_tags || option_branch) refspec_append(&remote->fetch,
    // TAG_REFSPEC)` — the tag refspec is appended *after* the branch refspec, so
    // `--single-branch` restricts the branches fetched and nothing else. A
    // single-branch clone keeps every tag, exactly as a full one does; only
    // `--no-tags` (which clears `option_tags`) drops them.
    let forced_all_tags =
        matches!(plan, Some(Refspecs::Bare) | Some(Refspecs::Wildcard)) && tags != Some(Tags::None);
    // A single-branch clone gets its tags the way git's does: `include-tag` on the
    // request, and a `refs/tags/<name>` written afterwards for every advertised tag
    // whose object the pack turned out to carry (`write_followtags()`,
    // clone.c:686-700). Wanting the tags outright instead — which `Tags::All` does —
    // is the difference between the two only until a depth is in play, where it
    // decides the shape of the clone: an explicit `want` for a tag outside the
    // window makes the server open a second shallow boundary at it and send its
    // commit, so `--depth 1` lands two `.git/shallow` entries and twice the objects
    // git fetches. `--depth` implies `--single-branch`, so that is the common case.
    let included_tags =
        matches!(plan, Some(Refspecs::Single { .. })) && tags != Some(Tags::None);
    let fetch_tags: Option<Tags> = if tags == Some(Tags::None) || mirror {
        Some(Tags::None)
    } else if forced_all_tags {
        Some(Tags::All)
    } else if included_tags {
        Some(Tags::Included)
    } else {
        None
    };

    // What `--single-branch` resolved against the remote: the ref actually
    // fetched, and the branch the remote's `HEAD` points at. The clean-up below
    // needs both to decide whether git would have created `refs/remotes/<name>/HEAD`
    // — it only does so when the remote's own `HEAD` branch is among the fetched refs.
    let single_outcome: SingleOutcome = Default::default();

    // `+refs/*:refs/*` cannot be turned into an ls-refs `ref-prefix`:
    // `RefSpecRef::prefix` gives up on a `*` in the first path component
    // (`gix-refspec/src/spec.rs:147`) and `expand_prefixes` then emits the pattern
    // itself, so the server is asked for refs literally starting with `refs/*` and
    // advertises none. Turning the prefix filter off makes the remote advertise
    // everything and lets gitoxide match locally — which is what `--mirror` wants
    // anyway. Only the mirror refspec needs this; every other plan has a literal
    // prefix (`refs/heads/`) that the filter handles correctly.
    // The transport-level knobs. `remote.<name>.uploadpack`/`serverOption` cannot apply here: the remote is
    // being created by this very clone, so only the command line can supply them.
    if upload_pack.is_some() || address_family.is_some() {
        prepare = prepare.with_connect_options(gix::remote::connect::Options {
            upload_pack: upload_pack.as_deref().map(Into::into),
            address_family,
            // `git clone` never connects for push.
            receive_pack: None,
        });
    }
    if !server_options.is_empty() {
        prepare = prepare.with_server_options(server_options.clone());
    }

    if matches!(plan, Some(Refspecs::Mirror)) {
        prepare = prepare.with_fetch_options(gix::remote::ref_map::Options {
            prefix_from_spec_as_filter_on_remote: false,
            ..Default::default()
        });
    }

    if plan.is_some() || fetch_tags.is_some() {
        let plan = plan.clone();
        let url_for_remote = url.clone();
        let explicit_origin = origin.clone();
        let single_outcome = single_outcome.clone();
        let probe_connect = gix::remote::connect::Options {
            upload_pack: upload_pack.as_deref().map(Into::into),
            address_family,
            // `git clone` never connects for push.
            receive_pack: None,
        };
        let probe_server_options = server_options.clone();
        prepare = prepare.configure_remote(move |r| {
            let Some(plan) = plan.as_ref() else {
                // Only the tag mode changes; keep the refspec gitoxide chose,
                // which for a shallow clone is its derived single-branch spec.
                return Ok(match fetch_tags {
                    Some(t) => r.with_fetch_tags(t),
                    None => r,
                });
            };
            let repo = r.repo;
            let remote_name = resolve_remote_name(repo, explicit_origin.as_deref());
            let specs = plan.refspecs(
                repo,
                &url_for_remote,
                &remote_name,
                &single_outcome,
                &probe_connect,
                &probe_server_options,
            )?;
            // Build a fresh remote rather than adding to `r`: `with_refspecs` only
            // appends, so replacing gitoxide's wildcard spec is impossible in place.
            let mut remote = repo.remote_at(url_for_remote.clone())?;
            if !specs.is_empty() {
                remote = remote.with_refspecs(
                    specs.iter().map(String::as_str),
                    gix::remote::Direction::Fetch,
                )?;
            }
            if let Some(t) = fetch_tags {
                remote = remote.with_fetch_tags(t);
            }
            Ok(remote)
        });
    }

    // Before fetching from the remote, download and install bundle data from the
    // `--bundle-uri` option. git does this after `create_reference_database()`
    // and before the first fetch (`builtin/clone.c`), so the bundled objects and
    // the `refs/bundles/*` tips they leave behind are already on disk when the
    // remote is contacted.
    //
    // git persists `fetch.bundleURI` right here with `repo_config_set_gently`;
    // this defers the write to `finalize_config` below because the fetch
    // rewrites `.git/config` in between, which would drop a value written now.
    // The end state is identical.
    let mut persist_bundle_uri: Option<String> = None;
    let mut persist_creation_token: Option<String> = None;
    if let Some(uri) = bundle_uri.as_deref() {
        match gix::open(&git_dir) {
            Err(_) => eprintln!("warning: failed to initialize the repo, skipping bundle URI"),
            Ok(bundle_repo) => {
                let (failed, has_heuristic) =
                    super::bundle::uri::fetch_bundle_uri(&bundle_repo, uri);
                if failed {
                    eprintln!("warning: failed to fetch objects from bundle URI '{uri}'");
                } else if has_heuristic {
                    // Only a list that advertised `bundle.heuristic` is worth
                    // revisiting on every fetch, so only that one is recorded.
                    persist_bundle_uri = Some(uri.to_string());
                }
                // `fetch_bundles_by_token` wrote `fetch.bundleCreationToken`
                // straight into the config file; carry it across the rewrite the
                // fetch performs, for the same reason `fetch.bundleURI` is
                // deferred.
                persist_creation_token = gix::open(&git_dir)
                    .ok()
                    .and_then(|r| {
                        r.config_snapshot()
                            .string("fetch.bundleCreationToken")
                            .map(|v| v.to_string())
                    });
            }
        }
    }

    // Drive gitoxide's fetch/checkout through a prodash tree and render it to
    // stderr with the line renderer. The tree relays the remote's sideband
    // progress (Enumerating/Counting/Compressing objects, Total …) as well as the
    // local pack receive/resolve and worktree checkout counters — the same
    // information git surfaces. When progress is suppressed the tree is still used
    // but no renderer is attached, so nothing is drawn.
    let show_progress = force_progress.unwrap_or_else(|| std::io::stderr().is_terminal()) && !quiet;
    let root = prodash::tree::Root::new();
    // Create the operation node BEFORE launching the renderer. prodash's line
    // renderer can otherwise race an empty tree at startup and exit before gix
    // adds any progress, rendering nothing at all (documented on
    // `keep_running_if_progress_is_empty`). gix's fetch adds a "remote" child here
    // (the server's Enumerating/Counting/Compressing sideband) plus local
    // "receiving pack"/"resolving" and checkout counters.
    let mut op = root.add_child(if bare { "clone (bare)" } else { "clone" });
    let no_color = std::env::var_os("NO_COLOR").is_some();
    let render = show_progress.then(|| {
        let mut opts = prodash::render::line::Options {
            throughput: true,
            ..Default::default()
        }
        .auto_configure(prodash::render::line::StreamKind::Stderr);
        // `--progress` forces the live display even when stderr is not a terminal,
        // matching git; auto_configure would otherwise disable it in that case.
        if force_progress == Some(true) {
            opts.output_is_terminal = true;
        }
        // auto_configure clobbers both of the following; reassert them after it:
        //   * git never hides the cursor, but auto_hide_cursor (signal-hook) forces
        //     hide_cursor on — that can strand the cursor hidden if a render is
        //     interrupted. Keep it visible.
        //   * colorize whenever we draw live to a terminal (git colors on a tty),
        //     honoring only the NO_COLOR standard — not the CLICOLOR/CLICOLOR_FORCE
        //     env quirks crosstermion::color::allowed keys on.
        opts.hide_cursor = false;
        opts.colored = opts.output_is_terminal && !no_color;
        prodash::render::line::render(std::io::stderr(), root.downgrade(), opts)
    });

    // Whether the remote advertised any `refs/remotes/*` of its own. gitoxide always
    // fetches `HEAD:refs/remotes/<name>/HEAD`, so for a bare or mirrored clone —
    // where git creates no remote-tracking refs at all — that ref has to be removed
    // again, unless a mirror refspec legitimately copied the remote's own ones over.
    let mut remote_advertised_tracking_refs = false;

    // Whether the server accepted `--filter`. git keeps going when it did not, warning that the
    // filter was ignored, so the warning is emitted from what the handshake advertised.
    let mut filter_supported = true;

    // git's `remote_head_points_at`: the branch the remote's `HEAD` names, which
    // `update_remote_refs()` makes `refs/remotes/<name>/HEAD` a symbolic ref to.
    let mut remote_head_branch: Option<String> = None;

    // Run the clone, capturing the result so the renderer is always torn down
    // (cursor restored, thread joined) before any error is propagated.
    let result = (|| -> Result<()> {
        // git's `transport.c` warns as soon as it has the capability list and then transfers
        // everything, leaving the promisor configuration in place regardless.
        // `guess_remote_head()`, shared with `git remote set-head` and the fetch path. Anything but a
        // single answer leaves the remote's `HEAD` undetermined, which git treats as nothing to link.
        let mut note_remote_head = |ref_map: &gix::remote::fetch::RefMap| {
            if let [head] = super::remote::remote_head_names(ref_map).as_slice() {
                remote_head_branch = Some(head.clone());
            }
        };
        let mut note_filter_support = |handshake: &gix::protocol::Handshake| {
            filter_supported = gix::protocol::fetch::filter::is_supported(
                handshake.server_protocol_version,
                &handshake.capabilities,
            );
            if filter.is_some() && !filter_supported {
                eprintln!("warning: filtering not recognized by server, ignoring");
            }
        };
        if bare || no_checkout {
            // A bare clone never checks out a worktree; `--no-checkout` likewise
            // fetches the pack and writes the refs/`HEAD` but leaves the worktree
            // (and index) empty. Fetching is the whole job in both cases.
            let (_repo, outcome) = prepare
                .fetch_only(op.add_child("fetch"), &should_interrupt)
                .map_err(short_pack)?;
            remote_advertised_tracking_refs = outcome
                .ref_map
                .remote_refs
                .iter()
                .any(|r| r.unpack().0.starts_with(b"refs/remotes/"));
            note_remote_head(&outcome.ref_map);
            note_filter_support(&outcome.handshake);
        } else {
            // `git clone 'url'...`
            let (mut checkout, outcome) = prepare
                .fetch_then_checkout(op.add_child("fetch"), &should_interrupt)
                .map_err(short_pack)?;
            note_remote_head(&outcome.ref_map);
            note_filter_support(&outcome.handshake);
            // Check out the branch `HEAD` points to. This is a no-op for an empty
            // remote, leaving an empty repository exactly like git does.
            checkout.main_worktree(op.add_child("checkout"), &should_interrupt)?;
        }
        Ok(())
    })();

    if let Some(handle) = render {
        handle.shutdown_and_wait();
    }
    // A transport that never connected is git's own `die()`, not a wrapped error:
    // the ssh child's stderr, then the fixed block, exit 128.
    if let Err(e) = &result {
        if let Some(code) = crate::transport_err::ssh_fatal(url_str, e) {
            return Ok(code);
        }
    }
    result?;

    // `--separate-git-dir`: move the git dir to the requested path and leave a
    // `gitdir: <abs>` link file behind, the same relocation `git init` performs.
    // Everything below works on the relocated dir, as git's post-init steps do.
    let git_dir = match &separate_git_dir {
        Some(real) => super::init::relocate_git_dir(&git_dir, dst, real)?,
        None => git_dir,
    };

    // `clone_local()`: with a local source, git never runs the transport at all — it
    // adopts the source object store, hardlinking each file where it can. Here the
    // local fetch has already produced a pack; the pack is dropped and the source's
    // objects are adopted in its place, so the clone ends up with the object layout git
    // leaves behind (loose objects stay loose, packs stay packs).
    if local_path_source && !no_local && filter.is_none() {
        adopt_local_objects(Path::new(url_str), &git_dir, hardlinks)?;
    }

    // `write_remote_refs()` commits the remote-tracking refs through
    // `initial_ref_transaction_commit()`, which writes no reflog at all — a fresh clone
    // has logs for `HEAD`, the checked-out branch and `refs/remotes/<remote>/HEAD`, and
    // nothing else. gitoxide logs every ref it writes, so the extra files are removed.
    remove_initial_remote_reflogs(&git_dir, origin.as_deref().unwrap_or("origin"));

    // `init_db()` creates `refs/heads` and `refs/tags` for every repository it
    // makes, clone included (`create_default_files()`, setup.c:2073). gitoxide
    // leaves out whichever of the two the clone had no loose ref to write, and a
    // bare clone whose refs all landed in `packed-refs` ends up with no `refs`
    // directory at all — which stock git then refuses to open, because
    // `is_git_directory()` requires it (setup.c:397).
    ensure_ref_directories(&git_dir)?;

    // Reconcile `.git/config` with what stock git leaves behind: strip the entries
    // gitoxide persisted that git does not, add the ones git writes itself, and
    // apply the `-c` pairs last so gitoxide's own config rewrites cannot drop them.
    finalize_config(
        &git_dir,
        &ConfigFixups {
            set_mirror: mirror,
            drop_fetch: bare && !mirror,
            drop_tag_opt: forced_all_tags,
            drop_branch_sections: bare,
            // "The resulting clone has submodule.active set to the provided
            // pathspec, or '.' (meaning all submodules) if no pathspec is
            // provided" (git-clone(1)). git records it whenever the option is
            // given, even for `--bare` / `-n`, where the update itself is skipped.
            // `partial_clone_register()`: a partial clone records the remote it may go back to for
            // the objects it skipped, and bumps the repository format so older git refuses it.
            promisor_filter: filter.as_ref().map(|f| f.as_str().to_owned()),
            submodule_active: recurse_submodules.then(|| {
                if submodule_pathspecs.is_empty() {
                    vec![".".to_string()]
                } else {
                    submodule_pathspecs.clone()
                }
            }),
            fetch_bundle_uri: persist_bundle_uri.clone(),
            fetch_bundle_creation_token: persist_creation_token.clone(),
            config_pairs: &config_pairs,
        },
    )?;

    // Drop gitoxide's implicit `refs/remotes/<name>/HEAD` wherever git would not
    // have written it: a bare or mirrored clone has no remote-tracking hierarchy at
    // all, and a `--single-branch` clone only gets the symbolic head when the branch
    // the remote's `HEAD` points at is the one that was fetched.
    let drop_head_tracking = if bare {
        !remote_advertised_tracking_refs
    } else {
        match &*single_outcome.lock().expect("clone is single-threaded here") {
            Some((fetched, head_target)) => head_target.as_deref() != fetched.as_deref(),
            None => false,
        }
    };
    if drop_head_tracking {
        remove_remote_tracking_head(&git_dir)?;
    } else if !bare {
        // `update_remote_refs()` writes `refs/remotes/<name>/HEAD` with `refs_update_symref()`,
        // pointing at the *tracking ref* of the branch the remote's `HEAD` names — not at its object
        // id. That is what lets a clone's `origin/HEAD` keep following `origin/<default>` as later
        // fetches move it. gitoxide's implicit `HEAD:refs/remotes/<name>/HEAD` mapping resolves the
        // symref to the object instead (`new_value_by_remote()` in `update_refs`), so it is relinked
        // here. git writes nothing at all for a bare clone, which the branch above already handles.
        if let Some(branch) = remote_head_branch.as_deref() {
            link_remote_tracking_head(&git_dir, branch)?;
        }
    }

    // `init.defaultSubmodulePathConfig=true` opts every new repository into the
    // submodule-path extension; git applies it to `git clone` exactly as it does
    // to `git init` (verified against stock git 2.55.0, which writes
    // `extensions.submodulepathconfig=true` and `core.repositoryformatversion=1`
    // into a fresh clone). Read from the clone itself, so every config scope the
    // repository sees participates.
    if gix::open(&git_dir)
        .ok()
        .and_then(|repo| repo.config_snapshot().boolean("init.defaultSubmodulePathConfig"))
        == Some(true)
    {
        super::init::enable_submodule_path_config(&git_dir)?;
    }

    // git closes the clone banner with `done.` (suppressed by `-q`).
    if !quiet {
        eprintln!("done.");
    }

    // `--sparse`: git initializes a cone-mode sparse-checkout containing only the
    // top-level files. This port's own `sparse-checkout set --cone` writes the
    // identical `info/sparse-checkout` pattern pair and `config.worktree` keys and
    // prunes the worktree, so it is re-executed here rather than duplicated.
    if sparse {
        let code = run_self(&dir, &["sparse-checkout", "set", "--cone"], true)?;
        if code != 0 {
            return Ok(ExitCode::from(code));
        }
    }

    // `--recursive` / `--recurse-submodules`: after a successful non-bare,
    // checked-out clone, initialize and update submodules recursively by
    // re-executing this binary's own ported `submodule update --init --recursive`
    // in the new worktree. `--remote-submodules` adds git's `--remote`, which
    // updates each submodule to its remote-tracking branch instead of the
    // superproject's recorded commit.
    if recurse_submodules && !bare && !no_checkout {
        let mut sub: Vec<String> = ["submodule", "update", "--require-init", "--recursive"]
            .iter()
            .map(ToString::to_string)
            .collect();
        // git's order (clone.c:707-741): `--depth=1`, then the job count, then
        // `--quiet`, then `--remote --no-fetch`.
        if shallow_submodules {
            sub.push("--depth=1".into());
        }
        // `max_jobs != -1` is the only thing that makes git forward the count.
        if let Some(j) = &jobs {
            sub.push(format!("--jobs={j}"));
        }
        if quiet {
            sub.push("--quiet".into());
        }
        // A submodule that is to follow its remote branch has nothing to gain from
        // the fetch `update` would otherwise run, since the clone just did one.
        if remote_submodules {
            sub.push("--remote".into());
            sub.push("--no-fetch".into());
        }
        let sub: Vec<&str> = sub.iter().map(String::as_str).collect();
        let code = run_self(&dir, &sub, false)?;
        if code != 0 {
            return Ok(ExitCode::from(code));
        }
    }

    Ok(ExitCode::SUCCESS)
}

/// The refspec set a clone needs, chosen from the option combination. gitoxide
/// installs `+refs/heads/*:refs/remotes/<name>/*` (or its own single-branch spec
/// for a shallow clone) and `Remote::with_refspecs` can only append to that, so
/// every variant here is realized by building a *replacement* remote from the
/// repository `configure_remote` hands out.
#[derive(Clone)]
enum Refspecs {
    /// `--mirror`: `+refs/*:refs/*` — every ref maps onto itself.
    Mirror,
    /// `--bare`: `+refs/heads/*:refs/heads/*` — remote branch heads become local
    /// branch heads, with no `refs/remotes/` mapping (git-clone(1) `--bare`).
    Bare,
    /// `--single-branch`: exactly one ref, resolved from the remote.
    Single {
        /// The `--branch` argument, if any; otherwise the branch `HEAD` points at.
        branch: Option<String>,
        /// Whether the destination is a bare clone (branch heads map onto
        /// themselves instead of into `refs/remotes/<name>/`).
        bare: bool,
    },
    /// `--no-single-branch`: the wildcard, stated explicitly so it also overrides
    /// the implicit single-branch refspec gitoxide derives for a shallow clone.
    Wildcard,
}

/// What a `--single-branch` clone resolved against the remote: `(the ref that is
/// being fetched, the branch the remote's `HEAD` points at)`. Shared with the
/// `configure_remote` closure, which is the only place that talks to the remote
/// early enough to know either.
type SingleOutcome = std::sync::Arc<std::sync::Mutex<Option<(Option<String>, Option<String>)>>>;

impl Refspecs {
    /// The concrete fetch refspecs for this plan. `Single` performs an ls-refs
    /// round trip against the remote — the same lookup git does to learn which
    /// branch to restrict the clone to — and records what it found in `outcome`.
    fn refspecs(
        &self,
        repo: &gix::Repository,
        url: &gix::Url,
        remote_name: &str,
        outcome: &SingleOutcome,
        connect_options: &gix::remote::connect::Options,
        server_options: &[gix::bstr::BString],
    ) -> Result<Vec<String>, Box<dyn std::error::Error + Send + Sync>> {
        Ok(match self {
            Refspecs::Mirror => vec!["+refs/*:refs/*".to_string()],
            Refspecs::Bare => vec!["+refs/heads/*:refs/heads/*".to_string()],
            Refspecs::Wildcard => vec![format!("+refs/heads/*:refs/remotes/{remote_name}/*")],
            Refspecs::Single { branch, bare } => {
                let (fetched, head_target) =
                    resolve_single_ref(repo, url, branch.as_deref(), connect_options, server_options)?;
                *outcome.lock().expect("clone is single-threaded here") =
                    Some((fetched.clone(), head_target));
                match fetched {
                    Some(full) => vec![single_branch_refspec(&full, remote_name, *bare)],
                    // "If the HEAD at the remote did not point at any branch when
                    // --single-branch clone was made, no remote-tracking branch is
                    // created" (git-clone(1)).
                    None => Vec::new(),
                }
            }
        })
    }
}

/// The refspec for a single-branch clone of `full_ref_name`. A branch maps into
/// `refs/remotes/<name>/<short>` (or onto itself for a bare clone); anything else
/// — `--branch` may name a tag — maps onto itself, as git does.
fn single_branch_refspec(full_ref_name: &str, remote_name: &str, bare: bool) -> String {
    let destination = match full_ref_name.strip_prefix("refs/heads/") {
        Some(short) if !bare => format!("refs/remotes/{remote_name}/{short}"),
        _ => full_ref_name.to_string(),
    };
    format!("+{full_ref_name}:{destination}")
}

/// Ask the remote which ref a `--single-branch` clone should be restricted to:
/// the `--branch` argument resolved against the advertisement (a branch first,
/// then a tag, matching git's `--branch` lookup order), or the branch the remote's
/// `HEAD` points at. Returns `(the ref to fetch, the remote `HEAD` branch)`; the
/// first is `None` when no `--branch` was given and the remote `HEAD` names no
/// branch, the second whenever the remote advertises no symbolic `HEAD`.
fn resolve_single_ref(
    repo: &gix::Repository,
    url: &gix::Url,
    branch: Option<&str>,
    connect_options: &gix::remote::connect::Options,
    server_options: &[gix::bstr::BString],
) -> Result<(Option<String>, Option<String>), Box<dyn std::error::Error + Send + Sync>> {
    let remote = repo.remote_at(url.clone())?;
    let connection = remote
        .connect_with_options(gix::remote::Direction::Fetch, connect_options.clone())?
        .with_server_options(server_options.to_vec());
    // The probe remote carries no refspecs, so the server must advertise
    // everything rather than only what a refspec prefix would allow.
    let (map, _handshake) = connection.ref_map(
        gix::progress::Discard,
        gix::remote::ref_map::Options {
            prefix_from_spec_as_filter_on_remote: false,
            ..Default::default()
        },
    )?;

    let advertised = |name: &str| {
        map.remote_refs
            .iter()
            .any(|r| r.unpack().0 == name.as_bytes())
    };

    // The branch the remote's own `HEAD` points at, if it advertises one.
    let head_target = map.remote_refs.iter().find_map(|r| match r {
        Ref::Symbolic {
            full_ref_name,
            target,
            ..
        }
        | Ref::Unborn {
            full_ref_name,
            target,
        } if full_ref_name == "HEAD" => Some(target.to_string()),
        _ => None,
    });

    let fetched = match branch {
        Some(name) => {
            let candidates = if name.starts_with("refs/") {
                vec![name.to_string()]
            } else {
                vec![
                    format!("refs/heads/{name}"),
                    format!("refs/tags/{name}"),
                    name.to_string(),
                ]
            };
            Some(
                candidates
                    .into_iter()
                    .find(|c| advertised(c))
                    .ok_or_else(|| format!("Remote branch {name} not found in upstream"))?,
            )
        }
        None => head_target.clone(),
    };
    Ok((fetched, head_target))
}

/// The remote name this clone will use, with git's precedence: `-o`/`--origin`,
/// else the `clone.defaultRemoteName` config, else `origin`. Mirrors the
/// resolution gitoxide performs internally, which it does not expose.
fn resolve_remote_name(repo: &gix::Repository, explicit: Option<&str>) -> String {
    match explicit {
        Some(name) => name.to_string(),
        None => repo
            .config_snapshot()
            .string("clone.defaultRemoteName")
            .map(|v| v.to_string())
            .filter(|v| !v.trim().is_empty())
            .unwrap_or_else(|| "origin".to_string()),
    }
}

/// The `.git/config` entries that have to be reconciled after gitoxide wrote the
/// remote section, so the result matches what stock git leaves behind.
struct ConfigFixups<'a> {
    /// `--mirror` records `remote.<name>.mirror = true`.
    set_mirror: bool,
    /// A bare clone persists no `remote.<name>.fetch` (git-clone(1): "neither
    /// remote-tracking branches nor the related configuration variables are
    /// created"), even though the fetch itself used a refspec.
    drop_fetch: bool,
    /// `remote.<name>.tagOpt` written only because asking gitoxide for
    /// `Tags::All` persists it; git writes the key solely for `--no-tags`.
    drop_tag_opt: bool,
    /// A bare clone creates no `branch.<name>` section.
    drop_branch_sections: bool,
    /// `--filter=<spec>`: register the remote as a promisor with this filter, the way git's
    /// `partial_clone_register()` does — `remote.<name>.promisor = true`,
    /// `remote.<name>.partialclonefilter = <spec>` and `core.repositoryformatversion = 1`.
    promisor_filter: Option<String>,
    /// `submodule.active` values recorded by `--recurse-submodules[=<pathspec>]`.
    submodule_active: Option<Vec<String>>,
    /// `fetch.bundleURI`, recorded by `--bundle-uri` when the list it fetched
    /// advertised a `bundle.heuristic` so later fetches can revisit it.
    fetch_bundle_uri: Option<String>,
    /// `fetch.bundleCreationToken`, the largest `bundle.<id>.creationToken` the
    /// bundle-URI client applied, so the next fetch downloads nothing older.
    fetch_bundle_creation_token: Option<String>,
    /// `-c <key>=<value>` pairs, in command-line order.
    config_pairs: &'a [(String, String)],
}

/// Apply [`ConfigFixups`] to `<git_dir>/config`.
///
/// The `-c` values are written here, after the clone, rather than before the
/// fetch: gitoxide re-serializes the whole local configuration from its own
/// in-memory snapshot when it records `branch.<name>.*`, which would drop
/// anything written to the file behind its back. The fetch and checkout still
/// observe them, because they are also handed to gitoxide as in-memory overrides.
/// The two ref directories every git repository has, whether or not anything was
/// ever written into them. Missing them is not cosmetic: `is_git_directory()`
/// tests for `refs`, so a bare clone without it is not a repository as far as
/// stock git is concerned.
fn ensure_ref_directories(git_dir: &Path) -> Result<()> {
    for sub in ["refs/heads", "refs/tags"] {
        std::fs::create_dir_all(git_dir.join(sub))
            .with_context(|| format!("could not create {}", git_dir.join(sub).display()))?;
    }
    Ok(())
}

fn finalize_config(git_dir: &Path, fixups: &ConfigFixups<'_>) -> Result<()> {
    if !fixups.set_mirror
        && !fixups.drop_fetch
        && !fixups.drop_tag_opt
        && !fixups.drop_branch_sections
        && fixups.submodule_active.is_none()
        && fixups.promisor_filter.is_none()
        && fixups.fetch_bundle_uri.is_none()
        && fixups.fetch_bundle_creation_token.is_none()
        && fixups.config_pairs.is_empty()
    {
        return Ok(());
    }

    let path = git_dir.join("config");
    let mut file =
        gix::config::File::from_path_no_includes(path.clone(), gix::config::Source::Local)
            .map_err(|e| anyhow::anyhow!("{e}"))?;

    let remote_name = file
        .sections()
        .find(|s| s.header().name() == "remote")
        .and_then(|s| s.header().subsection_name().map(ToString::to_string));

    if let Some(name) = remote_name {
        if let Ok(mut section) = file.section_mut("remote", Some(name.as_str().into())) {
            if fixups.drop_fetch {
                while section.remove("fetch").is_some() {}
            }
            if fixups.drop_tag_opt {
                while section.remove("tagOpt").is_some() {}
            }
            if fixups.set_mirror {
                section.push("mirror", Some("true".into()))?;
            }
            if let Some(spec) = &fixups.promisor_filter {
                section.push("promisor", Some("true".into()))?;
                section.push("partialclonefilter", Some(spec.as_str().into()))?;
            }
        }
    }

    // `upgrade_repository_format(1)`. git bumps the version without adding an extension — verified
    // against stock git 2.55.0, whose `--filter` clone leaves `core.repositoryformatversion = 1` and
    // no `[extensions]` section at all.
    if fixups.promisor_filter.is_some() {
        if let Ok(mut core) = file.section_mut("core", None) {
            core.set("repositoryformatversion", "1")?;
        }
    }

    if fixups.drop_branch_sections {
        let subsections: Vec<_> = file
            .sections()
            .filter(|s| s.header().name() == "branch")
            .filter_map(|s| s.header().subsection_name().map(ToString::to_string))
            .collect();
        for sub in subsections {
            while file.remove_section("branch", Some(sub.as_str().into())).is_some() {}
        }
    }

    if let Some(pathspecs) = &fixups.submodule_active {
        let mut section = file
            .section_mut_or_create_new("submodule", None)
            .map_err(|e| anyhow::anyhow!("{e}"))?;
        for pathspec in pathspecs {
            section.push("active", Some(pathspec.as_str().into()))?;
        }
    }

    if fixups.fetch_bundle_uri.is_some() || fixups.fetch_bundle_creation_token.is_some() {
        let mut section = file
            .section_mut_or_create_new("fetch", None)
            .map_err(|e| anyhow::anyhow!("{e}"))?;
        // Key spelling and order follow stock git: `fetch_bundles_by_token()`
        // writes `bundleCreationToken` first, then `builtin/clone.c` writes the
        // URI under the all-lowercase `fetch.bundleuri` it passes to
        // `repo_config_set_gently`.
        if let Some(token) = &fixups.fetch_bundle_creation_token {
            section.set("bundleCreationToken", token.as_str())?;
        }
        if let Some(uri) = &fixups.fetch_bundle_uri {
            section.set("bundleuri", uri.as_str())?;
        }
    }

    // git writes every `-c` value, so a key given twice ends up with two values.
    for (key, value) in fixups.config_pairs {
        let (section, subsection, name) = split_config_key(key)?;
        let mut sec = file
            .section_mut_or_create_new(&section, subsection.as_deref().map(Into::into))
            .map_err(|e| anyhow::anyhow!("{e}"))?;
        sec.push(name.as_str(), Some(value.as_str().into()))?;
    }

    std::fs::write(&path, file.to_bstring())?;
    Ok(())
}

/// Delete `refs/remotes/*/HEAD`, the remote-tracking ref gitoxide always creates
/// from its implicit `HEAD:refs/remotes/<name>/HEAD` refspec. Callers invoke this
/// only where git writes no such ref, so the only candidate present is gitoxide's
/// own. The ref is removed whether it is loose or packed; nothing else under
/// `refs/remotes/` is touched.
fn remove_remote_tracking_head(git_dir: &Path) -> Result<()> {
    let repo = match gix::open(git_dir) {
        Ok(r) => r,
        // A repository we cannot reopen has nothing for us to clean up; the clone
        // itself already succeeded.
        Err(_) => return Ok(()),
    };
    let names: Vec<gix::refs::FullName> = repo
        .references()
        .map_err(|e| anyhow::anyhow!("{e}"))?
        .prefixed("refs/remotes/")
        .map_err(|e| anyhow::anyhow!("{e}"))?
        .filter_map(Result::ok)
        .map(|r| r.name().to_owned())
        .filter(|n| n.as_bstr().ends_with(b"/HEAD"))
        .collect();
    for name in names {
        repo.edit_reference(gix::refs::transaction::RefEdit {
            change: gix::refs::transaction::Change::Delete {
                expected: gix::refs::transaction::PreviousValue::Any,
                log: gix::refs::transaction::RefLog::AndReference,
            },
            name,
            deref: false,
        })
        .map_err(|e| anyhow::anyhow!("{e}"))?;
    }
    // A deleted loose ref leaves its directories behind; git's clone never creates
    // them at all, so prune the empty ones back to (and excluding) `refs/`.
    prune_empty_dirs(&git_dir.join("refs").join("remotes"));
    Ok(())
}

/// Give the post-fetch connectivity check the wording `update_remote_refs()` uses on the clone side.
///
/// git's fetch says `<url> did not send all necessary objects`; its clone, which has no display state
/// to name the URL from yet, says `remote did not send all necessary objects` and takes the whole
/// clone directory down with it.
fn short_pack(err: gix::clone::fetch::Error) -> anyhow::Error {
    match err {
        gix::clone::fetch::Error::Fetch(gix::remote::fetch::Error::NotConnected) => {
            anyhow::anyhow!("remote did not send all necessary objects")
        }
        other => other.into(),
    }
}

/// Turn the `refs/remotes/<name>/HEAD` a clone just wrote into a symbolic ref to
/// `refs/remotes/<name>/<branch>`, as `update_remote_refs()` does with `refs_update_symref()`.
///
/// The tracking ref has to be there: git derives the symref from `remote_head_points_at->peer_ref`,
/// which by construction is one of the refs the clone stored. When it is not — a `--branch` clone of
/// a tag, say — there is nothing to point at and the ref is left as the object it already names.
fn link_remote_tracking_head(git_dir: &Path, branch: &str) -> Result<()> {
    let repo = match gix::open(git_dir) {
        Ok(r) => r,
        Err(_) => return Ok(()),
    };
    let names: Vec<gix::refs::FullName> = repo
        .references()
        .map_err(|e| anyhow::anyhow!("{e}"))?
        .prefixed("refs/remotes/")
        .map_err(|e| anyhow::anyhow!("{e}"))?
        .filter_map(Result::ok)
        .map(|r| r.name().to_owned())
        .filter(|n| n.as_bstr().ends_with(b"/HEAD"))
        .collect();
    for name in names {
        let Some(remote) = name
            .as_bstr()
            .to_str()
            .ok()
            .and_then(|n| n.strip_prefix("refs/remotes/"))
            .and_then(|n| n.strip_suffix("/HEAD"))
        else {
            continue;
        };
        let target = format!("refs/remotes/{remote}/{branch}");
        if repo.try_find_reference(target.as_str())?.is_none() {
            continue;
        }
        repo.edit_reference(gix::refs::transaction::RefEdit {
            change: gix::refs::transaction::Change::Update {
                log: gix::refs::transaction::LogChange {
                    mode: gix::refs::transaction::RefLog::AndReference,
                    force_create_reflog: false,
                    message: "clone: from remote".into(),
                },
                expected: gix::refs::transaction::PreviousValue::Any,
                new: gix::refs::Target::Symbolic(gix::refs::FullName::try_from(target.as_str())?),
            },
            name,
            deref: false,
        })
        .map_err(|e| anyhow::anyhow!("{e}"))?;
    }
    Ok(())
}

/// Remove `dir` and every empty directory inside it, deepest first. Non-empty
/// directories, and anything that is not a directory, are left alone.
fn prune_empty_dirs(dir: &Path) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        if entry.path().is_dir() {
            prune_empty_dirs(&entry.path());
        }
    }
    let _ = std::fs::remove_dir(dir);
}

/// Split a `-c <key>=<value>` argument the way git's `git_config_parse_parameter`
/// does: a bare `<key>` with no `=` is the boolean `true` (verified against stock
/// git 2.55.0 — `git clone -c foo.bar …` writes `bar = true`), and a key with no
/// section is rejected with git's `key does not contain a section: <key>`.
///
/// Validation happens here, during parsing, so a bad key fails before the
/// destination directory is created — exactly where git rejects it.
fn parse_config_pair(raw: &str) -> Result<(String, String)> {
    let (key, value) = match raw.split_once('=') {
        Some((k, v)) => (k, v.to_string()),
        None => (raw, "true".to_string()),
    };
    split_config_key(key)?;
    Ok((key.to_string(), value))
}

/// Split a config key into `(section, subsection, name)`. git splits at the first
/// dot for the section and the last dot for the variable name; everything between
/// is the (dot-tolerant) subsection.
fn split_config_key(key: &str) -> Result<(String, Option<String>, String)> {
    let first = key
        .find('.')
        .ok_or_else(|| anyhow::anyhow!("key does not contain a section: {key}"))?;
    let last = key.rfind('.').expect("a first dot implies a last dot");
    let section = &key[..first];
    let name = &key[last + 1..];
    if section.is_empty() || name.is_empty() {
        crate::git_fatal!("key does not contain variable name: {key}");
    }
    let subsection = (first != last).then(|| key[first + 1..last].to_string());
    Ok((section.to_string(), subsection, name.to_string()))
}

/// The local filesystem path a clone URL names, if any. `-s`/`--shared` only
/// applies "when the repository to clone is on the local machine", which is
/// exactly the `file://` and bare-path URLs gix classifies as `Scheme::File`.
fn local_path_of(url: &gix::Url) -> Option<PathBuf> {
    (url.scheme == gix::url::Scheme::File)
        .then(|| gix::path::from_bstr(url.path.as_bstr()).into_owned())
}

/// The object directory to borrow from for `--shared`/`--reference`: the
/// `objects` directory of the repository at `path`, absolute and symlink-resolved,
/// which is the form git records in `objects/info/alternates`.
fn objects_dir_of(path: &Path) -> Result<PathBuf> {
    let repo = gix::open(path)
        .map_err(|_| anyhow::anyhow!("reference repository '{}' is not a local repository.", path.display()))?;
    let objects = repo.git_dir().join("objects");
    Ok(std::fs::canonicalize(&objects).unwrap_or(objects))
}

/// Write `objects/info/alternates` with one absolute object-directory path per
/// line, the format git's `add_to_alternates_file` produces.
fn write_alternates(git_dir: &Path, alternates: &[PathBuf]) -> Result<()> {
    let info = git_dir.join("objects").join("info");
    std::fs::create_dir_all(&info)?;
    let mut body = String::new();
    for path in alternates {
        body.push_str(&path.to_string_lossy());
        body.push('\n');
    }
    std::fs::write(info.join("alternates"), body)?;
    Ok(())
}

/// Re-execute this binary's own porcelain inside the freshly cloned `dir`, the
/// way git's clone shells out to `git submodule update` and `git sparse-checkout`.
/// Returns the child's exit code.
///
/// `from_user` is git's `GIT_PROTOCOL_FROM_USER`: `git-submodule.sh:29` exports
/// it as `0` for everything it runs, because a submodule URL comes out of
/// `.gitmodules` rather than off the user's command line, and
/// [`transport_allowed`] refuses a `user`-scoped transport for it. Passing it
/// down here is what makes a recursive clone honor `protocol.file.allow`.
fn run_self(dir: &str, args: &[&str], from_user: bool) -> Result<u8> {
    let exe = std::env::current_exe()?;
    let mut cmd = std::process::Command::new(&exe);
    cmd.arg("-C").arg(dir).args(args);
    if !from_user {
        cmd.env("GIT_PROTOCOL_FROM_USER", "0");
    }
    let status = cmd.status()?;
    Ok(if status.success() {
        0
    } else {
        status.code().unwrap_or(1) as u8
    })
}

/// git's `is_transport_allowed()` (`transport.c:1124`) for `type`, the policy
/// added for CVE-2022-39253.
///
/// `GIT_ALLOW_PROTOCOL`, when set, is an exhaustive colon-separated allow-list
/// and nothing else is consulted. Otherwise `protocol.<type>.allow` decides,
/// falling back to `protocol.allow`, then to the built-in defaults: `http`,
/// `https`, `git` and `ssh` are `always`; `ext` is `never`; everything else —
/// `file` included — is `user`, meaning it is permitted only when the URL came
/// from the user rather than from a file the repository carries.
fn transport_allowed(kind: &str) -> Result<bool> {
    if let Ok(list) = std::env::var("GIT_ALLOW_PROTOCOL") {
        return Ok(list.split(':').any(|entry| entry == kind));
    }
    let config = crate::config::global_config();
    // git's `parse_protocol_config`: anything but these three is fatal.
    let parse = |key: &str, value: &str| match value.to_ascii_lowercase().as_str() {
        "always" => Ok(Allow::Always),
        "never" => Ok(Allow::Never),
        "user" => Ok(Allow::User),
        other => Err(anyhow::anyhow!("unknown value for config '{key}': {other}")),
    };
    let read = |key: &str| {
        config
            .string(key)
            .and_then(|v| v.to_str().ok().map(str::to_string))
    };
    let key = format!("protocol.{kind}.allow");
    let policy = match read(&key) {
        Some(v) => parse(&key, &v)?,
        None => match read("protocol.allow") {
            Some(v) => parse("protocol.allow", &v)?,
            None => match kind {
                "http" | "https" | "git" | "ssh" => Allow::Always,
                "ext" => Allow::Never,
                _ => Allow::User,
            },
        },
    };
    Ok(match policy {
        Allow::Always => true,
        Allow::Never => false,
        // `git_env_bool("GIT_PROTOCOL_FROM_USER", 1)`.
        Allow::User => !matches!(std::env::var("GIT_PROTOCOL_FROM_USER").as_deref(), Ok("0")),
    })
}

/// git's `protocol_allow_config` (`transport.c:1066`).
enum Allow {
    Never,
    User,
    Always,
}

/// git's `transport_check_allowed()` (`transport.c:1156`) for the transport
/// `url` selects. The error text is git's `transport '%s' not allowed`.
fn check_transport_allowed(url: &gix::Url) -> Result<()> {
    let kind = match url.scheme {
        gix::url::Scheme::File => "file",
        gix::url::Scheme::Git => "git",
        gix::url::Scheme::Ssh => "ssh",
        gix::url::Scheme::Http => "http",
        gix::url::Scheme::Https => "https",
        gix::url::Scheme::Ext(ref name) => name.as_str(),
    };
    if !transport_allowed(kind)? {
        crate::git_fatal!("transport '{kind}' not allowed");
    }
    Ok(())
}

/// Derive the default clone directory from a repository URL, mirroring git's
/// `guess_dir_name`: take the last `/`- or `:`-separated component, drop a
/// single trailing `.git`, and (for a bare clone) re-append `.git`.
///
/// Returns `None` when nothing usable remains (e.g. the URL is just `/` or `.`).
fn derive_dir_name(url: &str, bare: bool) -> Option<String> {
    let trimmed = url.trim_end_matches('/');
    let last = trimmed.rsplit(['/', ':']).next().unwrap_or("");
    let name = last.strip_suffix(".git").unwrap_or(last);
    if name.is_empty() || name == "." || name == ".." {
        return None;
    }
    Some(if bare {
        format!("{name}.git")
    } else {
        name.to_string()
    })
}

/// `clone_local()` + `copy_or_link_directory()` (builtin/clone.c): adopt the source's
/// object store wholesale.
///
/// Every file below `<source>/objects` is hardlinked into the destination — falling
/// back to a copy when the filesystem refuses, and copying outright under
/// `--no-hardlinks`. Whatever the local fetch left in `objects/pack` is dropped first,
/// so the destination holds exactly what the source holds rather than a re-packed
/// equivalent. `objects/info/alternates` is not inherited: the borrowed stores it names
/// are the source's business, and git resolves them into the copy instead.
fn adopt_local_objects(source: &Path, git_dir: &Path, hardlinks: bool) -> Result<()> {
    let src_objects = match source.join(".git").is_dir() {
        true => source.join(".git").join("objects"),
        false => source.join("objects"),
    };
    if !src_objects.is_dir() {
        return Ok(());
    }
    let dst_objects = git_dir.join("objects");

    // The fetch's own pack is what the adoption replaces.
    if let Ok(entries) = std::fs::read_dir(dst_objects.join("pack")) {
        for entry in entries.flatten() {
            let _ = std::fs::remove_file(entry.path());
        }
    }

    let mut stack = vec![(src_objects.clone(), dst_objects.clone())];
    while let Some((from, to)) = stack.pop() {
        std::fs::create_dir_all(&to)?;
        for entry in std::fs::read_dir(&from)? {
            let entry = entry?;
            let name = entry.file_name();
            let src_path = entry.path();
            let dst_path = to.join(&name);
            if entry.file_type()?.is_dir() {
                stack.push((src_path, dst_path));
                continue;
            }
            if from == src_objects.join("info") && name == "alternates" {
                continue;
            }
            if dst_path.exists() {
                continue;
            }
            let linked = hardlinks && std::fs::hard_link(&src_path, &dst_path).is_ok();
            if !linked {
                std::fs::copy(&src_path, &dst_path)?;
            }
        }
    }
    Ok(())
}

/// Drop the per-branch reflogs under `logs/refs/remotes/<remote>/`, which
/// `initial_ref_transaction_commit()` never creates. `HEAD` is kept: git writes that one
/// through the ordinary ref machinery when it records where the remote points.
fn remove_initial_remote_reflogs(git_dir: &Path, remote: &str) {
    let dir = git_dir.join("logs").join("refs").join("remotes").join(remote);
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return;
    };
    for entry in entries.flatten() {
        if entry.file_name() == "HEAD" {
            continue;
        }
        let path = entry.path();
        let _ = match entry.file_type().map(|t| t.is_dir()) {
            Ok(true) => std::fs::remove_dir_all(&path),
            _ => std::fs::remove_file(&path),
        };
    }
}

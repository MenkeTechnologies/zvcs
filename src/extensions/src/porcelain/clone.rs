use anyhow::{bail, Context, Result};
use std::num::NonZeroU32;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::atomic::AtomicBool;

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
///   * `--ref-format <format>`       → `files` or `reftable` (also from `GIT_DEFAULT_REF_FORMAT`
///     and `init.defaultRefFormat`)
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
/// Progress is git's: the `remote: ` sideband lines and `index-pack`'s
/// `Receiving objects`/`Resolving deltas` (see [`super::fetch_progress`]), shown
/// when stderr is a terminal, forced on with `--progress`, and suppressed by
/// `-q`/`--quiet` or `--no-progress`. A local clone transfers nothing and shows none.
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
/// The `remote.<name>.fetch` / `remote.<name>.tagOpt` / `branch.<name>.*` entries
/// git does not persist for a bare or mirrored clone are removed afterwards.
///
/// # Refs
///
/// gitoxide only fetches; the refs are written as `cmd_clone()` writes them once the
/// pack is in (see [`write_clone_refs`]): the mapped refs in one
/// `REF_TRANSACTION_FLAG_INITIAL` transaction, which puts them into `packed-refs`
/// without reflogs, then the tags a `--single-branch` clone follows,
/// `refs/remotes/<name>/HEAD` and `HEAD` in transactions of their own, each
/// running the `reference-transaction` hook as git's does. A reftable clone's
/// store is laid down just before, while it still holds no ref.
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
///   * A reftable clone is fetched through the files ref store it was laid down
///     with, which the fetch writes no ref into; the reftable stack replaces it
///     before the refs are written. Any `--ref-format` other than `files` and
///     `reftable` reproduces git's exact `unknown ref storage format '<v>'`.
///   * `--depth` combined with `--no-single-branch` costs one extra ls-refs round
///     trip, because gitoxide has already resolved its implicit single-branch
///     refspec by the time the replacement wildcard remote is installed.
/// ```
/// `git clone`'s usage block, byte-for-byte from stock git 2.55.0. Printed on
/// stdout for `-h` and on stderr for a usage error, both exiting 129.
const USAGE: &str = "usage: git clone [<options>] [--] <repo> [<dir>]\n\n    -v, --[no-]verbose    be more verbose\n    -q, --[no-]quiet      be more quiet\n    --[no-]progress       force progress reporting\n    --[no-]reject-shallow don't clone shallow repository\n    -n, --no-checkout     don't create a checkout\n    --checkout            opposite of --no-checkout\n    --[no-]bare           create a bare repository\n    --[no-]mirror         create a mirror repository (implies --bare)\n    -l, --[no-]local      to clone from a local repository\n    --no-hardlinks        don't use local hardlinks, always copy\n    --hardlinks           opposite of --no-hardlinks\n    -s, --[no-]shared     setup as shared repository\n    --[no-]recurse-submodules[=<pathspec>]\n                          initialize submodules in the clone\n    --[no-]recursive[=<pathspec>]\n                          alias of --recurse-submodules\n    -j, --[no-]jobs <n>   number of submodules cloned in parallel\n    --[no-]template <template-directory>\n                          directory from which templates will be used\n    --[no-]reference <repo>\n                          reference repository\n    --[no-]reference-if-able <repo>\n                          reference repository\n    --[no-]dissociate     use --reference only while cloning\n    -o, --[no-]origin <name>\n                          use <name> instead of 'origin' to track upstream\n    -b, --[no-]branch <branch>\n                          checkout <branch> instead of the remote's HEAD\n    --[no-]revision <rev> clone single revision <rev> and check out\n    -u, --[no-]upload-pack <path>\n                          path to git-upload-pack on the remote\n    --[no-]depth <depth>  create a shallow clone of that depth\n    --[no-]shallow-since <time>\n                          create a shallow clone since a specific time\n    --[no-]shallow-exclude <ref>\n                          deepen history of shallow clone, excluding ref\n    --[no-]single-branch  clone only one branch, HEAD or --branch\n    --[no-]tags           clone tags, and make later fetches not to follow them\n    --[no-]shallow-submodules\n                          any cloned submodules will be shallow\n    --[no-]separate-git-dir <gitdir>\n                          separate git dir from working tree\n    --[no-]ref-format <format>\n                          specify the reference format to use\n    -c, --[no-]config <key=value>\n                          set config inside the new repository\n    --[no-]server-option <server-specific>\n                          option to transmit\n    -4, --ipv4            use IPv4 addresses only\n    -6, --ipv6            use IPv6 addresses only\n    --[no-]filter <args>  object filtering\n    --[no-]also-filter-submodules\n                          apply partial clone filters to submodules\n    --[no-]remote-submodules\n                          any cloned submodules will use their remote-tracking branch\n    --[no-]sparse         initialize sparse-checkout file to include only files at root\n    --[no-]bundle-uri <uri>\n                          a URI for downloading bundles before fetching from origin remote\n\n";

/// `usage_with_options_internal()`'s `USAGE_FULL` rendering — what `--help-all`
/// prints. It is [`USAGE`] with the `PARSE_OPT_HIDDEN` entries left in:
/// `--[no-]naked`.
/// Captured byte-for-byte from stock git 2.55.0's `git clone --help-all`.
const USAGE_ALL: &str = r#"usage: git clone [<options>] [--] <repo> [<dir>]

    -v, --[no-]verbose    be more verbose
    -q, --[no-]quiet      be more quiet
    --[no-]progress       force progress reporting
    --[no-]reject-shallow don't clone shallow repository
    -n, --no-checkout     don't create a checkout
    --checkout            opposite of --no-checkout
    --[no-]bare           create a bare repository
    --[no-]naked          create a bare repository
    --[no-]mirror         create a mirror repository (implies --bare)
    -l, --[no-]local      to clone from a local repository
    --no-hardlinks        don't use local hardlinks, always copy
    --hardlinks           opposite of --no-hardlinks
    -s, --[no-]shared     setup as shared repository
    --[no-]recurse-submodules[=<pathspec>]
                          initialize submodules in the clone
    --[no-]recursive[=<pathspec>]
                          alias of --recurse-submodules
    -j, --[no-]jobs <n>   number of submodules cloned in parallel
    --[no-]template <template-directory>
                          directory from which templates will be used
    --[no-]reference <repo>
                          reference repository
    --[no-]reference-if-able <repo>
                          reference repository
    --[no-]dissociate     use --reference only while cloning
    -o, --[no-]origin <name>
                          use <name> instead of 'origin' to track upstream
    -b, --[no-]branch <branch>
                          checkout <branch> instead of the remote's HEAD
    --[no-]revision <rev> clone single revision <rev> and check out
    -u, --[no-]upload-pack <path>
                          path to git-upload-pack on the remote
    --[no-]depth <depth>  create a shallow clone of that depth
    --[no-]shallow-since <time>
                          create a shallow clone since a specific time
    --[no-]shallow-exclude <ref>
                          deepen history of shallow clone, excluding ref
    --[no-]single-branch  clone only one branch, HEAD or --branch
    --[no-]tags           clone tags, and make later fetches not to follow them
    --[no-]shallow-submodules
                          any cloned submodules will be shallow
    --[no-]separate-git-dir <gitdir>
                          separate git dir from working tree
    --[no-]ref-format <format>
                          specify the reference format to use
    -c, --[no-]config <key=value>
                          set config inside the new repository
    --[no-]server-option <server-specific>
                          option to transmit
    -4, --ipv4            use IPv4 addresses only
    -6, --ipv6            use IPv6 addresses only
    --[no-]filter <args>  object filtering
    --[no-]also-filter-submodules
                          apply partial clone filters to submodules
    --[no-]remote-submodules
                          any cloned submodules will use their remote-tracking branch
    --[no-]sparse         initialize sparse-checkout file to include only files at root
    --[no-]bundle-uri <uri>
                          a URI for downloading bundles before fetching from origin remote

"#;

/// `cmd_clone()`'s `struct option builtin_clone_options[]` (builtin/clone.c), in
/// table order, as [`super::resolve_long_aliased`] reads it.
const LONG_OPTS: &[super::LongOpt] = &[
    super::LongOpt { name: "verbose",                     neg: true,  arg: super::Arg::None },
    super::LongOpt { name: "quiet",                       neg: true,  arg: super::Arg::None },
    super::LongOpt { name: "progress",                    neg: true,  arg: super::Arg::None },
    super::LongOpt { name: "reject-shallow",              neg: true,  arg: super::Arg::None },
    super::LongOpt { name: "no-checkout",                 neg: true,  arg: super::Arg::None },
    super::LongOpt { name: "bare",                        neg: true,  arg: super::Arg::None },
    super::LongOpt { name: "naked",                       neg: true,  arg: super::Arg::None },
    super::LongOpt { name: "mirror",                      neg: true,  arg: super::Arg::None },
    super::LongOpt { name: "local",                       neg: true,  arg: super::Arg::None },
    super::LongOpt { name: "no-hardlinks",                neg: true,  arg: super::Arg::None },
    super::LongOpt { name: "shared",                      neg: true,  arg: super::Arg::None },
    super::LongOpt { name: "recurse-submodules",          neg: true,  arg: super::Arg::Optional },
    // `OPT_ALIAS(0, "recursive", "recurse-submodules")`: `preprocess_options()`
    // copies the source entry over the alias, keeping only the alias's own name.
    super::LongOpt { name: "recursive",                   neg: true,  arg: super::Arg::Optional },
    super::LongOpt { name: "jobs",                        neg: true,  arg: super::Arg::Required },
    super::LongOpt { name: "template",                    neg: true,  arg: super::Arg::Required },
    super::LongOpt { name: "reference",                   neg: true,  arg: super::Arg::Required },
    super::LongOpt { name: "reference-if-able",           neg: true,  arg: super::Arg::Required },
    super::LongOpt { name: "dissociate",                  neg: true,  arg: super::Arg::None },
    super::LongOpt { name: "origin",                      neg: true,  arg: super::Arg::Required },
    super::LongOpt { name: "branch",                      neg: true,  arg: super::Arg::Required },
    super::LongOpt { name: "revision",                    neg: true,  arg: super::Arg::Required },
    super::LongOpt { name: "upload-pack",                 neg: true,  arg: super::Arg::Required },
    super::LongOpt { name: "depth",                       neg: true,  arg: super::Arg::Required },
    super::LongOpt { name: "shallow-since",               neg: true,  arg: super::Arg::Required },
    super::LongOpt { name: "shallow-exclude",             neg: true,  arg: super::Arg::Required },
    super::LongOpt { name: "single-branch",               neg: true,  arg: super::Arg::None },
    super::LongOpt { name: "tags",                        neg: true,  arg: super::Arg::None },
    super::LongOpt { name: "shallow-submodules",          neg: true,  arg: super::Arg::None },
    super::LongOpt { name: "separate-git-dir",            neg: true,  arg: super::Arg::Required },
    super::LongOpt { name: "ref-format",                  neg: true,  arg: super::Arg::Required },
    super::LongOpt { name: "config",                      neg: true,  arg: super::Arg::Required },
    super::LongOpt { name: "server-option",               neg: true,  arg: super::Arg::Required },
    super::LongOpt { name: "ipv4",                        neg: false, arg: super::Arg::None },
    super::LongOpt { name: "ipv6",                        neg: false, arg: super::Arg::None },
    super::LongOpt { name: "filter",                      neg: true,  arg: super::Arg::Required },
    super::LongOpt { name: "also-filter-submodules",      neg: true,  arg: super::Arg::None },
    super::LongOpt { name: "remote-submodules",           neg: true,  arg: super::Arg::None },
    super::LongOpt { name: "sparse",                      neg: true,  arg: super::Arg::None },
    super::LongOpt { name: "bundle-uri",                  neg: true,  arg: super::Arg::Required },
];

/// The one `OPT_ALIAS()` group in `builtin_clone_options[]`. `is_alias()`
/// (parse-options.c:471) reads it so `--recurs` does not report itself as
/// ambiguous between the alias and the option it aliases.
const ALIAS_GROUPS: &[&[&str]] = &[&["recursive", "recurse-submodules"]];

/// `die()`: the `fatal: ` prefix and exit 128, which is what every clone refusal but the
/// usage errors exits with.
fn fatal(msg: &str) -> ExitCode {
    eprintln!("fatal: {msg}");
    ExitCode::from(128)
}

/// A `parse-options` usage error: the message, a blank line, then the usage block on
/// stderr, exit 129.
///
/// `usage_msg_opt()` renders `builtin_clone_options` as declared, not the copy
/// `parse_options()` worked on: `preprocess_options()` (parse-options.c:899-960)
/// gives that copy's `OPT_ALIAS` entry the argument help of the option it aliases,
/// and the original keeps none, so `usage_argh()` prints `...` for it
/// (parse-options.c:1296) and no continuation line follows.
fn usage_error(msg: &str) -> ExitCode {
    eprintln!("fatal: {msg}\n");
    eprint!(
        "{}",
        USAGE.replace(
            "    --[no-]recursive[=<pathspec>]\n                          alias of --recurse-submodules\n",
            "    --[no-]recursive ...  alias of --recurse-submodules\n",
        )
    );
    ExitCode::from(129)
}

pub fn clone(args: &[String]) -> Result<ExitCode> {
    // `git_clone_config()` takes `clone.defaultRemoteName` from every scope on its
    // first read, before the options are even parsed, a valueless one being
    // `config_error_nonbool()` (builtin/clone.c:750-754, :1012).
    let configured_remote_name = match crate::config::config_entries_in_order(None)
        .into_iter()
        .filter(|(key, _)| key == "clone.defaultremotename")
        .last()
    {
        Some((_, Some(name))) => Some(name),
        Some((_, None)) => crate::config::die_config(
            None,
            "clone.defaultremotename",
            Some("missing value for 'clone.defaultremotename'"),
        ),
        None => None,
    };

    // `-h` is answered by `parse-options` before anything else, on stdout.
    if args.iter().any(|a| a == "-h") {
        return Ok(super::show_usage(USAGE));
    }
    // `--help-all` likewise, rendering `USAGE_FULL` — `USAGE` plus the hidden
    // `--naked`. `parse_options_step()` *breaks* on `--` and `--end-of-options`
    // one line before it tests the name (parse-options.c:1112-1122), so the
    // search stops at the terminator; and because the test is a `strcmp`, no
    // abbreviation and no `=<value>` spelling reaches it.
    if args
        .iter()
        .take_while(|a| a.as_str() != "--" && a.as_str() != "--end-of-options")
        .any(|a| a == "--help-all")
    {
        return Ok(super::show_usage(USAGE_ALL));
    }
    let mut bare = false;
    let mut mirror = false;
    // `option_verbosity` (builtin/clone.c:75), which `OPT__VERBOSITY()` counts.
    let mut verbosity: i32 = 0;
    // `Some(true)` = `--progress` forced, `Some(false)` = `--no-progress`,
    // `None` = default (progress iff stderr is a terminal), matching git.
    let mut force_progress: Option<bool> = None;
    // `--no-local` / `--no-hardlinks`, which decide whether a local source's object
    // store is adopted directly and whether that adoption may hardlink.
    let mut no_local = false;
    // git's `option_local` is a tri-state: -1 by default, 0 for `--no-local`, 1 for an
    // explicit `-l`/`--local`. Only the last of the three makes git say the option was
    // ignored, so the explicit form is tracked apart from the effective value.
    let mut local_explicit = false;
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
    let mut depth: Option<String> = None;
    let mut shallow_exclude: Vec<gix::bstr::BString> = Vec::new();
    let mut shallow_since: Option<gix::date::Time> = None;
    // `-c <key>=<value>`, in command-line order; repeats of a key are kept so each
    // value is written, as git documents.
    let mut config_pairs: Vec<(String, String)> = Vec::new();
    let mut template: Option<String> = None;
    let mut separate_git_dir: Option<String> = None;
    let mut ref_format: Option<String> = None;
    // `--revision=<rev>`: the one ref or object id to clone, with `HEAD` detached at it.
    let mut revision: Option<String> = None;
    // `--reference <repo>` / `--reference-if-able <repo>`: git's two
    // `OPT_STRING_LIST`s, `option_required_reference` and
    // `option_optional_reference` (builtin/clone.c:76-77, :956-959). They are
    // kept apart because `setup_reference()` walks the whole required list before
    // the optional one, whatever order the options were typed in.
    let mut required_references: Vec<String> = Vec::new();
    let mut optional_references: Vec<String> = Vec::new();
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
        let typed = args[i].as_str();
        i += 1;

        if end_of_options {
            positionals.push(typed);
            continue;
        }

        // Respell a unique abbreviation as the name it resolves to, so `--single-b`
        // reaches the same arm as `--single-branch`. The aliased form is needed
        // because `builtin_clone_options[]` has an `OPT_ALIAS()`: without the group,
        // `--recurs` would report itself ambiguous between `--recursive` and the
        // `--recurse-submodules` it aliases.
        let canonical;
        if let Some(code) = super::long_takes_no_value_aliased(typed, LONG_OPTS, ALIAS_GROUPS) {
            return Ok(code);
        }
        let a = match super::canonical_long_aliased(typed, LONG_OPTS, ALIAS_GROUPS) {
            super::Long::Name(name) => {
                canonical = name;
                canonical.as_ref()
            }
            super::Long::Ambiguous(first, second) => {
                return Ok(super::ambiguous_option(typed, &first, &second, USAGE))
            }
        };

        // Split `--opt=value` for the value-taking long options.
        let (key, inline_val) = match (a.starts_with("--"), a.split_once('=')) {
            (true, Some((k, v))) => (k, Some(v.to_string())),
            _ => (a, None),
        };

        // `parse_long_opt()`'s refusal for an `=<value>` on an entry that takes
        // none (`OPT_BOOL` and friends):
        //
        // ```c
        // if (!rest)
        //         ...
        // return error(_("%s takes no value"), optname(options, flags));
        // ```
        //
        // `optname()` renders it as ``option `<name>'``, and the caller answers
        // `PARSE_OPT_ERROR`, which is `exit(129)` with no usage block.
        if inline_val.is_some() {
            if let Some(stem) = key.strip_prefix("--") {
                let takes_none = LONG_OPTS.iter().any(|o| {
                    o.arg == super::Arg::None
                        && (o.name == stem
                            || (o.neg
                                && stem.strip_prefix("no-").is_some_and(|s| s == o.name)))
                });
                if takes_none {
                    eprintln!("error: option `{stem}' takes no value");
                    return Ok(ExitCode::from(129));
                }
            }
        }

        // Fetch the value for a value-taking option (inline `=v` or next arg).
        // Kept as a plain expression (not a closure) so the `i` cursor stays
        // freely borrowable in the other match arms.
        // The option is named the way it was *typed*, because `optname()` is
        // (parse-options.c:30-45): `git clone -o` is ``switch `o'`` and
        // `--origin` is ``option `origin'``. Passing the long name for both is
        // what made every short spelling here report the wrong half — and the
        // whole message the wrong shape, since `zvcs: clone: --origin requires a
        // value` at exit 1 is neither git's text nor git's 129.
        macro_rules! take_value {
            () => {
                match inline_val.clone() {
                    Some(v) => v,
                    None => super::take_value(args, &mut i, key)?.to_string(),
                }
            };
        }

        match key {
            "--" => end_of_options = true,
            "--bare" => bare = true,
            "--no-bare" => bare = false,
            // `OPT_HIDDEN_BOOL(0, "naked", &option_bare, ...)` (builtin/clone.c:931):
            // a second, hidden name for `--bare`, negatable like it.
            "--naked" => bare = true,
            "--no-naked" => bare = false,
            // `--mirror` implies `--bare` (git-clone(1)); the implication is applied
            // after parsing so a later `--no-mirror` can still take it back.
            "--mirror" => mirror = true,
            "--no-mirror" => mirror = false,
            // `OPT__VERBOSITY(&option_verbosity)` (builtin/clone.c:923), whose
            // `parse_opt_verbosity_cb()` (parse-options-cb.c:65-87) counts `-v` up and
            // `-q` down, each starting over from the other's side of zero, and resets on
            // either negation.
            "-q" | "--quiet" => verbosity = if verbosity <= 0 { verbosity - 1 } else { -1 },
            "-v" | "--verbose" => verbosity = if verbosity >= 0 { verbosity + 1 } else { 1 },
            "--no-quiet" | "--no-verbose" => verbosity = 0,
            "--progress" => force_progress = Some(true),
            "--no-progress" => force_progress = Some(false),
            // `clone_local()`: a local source has its object store copied (hardlinked
            // where the filesystem allows) instead of being packed and unpacked.
            // `--no-local` forces the transport, `--no-hardlinks` copies the bytes.
            "-l" | "--local" => {
                no_local = false;
                local_explicit = true;
            }
            "--no-local" => {
                no_local = true;
                local_explicit = false;
            }
            "--no-hardlinks" => hardlinks = false,
            "--hardlinks" => hardlinks = true,
            // `-n`/`--no-checkout`: fetch refs and set `HEAD`, but do not populate
            // a worktree (leaves an empty index, exactly like git).
            "-n" | "--no-checkout" => no_checkout = true,
            "--checkout" => no_checkout = false,
            "-o" | "--origin" => origin = Some(take_value!()),
            "--no-origin" => origin = None,
            "-b" | "--branch" => branch = Some(take_value!()),
            "--no-branch" => branch = None,
            "-c" | "--config" => {
                let raw = take_value!();
                config_pairs.push(parse_config_pair(&raw));
            }
            "--no-tags" => tags = Some(Tags::None),
            // `--tags` resets to git's default (all tags fetched).
            "--tags" => tags = None,
            // `-u`/`--upload-pack <path>`: the program to run in place of `git-upload-pack`.
            "-u" | "--upload-pack" => upload_pack = Some(take_value!()),
            "--no-upload-pack" => upload_pack = None,
            // Protocol-v2 server options, repeatable. `-o` is `--origin` for clone, so only the long form.
            "--server-option" => server_options.push(take_value!().into()),
            "--no-server-option" => server_options.clear(),
            "-j" | "--jobs" => jobs = Some(take_value!()),
            // `OPT_INTEGER` unset stores 0 (parse-options.c:260-267),
            // which is not `max_jobs == -1`, so `--jobs=0` is forwarded.
            "--no-jobs" => jobs = Some("0".to_string()),
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
            "--template" => template = Some(take_value!()),
            "--no-template" => template = None,
            "--separate-git-dir" => separate_git_dir = Some(take_value!()),
            "--no-separate-git-dir" => separate_git_dir = None,
            "--ref-format" => ref_format = Some(take_value!()),
            "--no-ref-format" => ref_format = None,
            "--reference" => required_references.push(take_value!()),
            "--reference-if-able" => optional_references.push(take_value!()),
            // `OPT_STRING_LIST`'s unset arm is `string_list_clear(v, 0)`
            // (parse-options-cb.c:199-206): `--no-reference` empties the list the
            // earlier `--reference`s built, it takes no value.
            "--no-reference" => required_references.clear(),
            "--no-reference-if-able" => optional_references.clear(),
            "-s" | "--shared" => shared = true,
            "--no-shared" => shared = false,
            "--dissociate" => dissociate = true,
            "--no-dissociate" => dissociate = false,
            "--sparse" => sparse = true,
            "--no-sparse" => sparse = false,
            // `--revision=<rev>`: clone only what `<rev>` needs and detach `HEAD` there
            // (`OPT_STRING(0, "revision", ...)`, builtin/clone.c:966-967).
            "--revision" => revision = Some(take_value!()),
            "--no-revision" => revision = None,
            // `--filter=<spec>`: ask the remote to withhold the objects `<spec>` selects against.
            // git parses the spec in `cmd_clone`'s option table, so an invalid one never reaches the
            // network; `Filter::from_str` reproduces both the grammar and the messages.
            "--filter" => {
                let raw = take_value!();
                // `parse_list_objects_filter()` (list-objects-filter-options.c) ends on
                // `die(_("invalid filter-spec '%s'"), arg)` — git's words at 128, not this
                // port's at 1.
                filter = Some(raw.parse().map_err(|e| crate::fatal::die(format!("{e}")))?);
            }
            "--no-filter" => filter = None,
            // `--also-filter-submodules`: apply the same filter to the submodules the clone recurses
            // into (git's `option_also_filter_submodules`).
            "--also-filter-submodules" => also_filter_submodules = Some(true),
            "--no-also-filter-submodules" => also_filter_submodules = Some(false),
            "--bundle-uri" => bundle_uri = Some(take_value!()),
            "--no-bundle-uri" => bundle_uri = None,
            // `OPT_STRING` unset is NULL and `OPT_STRING_LIST` unset clears the
            // list, so each of these takes back every earlier spelling of its
            // option: the shallow requests, and the `-c` pairs.
            "--no-depth" => depth = None,
            "--no-shallow-since" => shallow_since = None,
            "--no-shallow-exclude" => shallow_exclude.clear(),
            "--no-config" => config_pairs.clear(),
            "--depth" => depth = Some(take_value!()),
            // Shallow boundary at a cutoff date (git's `deepen_since`). `fetch-pack.c:439` runs
            // the value through `approxidate()`, which never fails — an unreadable date is the
            // current time, not an error.
            "--shallow-since" => {
                let v = take_value!();
                shallow_since = Some(gix::date::Time::new(crate::date::approxidate(&v), 0));
            }
            // Exclude history reachable from a ref (git's repeatable `deepen_not`).
            "--shallow-exclude" => {
                let v = take_value!();
                // Sent to the server as `deepen-not <value>` whatever it is; the server judges it.
                shallow_exclude.push(gix::bstr::BString::from(v));
            }
            // `--recursive` / `--recurse-submodules[=<pathspec>]`: after the clone,
            // initialize and update submodules recursively. A pathspec-limited
            // recurse is honored as a full recursive update.
            //
            // `recurse_submodules_cb()` (builtin/clone.c:83-95) appends the
            // pathspec, or the option's `defval` of `"."` when none is given, so
            // `--recursive --recursive=a` records both `.` and `a`.
            "--recursive" | "--recurse-submodules" => {
                recurse_submodules = true;
                submodule_pathspecs.push(inline_val.clone().unwrap_or_else(|| ".".to_string()));
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
                config_pairs.push(parse_config_pair(&other[2..]));
            }
            // A long name no table entry claims is `parse_options()`' own refusal —
            // the `error:` line and the block, both on stderr, exit 129 — not a gap
            // in this port. It has to be decided against the table rather than by
            // spelling, because `-4`/`--ipv4` and `-6`/`--ipv6` carry
            // `PARSE_OPT_NONEG` and so have no `--no-` form to resolve.
            other
                if other.starts_with("--")
                    && matches!(
                        super::resolve_long(LONG_OPTS, &other[2..]),
                        super::Resolved::Unknown
                    ) =>
            {
                // `error(_("unknown option `%s'"), ctx.argv[0] + 2)`
                // (parse-options.c:1215-1216) names the argument as *typed*,
                // value and all: `--quux=x` is reported as `quux=x`, not `quux`.
                // `other` here is already split at the `=`, so the message has to
                // go back to the token.
                return Ok(super::unknown_option(typed, USAGE));
            }
            // Every remaining `-<chars>` token, walked the way
            // `parse_options_step()` walks a short cluster
            // (parse-options.c:1061-1107): each character is its own option, a
            // value-taking one swallows the rest of the token or the next argv
            // element, and the first character the table does not claim is named
            // on its own — against the synthetic `-<rest>` the C builds at :1095,
            // so `git clone -qZ` reports `Z` and not `q`. This whole arm used to
            // be `zvcs: clone: unsupported option "-a"` at exit 1.
            other if other.starts_with('-') && !other.starts_with("--") && other.len() > 1 => {
                for (off, c) in other.char_indices().skip(1) {
                    let rest = &other[off + c.len_utf8()..];
                    match c {
                        'q' => verbosity = if verbosity <= 0 { verbosity - 1 } else { -1 },
                        'v' => verbosity = if verbosity >= 0 { verbosity + 1 } else { 1 },
                        'l' => {
                            no_local = false;
                            local_explicit = true;
                        }
                        'n' => no_checkout = true,
                        's' => shared = true,
                        '4' => {
                            address_family =
                                Some(gix::protocol::transport::AddressFamily::V4)
                        }
                        '6' => {
                            address_family =
                                Some(gix::protocol::transport::AddressFamily::V6)
                        }
                        'o' | 'b' | 'u' | 'c' | 'j' => {
                            let v = match rest.is_empty() {
                                true => super::take_value(
                                    args,
                                    &mut i,
                                    &format!("-{c}"),
                                )?
                                .to_string(),
                                false => rest.to_string(),
                            };
                            match c {
                                'o' => origin = Some(v),
                                'b' => branch = Some(v),
                                'u' => upload_pack = Some(v),
                                'c' => config_pairs.push(parse_config_pair(&v)),
                                _ => jobs = Some(v),
                            }
                            break;
                        }
                        'h' => return Ok(super::show_usage(USAGE)),
                        _ => {
                            return Ok(super::unknown_option(
                                &format!("-{}", &other[off..]),
                                USAGE,
                            ))
                        }
                    }
                }
            }
            // A non-option argument is handed back unchanged by the resolver, so the
            // argv slice itself is pushed and the operand keeps `args`' lifetime.
            _ => positionals.push(typed),
        }
    }

    // Every `0 <= option_verbosity` test of `cmd_clone()` is `!quiet` here.
    let quiet = verbosity < 0;

    // `--mirror` implies `--bare` (git-clone(1): "This implies --bare").
    if mirror {
        bare = true;
    }

    // `usage_msg_opt()` for the operand count is the first thing `cmd_clone()` does
    // with what `parse_options()` left, so a bad count is 129 ahead of every option
    // conflict and every value check below. Measured against stock 2.55.0:
    //
    // ```text
    // $ git clone --ref-format=2m a b c         → fatal: Too many arguments.  (129)
    // $ git clone --ref-format=2m               → fatal: You must specify a repository to clone.  (129)
    // $ git clone --bundle-uri=x --depth=1      → fatal: You must specify a repository to clone.  (129)
    // $ git clone --bare --separate-git-dir=x   → fatal: You must specify a repository to clone.  (129)
    // ```
    if positionals.len() > 2 {
        return Ok(usage_error("Too many arguments."));
    }
    let url_str = match positionals.first() {
        Some(u) => *u,
        None => return Ok(usage_error("You must specify a repository to clone.")),
    };

    // git's ref storage formats are exactly `files` and `reftable`, and an unknown
    // name dies here.
    if let Some(fmt) = ref_format.as_deref() {
        match fmt {
            "files" | "reftable" => {}
            other => crate::git_fatal!("unknown ref storage format '{other}'"),
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

    // ```c
    // path = get_repo_path(repo_name, &is_bundle);
    // if (path) {
    //         ...
    // } else if (strchr(repo_name, ':')) {
    //         ...
    // } else
    //         die(_("repository '%s' does not exist"), repo_name);
    // ```
    //
    // (`cmd_clone()`, builtin/clone.c.) Before anything is created or announced, a
    // source that is neither a repository, a gitfile nor a bundle on disk, and has
    // no colon to be a URL, is refused — an existing directory that holds no
    // repository included. `get_repo_path()` runs whether or not the name has a
    // colon, and a gitfile it reads dies there, ahead of everything else.
    let classified = classify_local_source(url_str);
    if let LocalSource::Refused(msg) = &classified {
        return Ok(fatal(msg));
    }
    if !url_str.contains(':') && matches!(classified, LocalSource::Neither) {
        return Ok(fatal(&format!("repository '{url_str}' does not exist")));
    }

    // ```c
    // if (option_depth && atoi(option_depth) < 1)
    //         die(_("depth %s is not a positive number"), option_depth);
    // ```
    //
    // (`cmd_clone()`, builtin/clone.c:1065-1066.) `--depth` is an `OPT_STRING`, so
    // the value is only judged here — after the operand count and the source
    // check — and by `atoi()`, which reads ` 2` and `+2` as 2 and `3x` as 3. The
    // transport then takes it through `strtol(value, &end, 0)` and refuses
    // trailing junk (transport.c:261-268) — a die that comes later, once the
    // transport exists, so it is held in `depth_junk` until then.
    let mut depth_junk: Option<String> = None;
    let depth: Option<NonZeroU32> = match depth.as_deref() {
        None => None,
        Some(v) => {
            if c_atoi(v) < 1 {
                return Ok(fatal(&format!("depth {v} is not a positive number")));
            }
            let parsed = c_strtol_full(v).and_then(|n| u32::try_from(n).ok()).and_then(NonZeroU32::new);
            if parsed.is_none() {
                depth_junk = Some(v.to_string());
            }
            Some(parsed.unwrap_or(NonZeroU32::MIN))
        }
    };

    // Parse the URL up front so a malformed one is reported before touching disk.
    let url = gix::url::parse(url_str.into())?;

    // `transport_check_allowed()`. git reaches it only once the transport is being
    // opened, which is after the `Cloning into '<dir>'…` banner — reproduced by
    // deferring the check to the banner site below rather than refusing here.

    // Resolve the shallow-boundary selectors into a single `Shallow` value. The
    // exclude form supersedes a lone `--shallow-since`, which supersedes
    // `--depth`, matching git's treatment of them as one deepen group.
    // Remembered before the selectors are folded together, so the local-clone
    // warning below can name the flag the caller actually typed.
    let shallow_depth_given = depth.is_some();
    let shallow_since_given = shallow_since.is_some();
    let shallow_exclude_given = !shallow_exclude.is_empty();
    // ```c
    // path = get_repo_path(remote->url.v[0], &is_bundle);
    // is_local = option_local != 0 && path && !is_bundle;
    // ```
    //
    // (builtin/clone.c:1323-1324.) One flag decides everything that follows: whether the
    // shallow selectors and `--filter` are ignored with a warning, whether the object store
    // is adopted instead of fetched, and whether the clone ends with `done.`. `--no-local`
    // clears it, which is what makes `git clone --no-local --depth 1 <path>` a genuinely
    // shallow clone of a path on this machine.
    //
    // A source that is shallow itself takes `is_local` away again further down.
    //
    // `get_repo_path_1()` walks four repository spellings — `<path>/.git`, `<path>`,
    // `<path>.git/.git`, `<path>.git` — and only then tries `<path>.bundle` and
    // `<path>` as a *regular file* (builtin/clone.c:97-141), setting `*is_bundle` on
    // the second pass. So a bundle is never a local clone, and everything a local
    // clone would have ignored — the shallow selectors, `--filter` — a bundle
    // ignores too, because `transport_get()` gives a bundle
    // `ret->smart_options = NULL` and a vtable of three entries
    // (transport.c:1162-1166, 1212), none of which is `set_option`. A source that
    // only `<path>.git` names is local as well, and is read through that spelling.
    let (local_source, bundle_source) = match (!url_str.contains("://"))
        .then(|| classify_local_source(url_str))
    {
        Some(LocalSource::Repository(path)) => (Some(path), None),
        Some(LocalSource::Bundle(path)) => (None, Some(path)),
        _ => (None, None),
    };
    let mut is_local = !no_local && local_source.is_some();
    let is_bundle = bundle_source.is_some();
    let local_source = local_source.unwrap_or_else(|| PathBuf::from(url_str));
    let mut reject_shallow_source = false;
    if is_local && gix::open(&local_source).map(|r| r.is_shallow()).unwrap_or(false) {
        // ```c
        // if (!access(mkpath("%s/shallow", path), F_OK)) {
        //         if (reject_shallow)
        //                 die(_("source repository is shallow, reject to clone."));
        //         if (option_local > 0)
        //                 warning(_("source repository is shallow, ignoring --local"));
        //         is_local = 0;
        // }
        // ```
        //
        // (builtin/clone.c:1334-1340.) Copying a shallow object store would carry its
        // `shallow` boundary without the file that records it, so git falls back to the
        // transport, which negotiates the boundary properly.
        // The key is read from the configuration `git_config()` had merged by
        // then, which is the *ambient* one — `git -c clone.rejectShallow=true
        // clone …` sets it, and so does `~/.gitconfig`. `clone`'s own
        // `-c <key>=<value>` is written into the repository being created and is
        // consulted here too, since git replays those through
        // `git_config_parse_parameter()` before the transport runs.
        let reject = reject_shallow.unwrap_or_else(|| {
            let truthy = |v: &str| v != "false" && v != "0" && !v.is_empty();
            config_pairs
                .iter()
                .rev()
                .find(|(k, _)| k.eq_ignore_ascii_case("clone.rejectShallow"))
                .map(|(_, v)| truthy(v))
                .or_else(|| crate::setup::cli_override("clone.rejectShallow").map(truthy))
                .unwrap_or(false)
        });
        // The refusal itself waits for the banner: git reaches this block from
        // `cmd_clone` *after* `init_db()` has printed `Cloning into '<dir>'...`,
        // so the fatal is the second line and not the first.
        if reject {
            reject_shallow_source = true;
        }
        if local_explicit {
            eprintln!("warning: source repository is shallow, ignoring --local");
        }
        is_local = false;
    }
    let shallow = if is_local || is_bundle {
        // Ignored for a local path clone — see the warning at the banner — and for
        // a bundle, whose transport has no `set_option` to receive the depth with.
        // Measured on stock 2.55.0: `git clone --depth 1 ./all.bundle v3` leaves no
        // `.git/shallow` and the full history of the branch it checked out.
        Shallow::NoChange
    } else if !shallow_exclude.is_empty() {
        // `--depth` rides along rather than being dropped: git sets `TRANS_OPT_DEPTH`,
        // `TRANS_OPT_DEEPEN_SINCE` and `TRANS_OPT_DEEPEN_NOT` from three independent options
        // (builtin/clone.c:1372-1380) and sends every one that was named, leaving
        // `upload-pack.c:send_shallow_list()` to refuse `deepen` together with a rev-list
        // deepening. Folding them into one selector clones where git exits 128.
        Shallow::Exclude {
            remote_refs: shallow_exclude,
            since_cutoff: shallow_since,
            depth,
        }
    } else if let Some(cutoff) = shallow_since {
        Shallow::Since { cutoff, depth }
    } else if let Some(n) = depth {
        Shallow::DepthAtRemote(n)
    } else {
        Shallow::NoChange
    };

    // Destination directory: explicit second positional, else derived from the
    // URL the way git's `guess_dir_name` does (humanish last component).
    let dir = match positionals.get(1) {
        Some(d) => (*d).to_string(),
        None => match derive_dir_name(url_str, is_bundle, bare) {
            Some(dir) => dir,
            None => {
                return Ok(fatal(
                    "No directory name could be guessed.\nPlease specify a directory on the command line",
                ))
            }
        },
    };
    // `strip_dir_trailing_slashes(dir)`: `d/` and `d//` are `d`, but `/` stays `/`.
    let dir = match dir.trim_end_matches('/') {
        "" if !dir.is_empty() => "/".to_string(),
        trimmed => trimmed.to_string(),
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
    // The same test against the *repository* path, one message and one line of
    // `cmd_clone` later. Measured against stock 2.55.0:
    //
    // ```text
    // $ git clone --separate-git-dir=src . sgd
    // fatal: repository path 'src' already exists and is not an empty directory.
    // ```
    //
    // No banner precedes it and no `sgd` is left behind, so it belongs beside the
    // destination test above rather than with the relocation that consumes the
    // value. The path is named exactly as it was written, not resolved.
    if let Some(real) = separate_git_dir.as_deref() {
        let real_path = Path::new(real);
        if real_path.exists()
            && real_path
                .read_dir()
                .map(|mut e| e.next().is_some())
                .unwrap_or(true)
        {
            return Ok(fatal(&format!(
                "repository path '{real}' already exists and is not an empty directory."
            )));
        }
    }
    // builtin/clone.c:1091-1097, still before the banner:
    //
    // ```c
    // if (option_bare)
    //         work_tree = NULL;
    // else {
    //         work_tree = getenv("GIT_WORK_TREE");
    //         if (work_tree && path_exists(work_tree))
    //                 die(_("working tree '%s' already exists."), work_tree);
    // }
    // ```
    //
    // `--work-tree=.` reaches here as `GIT_WORK_TREE=.` (git.c's handle_options), so
    // `git --work-tree=. clone src dst` dies naming `.` exactly as written.
    // `path_exists()` (builtin/clone.c:863-867) is a `stat()`, so a dangling symlink
    // does not count and falls through to the `mkdir()` below.
    let env_work_tree = (!bare)
        .then(|| std::env::var_os("GIT_WORK_TREE"))
        .flatten()
        .map(|work_tree| work_tree.to_string_lossy().into_owned());
    if let Some(work_tree) = &env_work_tree {
        if std::fs::metadata(work_tree).is_ok() {
            return Ok(fatal(&format!("working tree '{work_tree}' already exists.")));
        }
    }
    // ```c
    // if (option_bare || work_tree)
    //         git_dir = xstrdup(dir);
    // else {
    //         work_tree = dir;
    //         git_dir = mkpathdup("%s/.git", dir);
    // }
    // ```
    //
    // (builtin/clone.c:1099-1104.) A `$GIT_WORK_TREE` the stat() above did not find is
    // the work tree to make, and the destination becomes the git directory itself.
    let work_tree: &str = env_work_tree.as_deref().unwrap_or(&dir);
    // `junk_work_tree` in `cmd_clone()`: git remembers the directory it made so `remove_junk()`
    // can take it down again on any death below, and leaves a directory it found alone.
    let created_destination = !dst.exists();
    // builtin/clone.c:1108-1123, all of it before the banner:
    //
    // ```c
    // if (!option_bare) {
    //         if (safe_create_leading_directories_const(the_repository, work_tree) < 0)
    //                 die_errno(_("could not create leading directories of '%s'"),
    //                           work_tree);
    //         if (dest_exists)
    //                 junk_work_tree_flags |= REMOVE_DIR_KEEP_TOPLEVEL;
    //         else if (mkdir(work_tree, 0777))
    //                 die_errno(_("could not create work tree dir '%s'"),
    //                           work_tree);
    // [...]
    // if (safe_create_leading_directories_const(the_repository, git_dir) < 0)
    //         die(_("could not create leading directories of '%s'"), git_dir);
    // ```
    //
    // `SCLD_EXISTS` counts as failure here. `create_dir_all("")` succeeds without
    // creating anything where `mkdir("")` is `ENOENT`, so `git clone src ""` used to go
    // on and open a repository at the empty path.
    //
    // `work_tree` is `$GIT_WORK_TREE` when that is set, which is why a dangling symlink there
    // ends at `could not create work tree dir 'ln': File exists`. It is only made when the
    // destination did not exist: under `$GIT_WORK_TREE` an existing empty destination leaves
    // the work tree unmade, and `checkout()` dies on it below.
    if !bare {
        if let Err(scld) = crate::path::safe_create_leading_directories(work_tree) {
            crate::git_fatal!(
                "could not create leading directories of '{work_tree}': {}",
                crate::external::strerror(&scld.errno())
            );
        }
        if created_destination {
            if let Err(e) = std::fs::create_dir(work_tree) {
                crate::git_fatal!(
                    "could not create work tree dir '{work_tree}': {}",
                    crate::external::strerror(&e)
                );
            }
        }
    }
    if (bare || env_work_tree.is_some()) && crate::path::safe_create_leading_directories(&dir).is_err() {
        crate::git_fatal!("could not create leading directories of '{dir}'");
    }
    std::fs::create_dir_all(dst)?;
    // ```c
    // atexit(remove_junk);
    // sigchain_push_common(remove_junk_on_signal);
    // ```
    //
    // (builtin/clone.c:1106-1107.) Everything between here and the checkout is covered: a
    // transport the policy forbids, an unreachable remote, a refused ref — each one is a
    // `die()` that takes the half-built clone down with it. The git dir it would also remove
    // lives inside `dst` until `--separate-git-dir` relocates it, which happens after the
    // fetch, so the one path covers both.
    //
    // Under `$GIT_WORK_TREE` the two are apart (builtin/clone.c:1113-1129): `junk_git_dir` is the
    // destination and `junk_work_tree` the work tree, each emptied but kept
    // (`REMOVE_DIR_KEEP_TOPLEVEL`) when the destination already existed.
    let junk = match &env_work_tree {
        Some(work_tree) => JunkDir::arm(vec![
            (dst.to_path_buf(), !created_destination),
            (PathBuf::from(work_tree), !created_destination),
        ]),
        None => JunkDir::arm(created_destination.then(|| (dst.to_path_buf(), false)).into_iter().collect()),
    };

    // Serialize the ref/index writes through the repo coordinator, matching the
    // other write commands. On a freshly created clone no daemon is listening, so
    // this degrades to a no-op guard; it is held for the whole fetch+checkout.
    let git_dir = if bare || env_work_tree.is_some() {
        dst.to_path_buf()
    } else {
        dst.join(".git")
    };
    let _lock = crate::lock::RepoLock::acquire(&git_dir);

    let should_interrupt = AtomicBool::new(false);

    // git prints the "Cloning into ..." banner before any progress. `0 <= option_verbosity`
    // (clone.c:1134) is what `-q` switches off.
    if !quiet {
        if bare {
            eprintln!("Cloning into bare repository '{dir}'...");
        } else {
            eprintln!("Cloning into '{dir}'...");
        }
    }

    // Without `-o`, the name `git_clone_config()` left must pass
    // `valid_remote_name()` (builtin/clone.c:1284-1292, remote.c) or the clone
    // dies, an empty name included.
    if origin.is_none() {
        if let Some(name) = &configured_remote_name {
            let refspec = format!("refs/heads/test:refs/remotes/{name}/test");
            if gix::refspec::parse(refspec.as_str().into(), gix::refspec::parse::Operation::Fetch).is_err() {
                return Err(crate::fatal::die(format!("'{name}' is not a valid remote name")));
            }
        }
    }

    // ```c
    // if (option_recurse_submodules.nr > 0) {
    //         [...]
    //         string_list_sort_u(&option_recurse_submodules, 0);
    //         for_each_string_list_item(item, &option_recurse_submodules) {
    //                 strbuf_addf(&sb, "submodule.active=%s", item->string);
    //                 string_list_append(&option_config, sb.buf);
    //         [...]
    //         if (!repo_config_get_bool(the_repository, "submodule.stickyRecursiveClone", &val) &&
    //             val)
    //                 string_list_append(&option_config, "submodule.recurse=true");
    //
    //         if (option_required_reference.nr &&
    //             option_optional_reference.nr)
    //                 die(_("clone --recursive is not compatible with "
    //                       "both --reference and --reference-if-able"));
    //         else if (option_required_reference.nr) {
    //                 [... "submodule.alternateLocation=superproject" ...]
    //                 [... "submodule.alternateErrorStrategy=die" ...]
    //         } else if (option_optional_reference.nr) {
    //                 [... "submodule.alternateLocation=superproject" ...]
    //                 [... "submodule.alternateErrorStrategy=info" ...]
    //         }
    // ```
    //
    // (builtin/clone.c:1141-1180, straight after the banner.) Everything lands on
    // `option_config`, behind the caller's own `-c` pairs, so `write_config()`
    // writes it in that order and the fetch sees it like any other `-c`. The
    // `stickyRecursiveClone` read is the ambient configuration: the new
    // repository's own file holds nothing but what `init_db()` wrote yet.
    if !submodule_pathspecs.is_empty() {
        let mut active = submodule_pathspecs.clone();
        active.sort();
        active.dedup();
        for pathspec in active {
            config_pairs.push(("submodule.active".to_string(), pathspec));
        }
        let sticky = "submodule.stickyRecursiveClone";
        match crate::config::global_config().boolean(sticky) {
            Ok(Some(true)) => config_pairs.push(("submodule.recurse".to_string(), "true".to_string())),
            Err(_) => {
                let value = crate::config::global_config()
                    .string(sticky)
                    .map(|v| v.to_string())
                    .unwrap_or_default();
                // `git_config_bool()` names the key the way the caller spelled it.
                crate::git_fatal!("bad boolean config value '{value}' for '{sticky}'");
            }
            _ => {}
        }
        let strategy = match (required_references.is_empty(), optional_references.is_empty()) {
            (false, false) => crate::git_fatal!(
                "clone --recursive is not compatible with both --reference and --reference-if-able"
            ),
            (false, true) => Some("die"),
            (true, false) => Some("info"),
            (true, true) => None,
        };
        if let Some(strategy) = strategy {
            config_pairs.push(("submodule.alternateLocation".to_string(), "superproject".to_string()));
            config_pairs.push(("submodule.alternateErrorStrategy".to_string(), strategy.to_string()));
        }
    }

    // `init_db()` opens with `real_pathdup(git_dir, 1)`, and `strbuf_realpath()` dies on
    // an empty path; only a bare clone gets here with one, below the banner.
    if dir.is_empty() {
        crate::git_fatal!("The empty string is not a valid path");
    }
    // ```c
    // if (real_git_dir) {
    //         struct stat st;
    //
    //         if (!exist_ok && !stat(git_dir, &st))
    //                 die(_("%s already exists"), git_dir);
    //
    //         if (!exist_ok && !stat(real_git_dir, &st))
    //                 die(_("%s already exists"), real_git_dir);
    // ```
    //
    // (`init_db()`, setup.c:2856-2864.) `git_dir` is the destination itself under
    // `$GIT_WORK_TREE`, so an existing empty destination is refused here; otherwise it is
    // `<dir>/.git`, which a destination that passed the emptiness test never holds.
    if let Some(real) = separate_git_dir.as_deref() {
        if env_work_tree.is_some() && !created_destination {
            crate::git_fatal!("{dir} already exists");
        }
        if std::fs::metadata(real).is_ok() {
            crate::git_fatal!("{real} already exists");
        }
    }

    // `init_db()` (builtin/clone.c:1188) → `repository_format_configure()` (setup.c:2765-2838):
    // the ref storage format is `--ref-format`, else `GIT_DEFAULT_REF_FORMAT` (dying on a name it
    // does not know), else `init.defaultRefFormat` / `feature.experimental`, else `files`. The
    // configured defaults are read here, below the banner, warning about values git does not know.
    let defaults = super::init::read_default_format_config()?;
    let reftable = match ref_format.as_deref() {
        Some(fmt) => fmt == "reftable",
        None => match std::env::var("GIT_DEFAULT_REF_FORMAT").ok().as_deref() {
            Some(env @ ("files" | "reftable")) => env == "reftable",
            Some(env) => crate::git_fatal!("unknown ref storage format '{env}'"),
            None => defaults.ref_format.as_deref() == Some("reftable"),
        },
    };

    // ```c
    // if (!access(mkpath("%s/shallow", path), F_OK)) {
    //         if (reject_shallow)
    //                 die(_("source repository is shallow, reject to clone."));
    // ```
    //
    // (builtin/clone.c:1334-1336.) The test itself ran while the transport was
    // being chosen; the refusal lands here, below the banner, because that is
    // where `cmd_clone` reaches it.
    if reject_shallow_source {
        crate::git_fatal!("source repository is shallow, reject to clone.");
    }

    // `--also-filter-submodules` is meaningless without something to pass down and something to pass
    // it to. Verified against stock git 2.55.0, which reports the missing option and exits 128 — and
    // which raises this only for the explicit flag, never for a `clone.filterSubmodules=true` that
    // happens to be configured.
    //
    // Both refusals come *after* `Cloning into '<dir>'...`: git reaches them from
    // `cmd_clone` below `init_db()`, and the destination it made is taken back by
    // the junk handler on the way out.
    if also_filter_submodules == Some(true) {
        if filter.is_none() {
            crate::git_fatal!("the option '--also-filter-submodules' requires '--filter'");
        }
        if !recurse_submodules {
            crate::git_fatal!("the option '--also-filter-submodules' requires '--recurse-submodules'");
        }
    }

    // `strbuf_realpath()` resolves the requested git directory inside the init
    // step, and a component of it that is not there is fatal. Measured against
    // stock 2.55.0, with the banner already printed and the destination created
    // and then taken away again by the junk handler:
    //
    // ```text
    // $ git clone --separate-git-dir=a/b/gitdir . sgd
    // Cloning into 'sgd'...
    // fatal: Invalid path '<cwd>/a': No such file or directory
    // ```
    //
    // The path named is absolute and is the *first* component that could not be
    // entered, not the argument and not its parent.
    if let Some(real) = separate_git_dir.as_deref() {
        if let Some(missing) = first_unreachable_component(Path::new(real)) {
            crate::git_fatal!("Invalid path '{}': No such file or directory", missing.display());
        }
    }

    // `write_config()` (builtin/clone.c) replays every `-c <key>=<value>` into the
    // new repository through `git_config_parse_parameter()`. A key that cannot be
    // split is an `error()` there and the caller ends the clone:
    //
    // ```c
    // if (git_config_parse_parameter(config->items[i].string,
    //                                write_one_config, NULL) < 0)
    //         die(_("unable to write parameters to config file"));
    // ```
    //
    // It runs after `init_db()`, which is why both lines sit *below* the banner.
    if let Some(msg) = config_pairs.iter().find_map(|(k, _)| config_key_error(k)) {
        eprintln!("error: {msg}");
        crate::git_fatal!("unable to write parameters to config file");
    }

    // `initialize_repository_version()` follows `write_config()` and sets
    // `core.repositoryformatversion` with `git_config_set()`. `init_db()` has
    // already written that key once, so a `-c` that writes it a second time leaves
    // two values in the file and the set cannot pick one:
    //
    // ```text
    // warning: core.repositoryformatversion has multiple values
    // fatal: could not set 'core.repositoryformatversion' to '0'
    // ```
    //
    // The value named is the version the *new* repository would get, not the one
    // `-c` supplied: 1 for a reftable clone, 0 otherwise (a sha256 source is refused
    // earlier, so no other path reaches this line wanting 1).
    if config_pairs
        .iter()
        .any(|(k, _)| k.eq_ignore_ascii_case("core.repositoryformatversion"))
    {
        eprintln!("warning: core.repositoryformatversion has multiple values");
        crate::git_fatal!("could not set 'core.repositoryformatversion' to '{}'", u8::from(reftable));
    }

    // `transport_get(remote, path ? path : remote->url.v[0])` (builtin/clone.c:1345)
    // tries a local regular-file source as a bundle before it picks a transport:
    //
    // ```c
    // } else if (url_is_local_not_ssh(url) && is_file(url) && is_bundle(url, 1)) {
    // ```
    //
    // (transport.c:1258.) `is_bundle()` reports a file without a bundle header right
    // there, so that `error:` precedes everything the transport does later — the
    // depth option, `transport_check_allowed("file")` in `git_connect()`, and the
    // `upload-pack` child that file falls through to.
    let not_a_bundle = bundle_source
        .as_ref()
        .filter(|path| !looks_like_bundle(path))
        .map(|path| absolute_pathdup(path));
    if let Some(shown) = &not_a_bundle {
        eprintln!("error: '{shown}' does not look like a v2 or v3 bundle file");
    }

    // `cmd_clone`'s local-clone block: when the source is a *path* on this
    // machine, git copies the object store instead of running the transport, so
    // every shallow selector is ignored — with a warning naming the `file://`
    // spelling that does go through the transport. Erroring instead (the server
    // "lacks" the `shallow` capability because there is no server) turns a
    // working `git clone --depth 1 ../repo` into a failure.
    // ```c
    // is_local = option_local != 0 && path && !is_bundle;
    // if (is_local) {
    //         if (option_depth)
    //                 warning(_("--depth is ignored in local clones; use file:// instead."));
    // ```
    //
    // (builtin/clone.c:1324-1331.) `--no-local` sets `option_local` to 0, so the clone runs
    // the transport after all and the selectors are honoured rather than warned about.
    // They are `warning()`s, which `-q` does not silence.
    if is_local {
        for (given, flag) in [
            (shallow_depth_given, "--depth"),
            (shallow_since_given, "--shallow-since"),
            (shallow_exclude_given, "--shallow-exclude"),
            (filter.is_some(), "--filter"),
        ] {
            if given {
                eprintln!("warning: {flag} is ignored in local clones; use file:// instead.");
            }
        }
    }

    // `transport_set_option(transport, TRANS_OPT_DEPTH, option_depth)`, after the
    // banner and the local-clone warnings and before `git_connect()`.
    if let Some(v) = depth_junk.as_deref() {
        eprintln!("fatal: transport: invalid depth option '{v}'");
        return Ok(ExitCode::from(128));
    }

    // git's `transport_check_allowed()`, reached from `git_connect()`
    // (`connect.c:1480`) right after the banner and before a single byte moves.
    // A transport the policy forbids is fatal here, which is what stops a
    // submodule URL from choosing the transport (CVE-2022-39253).
    if let Err(e) = check_transport_allowed(&url, &config_pairs) {
        eprintln!("fatal: {e}");
        return Ok(ExitCode::from(128));
    }

    // `get_repo_path_1()` reads a local source as a repository first and as a bundle second, so a
    // path that is neither — a plain file, a loose pack — reaches the transport as a bundle
    // candidate. After the bundle check that rejected it (above), git prints the `die()` of the
    // `upload-pack` child it fell back to — `enter_repo()` settles on the plain file and its
    // `read_gitfile()` refuses it, `invalid gitfile format` for a file that is not a gitfile —
    // and the block `git_connect()` ends with. Exit 128.
    let bundle_transport = match bundle_source {
        None => None,
        Some(path) => {
            let shown = absolute_pathdup(&path);
            if not_a_bundle.is_some() {
                junk.remove_now();
                let refusal = crate::transport_err::enter_repo_refusal(&shown, None)
                    .unwrap_or_else(|| format!("'{shown}' does not appear to be a git repository"));
                return Ok(crate::transport_err::enter_repo_fatal(&refusal));
            }
            // ```c
            // if (is_bundle) {
            //         struct bundle_header header = BUNDLE_HEADER_INIT;
            //         int fd = read_bundle_header(path, &header);
            //         int has_filter = header.filter.choice != LOFC_DISABLED;
            //         [...]
            //         if (has_filter)
            //                 die(_("cannot clone from filtered bundle"));
            // }
            // ```
            //
            // (builtin/clone.c:1350-1360, v2.55.0.) A filtered bundle's pack is
            // missing the objects its filter dropped; a clone has no promisor
            // remote to fetch them back from, so git refuses before the transport
            // runs at all.
            let header = match super::bundle::open_bundle(&path.to_string_lossy()) {
                Ok((header, _)) => header,
                Err(e) => return super::bundle::report(&shown, e),
            };
            if header.filter.is_some() {
                junk.remove_now();
                return Ok(fatal("cannot clone from filtered bundle"));
            }
            Some(BundleTransport::new(path, shown, &git_dir)?)
        }
    };

    // ```c
    // die_for_incompatible_opt2(!!option_rev, "--revision",
    //                           !!option_branch, "--branch");
    // die_for_incompatible_opt2(!!option_rev, "--revision",
    //                           option_mirror, "--mirror");
    // [...]
    // if (option_rev) {
    //         option_tags = 0;
    //         option_single_branch = 0;
    //         opts.wants_head = 0;
    //         opts.detach = 1;
    //
    //         refspec_append(&remote->fetch, option_rev);
    // }
    // ```
    //
    // (builtin/clone.c:1364-1367, 1405-1412.) Both refusals and the refspec parse come after
    // the banner, so the half-made clone is taken down with them. `option_tags = 0` lands
    // after `remote.<name>.tagOpt` was written (clone.c:1308-1312), so it only stops this
    // fetch following tags, which the gitoxide side applies.
    let revision_spec = match revision.as_deref() {
        None => None,
        Some(rev) => {
            if branch.is_some() {
                crate::git_fatal!("options '--revision' and '--branch' cannot be used together");
            }
            if mirror {
                crate::git_fatal!("options '--revision' and '--mirror' cannot be used together");
            }
            match gix::refspec::parse(rev.into(), gix::refspec::parse::Operation::Fetch) {
                Ok(spec) => Some(spec.to_owned()),
                Err(_) => crate::git_fatal!("invalid refspec '{rev}'"),
            }
        }
    };

    // `transport_get()` hands a bundle a vtable whose `get_refs_list` is the
    // bundle's own header and whose `fetch_refs` is `unbundle()` — the pack is
    // *installed*, not negotiated (transport.c:151-216, v2.55.0). gitoxide has no
    // such transport, so the bundle is materialized into a scratch repository
    // inside the new git dir and the clone runs against that: the header's refs
    // become the advertisement, and `unbundle()`'s `index-pack --fix-thin --stdin`
    // writes the very pack the bundle carries, which the local-object adoption
    // below then moves across byte for byte — which is what stock leaves behind
    // (`pack-<hash>.pack` in the clone is `sha1`-identical to the bundle's pack).
    //
    // Everything downstream — the refspec mapping, `guess_remote_head()`, the
    // `branch.<name>` section, the checkout — is the ordinary clone path, exactly
    // as it is in git, where only the three vtable entries differ.
    let (url, url_str) = match &bundle_transport {
        Some(bt) => (bt.url.clone(), bt.dir_str.as_str()),
        None => (url, url_str),
    };

    // Build the clone platform. This already lays the repository down on disk, so
    // the template and alternates below land in the very git dir the fetch will
    // populate — the window git uses for the same work between `init_db()` and
    // the first fetch.
    let mut prepare = if bare {
        gix::prepare_clone_bare(url.clone(), dst)?
    } else if env_work_tree.is_some() {
        // `init_db(git_dir = dir, ..., work_tree)` (builtin/clone.c:1188-1191): `set_git_work_tree()`
        // keeps the work tree's real path, and `create_default_files()` records it as
        // `core.worktree` unless `needs_work_tree_config()` finds the git dir is its `.git`
        // (setup.c:2434-2442, :2640-2641).
        let original_git_dir = super::init::real_pathdup(&dir)?;
        let real_work_tree = super::init::real_pathdup(work_tree)?;
        let needs_work_tree_config = !(real_work_tree == "/" && original_git_dir == "/.git")
            && original_git_dir.strip_prefix(real_work_tree.as_str()) != Some("/.git");
        gix::prepare_clone_with_separate_work_tree(
            url.clone(),
            dst,
            &real_work_tree,
            needs_work_tree_config.then_some(real_work_tree.as_str()),
        )?
    } else {
        gix::prepare_clone(url.clone(), dst)?
    };

    // `git clone` runs `init_db()` with whatever `--template` resolved to
    // (builtin/clone.c), so every one of the three template-provided payloads —
    // `description`, `info/exclude` and the fourteen `hooks/*.sample` — comes
    // from the template and from nothing else. That is true of the *default*
    // template too, which is the case this block used to leave alone.
    //
    // Leaving it alone was not neutral. `prepare_clone*()` above has already laid
    // the repository down, and gitoxide seeds it with a built-in payload of its
    // own (`gix/src/create.rs`, which `include_bytes!`es `gix/src/assets/init/`):
    // a different `description`, a different `info/exclude`, fourteen differently
    // worded hook samples, and — because it writes them as ordinary files — mode
    // `0644` where git's `templates/Makefile` installs `0755`. So a plain
    // `git clone` diverged from stock on eighteen files and their permission
    // bits, none of which any flag had asked for.
    //
    // The strip is spelled `apply_template("")` because that is exactly its
    // stripping half: `copy_templates()` returns on `if (!template_dir[0])`
    // without copying or warning, leaving only the removal of gitoxide's payload
    // behind. An *empty* `--template=` is therefore still a `--template` and ends
    // right there, with no `hooks/`, no `info/` and no `description` — which is
    // what stock leaves too, and is not the same thing as omitting the flag.
    //
    // The destination is the git directory rather than `repo_get_common_dir()`:
    // a clone always creates the repository it is filling in, so the two are the
    // same path here, unlike `git init` inside a linked worktree.
    //
    // `init_db()` resolves the template with `get_template_dir()`, so
    // `GIT_TEMPLATE_DIR` and `init.templateDir` reach a clone as they reach `git
    // init`, and a template's `config` is where the new repository's configuration
    // starts.
    let template = super::init::get_template_dir(template);
    match template.as_deref() {
        Some(tpl) => {
            super::init::apply_template(tpl, &git_dir)?;
            super::init::adopt_template_config(tpl, &git_dir)?;
        }
        None => {
            super::init::apply_template("", &git_dir)?;
            super::init::copy_default_template(&git_dir)?;
        }
    }
    super::init::drop_defaults_set_elsewhere(&git_dir, template.as_deref())?;
    // `init_db()` copies the template before the clone reads any of the configuration, so what the template's
    // `config` sets is in force for every ref the clone writes: `core.logAllRefUpdates = false` writes no reflog
    // (refs/files-backend.c `files_log_ref_write()`). gitoxide opened the repository before it was there.
    prepare.reload_repository()?;

    // `run_transaction_hook()` (refs.c:2663-2678) runs for every ref transaction of the new
    // repository, the template's `reference-transaction` hook included. Commands that find their
    // repository through setup have it installed there; a clone makes its own.
    gix::refs::file::set_transaction_hook(gix::refs::file::TransactionHook {
        run: crate::hooks::run_reference_transaction,
        die: crate::hooks::reference_transaction_die,
    });

    // `refs_create_refdir_stubs()` (refs.c:2202-2223), which `cmd_clone()` runs so the half-made
    // repository can be discovered before the remote's object format is known
    // (builtin/clone.c:1219-1234): `HEAD` names `refs/heads/.invalid`. `create_reference_database()`
    // then finds `HEAD` there and leaves it (setup.c:2541), so until `update_head()` it resolves to
    // nothing, and the reflog entry of the transaction that links it records the null id as its old
    // value.
    std::fs::write(git_dir.join("HEAD"), "ref: refs/heads/.invalid\n")?;

    // `-s`/`--shared` and `--reference`/`--reference-if-able`: record the object
    // stores to borrow from in `objects/info/alternates` *before* the fetch, which
    // is where git writes them. `--dissociate` records them too — it borrows for the
    // duration of the clone and only afterwards repacks and drops the file
    // (`dissociate_from_references()`, builtin/clone.c:845-861).
    //
    // The order is git's: `setup_reference()` (builtin/clone.c:182-190, called at
    // :1314-1315) adds every `--reference` and then every `--reference-if-able`,
    // and the `-s` line comes last, from `clone_local()` (:380-402), which runs
    // after it. Each goes through `odb_add_to_alternates_file()`, so a store
    // already listed is not written twice — see [`write_alternates`].
    {
        let mut alternates: Vec<PathBuf> = Vec::new();
        let references = required_references
            .iter()
            .map(|path| (path, false))
            .chain(optional_references.iter().map(|path| (path, true)));
        for (path, if_able) in references {
            match compute_alternate_path(path) {
                Ok(objects) => alternates.push(objects),
                // `--reference-if-able` downgrades only what
                // `compute_alternate_path()` wrote into its `err` buffer. The
                // gitfile refusal is a `die()` from inside `read_gitfile()`, which
                // has no error slot to write into and no caller to consult, so it
                // ends both spellings alike — see [`gitfile_target`].
                Err(e) if if_able && e.downcast_ref::<crate::fatal::Fatal>().is_none() => {
                    eprintln!("info: Could not add alternate for '{path}': {e}");
                }
                Err(e) => crate::git_fatal!("{e}"),
            }
        }
        // git only shares "when the repository to clone is on the local machine",
        // so a URL that names no openable local repository leaves `-s` a no-op.
        //
        // `is_local` is the same gate git uses. `--shared` is one arm of
        // `clone_local()` (builtin/clone.c:380-402), and `clone_local()` is only
        // ever reached through `if (is_local) clone_local(path, git_dir);`
        // (builtin/clone.c:1354-1355) — so every reason `is_local` was taken away
        // takes `--shared` with it. The one that bites is a *shallow* source
        // (clone.c:1334-1340, ported above): borrowing its object store would give
        // this repository objects whose history is cut off without the `shallow`
        // file that says where, so git falls back to the transport, which
        // negotiates the boundary and writes `shallow` here.
        if shared && is_local {
            if let Some(src_repo) = get_repo_path(url_str) {
                alternates.push(PathBuf::from(format!("{}/objects", get_common_dir(&src_repo))));
            }
        }
        if !alternates.is_empty() {
            write_alternates(&git_dir, &alternates)?;
        }
    }

    // `clone_local()` runs *instead of* the transport fetch — git never asks a local
    // source for a pack:
    // ```c
    // if (is_local)
    //         clone_local(path, git_dir);
    // else if (mapped_refs && complete_refs_before_fetch) {
    //         if (transport_fetch_refs(transport, mapped_refs))
    //                 die(_("remote transport reported error"));
    // }
    // ```
    //
    // (builtin/clone.c:1354-1359 — the two are the arms of one `if`.) gitoxide has no
    // local-clone shortcut, so the fetch machinery is still what maps the refs here;
    // what changes is the order. Adopting the source's objects *before* the fetch
    // leaves every commit the refspec wants already present, which is
    // `mark_complete_and_common_ref()`'s `SkipToRefUpdate`
    // (gix-protocol/src/fetch/negotiate.rs): the wants are never sent, the source's
    // `upload-pack` never starts a `pack-objects`, and the refs are updated from the
    // ref map alone.
    //
    // Leaving the source alone is the point on a *promisor* source. Answering a pack
    // request makes `upload-pack` read every object it was asked to send, and the ones
    // a partial clone deliberately left behind are fetched from its promisor remote to
    // answer with — so a local clone that requested a pack grew the *source* by the
    // blobs it was missing, where `git clone . <dst>` leaves both sides untouched.
    //
    // `-s`/`--shared` is `clone_local()`'s other arm and copies nothing: the alternates
    // line written just above already makes the source's objects readable from here, so
    // that spelling skips the pack for the same reason.
    // `fetch_refs_from_bundle()` → `unbundle()`: verify the prerequisites against
    // the repository being filled in, then run `index-pack --fix-thin --stdin`
    // over the pack the header is followed by (bundle.c:606-640, v2.55.0). git
    // does this from inside `transport_fetch_refs()`; here it happens just before
    // the object store is adopted, which is the same window — nothing has been
    // written to the new repository yet either way.
    if let Some(bt) = &bundle_transport {
        if let Some(code) = bt.unbundle(&git_dir)? {
            return Ok(code);
        }
    }

    // A bundle adopts the scratch repository `unbundle()` just filled, not the bundle file
    // that `local_source` still names.
    let adopt_from = bundle_transport.as_ref().map_or(local_source.as_path(), |bt| bt.dir.as_path());
    let adopted_pack_files = ((is_local || is_bundle) && !shared)
        .then(|| {
            adopt_local_objects(adopt_from, &git_dir, hardlinks)
                .map(|()| pack_dir_entries(&git_dir))
        })
        .transpose()?;

    // Apply the parsed options. `--origin` renames the remote (and its tracking
    // refspec), `--branch` selects the ref to fetch and check out,
    // `--depth`/`--shallow-*` set the shallow boundary, and `--reject-shallow` /
    // `-c <key>=<value>` are injected as in-memory config so the fetch and the
    // checkout that follows it see them — git likewise promises `-c` takes effect
    // "before the remote history is fetched or any files checked out".
    if let Some(name) = &origin {
        prepare = prepare
            .with_remote_name(name.as_str())
            // `die(_("'%s' is not a valid remote name"), option_origin)`
            // (builtin/clone.c) — single quotes, and 128.
            .map_err(|_| crate::fatal::die(format!("'{name}' is not a valid remote name")))?;
    }
    if let Some(name) = &branch {
        prepare = prepare
            .with_ref_name(Some(name.as_str()))
            .map_err(|_| anyhow::anyhow!("--branch expects a valid branch name, got {name:?}"))?;
    }
    prepare = prepare.with_shallow(shallow);
    // `repo_default_branch_name()` is what `HEAD` becomes for a remote that advertised none
    // (builtin/clone.c:1554-1586); its advice is given once the refs are in, see
    // [`advise_default_branch`].
    if branch.is_none() && revision_spec.is_none() {
        let default_branch = clone_default_branch_name(&git_dir, &config_pairs, true);
        prepare = prepare.with_default_branch(default_branch);
    }
    prepare = prepare.with_revision(revision_spec);
    // The refs, `HEAD` and the branch configuration are written below, as `cmd_clone()` writes them
    // once the pack is in: see [`write_clone_refs`].
    prepare = prepare.with_write_refs(false);
    // "ignored in local clones" is literal: git never puts the filter on the wire
    // for a local source, because there is no wire — `clone_local()` copies the
    // object store whole. Sending it anyway reaches a server that does not
    // advertise `filter` and earns a second warning,
    // `warning: filtering not recognized by server, ignoring`, that stock never
    // prints. The warning above already told the user the filter was dropped.
    if !is_local && !is_bundle {
        prepare = prepare.with_filter(filter.clone());
    }

    let mut overrides: Vec<String> = config_pairs
        .iter()
        .map(|(k, v)| format!("{k}={v}"))
        .collect();
    if let Some(reject) = reject_shallow {
        overrides.push(format!("clone.rejectShallow={reject}"));
    }
    // git decides the transport policy once, in transport_check_allowed()
    // (transport.c:1201-1205), which [`check_transport_allowed`] above has already
    // passed. gitoxide's `connect()` asks again with its own reading of
    // `protocol.*.allow`, which neither honours `GIT_ALLOW_PROTOCOL` nor ranks a
    // `git -c` over the clone's own `-c`, so it is handed that answer.
    overrides.push(format!("protocol.{}.allow=always", crate::setup::transport_name(&url)));
    if !overrides.is_empty() {
        prepare = prepare.with_in_memory_config_overrides(overrides);
    }

    // ```c
    // if (option_depth || option_since || option_not.nr)
    //         deepen = 1;
    // if (option_single_branch == -1)
    //         option_single_branch = deepen ? 1 : 0;
    // ```
    //
    // (builtin/clone.c:1025-1028.) git-clone(1) on `--depth`: "Implies `--single-branch`
    // unless `--no-single-branch` is given". The default is settled here, in argument
    // handling, long before git works out whether the source is a local path — so a local
    // clone that ignores the depth for the *transfer* still fetches one branch only.
    let single_branch = single_branch.or_else(|| {
        (shallow_depth_given || shallow_since_given || shallow_exclude_given).then_some(true)
    });
    // ```c
    // if (option_single_branch) {
    //         transport_set_option(transport, TRANS_OPT_FOLLOWTAGS, "1");
    //
    //         if (option_branch)
    //                 opts.wants_head = 0;
    // }
    // ```
    //
    // (builtin/clone.c:1380-1385.) Settled before `--revision` clears `option_single_branch`
    // (:1405-1407), and whatever `option_tags` says: a `--no-tags` or `--mirror` single-branch
    // clone still asks with `include-tag`, and so receives the tags pointing into its pack
    // without writing a ref for them. Without `HEAD` among the wants, a `--branch` clone fetches
    // that branch alone, and the tags it follows are the ones on its history.
    prepare = prepare
        .with_follow_tags(single_branch == Some(true))
        .with_wants_head(!(single_branch == Some(true) && branch.is_some()));

    // Decide which refspec set this clone needs and, with it, which tag mode.
    // Supplying `configure_remote` at all suppresses gitoxide's own clone default
    // of `Tags::All`, so every plan that is not a single-branch clone has to ask
    // for it back explicitly. `Tags::All` is written to `remote.<name>.tagOpt`,
    // which git does not do, so those cases strip the key again afterwards.
    let plan = if revision.is_some() {
        // `--revision` fetches its own refspec and writes none (builtin/clone.c:1319, 1590).
        None
    } else if mirror && single_branch != Some(true) {
        Some(Refspecs::Mirror)
    } else if single_branch == Some(true) {
        // `wanted_peer_refs()` hands `get_fetch_map()` the one ref a single-branch clone is
        // restricted to (builtin/clone.c:451-463), so a `--mirror` one, whose `+refs/*:refs/*`
        // maps that ref onto itself as `--bare` does, fetches it alone like any other; the
        // wildcard is still what `write_refspec_config()` records (:801-835).
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
    // Cloning a local path runs this binary's own `upload-pack` rather than
    // whatever `PATH` resolves the bare name to; see
    // [`super::fetch::local_service_program`]. That makes the option set
    // non-empty for every local clone, so the previous guard — which only
    // applied connect options when the command line carried one — is gone.
    let clone_upload_pack = super::fetch::local_service_program(
        Some(&url),
        upload_pack.as_deref().map(Into::into),
        "upload-pack",
    );
    if clone_upload_pack.is_some() || address_family.is_some() {
        prepare = prepare.with_connect_options(gix::remote::connect::Options {
            upload_pack: clone_upload_pack.clone(),
            address_family,
            // `git clone` never connects for push.
            receive_pack: None,
            // clone records a local source absolute, so there is nothing relative to resolve.
            current_dir: None,
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
    } else if tags == Some(Tags::None) && branch.is_some() && single_branch != Some(true) && revision.is_none() {
        // ```c
        // if (option_tags || option_branch)
        //         /*
        //          * Add tags refspec when user asked for tags (implicitly) or
        //          * specified --branch, whose argument might be a tag.
        //          */
        //         refspec_append(&remote->fetch, TAG_REFSPEC);
        // ```
        //
        // (builtin/clone.c:1414-1419.) `--no-tags --branch` still fetches and maps every tag,
        // after `remote.<name>.tagOpt = --no-tags` was written (:1308-1312): the refspec is
        // the fetch's own and never reaches the configuration, as an extra refspec here.
        prepare = prepare.with_fetch_options(gix::remote::ref_map::Options {
            extra_refspecs: vec![gix::refspec::parse("refs/tags/*:refs/tags/*".into(), gix::refspec::parse::Operation::Fetch)
                .expect("valid")
                .to_owned()],
            ..Default::default()
        });
    }

    if plan.is_some() || fetch_tags.is_some() {
        let plan = plan.clone();
        // ```c
        // path = get_repo_path(repo_name, &is_bundle);
        // if (path) {
        //         FREE_AND_NULL(path);
        //         repo = repo_to_free = absolute_pathdup(repo_name);
        // }
        // ```
        //
        // (`cmd_clone()`, builtin/clone.c:1054-1057.) A local source is recorded
        // absolute, so `remote.<name>.url` keeps working from any directory.
        // `prepare_clone*()` absolutizes its own copy of the url the same way
        // (`PrepareFetch::new_inner`, gix/src/clone/mod.rs:141) — but the remote
        // built below is built from *this* copy, and without the same treatment a
        // `git clone --bare ./peer` wrote `./peer` into the new repository's
        // config and its first `git fetch` from anywhere else failed.
        let url_for_remote = {
            let mut url = url.clone();
            url.absolutize(Path::new(""));
            url
        };
        let explicit_origin = origin.clone();
        let single_outcome = single_outcome.clone();
        let probe_connect = gix::remote::connect::Options {
            upload_pack: clone_upload_pack.clone(),
            address_family,
            // `git clone` never connects for push.
            receive_pack: None,
            current_dir: None,
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

    // ```c
    // remote = remote_get_early(remote_name);
    //
    // if (!option_rev)
    //         refspec_appendf(&remote->fetch, "+%s*:%s*", src_ref_prefix,
    //                         branch_top.buf);
    // ```
    //
    // (builtin/clone.c:1316-1321.) The configuration's `remote.<name>.fetch` refspecs come first, and
    // `wanted_peer_refs()` maps the refs through each of them in turn (:465-466), so a `-c`, template, global
    // or `git -c` refspec writes the refs it maps, and one that maps a ref the clone's own refspec maps too
    // makes `write_remote_refs()` queue it twice. `write_refspec_config()` records only the clone's own
    // refspec (:801-835), so these never reach the remote section gitoxide writes.
    {
        let explicit_origin = origin.clone();
        let single = matches!(plan, Some(Refspecs::Single { .. }));
        let single_outcome = single_outcome.clone();
        prepare = prepare.configure_remote_unsaved(move |mut remote| {
            let remote_name = resolve_remote_name(remote.repo, explicit_origin.as_deref());
            let mut specs = configured_fetch_refspecs(remote.repo, &remote_name)?;
            if single {
                let fetched = single_outcome
                    .lock()
                    .expect("clone is single-threaded here")
                    .as_ref()
                    .and_then(|probe| probe.fetched.clone());
                let mut restricted = Vec::new();
                for spec in &specs {
                    restricted.extend(restrict_to_single_ref(spec, fetched.as_deref())?);
                }
                specs = restricted;
            }
            if specs.is_empty() {
                return Ok(remote);
            }
            let all: Vec<gix::bstr::BString> = specs
                .iter()
                .chain(remote.refspecs(gix::remote::Direction::Fetch))
                .map(|spec| spec.to_ref().to_bstring())
                .collect();
            remote.replace_refspecs(all.iter().map(|spec| spec.as_bstr()), gix::remote::Direction::Fetch)?;
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

    // The pack is reported as `fetch` reports it (`fetch_progress`): the server's
    // `remote: ` lines and `index-pack`'s meters, with `transport->progress` from
    // `transport_set_verbosity(transport, option_verbosity, option_progress)`
    // (builtin/clone.c:1353). A local clone transfers nothing: `clone_local()`
    // copies or links the object store and the only transport call is the ls-refs
    // that built the ref map, so git has no progress to report however loudly
    // `--progress` asks for it. This port runs a real fetch underneath, which asks
    // the server for no progress and draws nothing.
    let transport_progress =
        !is_local && super::fetch_progress::transport_progress(quiet, force_progress);
    let meters = super::fetch_progress::Meters::new(super::fetch_progress::Plan {
        progress: transport_progress,
        quiet,
        // `transport_set_option(transport, TRANS_OPT_KEEP, "yes")` (builtin/clone.c:1362):
        // every pack goes to `index-pack`, so the unpack limit is never consulted.
        keep_pack: true,
        unpack_limit: 0,
        index_pack_required: false,
    });
    let remote_output = match gix::open(&git_dir) {
        Ok(repo) => super::fetch_progress::RemoteOutput::new(&repo),
        Err(_) => super::fetch_progress::RemoteOutput::plain(),
    };
    {
        let sideband = (!is_local).then(|| remote_output.handler());
        prepare = prepare.configure_connection(move |connection| {
            connection.set_transfer_progress(!transport_progress, sideband.clone());
            // `transport_set_verbosity(transport, option_verbosity, option_progress)` (builtin/clone.c:1346)
            // reaches remote-curl as its `verbosity` option. A configuration error is left to the
            // connection's own read of these options, which reports it.
            use gix::protocol::transport::client::TransportWithoutIO;
            let url = connection.transport_mut().to_url().into_owned();
            let repo = connection.remote().repo();
            let name = connection.remote().name().map(|n| n.as_bstr().to_owned());
            if let Ok(Some(options)) = repo.transport_options(
                gix::bstr::ByteSlice::as_bstr(url.as_slice()),
                name.as_ref().map(|n| gix::bstr::ByteSlice::as_bstr(n.as_slice())),
            ) {
                connection.set_transport_options(super::fetch_progress::with_helper_verbosity(options, verbosity));
            }
            Ok(())
        });
    }

    // Whether the server accepted `--filter`. git keeps going when it did not, warning that the
    // filter was ignored, so the warning is emitted from what the handshake advertised.
    let mut filter_supported = true;

    // ```c
    // if (!mapped_refs) {
    //         warning(_("You appear to have cloned an empty repository."));
    //         option_no_checkout = 1;
    // }
    // ```
    //
    // (builtin/clone.c:1560-1563.) `mapped_refs` is what the clone's refspec selected, so
    // this covers both an empty remote and one whose `HEAD` names a branch that is not
    // there — a `--single-branch` clone follows that `HEAD`, and its refspec then matches
    // nothing. Either way the clone succeeds and checks nothing out.
    let mut cloned_empty = false;
    // git's `refs` for a fetch gitoxide refused for mapping nothing — whether
    // `transport_get_remote_refs()` returned anything at all. `process_ref_v2()` hands an
    // `unborn HEAD` line to `unborn_head_target` and adds no ref for it (connect.c:417-435), so a
    // remote with no commits yields `refs == NULL`, and `update_remote_refs()` then skips
    // `write_remote_refs()` (builtin/clone.c:554-558).
    let mut remote_advertised_refs = false;
    // Set when gitoxide refused the fetch for mapping nothing, which leaves the refs to be written
    // after it.
    let mut fetch_mapped_nothing = false;
    // ```c
    //         copy_or_link_directory(&src, &dest, src_repo);
    //         [...]
    // }
    //
    // if (0 <= option_verbosity)
    //         fprintf(stderr, _("done.\n"));
    // ```
    //
    // (`clone_local()`, builtin/clone.c:365-371.) The line belongs to the local-clone
    // routine, not to `cmd_clone`: a clone that ran the transport prints whatever the
    // progress meter printed and nothing more, so `--no-local` ends after the banner.
    // `cmd_clone` calls it once the refs are mapped and the empty-repository warning
    // is out, and before `update_head()`, `--sparse` and `checkout()`
    // (builtin/clone.c:1559-1598, :1609-1631), so it precedes every meter and
    // warning those draw.
    let say_done = || {
        if is_local && !quiet {
            eprintln!("done.");
        }
    };
    // `warning()` is not gated on `option_verbosity`, so `-q` prints it too.
    let warn_empty = || eprintln!("warning: You appear to have cloned an empty repository.");

    // git names the remote in the diagnostic a missing `--branch` dies with, and the name it uses
    // is the one this clone is about to write: `-o`/`--origin`, else `clone.defaultRemoteName`,
    // else `origin`.
    let effective_remote_name = gix::open(&git_dir)
        .map(|repo| resolve_remote_name(&repo, origin.as_deref()))
        .unwrap_or_else(|_| origin.clone().unwrap_or_else(|| "origin".to_string()));

    // `index-pack`'s `Checking objects` meter. The child is `--strict` when
    // `check_self_contained_and_connected` is set — a clone that neither deepens
    // nor filters (builtin/clone.c:1402-1403, builtin/index-pack.c:1939-1941) —
    // or when `transfer.fsckObjects` asks for `--strict` on a non-promisor pack
    // (fetch-pack.c:1053-1063); a filtered clone is a promisor's.
    let deepen = shallow_depth_given || shallow_since_given || shallow_exclude_given;
    // A child that dies there leaves `fetch-pack` without the `keep` line it
    // reads back (`index_pack_lockfile()`, fetch-pack.c:1085-1090).
    let check_pack = |repo: &gix::Repository, outcome: &gix::remote::fetch::Outcome| -> Result<()> {
        let gix::remote::fetch::Status::Change { write_pack_bundle, .. } = &outcome.status else {
            return Ok(());
        };
        let strict = filter.is_none() && (!deepen || super::fetch::fetch_pack_fsck_objects(repo));
        if strict {
            let grafts = super::fetch_progress::shallow_grafts(repo);
            let verbose = !quiet && transport_progress;
            if let Err(e) = super::fetch_progress::check_objects(repo, write_pack_bundle, &grafts, verbose) {
                eprintln!("fatal: {e}");
                return Err(crate::fatal::die("fetch-pack: invalid index-pack output"));
            }
        }
        Ok(())
    };

    // Set when `git_sparse_checkout_init()` failed ahead of the checkout.
    let mut sparse_init_failed = false;
    // Set when the `$GIT_WORK_TREE` work tree was never made, so `checkout()`'s
    // `setup_work_tree()` cannot enter it.
    let mut work_tree_missing = false;
    // Set once `checkout()` has run `setup_work_tree()`, which leaves the process in the work
    // tree with `GIT_WORK_TREE=.` (setup.c:505-510) for every child that follows.
    let mut work_tree_entered = false;
    // Under `$GIT_WORK_TREE` a child does not find the repository from where it runs, so it
    // needs the `GIT_DIR` that `init_db()`'s `apply_and_export_relative_gitdir()` exported
    // (setup.c:1088, :2866-2868) and, past `setup_work_tree()`, the work tree to run in.
    let child_setup = |git_dir: &Path, entered: bool| -> Option<ChildSetup> {
        env_work_tree.as_ref()?;
        let git_dir = std::fs::canonicalize(git_dir).unwrap_or_else(|_| git_dir.to_path_buf());
        let work_tree = entered.then(|| super::init::real_pathdup(work_tree).ok()).flatten();
        Some(ChildSetup { git_dir, work_tree })
    };
    // Run the clone, capturing the result so a `remote: ` line the stream left
    // unterminated is flushed before any error is propagated.
    // `reflog_msg`: `clone: from <repo>`, where `repo` is `absolute_pathdup(repo_name)` for a path,
    // a bundle's included, and the url as given otherwise (builtin/clone.c:1054-1060, :1087-1088).
    let reflog_msg = match &bundle_transport {
        Some(bt) => format!("clone: from {}", bt.shown),
        None => {
            let mut shown = url.clone();
            shown.absolutize(Path::new(""));
            format!("clone: from {}", shown.to_bstring())
        }
    };
    // `opts.wants_head`, cleared by `--revision` and by `--single-branch --branch`
    // (builtin/clone.c:1380-1385, :1405-1409).
    let wants_head = revision.is_none() && !(single_branch == Some(true) && branch.is_some());
    // `option_tags` once `--mirror` and `--revision` cleared it (builtin/clone.c:1036-1038, :1406).
    let option_tags = tags != Some(Tags::None) && !mirror && revision.is_none();
    // `repo_default_branch_name(the_repository, REMOTE_GUESS_HEAD_QUIET)`, which `guess_remote_head()` asks.
    let quiet_default_branch =
        clone_default_branch_name(&git_dir, &config_pairs, true).unwrap_or_else(|| "master".to_string());
    // What `update_remote_refs()` and `update_head()` are handed. A single-branch fetch whose refspec
    // came out empty sent ls-refs no `refs/heads/` prefix, so what the remote had under it — whether
    // `refs` is NULL — is known only from the probe.
    let ref_writes = |advertised: bool| RefWrites {
        remote_name: &effective_remote_name,
        bare,
        mirror,
        single_branch: single_branch == Some(true),
        tags: option_tags,
        branch: branch.as_deref(),
        revision: revision.is_some(),
        msg: &reflog_msg,
        refs_advertised: advertised
            || single_outcome.lock().expect("clone is single-threaded here").as_ref().is_some_and(|probe| {
                probe.advertised_head_or_branch || (probe.advertised_tag && tags != Some(Tags::None))
            }),
        default_branch: &quiet_default_branch,
    };
    // `update_remote_refs()` and `update_head()` once the pack is in (builtin/clone.c:1604-1608), into a
    // reftable store laid down first for a reftable clone.
    let write_refs = |repo: &mut gix::Repository, outcome: &gix::remote::fetch::Outcome| -> Result<()> {
        if reftable {
            create_reftable_ref_store(repo.git_dir())?;
            reopen_as_reftable(repo);
        }
        let probe = single_outcome.lock().expect("clone is single-threaded here").clone();
        let plan = ref_writes(advertises_refs(&outcome.ref_map));
        let v2 = outcome.handshake.server_protocol_version == gix::protocol::transport::Protocol::V2;
        // The branch `HEAD` is left unborn on: a single-branch clone's probe already asked the
        // remote what its `HEAD` names, and `repo_default_branch_name(the_repository, 0)` comes
        // last, whose advice a clone that mapped refs gave already (see [`advise_default_branch`]).
        let fallback = || {
            probe
                .as_ref()
                .and_then(|probe| probe.head_target.clone())
                .filter(|target| target.starts_with("refs/heads/"))
                .or_else(|| {
                    clone_default_branch_name(&git_dir, &config_pairs, !mapped_refs_empty(&outcome.ref_map, wants_head))
                        .map(|branch| format!("refs/heads/{branch}"))
                })
        };
        // A bundle's refs are advertised in the reverse of the header's order (`get_refs_from_bundle()`
        // prepends, transport.c:163-172), which is the order `get_fetch_map()` maps them in. The
        // scratch repository standing in for the bundle advertises them sorted.
        let bundle_order = bundle_transport.as_ref().and_then(|bt| {
            let (header, _) = super::bundle::open_bundle(&bt.path.to_string_lossy()).ok()?;
            Some(header.refs.iter().rev().map(|(_, name)| name.clone()).collect::<Vec<_>>())
        });
        let reordered;
        let ref_map = match &bundle_order {
            Some(order) => {
                let position = |name: &[u8]| order.iter().position(|n| n.as_slice() == name).unwrap_or(usize::MAX);
                let mut map = outcome.ref_map.clone();
                map.remote_refs.sort_by_key(|r| position(r.unpack().0));
                map.mappings.sort_by_key(|m| m.remote.as_name().map_or(usize::MAX, |name| position(name)));
                reordered = map;
                &reordered
            }
            None => &outcome.ref_map,
        };
        write_clone_refs(&*repo, ref_map, v2, &plan, fallback)
    };
    let result = (|| -> Result<()> {
        // git's `transport.c` warns as soon as it has the capability list and then transfers
        // everything, leaving the promisor configuration in place regardless.
        let mut note_filter_support = |handshake: &gix::protocol::Handshake| {
            filter_supported = gix::protocol::fetch::filter::is_supported(
                handshake.server_protocol_version,
                &handshake.capabilities,
            );
            // ```c
            // if (server_supports("filter")) {
            //         server_supports_filtering = 1;
            //         print_verbose(args, _("Server supports %s"), "filter");
            // } else if (args->filter_options.choice) {
            //         warning("filtering not recognized by server, ignoring");
            // }
            // ```
            //
            // (fetch-pack.c:1173-1178, and the v2 spelling at 305-313.) The warning belongs
            // to `fetch-pack`, which a local clone never reaches — `clone_local()` runs in
            // place of `transport_fetch_refs()` — so a local source that would not have
            // understood the filter says nothing about it. The `--filter is ignored in local
            // clones` warning at the banner has already covered that case, and the filter was
            // never put on the wire here either (see `with_filter` above).
            if filter.is_some() && !filter_supported && !is_local && !is_bundle {
                eprintln!("warning: filtering not recognized by server, ignoring");
            }
        };
        if bare || no_checkout {
            // A bare clone never checks out a worktree; `--no-checkout` likewise
            // fetches the pack and writes the refs/`HEAD` but leaves the worktree
            // (and index) empty. Fetching is the whole job in both cases.
            let fetched = prepare.fetch_only(meters.clone(), &should_interrupt);
            let (mut repo, outcome) = match fetched {
                Ok((repo, outcome)) => {
                    check_exact_refspecs_mapped(&outcome.ref_map)?;
                    check_pack(&repo, &outcome)?;
                    (repo, outcome)
                }
                // gitoxide refuses a fetch whose refspecs matched nothing; git calls that an
                // empty clone and carries on, so the refusal is turned back into one. The
                // repository has to be kept explicitly: `PrepareFetch` deletes the
                // destination it created when it is dropped after a failed fetch.
                Err(gix::clone::fetch::Error::Fetch(
                    gix::remote::fetch::Error::NoMapping { num_remote_refs, .. },
                )) if branch.is_none() => {
                    cloned_empty = true;
                    fetch_mapped_nothing = true;
                    remote_advertised_refs = num_remote_refs > 0;
                    warn_empty();
                    say_done();
                    // An empty clone is a *successful* clone in git — `checkout()` runs and
                    // returns, `junk_mode` moves on, and the repository stays.
                    junk.leave();
                    prepare.persist();
                    return Ok(());
                }
                Err(gix::clone::fetch::Error::Fetch(gix::remote::fetch::Error::PackStream(stream))) => {
                    return Err(pack_stream_died(&git_dir, url_str, filter.is_some(), &stream));
                }
                Err(e) if conflicting_destination(&e).is_some() => {
                    say_done();
                    let name = conflicting_destination(&e).expect("checked");
                    return Err(crate::fatal::die(format!("multiple updates for ref '{name}' not allowed")));
                }
                Err(e) => {
                    return Err(short_pack(e, branch.as_deref(), &effective_remote_name));
                }
            };
            if mapped_refs_empty(&outcome.ref_map, wants_head) {
                cloned_empty = true;
                warn_empty();
            } else if branch.is_none() && revision.is_none() {
                advise_default_branch(&git_dir, &config_pairs, &outcome.ref_map);
            }
            say_done();
            note_filter_support(&outcome.handshake);
            write_refs(&mut repo, &outcome)?;
            // `checkout()` is still called for a bare or `--no-checkout` clone; it returns
            // before touching the worktree, but `junk_mode` has already moved on.
            junk.leave();
        } else {
            // `git clone 'url'...`
            let fetched = prepare.fetch_then_checkout(meters.clone(), &should_interrupt);
            let (mut checkout, outcome) = match fetched {
                Ok((checkout, outcome)) => {
                    check_exact_refspecs_mapped(&outcome.ref_map)?;
                    check_pack(checkout.repo(), &outcome)?;
                    (checkout, outcome)
                }
                Err(gix::clone::fetch::Error::Fetch(
                    gix::remote::fetch::Error::NoMapping { num_remote_refs, .. },
                )) if branch.is_none() => {
                    cloned_empty = true;
                    fetch_mapped_nothing = true;
                    remote_advertised_refs = num_remote_refs > 0;
                    warn_empty();
                    say_done();
                    // An empty clone is a *successful* clone in git — `checkout()` runs and
                    // returns, `junk_mode` moves on, and the repository stays.
                    junk.leave();
                    prepare.persist();
                    return Ok(());
                }
                Err(gix::clone::fetch::Error::Fetch(gix::remote::fetch::Error::PackStream(stream))) => {
                    return Err(pack_stream_died(&git_dir, url_str, filter.is_some(), &stream));
                }
                Err(e) if conflicting_destination(&e).is_some() => {
                    say_done();
                    let name = conflicting_destination(&e).expect("checked");
                    return Err(crate::fatal::die(format!("multiple updates for ref '{name}' not allowed")));
                }
                Err(e) => {
                    return Err(short_pack(e, branch.as_deref(), &effective_remote_name));
                }
            };
            if mapped_refs_empty(&outcome.ref_map, wants_head) {
                cloned_empty = true;
                warn_empty();
            } else if branch.is_none() && revision.is_none() {
                advise_default_branch(&git_dir, &config_pairs, &outcome.ref_map);
            }
            say_done();
            note_filter_support(&outcome.handshake);
            write_refs(checkout.repo_mut(), &outcome)?;
            // `--sparse` is set up before anything is checked out:
            //
            // ```c
            // if (option_sparse_checkout && git_sparse_checkout_init(dir))
            //         return 1;
            //
            // junk_mode = JUNK_LEAVE_REPO;
            // err = checkout(submodule_progress, ...);
            // ```
            //
            // (builtin/clone.c:1626-1631.) `git_sparse_checkout_init()` runs `sparse-checkout
            // set` as a child and sets `cfg->apply_sparse_checkout = 1` in this process
            // (builtin/clone.c:615-635), so `checkout()`'s `unpack_trees()` reads the pattern
            // file the child wrote and marks everything outside it `SKIP_WORKTREE` before
            // `check_updates()` counts what to write. The child's own config writes are not
            // re-read: `core_sparse_checkout_cone` keeps the value it had when the clone
            // started, so the patterns are matched in that dialect (dir.c:3513). A failure
            // returns before `junk_mode` moves on, so the whole clone goes.
            let sparse_patterns = if sparse {
                let cone = checkout
                    .repo()
                    .config_snapshot()
                    .boolean("core.sparseCheckoutCone")
                    .unwrap_or(false);
                if run_self(&dir, &["sparse-checkout", "set", "--cone"], true, child_setup(&git_dir, false).as_ref())? != 0 {
                    sparse_init_failed = true;
                    return Ok(());
                }
                super::sparse_checkout::UnpackPatterns::load(checkout.repo(), cone)
            } else {
                None
            };
            // `junk_mode = JUNK_LEAVE_REPO;` (builtin/clone.c:1629), immediately before
            // `checkout()`: from here on a failure keeps the repository and only warns.
            junk.leave();
            // ```c
            // if (option_no_checkout)
            //         return 0;
            // ```
            //
            // (`checkout()`, builtin/clone.c:652-653.) An empty clone set
            // `option_no_checkout` next to its warning (builtin/clone.c:1560-1563), so
            // nothing is unpacked and HEAD is never resolved.
            if cloned_empty {
                checkout.persist();
                return Ok(());
            }
            // ```c
            // /* We need to be in the new work tree for the checkout */
            // setup_work_tree(the_repository);
            // ```
            //
            // (`checkout()`, builtin/clone.c:671-672.) `setup_work_tree()` dies when it cannot
            // `chdir()` into the work tree (setup.c:494-503), which is what a `$GIT_WORK_TREE`
            // left unmade beside an existing destination comes to. Nothing is unpacked; the
            // death itself comes once `HEAD` is resolved, below.
            if env_work_tree.is_some() && !Path::new(work_tree).is_dir() {
                work_tree_missing = true;
                checkout.persist();
                return Ok(());
            }
            // Check out the branch `HEAD` points to.
            // `checkout()` unpacks `HEAD`'s tree with `opts.verbose_update =
            // (option_verbosity >= 0)` (builtin/clone.c:682), after returning early for an
            // unborn `HEAD`, so `prepare` only runs for a tree that is checked out.
            let mut updating = None;
            let mut prepare = |index: &mut gix::index::File| {
                if let Some(pl) = &sparse_patterns {
                    // Sparse checkout loop #1 over `o->src_index`, which a clone has not
                    // populated, then loop #2 over the result, where every entry is
                    // `CE_ADDED` (unpack-trees.c:1974-1976, :2035-2042). Each draws its own
                    // `Updating index flags` meter.
                    let mut src = gix::index::File::from_state(
                        gix::index::State::new(index.object_hash()),
                        index.path().to_owned(),
                    );
                    pl.mark_new_skip_worktree(&mut src, &|_| true, !quiet)
                        .unwrap_or_else(|e| checkout_died(e));
                    pl.mark_new_skip_worktree(index, &|_| true, !quiet)
                        .unwrap_or_else(|e| checkout_died(e));
                }
                // `get_progress()` counts only the entries still `CE_UPDATE`:
                // `apply_sparse_checkout()` took that bit off every skipped one
                // (unpack-trees.c:361-377, :523-560).
                let total = index
                    .entries()
                    .iter()
                    .filter(|e| !e.flags.contains(gix::index::entry::Flags::SKIP_WORKTREE))
                    .count();
                let meter = crate::worktree::UpdatingFiles::start(total, !quiet).unwrap_or_else(|e| checkout_died(e));
                let hook = meter.hook();
                updating = Some(meter);
                hook
            };
            let (repo, _) =
                checkout.main_worktree_with_index_hook(gix::progress::Discard, &should_interrupt, &mut prepare)?;
            work_tree_entered = true;
            if let Some(updating) = updating {
                updating.stop();
            }
            // `checkout()` (builtin/clone.c:677-698) is a `oneway_merge` `unpack_trees()`, which
            // ends with `cache_tree_update(..., WRITE_TREE_SILENT | WRITE_TREE_REPAIR)`
            // (unpack-trees.c:2086-2090) before `write_locked_index()` — so a fresh clone's
            // index carries a `TREE` extension. gix's checkout writes the entries only.
            if let Ok(mut index) = repo.open_index() {
                super::write_tree::rebuild_cache_tree(&repo, &mut index);
                crate::index_racy::write(&repo, &mut index)?;
            }
        }
        Ok(())
    })();

    remote_output.finish();
    // A transport that never connected is git's own `die()`, not a wrapped error:
    // the ssh child's stderr, then the fixed block, exit 128.
    if let Err(e) = &result {
        if let Some(code) = crate::transport_err::ssh_fatal(url_str, e) {
            return Ok(code);
        }
        if let Some(code) = crate::transport_err::hang_up_fatal(e) {
            return Ok(code);
        }
        // A `file://` source `get_repo_path()` did not claim reaches `git_connect()`,
        // whose `upload-pack` child dies in `enter_repo()`; the clone then dies in
        // `die_initial_contact()` and `remove_junk()` takes the new directory down.
        if let Some(code) = crate::transport_err::file_url_fatal_in(e) {
            return Ok(code);
        }
        // `discover_refs()` refused, or a fetch response cut off where its next section belongs.
        if let Some(code) = crate::transport_err::http_get_fatal(url_str, None, e.as_ref())
            .or_else(|| crate::transport_err::expected_section_fatal(e))
        {
            return Ok(code);
        }
    }
    result?;
    // `git_sparse_checkout_init()` failed ahead of `checkout()`: `error()`, then
    // `return 1` with `junk_mode` still unset, so the clone is removed.
    if sparse_init_failed {
        eprintln!("error: failed to initialize sparse-checkout");
        junk.remove_now();
        return Ok(ExitCode::from(1));
    }

    // `close_bundle()` (transport.c:219-227): the bundle transport is torn down
    // once the refs are in. The scratch repository it stood on goes with it — the
    // objects have been adopted into the clone by now, so nothing reads it again,
    // and leaving it behind would put a second repository inside `.git`.
    if let Some(bt) = &bundle_transport {
        let _ = std::fs::remove_dir_all(&bt.dir);
    }

    // `--separate-git-dir`: move the git dir to the requested path and leave a
    // `gitdir: <abs>` link file behind, the same relocation `git init` performs.
    // Everything below works on the relocated dir, as git's post-init steps do.
    let git_dir = match &separate_git_dir {
        Some(real) if env_work_tree.is_some() => super::init::relocate_git_dir_to_link(&git_dir, dst, real)?,
        Some(real) => super::init::relocate_git_dir(&git_dir, dst, real)?,
        None => git_dir,
    };

    // gitoxide refuses a fetch whose refspecs matched nothing before any ref is written, so the
    // refs of such a clone are written here: `write_remote_refs()` with no mapped ref when the
    // remote advertised anything (`if (refs)`, builtin/clone.c:554-558), and `update_head()` with
    // neither `our` nor `remote`:
    //
    // ```c
    // } else if (unborn && skip_prefix(unborn, "refs/heads/", &head)) {
    //         /*
    //          * Unborn head from remote; same as "our" case above except
    //          * that we have no ref to update.
    //          */
    //         if (refs_update_symref(get_main_ref_store(the_repository), "HEAD", unborn, NULL) < 0)
    //                 die(_("unable to update HEAD"));
    //         if (!option_bare)
    //                 install_branch_config(0, head, remote_name, unborn);
    // }
    // ```
    //
    // (`update_head()`, builtin/clone.c:603-611.) The clone adopts the remote's idea of the default
    // branch even though no ref was fetched. A single-branch plan already asked the remote what its
    // HEAD names while building its refspec, so that probe supplies the target; a remote that sent
    // no `unborn` line leaves the clone on this machine's default branch (builtin/clone.c:1569-1572).
    if fetch_mapped_nothing {
        if reftable {
            create_reftable_ref_store(&git_dir)?;
        }
        let repo = gix::open(&git_dir)?;
        let plan = ref_writes(remote_advertised_refs);
        update_remote_refs(&repo, &[], &[], None, &plan)?;
        let unborn_head = single_outcome
            .lock()
            .expect("clone is single-threaded here")
            .as_ref()
            .and_then(|probe| probe.head_target.clone())
            .or_else(|| {
                clone_default_branch_name(&git_dir, &config_pairs, false).map(|branch| format!("refs/heads/{branch}"))
            });
        update_head(&repo, None, None, unborn_head.as_deref(), &plan)?;
    }

    // `clone_local()`: with a local source, git never runs the transport at all — it
    // adopts the source object store, hardlinking each file where it can. Here the
    // local fetch has already produced a pack; the pack is dropped and the source's
    // objects are adopted in its place, so the clone ends up with the object layout git
    // leaves behind (loose objects stay loose, packs stay packs).
    // ```c
    // static void clone_local(const char *src_repo, const char *dest_repo)
    // {
    //         if (option_shared) {
    //                 [...]
    //                 odb_add_to_alternates_file(the_repository->objects, alt.buf);
    //         } else {
    //                 [...]
    //                 copy_or_link_directory(&src, &dest, src_repo);
    //         }
    // ```
    //
    // (builtin/clone.c:350-368.) The two halves are exclusive: `-s` borrows the source's
    // object store through `objects/info/alternates` — recorded above, where git records
    // the `--reference` ones — and copies nothing at all. The pack the local fetch left
    // behind is dropped either way, since git's transport never ran.
    if is_local && shared {
        drop_fetched_pack(&git_dir);
    } else if let Some(adopted) = adopted_pack_files {
        // The objects were adopted above, before the fetch, so this is only the
        // clean-up for a source the negotiation could not skip after all — a pack the
        // transport wrote is not part of what `copy_or_link_directory()` leaves behind.
        remove_pack_files_other_than(&git_dir, &adopted);
    } else {
        if let Some(cloned) = gix::open(&git_dir)
            .ok()
            .filter(|repo| repo.config_snapshot().boolean("pack.writeReverseIndex").unwrap_or(true))
        {
            // A clone that really fetched received its pack through gitoxide, which
            // writes no reverse index; git's `index-pack` writes one for every pack
            // it produces unless `pack.writeReverseIndex` says otherwise. The adopted
            // local objects above are exempt: they are the source's own pack files,
            // carried across with whatever sidecars the source already had.
            //
            // The hash comes from the cloned repository, which has already adopted
            // the remote's `object-format` — a sha256 remote leaves a sha256 clone,
            // and its `.rev` files have to be written in that algorithm.
            super::index_pack::write_missing_rev_indexes(
                &git_dir.join("objects").join("pack"),
                cloned.object_hash(),
            );
        }
        // The pack the transport received is index-pack's: `odb_mkstemp()` creates
        // its pack, its index and its reverse index 0444 and lets the umask do the
        // rest (builtin/index-pack.c:365-366, pack-write.c:87, :263, odb.c:72-84), where
        // gitoxide wrote them 0600.
        set_fetched_pack_modes(&git_dir.join("objects").join("pack"));
    }

    // ```c
    // if (!access(alternates, F_OK)) {
    //         [...]
    //         strvec_pushl(&cmd.args, "repack", "-a", "-d", NULL);
    //         if (run_command(&cmd))
    //                 die(_("cannot repack to clean up"));
    //         if (unlink(alternates) && errno != ENOENT)
    //                 die_errno(_("cannot unlink temporary alternates file"));
    // }
    // ```
    //
    // (`dissociate_from_references()`, builtin/clone.c:845-861.) The borrowed objects are
    // pulled into a pack of this repository's own, and only then does the alternates line
    // go away — which is also why a `--dissociate` clone ends with one pack and no loose
    // objects, where the same clone without it keeps whatever layout it adopted.
    if dissociate && git_dir.join("objects").join("info").join("alternates").exists() {
        let code = run_self(&dir, &["repack", "-a", "-d"], false, child_setup(&git_dir, work_tree_entered).as_ref())?;
        if code != 0 {
            crate::git_fatal!("cannot repack to clean up");
        }
        match std::fs::remove_file(git_dir.join("objects").join("info").join("alternates")) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => crate::git_fatal!("cannot unlink temporary alternates file: {e}"),
        }
    }

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
            // `partial_clone_register()`: a partial clone records the remote it may go back to for
            // the objects it skipped, and bumps the repository format so older git refuses it.
            promisor_filter: filter.as_ref().map(|f| f.as_str().to_owned()),
            fetch_bundle_uri: persist_bundle_uri.clone(),
            fetch_bundle_creation_token: persist_creation_token.clone(),
            // The remote a bundle clone records is the bundle, not the scratch
            // repository the objects came out of: git never had a second
            // repository, so `remote.<name>.url` is `absolute_pathdup(path)` —
            // the same `path` `get_repo_path()` returned.
            remote_url: bundle_transport.as_ref().map(|bt| bt.shown.clone()),
            config_pairs: &config_pairs,
        },
    )?;

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

    // `--sparse`: git initializes a cone-mode sparse-checkout containing only the
    // top-level files. This port's own `sparse-checkout set --cone` writes the
    // identical `info/sparse-checkout` pattern pair and `config.worktree` keys and
    // prunes the worktree, so it is re-executed here rather than duplicated.
    //
    // ```c
    // static int git_sparse_checkout_init(const char *repo)
    // {
    //         struct child_process cmd = CHILD_PROCESS_INIT;
    //         int result = 0;
    //         strvec_pushl(&cmd.args, "-C", repo, "sparse-checkout", "set", NULL);
    //         cmd.git_cmd = 1;
    //         if (run_command(&cmd)) {
    //                 error(_("failed to initialize sparse-checkout"));
    //                 result = 1;
    //         }
    //         return result;
    // }
    // ```
    //
    // (builtin/clone.c:783-798.) `--bare --sparse` is not refused during option
    // parsing: the clone runs to completion — banner, transfer and `done.` — and
    // only then does the child die `fatal: this operation must be run in a work
    // tree`, leaving `error: failed to initialize sparse-checkout` and exit **1**
    // behind it. Both lines are on stderr and the repository stays on disk.
    //
    // ```c
    // if (option_sparse_checkout && git_sparse_checkout_init(dir))
    //         return 1;
    // ```
    //
    // (builtin/clone.c:1490-1491.) The bare `return 1` skips the `cleanup:` label
    // that would have set `junk_mode = JUNK_LEAVE_ALL`, so the `atexit` handler
    // still holds the destination and takes it away — a `git clone --bare
    // --sparse` leaves nothing on disk. This port's junk guard was released when
    // the fetch finished, so the removal is re-stated here.
    //
    // A clone that checks out ran the child before `checkout()`, where it belongs
    // (see above); only a bare or `--no-checkout` clone, which never unpacks, is
    // set up here.
    if sparse && (bare || no_checkout) {
        let code = run_self(&dir, &["sparse-checkout", "set", "--cone"], true, child_setup(&git_dir, false).as_ref())?;
        if code != 0 {
            eprintln!("error: failed to initialize sparse-checkout");
            junk.remove_now();
            return Ok(ExitCode::from(1));
        }
    }

    // ```c
    // head = refs_resolve_refdup(get_main_ref_store(the_repository), "HEAD",
    //                            RESOLVE_REF_READING, &oid, NULL);
    // if (!head) {
    //         warning(_("remote HEAD refers to nonexistent ref, "
    //                   "unable to checkout"));
    //         return 0;
    // }
    // if (!strcmp(head, "HEAD")) {
    //         if (advice_enabled(ADVICE_DETACHED_HEAD))
    //                 detach_advice(oid_to_hex(&oid));
    // ```
    //
    // (`checkout()`, builtin/clone.c:655-664.) `update_head()` points HEAD at whatever the
    // remote's own HEAD named even when that branch was not among the refs fetched, so the
    // first thing the checkout does is find HEAD unresolvable and give up — without failing
    // the clone. An empty clone never gets here: it set `option_no_checkout` above.
    //
    // A detached `HEAD` — a remote `HEAD` that names no branch, `--branch <tag>`,
    // `--revision` — gets the advice block instead, which `-q` does not silence.
    if !bare && !no_checkout && !cloned_empty {
        if let Ok(repo) = gix::open(&git_dir) {
            match repo.head() {
                Ok(head) if head.id().is_some() => {
                    if head.is_detached() && crate::advice::Advice::DetachedHead.enabled_in(&repo) {
                        let id = head.id().expect("checked above");
                        super::checkout::print_detached_head_advice(&id.to_string());
                    }
                    if work_tree_missing {
                        checkout_died_with("this operation must be run in a work tree");
                    }
                }
                _ => eprintln!("warning: remote HEAD refers to nonexistent ref, unable to checkout"),
            }
        }
    }

    // `--recursive` / `--recurse-submodules`: after a successful non-bare,
    // checked-out clone, initialize and update submodules recursively by
    // re-executing this binary's own ported `submodule update --init --recursive`
    // in the new worktree. `--remote-submodules` adds git's `--remote`, which
    // updates each submodule to its remote-tracking branch instead of the
    // superproject's recorded commit.
    //
    // An empty clone set `option_no_checkout` (builtin/clone.c:1560-1563), so `checkout()`
    // returns before it reaches the submodule update (builtin/clone.c:652-653, :704).
    if recurse_submodules && !bare && !no_checkout && !cloned_empty {
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
        let code = run_self(&dir, &sub, false, child_setup(&git_dir, work_tree_entered).as_ref())?;
        if code != 0 {
            return Ok(ExitCode::from(code));
        }
    }

    Ok(ExitCode::SUCCESS)
}


/// builtin/clone.c's `remove_junk()`, as a guard rather than an `atexit()` hook.
///
/// ```c
/// if (dest_exists)
///         junk_work_tree_flags |= REMOVE_DIR_KEEP_TOPLEVEL;
/// else if (mkdir(work_tree, 0777))
///         die_errno(_("could not create work tree dir '%s'"), work_tree);
/// junk_work_tree = work_tree;
/// ```
///
/// (builtin/clone.c:1113-1118.) A directory the clone created is the clone's to take back
/// when it dies; one it found is left where it was. [`JunkDir::leave`] is `junk_mode =
/// JUNK_LEAVE_REPO`, after which git keeps what it built.
///
/// Each entry is a directory and whether `REMOVE_DIR_KEEP_TOPLEVEL` applies to it, in
/// `remove_junk()`'s order: the git dir, then the work tree (builtin/clone.c:404-412).
struct JunkDir {
    paths: Vec<(PathBuf, bool)>,
    armed: std::cell::Cell<bool>,
}

impl JunkDir {
    fn arm(paths: Vec<(PathBuf, bool)>) -> Self {
        JunkDir {
            paths,
            armed: std::cell::Cell::new(true),
        }
    }

    /// Stop removing on drop — the repository is far enough along to keep.
    fn leave(&self) {
        self.armed.set(false);
    }

    /// `remove_junk()` run now, for a `return 1` that bypasses the `cleanup:` label after
    /// [`JunkDir::leave`] was already called on this port's side.
    fn remove_now(&self) {
        for (path, keep_toplevel) in &self.paths {
            remove_dir_recursively(path, *keep_toplevel);
        }
    }
}

impl Drop for JunkDir {
    fn drop(&mut self) {
        if self.armed.get() {
            self.remove_now();
        }
    }
}

/// `remove_dir_recursively()` (dir.c:3392-3447) as `remove_junk()` calls it: everything below
/// `path` goes, and `path` itself too unless `keep_toplevel` (`REMOVE_DIR_KEEP_TOPLEVEL`).
fn remove_dir_recursively(path: &Path, keep_toplevel: bool) {
    if !keep_toplevel {
        let _ = std::fs::remove_dir_all(path);
        return;
    }
    let Ok(entries) = std::fs::read_dir(path) else { return };
    for entry in entries.flatten() {
        let child = entry.path();
        match entry.file_type() {
            Ok(kind) if kind.is_dir() => {
                let _ = std::fs::remove_dir_all(&child);
            }
            _ => {
                let _ = std::fs::remove_file(&child);
            }
        }
    }
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
type SingleOutcome = std::sync::Arc<std::sync::Mutex<Option<SingleProbe>>>;

/// What the `--single-branch` ls-refs probe learned.
#[derive(Clone, Default)]
struct SingleProbe {
    /// The ref the clone is restricted to.
    fetched: Option<String>,
    /// The branch the remote's `HEAD` names, born or not.
    head_target: Option<String>,
    /// A non-unborn `HEAD` or `refs/heads/*` was advertised. Those are the
    /// `ref-prefix`es git's ls-refs asks for — `+refs/heads/*` from
    /// `refspec_appendf(&remote->fetch, ...)` and `HEAD` for `wants_head`
    /// (builtin/clone.c:1320, :1421-1436) — so any of them makes `refs` non-NULL.
    advertised_head_or_branch: bool,
    /// A `refs/tags/*` was advertised, which counts only when `TAG_REFSPEC` was
    /// appended (builtin/clone.c:1414-1419).
    advertised_tag: bool,
}

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
                let probe = resolve_single_ref(repo, url, branch.as_deref(), connect_options, server_options)?;
                let fetched = probe.fetched.clone();
                *outcome.lock().expect("clone is single-threaded here") = Some(probe);
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

/// `remote.<remote_name>.fetch` as `remote_get_early()` reads it, in git's configuration order: the system and
/// global files, then the new repository's own, which starts as the template's `config` and has every
/// `-c <key>=<value>` appended by `write_config()` before the remote is read (builtin/clone.c:764-791, :1189),
/// then `git -c`. This clone holds its `-c` pairs in memory instead (`Source::Api`), so they are put back between
/// the two. `refspec_append()` dies on a value it cannot parse (refspec.c `refspec_item_init_or_die()`), and
/// `get_fetch_map()` skips a negative refspec (remote.c:2141-2142), which no other step of a clone consults.
fn configured_fetch_refspecs(
    repo: &gix::Repository,
    remote_name: &str,
) -> Result<Vec<gix::refspec::RefSpec>, Box<dyn std::error::Error + Send + Sync>> {
    use gix::config::Source;
    let config = repo.config_snapshot();
    let key = format!("remote.{remote_name}.fetch");
    let values = |wanted: &dyn Fn(Source) -> bool| {
        config
            .plumbing()
            .strings_filter(key.as_str(), &mut |meta: &gix::config::file::Metadata| wanted(meta.source))
            .unwrap_or_default()
    };
    let command_line = |source: Source| matches!(source, Source::Env | Source::Cli | Source::EnvOverride);
    let mut specs = Vec::new();
    for value in values(&|s| s != Source::Api && !command_line(s))
        .into_iter()
        .chain(values(&|s| s == Source::Api))
        .chain(values(&command_line))
    {
        let spec = gix::refspec::parse(value.as_ref(), gix::refspec::parse::Operation::Fetch)
            .map_err(|_| crate::fatal::Fatal(format!("invalid refspec '{value}'")))?;
        if let Some(spec) = with_local_destination(spec.to_owned()) {
            specs.push(spec);
        }
    }
    Ok(specs)
}

/// `get_fetch_map()`'s `ref_map->peer_ref = get_local_ref(refspec->dst)` (remote.c:2118-2132, :2159) for an
/// exact refspec: a destination outside `refs/` is a branch, unless it starts `heads/`, `tags/` or `remotes/`,
/// which only lack the `refs/`. A pattern's expansion is taken as it is (`get_expanded_map()`), and a negative
/// refspec is dropped (remote.c:2141-2142).
fn with_local_destination(spec: gix::refspec::RefSpec) -> Option<gix::refspec::RefSpec> {
    use gix::refspec::instruction::Fetch;
    let gix::refspec::Instruction::Fetch(instruction) = spec.to_ref().instruction() else {
        return Some(spec);
    };
    let (src, dst, force) = match instruction {
        Fetch::Exclude { .. } => return None,
        Fetch::AndUpdate { src, dst, allow_non_fast_forward } if !src.contains(&b'*') => (src, dst, allow_non_fast_forward),
        Fetch::Only { .. } | Fetch::AndUpdate { .. } => return Some(spec),
    };
    let local = if dst.starts_with(b"refs/") {
        return Some(spec);
    } else if dst.starts_with(b"heads/") || dst.starts_with(b"tags/") || dst.starts_with(b"remotes/") {
        format!("refs/{dst}")
    } else {
        format!("refs/heads/{dst}")
    };
    let text = format!("{}{src}:{local}", if force { "+" } else { "" });
    Some(
        gix::refspec::parse(text.as_str().into(), gix::refspec::parse::Operation::Fetch)
            .map_or(spec, |parsed| parsed.to_owned()),
    )
}

/// `get_fetch_map(refs, refspec, &tail, 0)` for a single-branch clone, whose `refs` is the one ref
/// `wanted_peer_refs()` kept (builtin/clone.c:451-466): `fetched`, or nothing when the remote's `HEAD` named no
/// branch. A pattern maps that ref or nothing; an exact refspec that does not name it dies
/// `couldn't find remote ref` (remote.c:2144-2157), and one spelling out an object id maps that id whatever the
/// refs are. The result names `fetched` exactly, so the fetch asks for nothing else.
fn restrict_to_single_ref(
    spec: &gix::refspec::RefSpec,
    fetched: Option<&str>,
) -> Result<Option<gix::refspec::RefSpec>, Box<dyn std::error::Error + Send + Sync>> {
    use gix::refspec::instruction::Fetch;
    let gix::refspec::Instruction::Fetch(instruction) = spec.to_ref().instruction() else {
        return Ok(Some(spec.clone()));
    };
    let (src, dst, force) = match instruction {
        Fetch::Exclude { .. } => return Ok(None),
        Fetch::Only { src } => (src, None, false),
        Fetch::AndUpdate { src, dst, allow_non_fast_forward } => (src, Some(dst), allow_non_fast_forward),
    };
    let local = if src.contains(&b'*') {
        let Some(fetched) = fetched else { return Ok(None) };
        let null = gix::ObjectId::null(gix::hash::Kind::Sha1);
        let group = gix::refspec::MatchGroup::from_fetch_specs([spec.to_ref()]);
        let outcome = group.match_lhs(std::iter::once(gix::refspec::match_group::Item {
            full_ref_name: fetched.into(),
            target: &null,
            object: None,
        }));
        let Some(mapping) = outcome.mappings.into_iter().next() else {
            return Ok(None);
        };
        mapping.rhs.map(|rhs| rhs.to_string())
    } else if gix::ObjectId::from_hex(src).is_ok() {
        return Ok(Some(spec.clone()));
    } else {
        let name = if src.is_empty() { "HEAD".into() } else { src.to_str_lossy() };
        match fetched {
            Some(fetched) if crate::refname::refname_match(&name, fetched) > 0 => dst.map(|dst| dst.to_string()),
            _ => return Err(Box::new(crate::fatal::Fatal(format!("couldn't find remote ref {name}")))),
        }
    };
    let fetched = fetched.expect("a mapping implies the ref");
    let text = match local {
        Some(local) => format!("{}{fetched}:{local}", if force { "+" } else { "" }),
        None => fetched.to_string(),
    };
    Ok(Some(gix::refspec::parse(text.as_str().into(), gix::refspec::parse::Operation::Fetch)?.to_owned()))
}

/// `get_fetch_map()`'s death for an exact refspec of `remote->fetch` that names no advertised ref
/// (remote.c:2144-2157), which `wanted_peer_refs()` reaches before anything is transferred
/// (builtin/clone.c:465-466). A clone that got this far mapped something, so `refs` was not NULL and
/// `wanted_peer_refs()` ran (:1512-1513); a `--revision` that matched nothing in a NULL `refs` is
/// `Remote revision ... not found` instead.
fn check_exact_refspecs_mapped(ref_map: &gix::remote::fetch::RefMap) -> Result<()> {
    use gix::refspec::instruction::Fetch;
    let null = gix::ObjectId::null(ref_map.object_hash);
    for spec in &ref_map.refspecs {
        let gix::refspec::Instruction::Fetch(Fetch::Only { src } | Fetch::AndUpdate { src, .. }) = spec.to_ref().instruction()
        else {
            continue;
        };
        if src.contains(&b'*') || gix::ObjectId::from_hex(src).is_ok() {
            continue;
        }
        if fetch_map(ref_map, spec, &null).is_empty() {
            let name = if src.is_empty() { "HEAD".into() } else { src.to_str_lossy() };
            crate::git_fatal!("couldn't find remote ref {name}");
        }
    }
    Ok(())
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
) -> Result<SingleProbe, Box<dyn std::error::Error + Send + Sync>> {
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
            // `copy_ref(find_remote_branch(refs, option_branch))` is NULL for a branch the remote
            // does not have (builtin/clone.c:459-462), which maps nothing; `cmd_clone()` dies of it
            // once the refs are in, naming the remote (:1538-1546).
            candidates.into_iter().find(|c| advertised(c))
        }
        // `guess_remote_head(head, refs, REMOTE_GUESS_HEAD_QUIET)` (builtin/clone.c:455-458)
        // only answers with a branch the remote advertised. A `HEAD` that is unborn, or a
        // symref to a missing branch, leaves `mapped_refs` empty and
        // `remote_head_points_at` NULL, so `write_refspec_config()` writes no
        // `remote.<name>.fetch` (builtin/clone.c:810-823).
        None => head_target.clone().filter(|target| advertised(target)),
    };
    let born = || {
        map.remote_refs
            .iter()
            .filter(|r| !matches!(r, Ref::Unborn { .. }))
            .map(|r| r.unpack().0)
    };
    Ok(SingleProbe {
        fetched,
        head_target,
        advertised_head_or_branch: born().any(|name| name == "HEAD" || name.starts_with(b"refs/heads/")),
        advertised_tag: born().any(|name| name.starts_with(b"refs/tags/")),
    })
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
    /// `fetch.bundleURI`, recorded by `--bundle-uri` when the list it fetched
    /// advertised a `bundle.heuristic` so later fetches can revisit it.
    fetch_bundle_uri: Option<String>,
    /// `fetch.bundleCreationToken`, the largest `bundle.<id>.creationToken` the
    /// bundle-URI client applied, so the next fetch downloads nothing older.
    fetch_bundle_creation_token: Option<String>,
    /// `remote.<name>.url`, rewritten when the clone fetched through something
    /// other than the source the caller named — which is only ever a bundle,
    /// whose objects arrive from a scratch repository this clone stood up.
    remote_url: Option<String>,
    /// `-c <key>=<value>` pairs, in command-line order.
    config_pairs: &'a [(String, String)],
}

/// Apply [`ConfigFixups`] to `<git_dir>/config`.
///
/// The `-c` values are written here, after the clone, rather than before the
/// fetch; the fetch and checkout observe them all the same, because they are
/// also handed to gitoxide as in-memory overrides.
fn finalize_config(git_dir: &Path, fixups: &ConfigFixups<'_>) -> Result<()> {
    let path = git_dir.join("config");
    let mut file =
        gix::config::File::from_path_no_includes(path.clone(), gix::config::Source::Local)
            .map_err(|e| anyhow::anyhow!("{e}"))?;

    // gitoxide appended the remote section it wrote, so it is the last; a template's `config` can hold one of
    // the same name ahead of it, where `repo_config_set()` would have put `url` and `fetch` (config.c
    // `git_config_set_multivar_in_file_gently()` appends to the last section of that name).
    let remote_name = file
        .sections()
        .filter(|s| s.header().name() == "remote")
        .last()
        .and_then(|s| s.header().subsection_name().map(ToString::to_string));
    let remote_section_repeated = file
        .sections()
        .filter(|s| s.header().name() == "remote" && s.header().subsection_name().map(ToString::to_string) == remote_name)
        .count()
        > 1;

    if !remote_section_repeated
        && !fixups.set_mirror
        && !fixups.drop_fetch
        && !fixups.drop_tag_opt
        && !fixups.drop_branch_sections
        && fixups.promisor_filter.is_none()
        && fixups.fetch_bundle_uri.is_none()
        && fixups.fetch_bundle_creation_token.is_none()
        && fixups.remote_url.is_none()
        && fixups.config_pairs.is_empty()
    {
        return Ok(());
    }

    if let Some(name) = remote_name {
        if let Ok(mut section) = file.section_mut("remote", Some(name.as_str().into())) {
            if fixups.drop_fetch {
                while section.remove("fetch").is_some() {}
            }
            if fixups.drop_tag_opt {
                while section.remove("tagOpt").is_some() {}
            }
            if fixups.set_mirror {
                // `write_refspec_config()` records `+refs/*:refs/*` for every `--mirror` clone, a
                // single-branch one included, as `option_single_branch && !option_mirror` keeps the
                // single ref out of it (builtin/clone.c:801-835).
                section.set("fetch", "+refs/*:refs/*")?;
                section.push("mirror", Some("true".into()))?;
            }
            if let Some(spec) = &fixups.promisor_filter {
                section.push("promisor", Some("true".into()))?;
                section.push("partialclonefilter", Some(spec.as_str().into()))?;
            }
            if let Some(url) = &fixups.remote_url {
                section.set("url", url.as_str())?;
            }
        }
        // `repo_config_set(the_repository, "remote.<name>.url", repo)` (builtin/clone.c:1303-1304) replaces a
        // `url` the template's section of that name already holds, where it stands.
        if remote_section_repeated {
            let sections: Vec<_> = file
                .sections_and_ids()
                .filter(|(s, _)| {
                    s.header().name() == "remote"
                        && s.header().subsection_name().map(ToString::to_string).as_ref() == Some(&name)
                })
                .map(|(s, id)| (id, s.value("url").is_some()))
                .collect();
            let ((written, _), earlier) = sections.split_last().expect("the section is repeated");
            let holder = earlier.iter().rev().find(|(_, has_url)| *has_url).map(|(id, _)| *id);
            if let Some(holder) = holder {
                let url = file
                    .section_mut_by_id(*written)
                    .and_then(|mut s| s.remove("url"));
                if let (Some(url), Some(mut section)) = (url, file.section_mut_by_id(holder)) {
                    section.set("url", url.as_bstr())?;
                }
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

    // git writes every `-c` value, so a key given twice ends up with two values —
    // except the two keys that name the repository's own formats.
    //
    // `cmd_clone()` runs `initialize_repository_version()` twice. The first call
    // precedes `write_config()` and only stubs the refdb out; the second runs
    // once the remote's object format is known, with `reinit` set:
    //
    // ```c
    // if (hash_algo != GIT_HASH_SHA1 && hash_algo != GIT_HASH_UNKNOWN)
    //         repo_config_set(repo, "extensions.objectformat", ...);
    // else if (reinit)
    //         repo_config_set_gently(repo, "extensions.objectformat", NULL);
    // ```
    //
    // and the same pair for `extensions.refstorage`. `NULL` is an *unset*, so a
    // clone that lands on the defaults — sha1 objects and `files` refs, which is
    // every clone this port performs — deletes both keys on its way out, taking
    // any `-c` that wrote them with it. A caller must not be able to assert a
    // format the clone did not use: the repository left behind would declare one
    // width over objects of another, and neither implementation could read it.
    // `extensions.worktreeConfig` and an unknown `extensions.*` are untouched by
    // that unset and are still written, which is what makes this a two-key rule
    // rather than a rule about the section.
    //
    // Where the pairs land is `write_config()`'s doing too (builtin/clone.c:764-791):
    // it runs straight after `init_db()` (:1189, :1241), so the `-c` sections follow
    // the `[core]` block `init_db()` wrote and come *before* everything the clone sets
    // afterwards. `remote.<name>.url`, its `fetch` refspec and the `branch.<name>`
    // keys are then `git_config_set()` calls, which append a new key to the end of
    // an existing section of the same name — so `-c remote.origin.x=y` leaves `x`
    // ahead of `url` inside one `[remote "origin"]`. gitoxide has already written
    // those sections by now, so the file is rebuilt in git's order: the sections
    // ahead of the first `[remote]`, then the pairs, then every later key appended
    // the way `git_config_set()` would.
    let sections: Vec<_> = file.sections().collect();
    let split = sections
        .iter()
        .rposition(|s| s.header().name() == "remote")
        .unwrap_or(sections.len());
    let mut out = gix::config::File::new(gix::config::file::Metadata::from(gix::config::Source::Local));
    let append = |out: &mut gix::config::File,
                      section: &str,
                      subsection: Option<&str>,
                      name: &str,
                      value: &gix::bstr::BStr|
     -> Result<()> {
        out.section_mut_or_create_new(section, subsection.map(Into::into))
            .map_err(|e| anyhow::anyhow!("{e}"))?
            .push(name, Some(value))?;
        Ok(())
    };
    let replay = |out: &mut gix::config::File, sections: &[gix::config::file::SectionRef<'_>]| -> Result<()> {
        for s in sections {
            let header = s.header();
            let section = header.name().to_string();
            let subsection = header.subsection_name().map(|n| n.to_string());
            let mut seen: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
            for name in s.value_names() {
                let nth = seen.entry(name.to_ascii_lowercase()).or_insert(0);
                if let Some(value) = s.values(&name).get(*nth) {
                    append(out, &section, subsection.as_deref(), &name, value.as_ref())?;
                }
                *nth += 1;
            }
        }
        Ok(())
    };
    replay(&mut out, &sections[..split])?;
    for (key, value) in fixups.config_pairs {
        if key.eq_ignore_ascii_case("extensions.objectformat")
            || key.eq_ignore_ascii_case("extensions.refstorage")
        {
            continue;
        }
        // `write_one_config()` hands the key to `git_config_set_multivar_gently()`,
        // which canonicalizes it with `git_config_parse_key()`: the section and the
        // variable name are lowercased, a subsection keeps its case.
        let (section, subsection, name) = split_config_key(key)?;
        append(
            &mut out,
            &section.to_ascii_lowercase(),
            subsection.as_deref(),
            &name.to_ascii_lowercase(),
            value.as_str().into(),
        )?;
    }
    replay(&mut out, &sections[split..])?;

    std::fs::write(&path, out.to_bstring())?;
    Ok(())
}

/// `get_pack()`'s death of a pack stream that ended early, for a clone: `transport_set_option(transport,
/// TRANS_OPT_KEEP, "yes")` (builtin/clone.c:1362) with the `unpack_limit` `fetch_pack_setup()` read
/// (fetch-pack.c:2015-2026), and `index-pack` required by a filter's `--from-promisor` or by
/// `fetch_pack_fsck_objects()`. The clone is removed on the way out, as for any `die()` before `junk_mode` moves on.
fn pack_stream_died(
    git_dir: &Path,
    url: &str,
    filtered: bool,
    stream: &gix::remote::fetch::PackStream,
) -> anyhow::Error {
    if let Ok(repo) = gix::open(git_dir) {
        let plan = super::fetch_progress::Plan {
            progress: false,
            quiet: true,
            keep_pack: true,
            unpack_limit: super::fetch_progress::unpack_limit(&repo),
            index_pack_required: filtered || super::fetch::fetch_pack_fsck_objects(&repo),
        };
        let smart_http = url.starts_with("http://") || url.starts_with("https://");
        super::fetch_progress::pack_stream_died(&repo, plan, smart_http, stream);
    }
    anyhow::Error::new(crate::fatal::Silent(128))
}

/// Give the post-fetch connectivity check the wording `update_remote_refs()` uses on the clone side,
/// and the `--branch` lookup the diagnostic git dies with.
///
/// git's fetch says `<url> did not send all necessary objects`; its clone, which has no display state
/// to name the URL from yet, says `remote did not send all necessary objects` and takes the whole
/// clone directory down with it.
///
/// `--branch <name>` that the remote does not advertise is git's
/// `die(_("Remote branch %s not found in upstream %s"), option_branch, remote_name)` — a `fatal:`
/// naming the branch as it was typed and the remote it was looked for in, with exit 128. gitoxide
/// reports the same condition as `RefNameMissing`, whose text names neither the remote nor the
/// spelling the caller used, and would arrive as this binary's own error with exit 1.
fn short_pack(err: gix::clone::fetch::Error, branch: Option<&str>, remote_name: &str) -> anyhow::Error {
    match err {
        gix::clone::fetch::Error::Fetch(gix::remote::fetch::Error::NotConnected) => {
            anyhow::anyhow!("remote did not send all necessary objects")
        }
        // ```c
        // if (do_keep && (pack_lockfiles || fsck_objects)) {
        //         int is_well_formed;
        //         char *pack_lockfile = index_pack_lockfile(the_repository,
        //                                                   cmd.out,
        //                                                   &is_well_formed);
        //
        //         if (!is_well_formed)
        //                 die(_("fetch-pack: invalid index-pack output"));
        // ```
        //
        // (`get_pack()`, fetch-pack.c:1074-1081.) A server whose `pack-objects`
        // died mid-stream leaves the receiver with a pack that stops in the
        // middle of an entry, and git ends the whole clone at 128 on it — where
        // gitoxide's `ConsumePack` arrived here as an ordinary error and left
        // this port reporting 1. Reached whenever the far side gives up while
        // writing: an `upload-pack` serving a partial clone it cannot complete
        // (`fatal: could not fetch <oid> from promisor remote`) is the one this
        // corpus exercises.
        gix::clone::fetch::Error::Fetch(gix::remote::fetch::Error::Fetch(
            gix::protocol::fetch::Error::ConsumePack(_),
        )) => crate::fatal::die("fetch-pack: invalid index-pack output"),
        // ```c
        // /* And complain if we didn't get enough bytes to satisfy the read. */
        // if (bytes_read != size) {
        //         if (options & PACKET_READ_GENTLE_ON_EOF)
        //                 return -1;
        //
        //         if (options & PACKET_READ_GENTLE_ON_READ_ERROR)
        //                 return error(_("the remote end hung up unexpectedly"));
        //         die(_("the remote end hung up unexpectedly"));
        // }
        // ```
        //
        // (`get_packet_data()`, pkt-line.c:364-372.) A server that `die()`s part-way
        // through a response closes the pipe where the client is waiting for the rest of
        // a packet, and every reader git has on that path ends the process at 128 with
        // that one sentence — the server's own message having already reached the
        // terminal through the inherited stderr. `upload-pack` refusing `deepen`
        // together with `deepen-since` is the case the corpus exercises.
        //
        // The condition is `UnexpectedEof` on the packet reader specifically, which is
        // `read_exact`'s verdict on a stream that stopped mid-packet. Any other I/O
        // failure is a different fault and keeps its own reporting: a broken pipe while
        // *writing*, a refused connection, a transport that never opened.
        gix::clone::fetch::Error::Fetch(gix::remote::fetch::Error::Fetch(
            gix::protocol::fetch::Error::FetchResponse(gix::protocol::fetch::response::Error::Io(ref io)),
        )) if io.kind() == std::io::ErrorKind::UnexpectedEof => {
            crate::fatal::die("the remote end hung up unexpectedly")
        }
        // `die(_("Remote revision %s not found in upstream %s"), ...)` and
        // `die(_("could not parse %s"), ref_name)` (builtin/clone.c:1550, commit.c:85).
        e @ (gix::clone::fetch::Error::RevisionMissing { .. }
        | gix::clone::fetch::Error::RevisionNotACommit { .. }) => crate::fatal::die(e.to_string()),
        // A `die()` of the remote's own configuration, `refspec_append()`'s or `get_fetch_map()`'s, which
        // `remote_get_early()` and `wanted_peer_refs()` reach before anything is fetched.
        gix::clone::fetch::Error::RemoteConfiguration(e) if e.is::<crate::fatal::Fatal>() => {
            anyhow::Error::new(*e.downcast::<crate::fatal::Fatal>().expect("checked"))
        }
        gix::clone::fetch::Error::RefNameMissing { ref wanted } => {
            let wanted = branch.map_or_else(|| wanted.as_ref().as_bstr().to_string(), ToOwned::to_owned);
            crate::fatal::die(format!("Remote branch {wanted} not found in upstream {remote_name}"))
        }
        other => other.into(),
    }
}

/// The ref two refspecs of `remote->fetch` map different remote refs onto, which gitoxide refuses to map at all.
/// git maps both and `write_remote_refs()` queues the ref twice, so the clone dies in
/// `ref_update_reject_duplicates()` (builtin/clone.c:473-498, refs.c:2574-2593, :2708-2709) on the first such
/// name in sorted order, once the objects are in.
///
/// Only the destinations gitoxide reports are known here: a ref two refspecs map from the *same* remote ref is
/// no conflict to it, so such a ref sorting ahead of every reported one is not the name chosen.
fn conflicting_destination(err: &gix::clone::fetch::Error) -> Option<String> {
    use gix::protocol::fetch::refmap::init::Error as InitRefMap;
    use gix::refspec::match_group::validate::Issue;
    let gix::clone::fetch::Error::PrepareFetch(gix::remote::fetch::prepare::Error::RefMap(
        gix::remote::ref_map::Error::InitRefMap(InitRefMap::MappingValidation(validation)),
    )) = err
    else {
        return None;
    };
    validation
        .issues
        .iter()
        .map(|Issue::Conflict { destination_full_ref_name, .. }| destination_full_ref_name)
        .min()
        .map(|name| name.to_string())
}

/// Split a `-c <key>=<value>` argument the way git's `git_config_parse_parameter`
/// does: a bare `<key>` with no `=` is the boolean `true` (verified against stock
/// git 2.55.0 — `git clone -c foo.bar …` writes `bar = true`), and a key with no
/// section is rejected with git's `key does not contain a section: <key>`.
///
/// Validation happens here, during parsing, so a bad key fails before the
/// destination directory is created — exactly where git rejects it.
fn parse_config_pair(raw: &str) -> (String, String) {
    let (key, value) = match raw.split_once('=') {
        Some((k, v)) => (k, v.to_string()),
        None => (raw, "true".to_string()),
    };
    (key.to_string(), value)
}

/// The complaint `git_config_parse_parameter()` makes about a `-c` key it cannot
/// split, or `None` when the key is usable.
///
/// This is deliberately *not* raised where the argument is parsed. git collects
/// every `-c` into `option_config` during option parsing and only replays them in
/// `write_config()`, which runs after `init_db()` — so the diagnostic lands under
/// the `Cloning into '<dir>'...` banner, not above it, and the caller turns it
/// into `die(_("unable to write parameters to config file"))` rather than
/// reporting the key itself as fatal.
fn config_key_error(key: &str) -> Option<String> {
    let Some(first) = key.find('.') else {
        return Some(format!("key does not contain a section: {key}"));
    };
    let last = key.rfind('.').expect("a first dot implies a last dot");
    if key[..first].is_empty() || key[last + 1..].is_empty() {
        return Some(format!("key does not contain variable name: {key}"));
    }
    None
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

/// What `get_repo_path_1()` (`builtin/clone.c:97`) makes of a local source path.
enum LocalSource {
    /// One of `<path>/.git`, `<path>`, `<path>.git/.git`, `<path>.git` is a repository — the
    /// ordinary local clone. Carries the path the rest of the clone reads the source through:
    /// the operand itself, or `<path>.git` when only that spelling names a repository.
    Repository(PathBuf),
    /// None of those is, but the path (or `<path>.bundle`) is a regular file, which is git's
    /// second reading of a local source: a bundle. Carries the path git would name.
    Bundle(PathBuf),
    /// Neither — git leaves `path` NULL and hands the raw URL to the transport.
    Neither,
    /// A candidate that starts `gitdir: ` and that `read_gitfile()` refuses: its `die()`
    /// text (`read_gitfile_error_die()`, setup.c:927-952).
    Refused(String),
}

/// Classify a local source the way `get_repo_path_1()` does, in its order: the four repository
/// spellings first, then the two bundle ones. A gitfile (`gitdir: …`) counts as a repository only
/// when it is longer than its own signature, which is git's `st.st_size > 8` guard.
fn classify_local_source(repo_name: &str) -> LocalSource {
    let base = Path::new(repo_name);
    // `<path>/.git` and `<path>` are both read through `<path>`; the other two need the suffix.
    let source_path = |suffix: &str| match suffix {
        ".git/.git" | ".git" => PathBuf::from(format!("{repo_name}.git")),
        _ => base.to_path_buf(),
    };
    for suffix in ["/.git", "", ".git/.git", ".git"] {
        let candidate = PathBuf::from(format!("{}{suffix}", base.display()));
        let Ok(meta) = std::fs::metadata(&candidate) else {
            continue;
        };
        if meta.is_dir() {
            if gix::open(&candidate).is_ok() {
                return LocalSource::Repository(source_path(suffix));
            }
        } else if meta.is_file() && meta.len() > 8 {
            let is_gitfile = std::fs::read(&candidate)
                .map(|bytes| bytes.starts_with(b"gitdir: "))
                .unwrap_or(false);
            if is_gitfile {
                // `dst = read_gitfile(path->buf);` (builtin/clone.c:122) is the dying
                // spelling, so a gitfile that names no repository ends the clone here.
                if let Err(err) = gix::discover::setup::read_gitfile_gently(&candidate) {
                    return LocalSource::Refused(gix::discover::setup::gitfile_error_message(&candidate, err));
                }
                return LocalSource::Repository(source_path(suffix));
            }
        }
    }
    for suffix in [".bundle", ""] {
        let candidate = PathBuf::from(format!("{}{suffix}", base.display()));
        if std::fs::metadata(&candidate).is_ok_and(|m| m.is_file()) {
            return LocalSource::Bundle(candidate);
        }
    }
    LocalSource::Neither
}

/// The spelling git names a local source with: `absolute_pathdup()` (`abspath.c:293`), which
/// prefixes a relative path with the current directory and normalizes nothing. Shared with the
/// clone url itself through [`gix::Url::absolutize`], so a diagnostic and `remote.<name>.url`
/// cannot disagree about the same path.
fn absolute_pathdup(path: &Path) -> String {
    let bytes = gix::path::into_bstr(path);
    match gix::url::parse(bytes.as_ref()) {
        Ok(mut url) => {
            url.absolutize(Path::new(""));
            gix::path::from_bstr(url.path.as_bstr()).display().to_string()
        }
        Err(_) => path.display().to_string(),
    }
}

/// `get_repo_path()` (builtin/clone.c:144-155) for a source that is a
/// repository: the first of `<repo>/.git`, `<repo>`, `<repo>.git/.git` and
/// `<repo>.git` that is a git directory, through `absolute_pathdup()` and so
/// spelled as given (`./src` stays `<cwd>/./src/.git`), or the real path a
/// gitfile among them names. `clone_local()` borrows `<that>/objects` under `-s`.
fn get_repo_path(repo_name: &str) -> Option<String> {
    for suffix in ["/.git", "", ".git/.git", ".git"] {
        let candidate = PathBuf::from(format!("{repo_name}{suffix}"));
        let Ok(meta) = std::fs::metadata(&candidate) else {
            continue;
        };
        if meta.is_dir() && gix::discover::setup::is_git_directory(&candidate) {
            return Some(absolute_pathdup(&candidate));
        }
        if meta.is_file() && meta.len() > 8 {
            let is_gitfile = std::fs::read(&candidate).is_ok_and(|bytes| bytes.starts_with(b"gitdir: "));
            if let (true, Ok(Some(dst))) =
                (is_gitfile, gix::discover::setup::read_gitfile_gently(&candidate))
            {
                return Some(dst.display().to_string());
            }
        }
    }
    None
}

/// `get_common_dir_noenv()` (setup.c:321-348): the real path a `commondir` file
/// names, else `gitdir` exactly as given.
fn get_common_dir(gitdir: &str) -> String {
    let file = Path::new(gitdir).join("commondir");
    let Ok(data) = std::fs::read_to_string(&file) else {
        return gitdir.to_string();
    };
    let data = data.trim_end_matches(['\n', '\r']);
    let path = match Path::new(data).is_absolute() {
        true => PathBuf::from(data),
        false => PathBuf::from(format!("{gitdir}/{data}")),
    };
    std::fs::canonicalize(&path).unwrap_or(path).display().to_string()
}

/// A file git would read as a bundle really carrying a bundle header.
///
/// `read_bundle_header_fd()` (`bundle.c`) accepts `# v2 git bundle` and `# v3 git bundle`; anything
/// else is `error: '%s' does not look like a v2 or v3 bundle file`, which `transport_get()` prints
/// through `is_bundle(url, 1)` before the transport is chosen.
fn looks_like_bundle(path: &Path) -> bool {
    let Ok(bytes) = std::fs::read(path) else {
        return false;
    };
    bytes.starts_with(b"# v2 git bundle\n") || bytes.starts_with(b"# v3 git bundle\n")
}

/// The bundle transport, as far as a clone can see it.
///
/// git gives a bundle source a three-entry vtable — `get_refs_list` reads the
/// header, `fetch_refs` runs `unbundle()`, `disconnect` closes the descriptor
/// (`bundle_vtable`, transport.c:1162-1166, v2.55.0) — and changes nothing else
/// about `cmd_clone`. gitoxide's fetch machinery has no such transport, so the
/// bundle is turned into the one thing that machinery does understand: a
/// repository holding the bundle's pack and advertising the bundle's refs.
///
/// It is scratch, it lives inside the new repository's git dir (so adopting its
/// objects is a same-filesystem hard link rather than a copy), and it is removed
/// as soon as the fetch that read it returns — the two paths the caller named,
/// `remote.<name>.url` and the `clone: from <url>` reflog message, are corrected
/// back to the bundle afterwards.
struct BundleTransport {
    /// The bundle file, for `read_bundle_header()`/`unbundle()`.
    path: PathBuf,
    /// `absolute_pathdup(path)`: the spelling git records for the source.
    shown: String,
    /// The scratch repository.
    dir: PathBuf,
    /// `dir` as the clone's `url_str`.
    dir_str: String,
    /// `dir` as the clone's url.
    url: gix::Url,
}

impl BundleTransport {
    fn new(path: PathBuf, shown: String, git_dir: &Path) -> Result<Self> {
        let dir = git_dir.join("zvcs-bundle-transport");
        let dir_str = dir.to_string_lossy().into_owned();
        let url = gix::url::parse(dir_str.as_str().into())?;
        Ok(BundleTransport {
            path,
            shown,
            dir,
            dir_str,
            url,
        })
    }

    /// `fetch_refs_from_bundle()` → `unbundle()` (transport.c:188-217,
    /// bundle.c:606-640): verify the prerequisites against the repository being
    /// filled in, then `index-pack --fix-thin --stdin` the pack that follows the
    /// header. The refs the header carries become the scratch repository's, which
    /// is what `get_refs_from_bundle()` turns into the advertisement.
    ///
    /// `Some(code)` is a failure git reports and exits with; the half-built clone
    /// is taken down by the junk directory on the way out, as `remove_junk()` does.
    fn unbundle(&self, git_dir: &Path) -> Result<Option<ExitCode>> {
        let scratch = gix::init_bare(&self.dir)?;
        let name = self.path.to_string_lossy().into_owned();
        let (header, source) = match super::bundle::open_bundle(&name) {
            Ok(pair) => pair,
            Err(e) => return super::bundle::report(&self.shown, e).map(Some),
        };

        // `unbundle()` verifies against `the_repository` — the repository the
        // clone is creating, which is still empty here, exactly as it is when git
        // reaches this from `transport_fetch_refs()`.
        let dest = gix::open(git_dir)?;
        if !super::bundle::verify_bundle(&dest, &header, false) {
            // `die(_("remote transport reported error"))` (builtin/clone.c), the
            // caller of the `transport_fetch_refs()` that just failed.
            eprintln!("fatal: remote transport reported error");
            return Ok(Some(ExitCode::from(128)));
        }
        if !super::bundle::index_pack(source, &scratch, &[])? {
            eprintln!("error: index-pack died");
            eprintln!("fatal: remote transport reported error");
            return Ok(Some(ExitCode::from(128)));
        }

        // The advertisement `get_refs_from_bundle()` builds is the header's list
        // *reversed*: it prepends (`ref->next = result; result = ref;`,
        // transport.c:163-172), and that order is what `guess_remote_head()` walks.
        let advertised: Vec<&(gix::ObjectId, Vec<u8>)> = header.refs.iter().rev().collect();
        for (oid, name) in &advertised {
            let Ok(name) = std::str::from_utf8(name) else {
                continue;
            };
            if name == "HEAD" {
                continue;
            }
            let Ok(full) = gix::refs::FullName::try_from(name) else {
                continue;
            };
            let file = self.dir.join(gix::path::from_bstr(full.as_bstr()).as_ref());
            if let Some(parent) = file.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::write(&file, format!("{oid}\n"))?;
        }

        // `guess_remote_head()` is what turns the advertised `HEAD` into the
        // branch a clone checks out, and a bundle advertises `HEAD` as a plain
        // object id with no symref attached. The scratch repository has to answer
        // with the branch that algorithm picks, because the file transport *does*
        // carry a symref and gitoxide would otherwise believe it over the guess.
        let head = guess_remote_head(&advertised, &default_branch_name(&dest));
        let target = head.unwrap_or_else(|| format!("refs/heads/{}", default_branch_name(&dest)));
        std::fs::write(self.dir.join("HEAD"), format!("ref: {target}\n"))?;
        Ok(None)
    }
}

/// `repo_default_branch_name()`: the `GIT_TEST_DEFAULT_INITIAL_BRANCH_NAME`
/// override (refs.c:696-699), else `init.defaultBranch`, else `master`. The key
/// is read with `repo_config_get_string()` (refs.c:700), which dies through
/// `git_die_config()` on a valueless key.
fn default_branch_name(repo: &gix::Repository) -> String {
    crate::refname::default_branch_name_override().unwrap_or_else(|| {
        crate::config::config_get_string(Some(repo), "init.defaultbranch")
            .filter(|v| !v.is_empty())
            .unwrap_or_else(|| "master".to_string())
    })
}

/// `guess_remote_head(head, refs, 0)` (remote.c:2492-2545, v2.55.0) over an
/// advertisement that carries no symref: the default branch first if it points
/// where `HEAD` does, then `refs/heads/master`, then the first other branch that
/// points there. `None` where git returns NULL, which is a clone whose `HEAD`
/// names no branch it was told about.
fn guess_remote_head(
    advertised: &[&(gix::ObjectId, Vec<u8>)],
    default_branch: &str,
) -> Option<String> {
    let head = advertised.iter().find(|(_, name)| name == b"HEAD")?;
    let at = |want: &str| {
        advertised
            .iter()
            .find(|(_, name)| name.as_slice() == want.as_bytes())
            .filter(|(oid, _)| *oid == head.0)
            .map(|_| want.to_string())
    };
    at(&format!("refs/heads/{default_branch}"))
        .or_else(|| at("refs/heads/master"))
        .or_else(|| {
            advertised
                .iter()
                .filter(|r| !std::ptr::eq(**r, *head))
                .find(|(oid, name)| *oid == head.0 && name.starts_with(b"refs/heads/"))
                .and_then(|(_, name)| String::from_utf8(name.clone()).ok())
        })
}

/// The object store `clone_local()` copies from.
///
/// ```c
///                 get_common_dir(&src, src_repo);
///                 strbuf_addstr(&src, "/objects");
///                 copy_or_link_directory(&src, &dest, src_repo);
/// ```
/// (builtin/clone.c:411-423)
///
/// `src_repo` is not the path the user typed: it is what `get_repo_path_1()`
/// (builtin/clone.c:127-165) resolved it to, and that function reads a `<path>/.git`
/// **gitfile** and returns its target —
///
/// ```c
///                 } else if (S_ISREG(st.st_mode) && st.st_size > 8) {
///                         /* Is it a "gitfile"? */
///                         …
///                         dst = read_gitfile(path->buf);
///                         if (dst) { *is_bundle = 0; return dst; }
///                 }
/// ```
///
/// — which is what a submodule's checkout and a `--separate-git-dir` worktree have
/// instead of a `.git` directory. Testing `<path>/.git` for *directory*-ness and
/// falling back to `<path>/objects` finds nothing on those, and a local clone that
/// adopts nothing reports `done.` and leaves a repository whose `HEAD` names an
/// object it does not have: `git clone ./sub other` produced
/// `fatal: bad object HEAD`, and `git submodule add ./sub other` failed in the
/// checkout that followed. `get_common_dir` is the second half — a source that is
/// itself a linked worktree keeps its objects in the common directory.
/// `src_repo` itself — the repository directory `get_repo_path()` resolved
/// the source to, absolute, which is what `copy_or_link_directory()` checks the
/// ownership of and names in its refusal.
fn local_source_repo(source: &Path) -> PathBuf {
    let dir = match gix::open(source) {
        Ok(repo) => repo.git_dir().to_owned(),
        Err(_) if source.join(".git").is_dir() => source.join(".git"),
        Err(_) => source.to_owned(),
    };
    std::fs::canonicalize(&dir).unwrap_or(dir)
}

fn local_source_objects(source: &Path) -> PathBuf {
    match gix::open(source) {
        Ok(repo) => repo.common_dir().join("objects"),
        // `get_repo_path_1` accepts a few spellings gitoxide's discovery does not
        // (`<path>.git`, `<path>/.git/.git`); the plain layouts stay reachable
        // without it.
        Err(_) if source.join(".git").is_dir() => source.join(".git").join("objects"),
        Err(_) => source.join("objects"),
    }
}

/// `compute_alternate_path()` (object-file.c), the whole of what
/// `--reference`/`--reference-if-able` runs before recording an alternate.
///
/// ```c
/// ref_git = real_pathdup(path, 1);
/// repo = read_gitfile(ref_git);
/// if (!repo)
///         repo = read_gitfile(mkpath("%s/.git", ref_git));
/// [...]
/// if (!is_directory(mkpath("%s/objects", ref_git))) {
///         strbuf_addf(err, _("reference repository '%s' is not a local repository."), path);
///         goto out;
/// }
/// if (!access(mkpath("%s/shallow", ref_git), F_OK)) {
///         strbuf_addf(err, _("reference repository '%s' is shallow"), path);
///         goto out;
/// }
/// if (!access(mkpath("%s/info/grafts", ref_git), F_OK)) {
///         strbuf_addf(err, _("reference repository '%s' is grafted"), path);
///         goto out;
/// }
/// ```
///
/// The three refusals are `strbuf_addf` into the caller's `err`, so
/// `--reference-if-able` downgrades all three to its `info:` line and clones
/// anyway. [`gitfile_target`]'s is not: `read_gitfile()` passes no error slot, so
/// a file that is not a gitfile is a `die()` from inside this function and the
/// `if-able` spelling dies with it.
///
/// Each message names `path` **as it was written**, never the resolved form.
fn compute_alternate_path(given: &str) -> Result<PathBuf> {
    let path = Path::new(given);
    // `real_pathdup(path, 1)`: absolute and symlink-resolved where it can be.
    let real = std::fs::canonicalize(path).unwrap_or_else(|_| {
        std::env::current_dir().map(|cwd| cwd.join(path)).unwrap_or_else(|_| path.to_path_buf())
    });
    let from_gitfile = match gitfile_target(&real)? {
        Some(dir) => Some(dir),
        None => gitfile_target(&real.join(".git"))?,
    };
    // ```c
    // if (!repo && is_directory(mkpath("%s/.git/objects", ref_git))) {
    //         char *ref_git_git = mkpathdup("%s/.git", ref_git);
    //         free(ref_git);
    //         ref_git = ref_git_git;
    // } else if (!is_directory(mkpath("%s/objects", ref_git))) {
    //         [...]
    //         strbuf_addf(err, _("reference repository '%s' is not a local repository."), path);
    //         goto out;
    // }
    // ```
    //
    // The `.git/objects` branch is what lets an ordinary worktree be a reference:
    // the path handed in is the working directory, and the object store is one
    // level down. Only a path that is neither a gitfile, nor a worktree, nor a
    // bare repository is refused.
    let ref_git = match from_gitfile {
        Some(dir) => dir,
        None if real.join(".git").join("objects").is_dir() => real.join(".git"),
        None if real.join("objects").is_dir() => real,
        None => anyhow::bail!("reference repository '{given}' is not a local repository."),
    };
    if ref_git.join("shallow").exists() {
        anyhow::bail!("reference repository '{given}' is shallow");
    }
    if ref_git.join("info").join("grafts").exists() {
        anyhow::bail!("reference repository '{given}' is grafted");
    }
    let objects = ref_git.join("objects");
    Ok(std::fs::canonicalize(&objects).unwrap_or(objects))
}

/// `read_gitfile()` — `read_gitfile_gently(path, NULL)`, whose null error slot is
/// what turns one of its four failures into a `die()`.
///
/// A path that is missing or is not a regular file is `READ_GITFILE_ERR_STAT_FAILED`
/// / `_NOT_A_FILE`, both of which that switch lets through as "no gitfile here".
/// A regular file whose first eight bytes are not `gitdir: ` is
/// `READ_GITFILE_ERR_INVALID_FORMAT`, and `die(_("invalid gitfile format: %s"), path)`
/// names the resolved path.
fn gitfile_target(path: &Path) -> Result<Option<PathBuf>> {
    if !path.is_file() {
        return Ok(None);
    }
    let Ok(body) = std::fs::read_to_string(path) else {
        return Ok(None);
    };
    let Some(target) = body.trim_end().strip_prefix("gitdir: ") else {
        crate::git_fatal!("invalid gitfile format: {}", path.display());
    };
    let target = Path::new(target);
    Ok(Some(match target.is_absolute() {
        true => target.to_path_buf(),
        false => path.parent().unwrap_or(Path::new(".")).join(target),
    }))
}

/// Write `objects/info/alternates` with one absolute object-directory path per
/// line, the way a run of `odb_add_to_alternates_file()` calls leaves it.
///
/// ```c
/// while (strbuf_getline(&line, in) != EOF) {
///         if (!strcmp(alternate, line.buf)) {
///                 found = 1;
///                 break;
///         }
///         fprintf_or_die(out, "%s\n", line.buf);
/// }
/// ```
///
/// (`odb_source_files_write_alternate()`, odb/source-files.c:227-249.) A store
/// the file already names is not appended again, so `--reference r --reference
/// r/.git` or `-s --reference <src>` record it once.
fn write_alternates(git_dir: &Path, alternates: &[PathBuf]) -> Result<()> {
    let info = git_dir.join("objects").join("info");
    std::fs::create_dir_all(&info)?;
    let mut lines: Vec<String> = Vec::new();
    for path in alternates {
        let line = path.to_string_lossy().into_owned();
        if !lines.contains(&line) {
            lines.push(line);
        }
    }
    let body: String = lines.iter().map(|line| format!("{line}\n")).collect();
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
/// A `die()` inside `checkout()`'s `unpack_trees()`, such as `start_delayed_progress()`
/// refusing `GIT_PROGRESS_DELAY`: with `junk_mode == JUNK_LEAVE_REPO` the
/// `remove_junk()` handler keeps the repository and says so
/// (builtin/clone.c:384-398). The exit is taken here, as `die()` takes it, so
/// nothing unwinds into the cleanup that would delete the clone.
fn checkout_died(e: crate::progress::DelayError) -> ! {
    checkout_died_with(&e.to_string())
}

/// A `die()` inside `checkout()`, with `junk_mode == JUNK_LEAVE_REPO`: the message, then
/// `remove_junk()`'s warning, and the repository stays.
fn checkout_died_with(msg: &str) -> ! {
    eprintln!("fatal: {msg}");
    eprintln!(
        "warning: Clone succeeded, but checkout failed.\n\
         You can inspect what was checked out with 'git status'\n\
         and retry with 'git restore --source=HEAD :/'\n"
    );
    std::process::exit(128);
}

/// What a child of a `$GIT_WORK_TREE` clone inherits: the exported `GIT_DIR`, and the work
/// tree `setup_work_tree()` entered, if it has run.
struct ChildSetup {
    git_dir: PathBuf,
    work_tree: Option<String>,
}

fn run_self(dir: &str, args: &[&str], from_user: bool, setup: Option<&ChildSetup>) -> Result<u8> {
    let exe = crate::hosted::git_exe()?;
    let mut cmd = std::process::Command::new(&exe);
    let dir = match setup {
        Some(setup) => {
            cmd.env("GIT_DIR", &setup.git_dir);
            match &setup.work_tree {
                Some(work_tree) => {
                    cmd.env("GIT_WORK_TREE", ".");
                    work_tree.as_str()
                }
                None => dir,
            }
        }
        None => dir,
    };
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
///
/// `config_pairs` are clone's own `-c <key>=<value>` options. `write_config()` has
/// written them into the new repository before `transport_get()` runs
/// (builtin/clone.c:1241, :1345), so the policy reads them as repository-local
/// configuration: over the system and global files, under the command line's
/// `git -c` (do_git_config_sequence(), config.c:1556-1622).
fn transport_allowed(kind: &str, config_pairs: &[(String, String)]) -> Result<bool> {
    if let Ok(list) = std::env::var("GIT_ALLOW_PROTOCOL") {
        return Ok(list.split(':').any(|entry| entry == kind));
    }
    let config = crate::config::global_config();
    let command_line = gix::config::File::from_environment_overrides().unwrap_or_default();
    // git's `parse_protocol_config`: anything but these three is fatal.
    let parse = |key: &str, value: &str| match value.to_ascii_lowercase().as_str() {
        "always" => Ok(Allow::Always),
        "never" => Ok(Allow::Never),
        "user" => Ok(Allow::User),
        other => Err(anyhow::anyhow!("unknown value for config '{key}': {other}")),
    };
    let string = |file: &gix::config::File, key: &str| {
        file.string(key).and_then(|v| v.to_str().ok().map(str::to_string))
    };
    let read = |key: &str| {
        string(&command_line, key)
            .or_else(|| {
                config_pairs
                    .iter()
                    .rev()
                    .find(|(k, _)| same_config_key(k, key))
                    .map(|(_, v)| v.clone())
            })
            .or_else(|| string(&config, key))
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

/// Whether a `-c` key names `want`: section and variable compare without case,
/// the subsection exactly, as `git_config_parse_key()` normalizes them.
fn same_config_key(given: &str, want: &str) -> bool {
    let split = |k: &str| -> Option<(String, String, String)> {
        let (section, rest) = k.split_once('.')?;
        let (sub, var) = rest.rsplit_once('.').unwrap_or(("", rest));
        Some((section.to_ascii_lowercase(), sub.to_string(), var.to_ascii_lowercase()))
    };
    split(given).is_some_and(|g| Some(g) == split(want))
}

/// git's `protocol_allow_config` (`transport.c:1066`).
enum Allow {
    Never,
    User,
    Always,
}

/// git's `transport_check_allowed()` (`transport.c:1156`) for the transport
/// `url` selects. The error text is git's `transport '%s' not allowed`.
fn check_transport_allowed(url: &gix::Url, config_pairs: &[(String, String)]) -> Result<()> {
    let kind = match url.scheme {
        gix::url::Scheme::File => "file",
        gix::url::Scheme::Git => "git",
        gix::url::Scheme::Ssh => "ssh",
        gix::url::Scheme::Http => "http",
        gix::url::Scheme::Https => "https",
        gix::url::Scheme::Ext(ref name) => name.as_str(),
    };
    if !transport_allowed(kind, config_pairs)? {
        crate::git_fatal!("transport '{kind}' not allowed");
    }
    Ok(())
}

/// `git_url_basename()` (dir.c): the directory `git clone <repo>` creates when
/// none is given.
///
/// Skip the scheme and any `user@` before the first slash; strip trailing
/// slashes and whitespace, then one `/.git` and the slashes before it; strip a
/// `:<port>` from a bare `host:port`; take the last `/`- or `:`-separated
/// component and drop `.bundle` (for a bundle) or `.git` from it; append `.git`
/// for a bare clone; and finally fold every run of whitespace and control
/// characters into one space, trimming both ends.
///
/// `None` is git's `die(_("No directory name could be guessed.\n..."))`.
fn derive_dir_name(url: &str, is_bundle: bool, bare: bool) -> Option<String> {
    let is_dir_sep = |b: u8| b == b'/';
    // C-locale `isspace()`.
    let is_space = |b: u8| matches!(b, b' ' | b'\t' | b'\n' | 0x0b | 0x0c | b'\r');
    let repo = url.as_bytes();
    let mut end = repo.len();

    // Skip scheme.
    let mut start = match url.find("://") {
        Some(i) => i + 3,
        None => 0,
    };
    // Skip authentication data, greedily up to the last '@' in the host part.
    let mut ptr = start;
    while ptr < end && !is_dir_sep(repo[ptr]) {
        if repo[ptr] == b'@' {
            start = ptr + 1;
        }
        ptr += 1;
    }

    // Strip trailing spaces, slashes and /.git.
    while start < end && (is_dir_sep(repo[end - 1]) || is_space(repo[end - 1])) {
        end -= 1;
    }
    if end > start + 5 && is_dir_sep(repo[end - 5]) && &repo[end - 4..end] == b".git" {
        end -= 5;
        while start < end && is_dir_sep(repo[end - 1]) {
            end -= 1;
        }
    }
    if end < start {
        return None;
    }

    // Strip a trailing port number when there is only a hostname: no separator,
    // but a colon. `/foo/bar:2222.git` keeps its `2222`.
    let host = &repo[start..end];
    if !host.contains(&b'/') && host.contains(&b':') {
        let mut p = end;
        while start < p && repo[p - 1].is_ascii_digit() {
            p -= 1;
        }
        if start < p && repo[p - 1] == b':' {
            end = p - 1;
        }
    }

    // Find the last component; colons count as separators too.
    let mut p = end;
    while start < p && !is_dir_sep(repo[p - 1]) && repo[p - 1] != b':' {
        p -= 1;
    }
    start = p;

    // Strip .{bundle,git}.
    let mut name = &repo[start..end];
    let suffix: &[u8] = if is_bundle { b".bundle" } else { b".git" };
    if let Some(stripped) = name.strip_suffix(suffix) {
        name = stripped;
    }
    if name.is_empty() || name == b"/" {
        return None;
    }

    let mut dir = name.to_vec();
    if bare {
        dir.extend_from_slice(b".git");
    }

    // Fold runs of control characters and whitespace into one space, dropping
    // leading and trailing ones.
    let mut out = Vec::with_capacity(dir.len());
    let mut prev_space = true;
    for mut ch in dir {
        if ch < 0x20 {
            ch = b' ';
        }
        if is_space(ch) {
            if prev_space {
                continue;
            }
            prev_space = true;
        } else {
            prev_space = false;
        }
        out.push(ch);
    }
    if prev_space {
        out.pop();
    }
    Some(String::from_utf8_lossy(&out).into_owned())
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
/// The `refs/heads/<name>` a protocol-v2 remote reported as its unborn HEAD, if it did.
///
/// The `unborn` extension answers `unborn HEAD symref-target:refs/heads/<name>` for a HEAD
/// that names a branch with no commits, which is the only way a clone can learn the
/// remote's default branch when there is nothing to fetch.
fn unborn_head_target(ref_map: &gix::remote::fetch::RefMap) -> Option<String> {
    ref_map.remote_refs.iter().find_map(|r| match r {
        gix::protocol::handshake::Ref::Unborn { full_ref_name, target }
            if full_ref_name.as_slice() == b"HEAD" =>
        {
            Some(target.to_string())
        }
        _ => None,
    })
}

/// Whether git's `mapped_refs` is empty: `wanted_peer_refs()` starts the list with the
/// remote's `HEAD` when `opts->wants_head` and the remote advertised one, and runs
/// `get_fetch_map()` over the remote's own `fetch` refspecs (builtin/clone.c:443-471). The
/// `HEAD:refs/remotes/<name>/HEAD` refspec gitoxide adds for itself is implicit and maps
/// even an unborn `HEAD`, which protocol v2 reports apart from the refs (connect.c:417-435),
/// so its mappings do not count.
fn mapped_refs_empty(ref_map: &gix::remote::fetch::RefMap, wants_head: bool) -> bool {
    let head = wants_head
        && ref_map
            .remote_refs
            .iter()
            .any(|r| r.unpack().0 == "HEAD" && !matches!(r, Ref::Unborn { .. }));
    !head
        && !ref_map
            .mappings
            .iter()
            .any(|m| matches!(m.spec_index, gix::protocol::fetch::refmap::SpecIndex::ExplicitInRemote(_)))
}

/// The advice `repo_default_branch_name(the_repository, 0)` gives when `cmd_clone()` falls back to it for a
/// remote that advertised no `HEAD` (builtin/clone.c:1554-1572, refs.c:701-710): protocol v0/v1 leaves out a
/// `HEAD` that names a missing branch, where v2's `ls-refs` reports it as `unborn`. The branch itself was
/// handed to the fetch through `with_default_branch()`.
fn advise_default_branch(git_dir: &Path, config_pairs: &[(String, String)], ref_map: &gix::remote::fetch::RefMap) {
    if ref_map.remote_refs.iter().any(|r| r.unpack().0 == "HEAD") {
        return;
    }
    clone_default_branch_name(git_dir, config_pairs, false);
}

/// `repo_default_branch_name(the_repository, quiet)` (refs.c:691-716) as `cmd_clone()` asks it: after
/// `write_config()` has put every `-c <key>=<value>` into the new repository's config (builtin/clone.c:764-791),
/// so a `-c init.defaultBranch=<name>` answers it, ahead of the global config. This clone writes those pairs
/// only once the fetch is done, so they are consulted here first.
fn clone_default_branch_name(git_dir: &Path, config_pairs: &[(String, String)], quiet: bool) -> Option<String> {
    if let Some(name) = crate::refname::default_branch_name_override() {
        return Some(name);
    }
    if let Some((_, name)) = config_pairs.iter().rev().find(|(key, _)| key.eq_ignore_ascii_case("init.defaultBranch")) {
        return Some(name.clone());
    }
    gix::open(git_dir).ok().map(|repo| crate::refname::repo_default_branch_name(&repo, quiet))
}

/// Whether `transport_get_remote_refs()` returned a non-NULL list: an `unborn` line is
/// reported through `unborn_head_target` and is not a ref (connect.c:417-435).
fn advertises_refs(ref_map: &gix::remote::fetch::RefMap) -> bool {
    ref_map
        .remote_refs
        .iter()
        .any(|r| !matches!(r, gix::protocol::handshake::Ref::Unborn { .. }))
}

/// The first directory on the way to `path` that does not exist, absolute — what
/// `strbuf_realpath()` stops at.
///
/// The final component is exempt: it is the directory about to be created. Only
/// the path *to* it has to be there already.
fn first_unreachable_component(path: &Path) -> Option<PathBuf> {
    let absolute = match path.is_absolute() {
        true => path.to_path_buf(),
        false => std::env::current_dir().ok()?.join(path),
    };
    let parent = absolute.parent()?;
    let mut walked = PathBuf::new();
    for component in parent.components() {
        walked.push(component);
        if !walked.as_os_str().is_empty() && !walked.exists() {
            return Some(walked);
        }
    }
    None
}

/// Remove whatever the local fetch wrote into `objects/pack`.
///
/// git reaches `clone_local()` instead of the transport for a local source, so neither
/// half of it — the alternates line or the copied object store — has a fetched pack to
/// live beside.
fn drop_fetched_pack(git_dir: &Path) {
    if let Ok(entries) = std::fs::read_dir(git_dir.join("objects").join("pack")) {
        for entry in entries.flatten() {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

/// The names of the files currently in `objects/pack`, which is how a pack the local
/// adoption brought in is told apart from one the transport wrote afterwards.
fn pack_dir_entries(git_dir: &Path) -> std::collections::BTreeSet<std::ffi::OsString> {
    std::fs::read_dir(git_dir.join("objects").join("pack"))
        .into_iter()
        .flatten()
        .flatten()
        .map(|entry| entry.file_name())
        .collect()
}

/// Remove everything in `objects/pack` whose name is not in `keep`.
fn remove_pack_files_other_than(
    git_dir: &Path,
    keep: &std::collections::BTreeSet<std::ffi::OsString>,
) {
    let Ok(entries) = std::fs::read_dir(git_dir.join("objects").join("pack")) else {
        return;
    };
    for entry in entries.flatten() {
        if keep.contains(&entry.file_name()) {
            continue;
        }
        let _ = std::fs::remove_file(entry.path());
    }
}

/// The modes `index-pack` leaves on the `.pack`, `.idx` and `.rev` a clone's fetch kept:
/// `0444` under the umask.
fn set_fetched_pack_modes(pack_dir: &Path) {
    use std::os::unix::fs::PermissionsExt as _;
    // SAFETY: `umask()` cannot fail, and the mask is put straight back.
    let umask = unsafe {
        let mask = libc::umask(0);
        libc::umask(mask);
        mask as u32
    };
    let Ok(entries) = std::fs::read_dir(pack_dir) else { return };
    for path in entries.flatten().map(|e| e.path()) {
        if matches!(path.extension().and_then(|e| e.to_str()), Some("pack" | "idx" | "rev")) {
            let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o444 & !umask));
        }
    }
}

/// Copy (or hardlink) the source's object store into the clone: `copy_or_link_directory()`
/// under `clone_local()`.
///
/// Called *before* the fetch, so `objects/pack` is empty when it runs and the
/// `drop_fetched_pack()` below has nothing to remove. That call stays because it states the
/// invariant `clone_local()` leaves behind — nothing but the source's own store lives here —
/// and because it is what makes a repeated adoption idempotent.
fn adopt_local_objects(source: &Path, git_dir: &Path, hardlinks: bool) -> Result<()> {
    let src_objects = local_source_objects(source);
    if !src_objects.is_dir() {
        return Ok(());
    }
    // ```c
    // /*
    //  * Refuse copying directories by default which aren't owned by us. …
    //  */
    // die_upon_dubious_ownership(NULL, NULL, src_repo);
    // ```
    //
    // (builtin/clone.c:253-265, `copy_or_link_directory()`.) Copying or
    // hardlinking another user's object store is refused before anything is
    // made, unless `safe.directory` names it.
    let src_repo = local_source_repo(source);
    if let Some(message) = crate::setup::die_upon_dubious_ownership(None, None, &src_repo) {
        crate::git_fatal!("{message}");
    }
    let dst_objects = git_dir.join("objects");

    // Nothing of the clone's own may survive beside the adopted store.
    drop_fetched_pack(git_dir);

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
                // `copy_file_with_time(dest, src, 0666)` (builtin/clone.c:338, copy.c:67-74):
                // a `0666` file under the umask, not the source's `0444`, carrying the
                // source's access and modification times.
                let copy = super::init::copy_file(&dst_path, &src_path, 0o666)?;
                let meta = std::fs::metadata(&src_path)?;
                copy.set_times(
                    std::fs::FileTimes::new().set_accessed(meta.accessed()?).set_modified(meta.modified()?),
                )?;
            }
        }
    }
    Ok(())
}

/// One entry of `wanted_peer_refs()`'s list (builtin/clone.c:443-471): a remote ref the clone's
/// refspecs selected, and the local ref it is written to when it has a `peer_ref`.
#[derive(Clone)]
struct MappedRef {
    /// The remote's name for it, or the hex id a `--revision` refspec spelled out.
    name: gix::bstr::BString,
    /// `old_oid`, as advertised.
    id: gix::ObjectId,
    /// `peer_ref->name`.
    peer: Option<gix::refs::FullName>,
}

/// What `cmd_clone()` has settled by the time it calls `update_remote_refs()` and
/// `update_head()` (builtin/clone.c:1604-1608).
struct RefWrites<'a> {
    /// `remote_name`.
    remote_name: &'a str,
    /// `option_bare`.
    bare: bool,
    /// `option_mirror`.
    mirror: bool,
    /// `option_single_branch`, as `cmd_clone()` resolved it.
    single_branch: bool,
    /// `option_tags`: cleared by `--no-tags`, `--mirror` and `--revision`
    /// (builtin/clone.c:1036-1038, :1406).
    tags: bool,
    /// `option_branch`.
    branch: Option<&'a str>,
    /// `option_rev` given, which also sets `opts.detach` (builtin/clone.c:1405-1409).
    revision: bool,
    /// `reflog_msg`: `clone: from <repo>` (builtin/clone.c:1087-1088).
    msg: &'a str,
    /// `refs != NULL`: the remote advertised anything at all.
    refs_advertised: bool,
    /// `repo_default_branch_name(the_repository, REMOTE_GUESS_HEAD_QUIET)`, which `guess_remote_head()` asks.
    default_branch: &'a str,
}

/// `wanted_peer_refs()` (builtin/clone.c:443-471) over the refs the fetch mapped: `get_fetch_map()`
/// for each refspec of `remote->fetch` in turn, which is the clone's own refspec followed by
/// `TAG_REFSPEC` when `option_tags || option_branch` appended it (builtin/clone.c:1414-1419).
///
/// gitoxide maps `TAG_REFSPEC` as an implicit refspec of its own (`Tags::All`) where it maps it at
/// all, so those entries are taken from the advertisement instead: every `refs/tags/*` maps onto
/// itself. A `--single-branch` clone applies the refspecs to its one ref only, which its explicit
/// refspec already names, tag or branch. A `--mirror --branch` clone maps every tag twice, once
/// through `+refs/*:refs/*` and once through `TAG_REFSPEC`.
fn wanted_peer_refs(ref_map: &gix::remote::fetch::RefMap, plan: &RefWrites<'_>) -> Vec<MappedRef> {
    use gix::refspec::match_group::SourceRef;
    // Each refspec maps the refs on its own, as `get_fetch_map()` does: the mappings of the fetch were folded
    // across refspecs the way `ref_remove_duplicates()` folds a fetch's (remote.c:915-947), which a clone never
    // calls, so a ref two refspecs map alike is listed twice and its creation queued twice.
    let null = gix::ObjectId::null(ref_map.object_hash);
    let mut mapped: Vec<MappedRef> = Vec::new();
    for spec in &ref_map.refspecs {
        for m in fetch_map(ref_map, spec, &null) {
            let (name, id) = match (m.item_index, &m.lhs) {
                (Some(index), _) => match ref_map.remote_refs[index].unpack() {
                    (name, Some(id), _) => (name.to_owned(), id.to_owned()),
                    (_, None, _) => continue,
                },
                (None, SourceRef::ObjectId(id)) => (id.to_string().into(), *id),
                (None, _) => continue,
            };
            // `get_fetch_map()` lets no peer outside `refs/` through (remote.c:2165-2179).
            let peer = match m.rhs {
                Some(rhs) if rhs.starts_with(b"refs/") || rhs.as_ref() == "HEAD" => {
                    gix::refs::FullName::try_from(rhs.into_owned()).ok()
                }
                Some(_) => continue,
                None => None,
            };
            mapped.push(MappedRef { name, id, peer });
        }
    }
    if (plan.tags || plan.branch.is_some()) && !plan.revision && !plan.single_branch {
        for r in &ref_map.remote_refs {
            let (name, Some(id), _) = r.unpack() else { continue };
            if !name.starts_with(b"refs/tags/") {
                continue;
            }
            mapped.push(MappedRef {
                name: name.to_owned(),
                id: id.to_owned(),
                peer: gix::refs::FullName::try_from(name.to_owned()).ok(),
            });
        }
    } else if plan.mirror && plan.single_branch && plan.branch.is_some() {
        // `TAG_REFSPEC` is applied to the one ref `find_remote_branch()` returned as well
        // (builtin/clone.c:459-466): the explicit refspec mapped a tag onto itself as
        // `+refs/*:refs/*`, and `TAG_REFSPEC` maps it a second time, which the initial transaction
        // refuses. Outside `--mirror` the explicit refspec is the one mapping a tag gets.
        let tags: Vec<MappedRef> = mapped.iter().filter(|r| r.name.starts_with(b"refs/tags/")).cloned().collect();
        mapped.extend(tags);
    }
    mapped
}

/// The refs `spec` maps out of the advertisement on its own, as `get_fetch_map()` maps them for one refspec
/// (remote.c:2134-2185); `null` stands in for the id of a ref advertised without one.
fn fetch_map<'a>(
    ref_map: &'a gix::remote::fetch::RefMap,
    spec: &'a gix::refspec::RefSpec,
    null: &'a gix::ObjectId,
) -> Vec<gix::refspec::match_group::Mapping<'a, 'a>> {
    let items = ref_map.remote_refs.iter().map(|r| {
        let (full_ref_name, target, object) = r.unpack();
        gix::refspec::match_group::Item {
            full_ref_name,
            target: target.unwrap_or(null),
            object,
        }
    });
    gix::refspec::MatchGroup::from_fetch_specs([spec.to_ref()]).match_lhs(items).mappings
}

/// `find_remote_branch()` (builtin/clone.c:422-441): `refs/heads/<branch>`, else `refs/tags/<branch>`.
fn find_remote_branch<'m>(mapped: &'m [MappedRef], branch: &str) -> Option<&'m MappedRef> {
    let find = |full: String| mapped.iter().find(|r| r.name == full.as_bytes());
    find(format!("refs/heads/{branch}")).or_else(|| find(format!("refs/tags/{branch}")))
}

/// `guess_remote_head(head, refs, REMOTE_GUESS_HEAD_QUIET)` (remote.c:2583-2638) over the mapped refs:
/// the branch a symbolic `HEAD` names, else the default branch, then `master`, then the first other
/// branch at `HEAD`'s object.
fn guess_mapped_head<'m>(head: Option<&Ref>, mapped: &'m [MappedRef], default_branch: &str) -> Option<&'m MappedRef> {
    let find = |name: &[u8]| mapped.iter().find(|r| r.name == name);
    match head? {
        Ref::Symbolic { target, .. } => find(target),
        Ref::Direct { object, .. } => {
            let at = |name: &str| find(name.as_bytes()).filter(|r| r.id == *object);
            at(&format!("refs/heads/{default_branch}"))
                .or_else(|| at("refs/heads/master"))
                .or_else(|| {
                    mapped
                        .iter()
                        .find(|r| r.name.starts_with(b"refs/heads/") && r.id == *object)
                })
        }
        Ref::Peeled { .. } | Ref::Unborn { .. } => None,
    }
}

/// Commit `edit` as a transaction of its own, the way `refs_update_ref()` and
/// `refs_update_symref()` do. A reftable store is handed the object database for the peeled
/// value it records with an annotated tag; the files backend writes the loose ref alone.
fn commit_ref_edit(repo: &gix::Repository, edit: gix::refs::transaction::RefEdit) -> Result<()> {
    use gix::refs::file::transaction::PackedRefs;
    let packed = if crate::refstore::is_reftable(repo) {
        PackedRefs::DeletionsAndNonSymbolicUpdates(Box::new(repo.objects.clone()))
    } else {
        PackedRefs::DeletionsOnly
    };
    repo.refs
        .transaction()
        .packed_refs(packed)
        .log_symref_updates()
        .prepare([edit], gix::lock::acquire::Fail::Immediately, gix::lock::acquire::Fail::Immediately)?
        .commit(repo.committer().transpose()?)?;
    Ok(())
}

/// `refs_update_ref(msg, name, new, NULL, flags, UPDATE_REFS_DIE_ON_ERR)` (refs.c), `deref` being
/// `!(flags & REF_NO_DEREF)`; `msg` is `None` for a NULL message, which logs no text at all.
fn update_ref(repo: &gix::Repository, msg: Option<&str>, name: &str, new: gix::refs::Target, deref: bool) -> Result<()> {
    use gix::refs::transaction::{Change, LogChange, PreviousValue, RefEdit, RefLog};
    commit_ref_edit(
        repo,
        RefEdit {
            change: Change::Update {
                log: LogChange {
                    mode: RefLog::AndReference,
                    force_create_reflog: false,
                    message: msg.unwrap_or_default().into(),
                },
                expected: PreviousValue::Any,
                new,
            },
            name: name.try_into()?,
            deref,
        },
    )
}

/// `refs_update_symref(refs, name, target, msg)` (refs.c): `name` made a symbolic ref to `target`
/// with `REF_NO_DEREF`, in a transaction of its own.
fn update_symref(repo: &gix::Repository, name: &str, target: &str, msg: Option<&str>) -> Result<()> {
    update_ref(repo, msg, name, gix::refs::Target::Symbolic(target.try_into()?), false)
}

/// `update_remote_refs()` (builtin/clone.c:534-570) without the connectivity check, which the fetch
/// already made:
///
/// ```c
/// if (refs) {
///         write_remote_refs(mapped_refs);
///         if (option_single_branch && option_tags)
///                 write_followtags(refs, msg);
/// }
///
/// if (remote_head_points_at && remote_head_points_at->peer_ref && !option_bare) {
///         ...
///         if (refs_update_symref(get_main_ref_store(the_repository), head_ref.buf,
///                                remote_head_points_at->peer_ref->name,
///                                msg) < 0)
///                 die(_("unable to update %s"), head_ref.buf);
/// ```
///
/// `write_remote_refs()` creates every mapped ref that has a `peer_ref` in one
/// `REF_TRANSACTION_FLAG_INITIAL` transaction (builtin/clone.c:473-498), which the files backend
/// writes straight into `packed-refs` without reflogs. `write_followtags()` then updates each
/// advertised `refs/tags/*` whose object the clone has, one `refs_update_ref()` each
/// (builtin/clone.c:500-514), and `<branch_top>HEAD` becomes a symbolic ref to the tracking ref of
/// the branch the remote's `HEAD` names.
fn update_remote_refs(
    repo: &gix::Repository,
    advertised: &[Ref],
    mapped: &[MappedRef],
    remote_head_points_at: Option<&MappedRef>,
    plan: &RefWrites<'_>,
) -> Result<()> {
    if plan.refs_advertised {
        let creations: Vec<(gix::refs::FullName, gix::ObjectId)> = mapped
            .iter()
            .filter_map(|r| Some((r.peer.clone()?, r.id)))
            .collect();
        repo.refs
            .initial_transaction_commit(
                &creations,
                Box::new(repo.objects.clone()),
                repo.committer().transpose()?,
                gix::lock::acquire::Fail::Immediately,
            )
            .map_err(|e| crate::fatal::die(e.to_string()))?;
        if plan.single_branch && plan.tags {
            for r in advertised {
                let (name, Some(id), _) = r.unpack() else { continue };
                // `odb_has_object(the_repository->objects, &ref->old_oid, 0)`: a tag outside what
                // the pack brought is not followed, and without `HAS_OBJECT_FETCH_PROMISOR` a partial
                // clone's promisor remote is not asked for it (builtin/clone.c:508).
                if !name.starts_with(b"refs/tags/") {
                    continue;
                }
                let fetch_if_missing = gix::odb::store::fetch_if_missing();
                gix::odb::store::set_fetch_if_missing(false);
                let has_object = gix::objs::Exists::exists(&repo.objects, id);
                gix::odb::store::set_fetch_if_missing(fetch_if_missing);
                if !has_object {
                    continue;
                }
                update_ref(repo, Some(plan.msg), &name.to_str_lossy(), gix::refs::Target::Object(id.to_owned()), true)?;
            }
        }
    }
    if let Some(peer) = remote_head_points_at.and_then(|r| r.peer.as_ref()).filter(|_| !plan.bare) {
        // `branch_top` is `refs/remotes/<name>/` outside a bare clone (builtin/clone.c:1298-1300).
        let head_ref = format!("refs/remotes/{}/HEAD", plan.remote_name);
        update_symref(repo, &head_ref, &peer.as_bstr().to_str_lossy(), Some(plan.msg))
            .map_err(|_| crate::fatal::die(format!("unable to update {head_ref}")))?;
    }
    Ok(())
}

/// `update_head()` (builtin/clone.c:572-613): `HEAD` linked to the branch the clone checks out and,
/// outside a bare clone, that branch created at the remote's value with its upstream configured; a
/// detached `HEAD` for `--branch <tag>`, `--revision` and a remote `HEAD` that names no branch; or
/// `HEAD` left unborn on the branch the remote's `HEAD` will become.
fn update_head(
    repo: &gix::Repository,
    our: Option<&MappedRef>,
    remote: Option<gix::ObjectId>,
    unborn: Option<&str>,
    plan: &RefWrites<'_>,
) -> Result<()> {
    let install_branch_config = |head: &str, merge: &str| {
        // `install_branch_config(0, head, remote_name, our->name)`: quiet.
        super::branch::install_tracking(
            repo,
            head,
            &(plan.remote_name.to_string(), merge.to_string(), head.to_string()),
            true,
        )
    };
    match (our, remote, unborn) {
        (Some(our), ..) if !plan.revision && our.name.starts_with(b"refs/heads/") => {
            let name = our.name.to_str_lossy();
            update_symref(repo, "HEAD", &name, None).map_err(|_| crate::fatal::die("unable to update HEAD"))?;
            if !plan.bare {
                update_ref(repo, Some(plan.msg), "HEAD", gix::refs::Target::Object(our.id), true)?;
                install_branch_config(&name["refs/heads/".len()..], &name)?;
            }
        }
        (Some(our), ..) => {
            // `lookup_commit_or_die(&our->old_oid, our->name)` (commit.c:81-91) peels to the commit
            // and says so when it had to.
            let commit = repo
                .find_object(our.id)
                .ok()
                .and_then(|object| object.peel_to_commit().ok())
                .map(|commit| commit.id)
                .ok_or_else(|| crate::fatal::die(format!("could not parse {}", our.name)))?;
            if commit != our.id {
                eprintln!("warning: {} {} is not a commit!", our.name, our.id);
            }
            update_ref(repo, Some(plan.msg), "HEAD", gix::refs::Target::Object(commit), false)?;
        }
        (None, Some(remote), _) => {
            update_ref(repo, Some(plan.msg), "HEAD", gix::refs::Target::Object(remote), false)?;
        }
        (None, None, Some(unborn)) if unborn.starts_with("refs/heads/") => {
            update_symref(repo, "HEAD", unborn, None).map_err(|_| crate::fatal::die("unable to update HEAD"))?;
            if !plan.bare {
                install_branch_config(&unborn["refs/heads/".len()..], unborn)?;
            }
        }
        (None, None, _) => {}
    }
    Ok(())
}

/// `update_remote_refs()` and `update_head()` for a clone whose fetch mapped `ref_map`, deciding
/// what `cmd_clone()` decides between them (builtin/clone.c:1538-1586):
///
/// ```c
/// remote_head = find_ref_by_name(refs, "HEAD");
/// remote_head_points_at = guess_remote_head(remote_head, mapped_refs,
///                                           REMOTE_GUESS_HEAD_QUIET);
///
/// if (option_branch) {
///         our_head_points_at = find_remote_branch(mapped_refs, option_branch);
///         [...]
/// } else if (option_rev) {
///         our_head_points_at = mapped_refs;
///         [...]
/// } else if (remote_head_points_at) {
///         our_head_points_at = remote_head_points_at;
/// } else if (remote_head) {
///         our_head_points_at = NULL;
/// } else {
///         [...]
///         if (transport_ls_refs_options.unborn_head_target &&
///             skip_prefix(transport_ls_refs_options.unborn_head_target,
///                         "refs/heads/", &branch)) {
///                 unborn_head  = xstrdup(transport_ls_refs_options.unborn_head_target);
///         } else {
///                 branch = to_free = repo_default_branch_name(the_repository, 0);
///                 unborn_head = xstrfmt("refs/heads/%s", branch);
///         }
///         [...]
///         our_head_points_at = find_remote_branch(mapped_refs, branch);
/// ```
///
/// `HEAD` is among the refs only where `cmd_clone()` asked the remote for it — `opts.wants_head`,
/// cleared by `--revision` and by `--single-branch --branch` (builtin/clone.c:1380-1385,
/// :1405-1409, :1435-1436) — or where protocol v0/v1 advertised everything anyway.
/// `fallback_branch` answers `refs/heads/` and `repo_default_branch_name(the_repository, 0)`.
fn write_clone_refs(
    repo: &gix::Repository,
    ref_map: &gix::remote::fetch::RefMap,
    v2: bool,
    plan: &RefWrites<'_>,
    fallback_branch: impl FnOnce() -> Option<String>,
) -> Result<()> {
    let mapped = wanted_peer_refs(ref_map, plan);
    let wants_head = !plan.revision && !(plan.single_branch && plan.branch.is_some());
    let remote_head = ref_map
        .remote_refs
        .iter()
        .find(|r| r.unpack().0 == "HEAD" && !matches!(r, Ref::Unborn { .. }))
        .filter(|_| wants_head || !v2);
    let remote_head_points_at = guess_mapped_head(remote_head, &mapped, plan.default_branch);

    let mut unborn_head = None;
    let our_head_points_at = if let Some(branch) = plan.branch {
        Some(find_remote_branch(&mapped, branch).ok_or_else(|| {
            crate::fatal::die(format!("Remote branch {branch} not found in upstream {}", plan.remote_name))
        })?)
    } else if plan.revision {
        mapped.first()
    } else if remote_head_points_at.is_some() {
        remote_head_points_at
    } else if remote_head.is_some() {
        None
    } else {
        let unborn = unborn_head_target(ref_map)
            .filter(|target| target.starts_with("refs/heads/"))
            .or_else(fallback_branch);
        let our = unborn
            .as_deref()
            .and_then(|head| find_remote_branch(&mapped, &head["refs/heads/".len().min(head.len())..]));
        unborn_head = unborn;
        our
    };

    update_remote_refs(repo, &ref_map.remote_refs, &mapped, remote_head_points_at, plan)?;
    let remote = remote_head.and_then(|head| head.unpack().1).map(ToOwned::to_owned);
    update_head(repo, our_head_points_at, remote, unborn_head.as_deref(), plan)
}

/// `ref_store_create_on_disk()` for a reftable clone (refs.c:2226-2244), run where git's `init_db()`
/// ran it: before any ref is written. The fetch went through the files ref store the clone was laid
/// down with and wrote no ref, so that store holds nothing but the `HEAD` `init_db()` points at the
/// default branch; it goes, and the reftable stack and its stubs come. The config gets what
/// `initialize_repository_version()` (setup.c:2444-2512) writes into a clone's still-empty config
/// before anything else: an `[extensions]` section ahead of `[core]`, and version 1.
fn create_reftable_ref_store(git_dir: &Path) -> Result<()> {
    let _ = std::fs::remove_file(git_dir.join("packed-refs"));
    let _ = std::fs::remove_dir_all(git_dir.join("refs"));
    let _ = std::fs::remove_dir_all(git_dir.join("logs"));
    gix::refs::reftable::Backend::create_on_disk(git_dir)?;
    let config = git_dir.join("config");
    let text = std::fs::read(&config)?;
    let has_extensions = gix::config::File::from_bytes_no_includes(
        &text,
        gix::config::file::Metadata::from(gix::config::Source::Local),
        Default::default(),
    )
    .is_ok_and(|f| f.section_by_key("extensions").is_ok());
    if !has_extensions {
        let mut prefixed = b"[extensions]\n\trefstorage = reftable\n".to_vec();
        prefixed.extend_from_slice(&text);
        std::fs::write(&config, prefixed)?;
    }
    let set = |key: &str, value: &str| {
        crate::config_store::set_multivar_in_file(
            &config,
            key,
            key,
            key.rfind('.').expect("the key has a section"),
            Some(value.as_bytes()),
            crate::config_store::ValuePattern::Any,
            None,
            false,
        )
        .map_err(|_| crate::fatal::die(format!("could not set '{key}' to '{value}'")))
    };
    if has_extensions {
        set("extensions.refstorage", "reftable")?;
    }
    set("core.repositoryformatversion", "1")?;
    Ok(())
}

/// Point `repo` at the reftable store [`create_reftable_ref_store`] laid down, keeping its
/// in-memory configuration (the clone's `-c` pairs among it).
fn reopen_as_reftable(repo: &mut gix::Repository) {
    let opts = gix::refs::store::init::Options {
        write_reflog: repo.refs.write_reflog,
        object_hash: repo.object_hash(),
        precompose_unicode: repo.refs.precompose_unicode,
        prohibit_windows_device_names: repo.refs.prohibit_windows_device_names,
        ref_storage: gix::refs::store::RefStorage::Reftable,
    };
    repo.refs = gix::refs::file::Store::at(repo.git_dir().to_owned(), opts);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn oid(byte: u8) -> gix::ObjectId {
        gix::ObjectId::from_bytes_or_panic(&[byte; 20])
    }

    fn advertisement(pairs: &[(u8, &str)]) -> Vec<(gix::ObjectId, Vec<u8>)> {
        pairs
            .iter()
            .map(|(id, name)| (oid(*id), name.as_bytes().to_vec()))
            .collect()
    }

    /// `guess_remote_head()` prefers the default branch and then `master`,
    /// *only* when either points where `HEAD` does — the oid check is the half
    /// that is easy to drop, and dropping it makes a clone check out a branch
    /// the remote's `HEAD` does not name.
    #[test]
    fn a_default_branch_wins_only_when_it_points_where_head_does() {
        let refs =
            advertisement(&[(1, "HEAD"), (2, "refs/heads/master"), (1, "refs/heads/main")]);
        let refs: Vec<&(gix::ObjectId, Vec<u8>)> = refs.iter().collect();

        // `main` is the default and matches, so it is taken over `master`.
        assert_eq!(guess_remote_head(&refs, "main").as_deref(), Some("refs/heads/main"));
        // `master` is the default but points elsewhere, so the fallback
        // `refs/heads/master` check fails too and the scan finds `main`.
        assert_eq!(guess_remote_head(&refs, "master").as_deref(), Some("refs/heads/main"));
    }

    /// With no branch at `HEAD`'s object, git returns NULL and the clone warns
    /// rather than checking something out.
    #[test]
    fn a_head_no_branch_points_at_guesses_nothing() {
        let refs = advertisement(&[(9, "HEAD"), (2, "refs/heads/main"), (9, "refs/tags/v1")]);
        let refs: Vec<&(gix::ObjectId, Vec<u8>)> = refs.iter().collect();
        // The tag points where HEAD does, but `starts_with("refs/heads/")`
        // excludes it — a scan that forgot the prefix would answer `refs/tags/v1`.
        assert_eq!(guess_remote_head(&refs, "main"), None);
    }

    /// A bundle carrying no `HEAD` line advertises none, which is git's
    /// `guess_remote_head(NULL, ...)`.
    #[test]
    fn an_advertisement_without_head_guesses_nothing() {
        let refs = advertisement(&[(2, "refs/heads/main")]);
        let refs: Vec<&(gix::ObjectId, Vec<u8>)> = refs.iter().collect();
        assert_eq!(guess_remote_head(&refs, "main"), None);
    }

    /// The scan takes the *first* matching branch in advertisement order, which
    /// for a bundle is the reverse of the header's order.
    #[test]
    fn the_scan_takes_the_first_matching_branch_in_advertisement_order() {
        let refs = advertisement(&[(7, "HEAD"), (7, "refs/heads/b"), (7, "refs/heads/a")]);
        let refs: Vec<&(gix::ObjectId, Vec<u8>)> = refs.iter().collect();
        assert_eq!(guess_remote_head(&refs, "main").as_deref(), Some("refs/heads/b"));
    }
}

/// C's `atoi()`: leading blanks, one sign, then decimal digits up to the first
/// other byte; no digits is 0.
pub(super) fn c_atoi(s: &str) -> i64 {
    let s = s.trim_start_matches([' ', '\t', '\n', '\x0b', '\x0c', '\r']);
    let (negative, digits) = match s.as_bytes().first() {
        Some(b'-') => (true, &s[1..]),
        Some(b'+') => (false, &s[1..]),
        _ => (false, s),
    };
    let value = digits
        .bytes()
        .take_while(u8::is_ascii_digit)
        .fold(0i64, |acc, d| acc.saturating_mul(10).saturating_add(i64::from(d - b'0')));
    if negative { -value } else { value }
}

/// `strtol(value, &end, 0)` that must consume the whole string (`*end == 0`):
/// leading blanks, one sign, and a `0x`/`0` prefix choosing hex or octal. `None`
/// for trailing junk; an empty digit run leaves `end` at the start, which is junk
/// unless the string is empty.
pub(super) fn c_strtol_full(s: &str) -> Option<i64> {
    if s.is_empty() {
        return Some(0);
    }
    let body = s.trim_start_matches([' ', '\t', '\n', '\x0b', '\x0c', '\r']);
    let (negative, rest) = match body.as_bytes().first() {
        Some(b'-') => (true, &body[1..]),
        Some(b'+') => (false, &body[1..]),
        _ => (false, body),
    };
    let (radix, digits) = if (rest.starts_with("0x") || rest.starts_with("0X"))
        && rest[2..].bytes().next().is_some_and(|b| b.is_ascii_hexdigit())
    {
        (16, &rest[2..])
    } else if rest.starts_with('0') {
        (8, rest)
    } else {
        (10, rest)
    };
    if digits.is_empty() || !digits.chars().all(|c| c.is_digit(radix)) {
        return None;
    }
    let value = i64::from_str_radix(digits, radix).unwrap_or(i64::MAX);
    Some(if negative { -value } else { value })
}

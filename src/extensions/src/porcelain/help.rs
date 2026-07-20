//! `git help` — display help information about Git.
//!
//! Stock `git help` prints tables that live in git's generated
//! `command-list.h`, compiled from `command-list.txt`. gitoxide carries no
//! equivalent table, so this port holds the same data verbatim in the
//! [`COMMON_HELP`], [`ALL_COMMANDS`], [`GUIDES`], [`USER_INTERFACES`] and
//! [`DEVELOPER_INTERFACES`] blocks below, transcribed from git 2.55.0. These
//! are static in git as well — they move only when git itself gains, drops or
//! renames a command — so they are reproduced rather than computed, and they
//! are the one part of this module that is pinned to a git version.
//!
//! Covered, byte-identically with stock git:
//!   * `git help` with no arguments — usage banner plus the common-command list.
//!   * `git help -a`/`--all` in its verbose form (git's default), including the
//!     dynamic `External commands` section (a `git-*` scan of `PATH`) and the
//!     `Command aliases` section (from `alias.*` config), plus the
//!     `--[no-]external-commands` / `--[no-]aliases` toggles.
//!   * `git help -g`/`--guides`, `--user-interfaces`, `--developer-interfaces`.
//!   * `git help <command>|<doc>` — the man-page path, reproducing git's
//!     `cmd_to_page` naming rules (`add` → `git-add`, `revisions` →
//!     `gitrevisions`, `gitk` → `gitk`) and propagating `man`'s exit code.
//!   * `git help <alias>` — prints `'<name>' is aliased to '<value>'`, and, as
//!     in git, a real command wins over an alias of the same name.
//!   * `-h`, unknown options/switches, the cmdmode conflict errors and the
//!     "doesn't take any non-option arguments" errors, with git's exact text,
//!     stream and exit code 129.
//!
//! Faithfully unsupported — each `bail!`s rather than emitting divergent
//! output: `-c`/`--config` (git prints ~1000 configuration variable names from
//! a second generated table this port does not carry), `-a --no-verbose` (the
//! column-formatted listing of git's exec-path), `-w`/`--web` and `-i`/`--info`
//! (HTML and info viewers), and a configured `help.format` / `man.viewer` other
//! than plain `man`.

use anyhow::{bail, Result};
use std::collections::{BTreeMap, BTreeSet};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, ExitStatus};

/// `git help` with no arguments.
const COMMON_HELP: &str = r#"usage: git [-v | --version] [-h | --help] [-C <path>] [-c <name>=<value>]
           [--exec-path[=<path>]] [--html-path] [--man-path] [--info-path]
           [-p | --paginate | -P | --no-pager] [--no-replace-objects] [--no-lazy-fetch]
           [--no-optional-locks] [--no-advice] [--bare] [--git-dir=<path>]
           [--work-tree=<path>] [--namespace=<name>] [--config-env=<name>=<envvar>]
           <command> [<args>]

These are common Git commands used in various situations:

start a working area (see also: git help tutorial)
   clone      Clone a repository into a new directory
   init       Create an empty Git repository or reinitialize an existing one

work on the current change (see also: git help everyday)
   add        Add file contents to the index
   mv         Move or rename a file, a directory, or a symlink
   restore    Restore working tree files
   rm         Remove files from the working tree and from the index

examine the history and state (see also: git help revisions)
   bisect     Use binary search to find the commit that introduced a bug
   diff       Show changes between commits, commit and working tree, etc
   grep       Print lines matching a pattern
   log        Show commit logs
   show       Show various types of objects
   status     Show the working tree status

grow, mark and tweak your common history
   backfill   Download missing objects in a partial clone
   branch     List, create, or delete branches
   commit     Record changes to the repository
   history    EXPERIMENTAL: Rewrite history
   merge      Join two or more development histories together
   rebase     Reapply commits on top of another base tip
   reset      Set `HEAD` or the index to a known state
   switch     Switch branches
   tag        Create, list, delete or verify tags

collaborate (see also: git help workflows)
   fetch      Download objects and refs from another repository
   pull       Fetch from and integrate with another repository or a local branch
   push       Update remote refs along with associated objects

'git help -a' and 'git help -g' list available subcommands and some
concept guides. See 'git help <command>' or 'git help <concept>'
to read about a specific subcommand or concept.
See 'git help git' for an overview of the system.
"#;

/// The static half of `git help -a`: every category from `command-list.txt`
/// with its commands and descriptions. External commands and aliases are
/// appended at runtime.
const ALL_COMMANDS: &str = r#"See 'git help <command>' to read about a specific subcommand

Main Porcelain Commands
   add                     Add file contents to the index
   am                      Apply a series of patches from a mailbox
   archive                 Create an archive of files from a named tree
   backfill                Download missing objects in a partial clone
   bisect                  Use binary search to find the commit that introduced a bug
   branch                  List, create, or delete branches
   bundle                  Move objects and refs by archive
   checkout                Switch branches or restore working tree files
   cherry-pick             Apply the changes introduced by some existing commits
   citool                  Graphical alternative to git-commit
   clean                   Remove untracked files from the working tree
   clone                   Clone a repository into a new directory
   commit                  Record changes to the repository
   describe                Give an object a human readable name based on an available ref
   diff                    Show changes between commits, commit and working tree, etc
   fetch                   Download objects and refs from another repository
   format-patch            Prepare patches for e-mail submission
   gc                      Cleanup unnecessary files and optimize the local repository
   gitk                    The Git repository browser
   grep                    Print lines matching a pattern
   gui                     A portable graphical interface to Git
   history                 EXPERIMENTAL: Rewrite history
   init                    Create an empty Git repository or reinitialize an existing one
   log                     Show commit logs
   maintenance             Run tasks to optimize Git repository data
   merge                   Join two or more development histories together
   mv                      Move or rename a file, a directory, or a symlink
   notes                   Add or inspect object notes
   pull                    Fetch from and integrate with another repository or a local branch
   push                    Update remote refs along with associated objects
   range-diff              Compare two commit ranges (e.g. two versions of a branch)
   rebase                  Reapply commits on top of another base tip
   reset                   Set `HEAD` or the index to a known state
   restore                 Restore working tree files
   revert                  Revert some existing commits
   rm                      Remove files from the working tree and from the index
   scalar                  A tool for managing large Git repositories
   shortlog                Summarize `git log` output
   show                    Show various types of objects
   sparse-checkout         Reduce your working tree to a subset of tracked files
   stash                   Stash the changes in a dirty working directory away
   status                  Show the working tree status
   submodule               Initialize, update or inspect submodules
   switch                  Switch branches
   tag                     Create, list, delete or verify tags
   worktree                Manage multiple working trees

Ancillary Commands / Manipulators
   config                  Get and set repository or global options
   fast-export             Git data exporter
   fast-import             Backend for fast Git data importers
   filter-branch           Rewrite branches
   mergetool               Run merge conflict resolution tools to resolve merge conflicts
   pack-refs               Pack heads and tags for efficient repository access
   prune                   Prune all unreachable objects from the object database
   reflog                  Manage reflog information
   refs                    Low-level access to refs
   remote                  Manage set of tracked repositories
   repack                  Pack unpacked objects in a repository
   replace                 Create, list, delete refs to replace objects

Ancillary Commands / Interrogators
   annotate                Annotate file lines with commit information
   blame                   Show what revision and author last modified each line of a file
   bugreport               Collect information for user to file a bug report
   count-objects           Count unpacked number of objects and their disk consumption
   diagnose                Generate a zip archive of diagnostic information
   difftool                Show changes using common diff tools
   fsck                    Verifies the connectivity and validity of the objects in the database
   gitweb                  Git web interface (web frontend to Git repositories)
   help                    Display help information about Git
   instaweb                Instantly browse your working repository in gitweb
   merge-tree              Perform merge without touching index or working tree
   rerere                  Reuse recorded resolution of conflicted merges
   show-branch             Show branches and their commits
   verify-commit           Check the GPG signature of commits
   verify-tag              Check the GPG signature of tags
   version                 Display version information about Git
   whatchanged             Show logs with differences each commit introduces

Interacting with Others
   archimport              Import a GNU Arch repository into Git
   cvsexportcommit         Export a single commit to a CVS checkout
   cvsimport               Salvage your data out of another SCM people love to hate
   cvsserver               A CVS server emulator for Git
   imap-send               Send a collection of patches from stdin to an IMAP folder
   p4                      Import from and submit to Perforce repositories
   quiltimport             Applies a quilt patchset onto the current branch
   request-pull            Generates a summary of pending changes
   send-email              Send a collection of patches as emails
   svn                     Bidirectional operation between a Subversion repository and Git

Low-level Commands / Manipulators
   apply                   Apply a patch to files and/or to the index
   checkout-index          Copy files from the index to the working tree
   commit-graph            Write and verify Git commit-graph files
   commit-tree             Create a new commit object
   hash-object             Compute object ID and optionally create an object from a file
   index-pack              Build pack index file for an existing packed archive
   merge-file              Run a three-way file merge
   merge-index             Run a merge for files needing merging
   mktag                   Creates a tag object with extra validation
   mktree                  Build a tree-object from ls-tree formatted text
   multi-pack-index        Write and verify multi-pack-indexes
   pack-objects            Create a packed archive of objects
   prune-packed            Remove extra objects that are already in pack files
   read-tree               Reads tree information into the index
   replay                  EXPERIMENTAL: Replay commits on a new base, works with bare repos too
   symbolic-ref            Read, modify and delete symbolic refs
   unpack-objects          Unpack objects from a packed archive
   update-index            Register file contents in the working tree to the index
   update-ref              Update the object name stored in a ref safely
   write-tree              Create a tree object from the current index

Low-level Commands / Interrogators
   cat-file                Provide contents or details of repository objects
   cherry                  Find commits yet to be applied to upstream
   diff-files              Compares files in the working tree and the index
   diff-index              Compare a tree to the working tree or index
   diff-pairs              Compare the content and mode of provided blob pairs
   diff-tree               Compares the content and mode of blobs found via two tree objects
   for-each-ref            Output information on each ref
   for-each-repo           Run a Git command on a list of repositories
   format-rev              EXPERIMENTAL: Pretty format revisions on demand
   get-tar-commit-id       Extract commit ID from an archive created using git-archive
   last-modified           EXPERIMENTAL: Show when files were last modified
   ls-files                Show information about files in the index and the working tree
   ls-remote               List references in a remote repository
   ls-tree                 List the contents of a tree object
   merge-base              Find as good common ancestors as possible for a merge
   name-rev                Find symbolic names for given revs
   pack-redundant          Find redundant pack files
   repo                    Retrieve information about the repository
   rev-list                Lists commit objects in reverse chronological order
   rev-parse               Pick out and massage parameters
   show-index              Show packed archive index
   show-ref                List references in a local repository
   unpack-file             Creates a temporary file with a blob's contents
   var                     Show a Git logical variable
   verify-pack             Validate packed Git archive files

Low-level Commands / Syncing Repositories
   daemon                  A really simple server for Git repositories
   fetch-pack              Receive missing objects from another repository
   http-backend            Server side implementation of Git over HTTP
   send-pack               Push objects over Git protocol to another repository
   update-server-info      Update auxiliary info file to help dumb servers

Low-level Commands / Internal Helpers
   check-attr              Display gitattributes information
   check-ignore            Debug gitignore / exclude files
   check-mailmap           Show canonical names and email addresses of contacts
   check-ref-format        Ensures that a reference name is well formed
   column                  Display data in columns
   credential              Retrieve and store user credentials
   credential-cache        Helper to temporarily store passwords in memory
   credential-store        Helper to store credentials on disk
   fmt-merge-msg           Produce a merge commit message
   hook                    Run Git hooks
   interpret-trailers      Add or parse structured information in commit messages
   mailinfo                Extracts patch and authorship from a single e-mail message
   mailsplit               Simple UNIX mbox splitter program
   merge-one-file          The standard helper program to use with git-merge-index
   patch-id                Compute unique IDs for patches
   sh-i18n                 Git's i18n setup code for shell scripts
   sh-setup                Common Git shell script setup code
   stripspace              Remove unnecessary whitespace
   url-parse               Parse and extract git URL components

User-facing repository, command and file interfaces
   attributes              Defining attributes per path
   cli                     Git command-line interface and conventions
   hooks                   Hooks used by Git
   ignore                  Specifies intentionally untracked files to ignore
   mailmap                 Map author/committer names and/or E-Mail addresses
   modules                 Defining submodule properties
   repository-layout       Git Repository Layout
   revisions               Specifying revisions and ranges for Git

Developer-facing file formats, protocols and other interfaces
   format-bundle           The bundle file format
   format-chunk            Chunk-based file formats
   format-commit-graph     Git commit-graph format
   format-index            Git index format
   format-pack             Git pack format
   format-signature        Git cryptographic signature formats
   protocol-capabilities   Protocol v0 and v1 capabilities
   protocol-common         Things common to various protocols
   protocol-http           Git HTTP-based protocols
   protocol-pack           How packs are transferred over-the-wire
   protocol-v2             Git Wire Protocol, Version 2
"#;

/// `git help -g`.
const GUIDES: &str = r#"The Git concept guides are:
   core-tutorial    A Git core tutorial for developers
   credentials      Providing usernames and passwords to Git
   cvs-migration    Git for CVS users
   diffcore         Tweaking diff output
   everyday         A useful minimum set of commands for Everyday Git
   faq              Frequently asked questions about using Git
   glossary         A Git Glossary
   namespaces       Git namespaces
   remote-helpers   Helper programs to interact with remote repositories
   submodules       Mounting one repository inside another
   tutorial         A tutorial introduction to Git
   tutorial-2       A tutorial introduction to Git: part two
   workflows        An overview of recommended workflows with Git

'git help -a' and 'git help -g' list available subcommands and some
concept guides. See 'git help <command>' or 'git help <concept>'
to read about a specific subcommand or concept.
See 'git help git' for an overview of the system.
"#;

/// `git help --user-interfaces`.
const USER_INTERFACES: &str = r#"User-facing repository, command and file interfaces:
   attributes          Defining attributes per path
   cli                 Git command-line interface and conventions
   hooks               Hooks used by Git
   ignore              Specifies intentionally untracked files to ignore
   mailmap             Map author/committer names and/or E-Mail addresses
   modules             Defining submodule properties
   repository-layout   Git Repository Layout
   revisions           Specifying revisions and ranges for Git

"#;

/// `git help --developer-interfaces`.
const DEVELOPER_INTERFACES: &str = r#"File formats, protocols and other developer interfaces:
   format-bundle           The bundle file format
   format-chunk            Chunk-based file formats
   format-commit-graph     Git commit-graph format
   format-index            Git index format
   format-pack             Git pack format
   format-signature        Git cryptographic signature formats
   protocol-capabilities   Protocol v0 and v1 capabilities
   protocol-common         Things common to various protocols
   protocol-http           Git HTTP-based protocols
   protocol-pack           How packs are transferred over-the-wire
   protocol-v2             Git Wire Protocol, Version 2

"#;

/// The usage block git prints for `-h`, for unknown options, and after the
/// fatal argument-combination errors. Ends with a blank line, as git's does.
const USAGE: &str = r#"usage: git help [-a|--all] [--[no-]verbose] [--[no-]external-commands] [--[no-]aliases]
   or: git help [[-i|--info] [-m|--man] [-w|--web]] [<command>|<doc>]
   or: git help [-g|--guides]
   or: git help [-c|--config]
   or: git help [--user-interfaces]
   or: git help [--developer-interfaces]

    -a, --all             print all available commands
    --[no-]external-commands
                          show external commands in --all
    --[no-]aliases        show aliases in --all
    -m, --[no-]man        show man page
    -w, --[no-]web        show manual in web browser
    -i, --[no-]info       show info page
    -v, --[no-]verbose    print command description
    -g, --guides          print list of useful guides
    --user-interfaces     print list of user-facing repository, command and file interfaces
    --developer-interfaces
                          print list of file formats, protocols and other developer interfaces
    -c, --config          print all configuration variable names

"#;

/// Column width of the name field in `git help -a`, i.e. git's `longest + 3`.
/// The longest name in [`ALL_COMMANDS`] is `protocol-capabilities` (21).
const ALL_WIDTH: usize = 24;

/// [`ALL_COMMANDS`] categories that list documentation topics rather than
/// commands. Their entries are not git commands, so `git help revisions`
/// resolves to `gitrevisions`, not `git-revisions`.
const DOC_CATEGORIES: [&str; 2] = [
    "User-facing repository, command and file interfaces",
    "Developer-facing file formats, protocols and other interfaces",
];

/// The mutually exclusive listing modes — git's `OPT_CMDMODE` group.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    All,
    Guides,
    Config,
    UserInterfaces,
    DeveloperInterfaces,
}

impl Mode {
    /// The spelling git uses in `options '%s' and '%s' cannot be used
    /// together`, which names the short form when one exists.
    fn short_spelling(self) -> &'static str {
        match self {
            Mode::All => "-a",
            Mode::Guides => "-g",
            Mode::Config => "-c",
            Mode::UserInterfaces => "--user-interfaces",
            Mode::DeveloperInterfaces => "--developer-interfaces",
        }
    }

    /// The spelling git uses in its `fatal:` messages, always the long form.
    fn long_spelling(self) -> &'static str {
        match self {
            Mode::All => "--all",
            Mode::Guides => "--guides",
            Mode::Config => "--config",
            Mode::UserInterfaces => "--user-interfaces",
            Mode::DeveloperInterfaces => "--developer-interfaces",
        }
    }
}

/// `git help` — display help information about Git.
pub fn help(args: &[String]) -> Result<ExitCode> {
    let mut mode: Option<Mode> = None;
    let mut viewer: Option<&'static str> = None; // last of --man/--web/--info wins
    let mut verbose = true;
    let mut externals = true;
    let mut aliases = true;
    let mut list_toggle_seen = false; // --[no-]external-commands / --[no-]aliases
    let mut rest: Vec<&str> = Vec::new();
    let mut no_more_opts = false;

    for a in args {
        if no_more_opts {
            rest.push(a.as_str());
            continue;
        }
        if a == "--" {
            no_more_opts = true;
            continue;
        }
        if let Some(long) = a.strip_prefix("--") {
            match long {
                "help" => {
                    print!("{USAGE}");
                    return Ok(ExitCode::from(129));
                }
                "all" | "guides" | "config" | "user-interfaces" | "developer-interfaces" => {
                    let m = match long {
                        "all" => Mode::All,
                        "guides" => Mode::Guides,
                        "config" => Mode::Config,
                        "user-interfaces" => Mode::UserInterfaces,
                        _ => Mode::DeveloperInterfaces,
                    };
                    if let Some(code) = set_mode(&mut mode, m) {
                        return Ok(code);
                    }
                }
                "verbose" => verbose = true,
                "no-verbose" => verbose = false,
                "external-commands" | "no-external-commands" => {
                    externals = long == "external-commands";
                    list_toggle_seen = true;
                }
                "aliases" | "no-aliases" => {
                    aliases = long == "aliases";
                    list_toggle_seen = true;
                }
                "man" => viewer = Some("--man"),
                "web" => viewer = Some("--web"),
                "info" => viewer = Some("--info"),
                "no-man" | "no-web" | "no-info" => viewer = None,
                // Kept by git as a hidden no-op for backwards compatibility.
                "exclude-guides" => {}
                _ => return Ok(unknown_option(long, false)),
            }
            continue;
        }
        if a.len() > 1 && a.starts_with('-') {
            // parse-options splits a short-option cluster like `-av`.
            for c in a[1..].chars() {
                let m = match c {
                    'h' => {
                        print!("{USAGE}");
                        return Ok(ExitCode::from(129));
                    }
                    'a' => Some(Mode::All),
                    'g' => Some(Mode::Guides),
                    'c' => Some(Mode::Config),
                    'v' => {
                        verbose = true;
                        None
                    }
                    'm' => {
                        viewer = Some("--man");
                        None
                    }
                    'w' => {
                        viewer = Some("--web");
                        None
                    }
                    'i' => {
                        viewer = Some("--info");
                        None
                    }
                    _ => return Ok(unknown_option(&c.to_string(), true)),
                };
                if let Some(m) = m {
                    if let Some(code) = set_mode(&mut mode, m) {
                        return Ok(code);
                    }
                }
            }
            continue;
        }
        rest.push(a.as_str());
    }

    // git's post-parse validation, in git's own order.
    if let Some(m) = mode {
        if !rest.is_empty() {
            return Ok(fatal(&format!(
                "the '{}' option doesn't take any non-option arguments",
                m.long_spelling()
            )));
        }
        if let Some(v) = viewer {
            return Ok(fatal(&format!(
                "options '{}' and '{v}' cannot be used together",
                m.long_spelling()
            )));
        }
    }
    if list_toggle_seen && mode != Some(Mode::All) {
        return Ok(fatal(
            "the '--no-[external-commands|aliases]' options can only be used with '--all'",
        ));
    }

    match mode {
        Some(Mode::All) => {
            if !verbose {
                bail!(
                    "`git help --all --no-verbose` is not supported: it column-formats the \
                     contents of git's exec-path, which the vendored crates have no notion of"
                );
            }
            print!("{}", render_all(externals, aliases));
            Ok(ExitCode::SUCCESS)
        }
        Some(Mode::Guides) => {
            print!("{GUIDES}");
            Ok(ExitCode::SUCCESS)
        }
        Some(Mode::UserInterfaces) => {
            print!("{USER_INTERFACES}");
            Ok(ExitCode::SUCCESS)
        }
        Some(Mode::DeveloperInterfaces) => {
            print!("{DEVELOPER_INTERFACES}");
            Ok(ExitCode::SUCCESS)
        }
        Some(Mode::Config) => bail!(
            "`git help --config` is not supported: it prints git's generated list of every \
             configuration variable name, a table this port does not carry"
        ),
        None => {
            let Some(topic) = rest.first() else {
                print!("{COMMON_HELP}");
                return Ok(ExitCode::SUCCESS);
            };
            match viewer {
                Some("--web") => bail!("`git help --web` is not supported: no HTML doc viewer"),
                Some("--info") => bail!("`git help --info` is not supported: no info viewer"),
                _ => show_help_for(topic),
            }
        }
    }
}

/// Record a listing mode. A second, different one is rejected exactly as git's
/// `OPT_CMDMODE` does: one `error:` line on stderr, exit 129, no usage block.
fn set_mode(slot: &mut Option<Mode>, new: Mode) -> Option<ExitCode> {
    if let Some(prev) = *slot {
        if prev != new {
            eprintln!(
                "error: options '{}' and '{}' cannot be used together",
                new.short_spelling(),
                prev.short_spelling()
            );
            return Some(ExitCode::from(129));
        }
    }
    *slot = Some(new);
    None
}

/// git's parse-options response to an unrecognised option: one `error:` line,
/// then the usage block, exit 129. Long options are "option", short ones
/// "switch", matching parse-options' own wording.
fn unknown_option(name: &str, short: bool) -> ExitCode {
    let kind = if short { "switch" } else { "option" };
    eprint!("error: unknown {kind} `{name}'\n{USAGE}");
    ExitCode::from(129)
}

/// git's `usage_msg_optf` path: a `fatal:` line, a blank line, the usage block,
/// exit 129.
fn fatal(msg: &str) -> ExitCode {
    eprint!("fatal: {msg}\n\n{USAGE}");
    ExitCode::from(129)
}

/// The full `git help -a` output: the static category tables, then the
/// `External commands` and `Command aliases` sections when enabled.
fn render_all(externals: bool, aliases: bool) -> String {
    let mut out = String::from(ALL_COMMANDS);

    if externals {
        let names = external_commands();
        if !names.is_empty() {
            out.push_str("\nExternal commands\n");
            for n in &names {
                out.push_str(&format!("   {n}\n"));
            }
        }
    }

    if aliases {
        let list = alias_list();
        if !list.is_empty() {
            out.push_str("\nCommand aliases\n");
            for (name, value) in &list {
                out.push_str(&format!("   {name:<width$}{value}\n", width = ALL_WIDTH));
            }
        }
    }

    out
}

/// `git-*` executables reachable through `PATH` that are neither known git
/// commands nor shipped in git's exec-path — git lists the latter under their
/// own categories instead.
fn external_commands() -> BTreeSet<String> {
    let known = command_names();
    let exec_path = git_exec_path();
    let mut found = BTreeSet::new();

    let Some(path) = std::env::var_os("PATH") else {
        return found;
    };
    for dir in std::env::split_paths(&path) {
        if exec_path.as_deref() == Some(dir.as_path()) {
            continue;
        }
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let file_name = entry.file_name();
            let Some(name) = file_name.to_str().and_then(|n| n.strip_prefix("git-")) else {
                continue;
            };
            if name.is_empty() || known.contains(name) || !is_executable_file(&entry.path()) {
                continue;
            }
            found.insert(name.to_string());
        }
    }
    found
}

/// Every command name listed in [`ALL_COMMANDS`], excluding the documentation
/// categories named in [`DOC_CATEGORIES`].
fn command_names() -> BTreeSet<String> {
    let mut names = BTreeSet::new();
    let mut in_doc_category = false;
    for line in ALL_COMMANDS.lines() {
        if let Some(entry) = line.strip_prefix("   ") {
            if !in_doc_category {
                if let Some(name) = entry.split_whitespace().next() {
                    names.insert(name.to_string());
                }
            }
        } else if !line.is_empty() && !line.starts_with("See ") {
            in_doc_category = DOC_CATEGORIES.contains(&line);
        }
    }
    names
}

/// Whether `path` is a regular file carrying an execute bit — git's criterion
/// for a candidate `git-*` helper.
fn is_executable_file(path: &Path) -> bool {
    let Ok(meta) = std::fs::metadata(path) else {
        return false;
    };
    if !meta.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        meta.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        true
    }
}

/// git's exec-path: `GIT_EXEC_PATH` when set, else whatever the installed git
/// reports. `None` when neither is available, in which case nothing is excluded
/// from the `PATH` scan.
fn git_exec_path() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("GIT_EXEC_PATH") {
        if !p.is_empty() {
            return Some(PathBuf::from(p));
        }
    }
    let out = Command::new("git").arg("--exec-path").output().ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8(out.stdout).ok()?;
    let text = text.trim_end_matches(['\n', '\r']);
    (!text.is_empty()).then(|| PathBuf::from(text))
}

/// Every `alias.<name>` in the effective configuration, sorted by name, the
/// last definition winning as git's config resolution does. Outside a
/// repository only the global files are consulted, which is where git looks
/// too.
fn alias_list() -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();

    let repo = gix::discover(".").ok();
    let snapshot = repo.as_ref().map(|r| r.config_snapshot());
    let globals;
    let file = match snapshot.as_ref() {
        Some(s) => s.plumbing(),
        None => match gix::config::File::from_globals() {
            Ok(f) => {
                globals = f;
                &globals
            }
            Err(_) => return out,
        },
    };

    let Some(sections) = file.sections_by_name("alias") else {
        return out;
    };
    for section in sections {
        for name in section.value_names() {
            if let Some(value) = section.values(&name).last() {
                // git's config parser lower-cases value names before they reach
                // its alias listing, so `[alias] Foo` prints as `foo`.
                out.insert(name.to_lowercase(), value.to_string());
            }
        }
    }
    out
}

/// `git help <command>|<doc>|<alias>`: an alias prints its expansion, anything
/// else opens the corresponding man page.
fn show_help_for(topic: &str) -> Result<ExitCode> {
    let is_command = command_names().contains(topic) || external_commands().contains(topic);

    // git resolves a real command before consulting aliases, so an alias that
    // shadows a builtin still shows the builtin's manual.
    if !is_command {
        if let Some(value) = alias_list().get(topic) {
            println!("'{topic}' is aliased to '{value}'");
            return Ok(ExitCode::SUCCESS);
        }
    }

    reject_unsupported_viewer_config()?;

    // git's cmd_to_page().
    let page = if topic.starts_with("git") {
        topic.to_string()
    } else if is_command {
        format!("git-{topic}")
    } else {
        format!("git{topic}")
    };

    std::io::stdout().flush().ok();
    let status = Command::new("man")
        .arg(&page)
        .status()
        .map_err(|e| anyhow::anyhow!("failed to run man: {e}"))?;
    Ok(ExitCode::from(exit_status_code(status)))
}

/// This port only drives plain `man`, so a configured HTML/info format or a
/// custom man viewer is rejected instead of silently ignored.
fn reject_unsupported_viewer_config() -> Result<()> {
    let Ok(repo) = gix::discover(".") else {
        return Ok(());
    };
    let config = repo.config_snapshot();
    if let Some(format) = config.string("help.format") {
        if format.to_string() != "man" {
            bail!("help.format={format} is not supported: only the plain `man` viewer is");
        }
    }
    if let Some(viewer) = config.string("man.viewer") {
        if viewer.to_string() != "man" {
            bail!("man.viewer={viewer} is not supported: only the plain `man` viewer is");
        }
    }
    Ok(())
}

/// A child's exit status as git reports it: its exit code, or 128 + signal.
fn exit_status_code(status: ExitStatus) -> u8 {
    if let Some(code) = status.code() {
        return code as u8;
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        if let Some(sig) = status.signal() {
            return 128u8.wrapping_add(sig as u8);
        }
    }
    1
}

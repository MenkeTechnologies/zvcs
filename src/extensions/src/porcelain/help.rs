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
//!   * `git help -c`/`--config` — every configuration variable name plus git's
//!     `'git help config' for more information` trailer, from the [`CONFIG_VARS`]
//!     block transcribed from git 2.55.0.
//!   * `git help <command>|<doc>` — the man-page path, reproducing git's
//!     `cmd_to_page` naming rules (`add` → `git-add`, `revisions` →
//!     `gitrevisions`, `gitk` → `gitk`) and propagating `man`'s exit code.
//!   * `git help <alias>` — prints `'<name>' is aliased to '<value>'`, and, as
//!     in git, a real command wins over an alias of the same name.
//!   * `-i`/`--info` — `show_info_page()`: `INFOPATH` is pointed at the install's
//!     info directory and `info gitman <page>` runs, propagating its status; with
//!     no `info` on `$PATH` git's `fatal: no info viewer handled the request`
//!     (exit 128) is what is left.
//!   * `-w`/`--web` — `show_html_page()`: `get_html_page_path()` resolves
//!     `<html-path>/<page>.html` (honoring `help.htmlpath`, and taking a `://`
//!     path on trust as git does), dying `'%s/%s.html': documentation file not
//!     found.` when it is absent, then `open_html()` hands the path to
//!     `git web--browse -c help.browser`. The HTML set the lookup lands in is
//!     [`crate::superset::htmldoc`], generated from the same tables the rest of
//!     this module prints.
//!   * `help.format` (`man`/`info`/`web`/`html`, with git's
//!     `unrecognized help format '%s'` on anything else) and `help.htmlpath`,
//!     read after the command line so an explicit `-m`/`-w`/`-i` still wins.
//!   * `-h`, unknown options/switches, the cmdmode conflict errors and the
//!     "doesn't take any non-option arguments" errors, with git's exact text,
//!     stream and exit code 129.
//!
//! Faithfully unsupported — each `bail!`s rather than emitting divergent
//! output: `-a --no-verbose` (git scans the git-core exec-path directory for
//! its individual `git-*` helper binaries and column-formats that on-disk set,
//! which a single-binary port cannot reproduce byte-for-byte), and a configured
//! `man.viewer` other than plain `man`.

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

/// `git help -c`/`--config`: every configuration variable name git knows,
/// sorted, one per line. git builds this at runtime from `config_name_list`
/// in its generated `config-list.h` (compiled from `Documentation/config/*.txt`
/// by `generate-configlist.sh`), a table gitoxide does not carry, so — like
/// [`ALL_COMMANDS`] and the guide blocks — it is transcribed verbatim from git
/// 2.55.0 and moves only when git itself adds, drops or renames a variable.
/// Wildcard/placeholder segments (`alias.*`, `branch.<name>.remote`) are kept
/// exactly as git emits them. The `'git help config' for more information`
/// trailer git prints after the list is appended in code, not stored here.
const CONFIG_VARS: &str = r#"add.ignoreErrors
advice.addEmbeddedRepo
advice.addEmptyPathspec
advice.addIgnoredFile
advice.amWorkDir
advice.ambiguousFetchRefspec
advice.checkoutAmbiguousRemoteBranchName
advice.commitBeforeMerge
advice.defaultBranchName
advice.detachedHead
advice.diverging
advice.fetchRemoteHEADWarn
advice.fetchShowForcedUpdates
advice.forceDeleteBranch
advice.graftFileDeprecated
advice.ignoredHook
advice.implicitIdentity
advice.mergeConflict
advice.nestedTag
advice.objectNameWarning
advice.pushAlreadyExists
advice.pushFetchFirst
advice.pushNeedsForce
advice.pushNonFFCurrent
advice.pushNonFFMatching
advice.pushNonFastForward
advice.pushRefNeedsUpdate
advice.pushUnqualifiedRefName
advice.pushUpdateRejected
advice.rebaseTodoError
advice.refSyntax
advice.resetNoRefresh
advice.resolveConflict
advice.rmHints
advice.sequencerInUse
advice.setUpstreamFailure
advice.skippedCherryPicks
advice.sparseIndexExpanded
advice.statusAheadBehindWarning
advice.statusHints
advice.statusUoption
advice.submoduleAlternateErrorStrategyDie
advice.submoduleMergeConflict
advice.submodulesNotUpdated
advice.suggestDetachingHead
advice.updateSparsePath
advice.waitingForEditor
advice.worktreeAddOrphan
alias.*
alias.*.command
am.keepcr
am.messageId
am.threeWay
apply.ignoreWhitespace
apply.whitespace
attr.tree
author.email
author.name
bitmapPseudoMerge.<name>.decay
bitmapPseudoMerge.<name>.maxMerges
bitmapPseudoMerge.<name>.pattern
bitmapPseudoMerge.<name>.sampleRate
bitmapPseudoMerge.<name>.stableSize
bitmapPseudoMerge.<name>.stableThreshold
bitmapPseudoMerge.<name>.threshold
blame.blankBoundary
blame.coloring
blame.date
blame.ignoreRevsFile
blame.markIgnoredLines
blame.markUnblamableLines
blame.showEmail
blame.showRoot
branch.<name>.description
branch.<name>.merge
branch.<name>.mergeOptions
branch.<name>.pushRemote
branch.<name>.rebase
branch.<name>.remote
branch.autoSetupMerge
branch.autoSetupRebase
branch.sort
browser.<tool>.cmd
browser.<tool>.path
bundle.*
bundle.<id>.*
bundle.<id>.uri
bundle.heuristic
bundle.mode
bundle.version
checkout.defaultRemote
checkout.guess
checkout.thresholdForParallelism
checkout.workers
clean.requireForce
clone.defaultRemoteName
clone.filterSubmodules
clone.rejectShallow
color.advice
color.advice.hint
color.blame.highlightRecent
color.blame.repeatedLines
color.branch
color.branch.current
color.branch.local
color.branch.plain
color.branch.remote
color.branch.reset
color.branch.upstream
color.branch.worktree
color.decorate.HEAD
color.decorate.branch
color.decorate.grafted
color.decorate.remoteBranch
color.decorate.stash
color.decorate.tag
color.diff
color.diff.commit
color.diff.context
color.diff.contextBold
color.diff.contextDimmed
color.diff.frag
color.diff.func
color.diff.meta
color.diff.new
color.diff.newBold
color.diff.newDimmed
color.diff.newMoved
color.diff.newMovedAlternative
color.diff.newMovedAlternativeDimmed
color.diff.newMovedDimmed
color.diff.old
color.diff.oldBold
color.diff.oldDimmed
color.diff.oldMoved
color.diff.oldMovedAlternative
color.diff.oldMovedAlternativeDimmed
color.diff.oldMovedDimmed
color.diff.plain
color.diff.whitespace
color.grep
color.grep.column
color.grep.context
color.grep.filename
color.grep.function
color.grep.lineNumber
color.grep.match
color.grep.matchContext
color.grep.matchSelected
color.grep.selected
color.grep.separator
color.interactive
color.interactive.error
color.interactive.header
color.interactive.help
color.interactive.plain
color.interactive.prompt
color.interactive.reset
color.pager
color.push
color.push.error
color.remote
color.remote.error
color.remote.hint
color.remote.success
color.remote.warning
color.showBranch
color.status
color.status.added
color.status.branch
color.status.changed
color.status.header
color.status.localBranch
color.status.noBranch
color.status.remoteBranch
color.status.unmerged
color.status.untracked
color.status.updated
color.transport
color.transport.rejected
color.ui
column.branch
column.clean
column.status
column.tag
column.ui
commit.cleanup
commit.gpgSign
commit.status
commit.template
commit.verbose
commitGraph.changedPaths
commitGraph.changedPathsVersion
commitGraph.generationVersion
commitGraph.maxNewFilters
commitGraph.readChangedPaths
committer.email
committer.name
completion.commands
core.abbrev
core.alternateRefsCommand
core.alternateRefsPrefixes
core.askPass
core.attributesFile
core.autocrlf
core.bare
core.bigFileThreshold
core.checkRoundtripEncoding
core.checkStat
core.commentChar
core.commentString
core.commitGraph
core.compression
core.createObject
core.deltaBaseCacheLimit
core.editor
core.eol
core.excludesFile
core.fileMode
core.filesRefLockTimeout
core.fsmonitor
core.fsmonitorHookVersion
core.fsync
core.fsyncMethod
core.fsyncObjectFiles
core.gitProxy
core.hideDotFiles
core.hooksPath
core.ignoreCase
core.ignoreStat
core.lockfilePid
core.logAllRefUpdates
core.looseCompression
core.maxTreeDepth
core.multiPackIndex
core.notesRef
core.packedGitLimit
core.packedGitWindowSize
core.packedRefsTimeout
core.pager
core.precomposeUnicode
core.preferSymlinkRefs
core.preloadIndex
core.protectHFS
core.protectNTFS
core.quotePath
core.repositoryFormatVersion
core.safecrlf
core.sharedRepository
core.sparseCheckout
core.sparseCheckoutCone
core.splitIndex
core.sshCommand
core.symlinks
core.trustctime
core.unsetenvvars
core.untrackedCache
core.useReplaceRefs
core.warnAmbiguousRefs
core.whitespace
core.worktree
credential.<url>.*
credential.helper
credential.interactive
credential.protectProtocol
credential.sanitizePrompt
credential.useHttpPath
credential.username
credentialCache.ignoreSIGHUP
credentialStore.lockTimeoutMS
diff.<driver>.binary
diff.<driver>.cachetextconv
diff.<driver>.command
diff.<driver>.textconv
diff.<driver>.trustExitCode
diff.<driver>.wordRegex
diff.<driver>.xfuncname
diff.algorithm
diff.autoRefreshIndex
diff.colorMoved
diff.colorMovedWS
diff.context
diff.dirstat
diff.dstPrefix
diff.external
diff.guitool
diff.ignoreSubmodules
diff.indentHeuristic
diff.interHunkContext
diff.mnemonicPrefix
diff.noPrefix
diff.orderFile
diff.relative
diff.renameLimit
diff.renames
diff.srcPrefix
diff.statGraphWidth
diff.statNameWidth
diff.submodule
diff.suppressBlankEmpty
diff.tool
diff.trustExitCode
diff.wordRegex
diff.wsErrorHighlight
difftool.<tool>.cmd
difftool.<tool>.path
difftool.guiDefault
difftool.prompt
difftool.trustExitCode
extensions.*
fastimport.unpackLimit
feature.*
feature.experimental
feature.manyFiles
fetch.all
fetch.bundleCreationToken
fetch.bundleURI
fetch.fsck.<msg-id>
fetch.fsck.skipList
fetch.fsckObjects
fetch.negotiationAlgorithm
fetch.output
fetch.parallel
fetch.prune
fetch.pruneTags
fetch.recurseSubmodules
fetch.showForcedUpdates
fetch.unpackLimit
fetch.writeCommitGraph
filter.<driver>.clean
filter.<driver>.smudge
format.attach
format.cc
format.commitListFormat
format.coverFromDescription
format.coverLetter
format.encodeEmailHeaders
format.filenameMaxLength
format.forceInBodyFrom
format.from
format.headers
format.mboxrd
format.noprefix
format.notes
format.numbered
format.outputDirectory
format.pretty
format.signOff
format.signature
format.signatureFile
format.subjectPrefix
format.suffix
format.thread
format.to
format.useAutoBase
fsck.badDate
fsck.badDateOverflow
fsck.badEmail
fsck.badFilemode
fsck.badGpgsig
fsck.badHeadTarget
fsck.badHeaderContinuation
fsck.badName
fsck.badObjectSha1
fsck.badPackedRefEntry
fsck.badPackedRefHeader
fsck.badParentSha1
fsck.badRefContent
fsck.badRefFiletype
fsck.badRefName
fsck.badRefOid
fsck.badReferentName
fsck.badReftableTableName
fsck.badTagName
fsck.badTimezone
fsck.badTree
fsck.badTreeSha1
fsck.badType
fsck.duplicateEntries
fsck.emptyName
fsck.emptyPackedRefsFile
fsck.extraHeaderEntry
fsck.fullPathname
fsck.gitattributesBlob
fsck.gitattributesLarge
fsck.gitattributesLineLength
fsck.gitattributesMissing
fsck.gitattributesSymlink
fsck.gitignoreSymlink
fsck.gitmodulesBlob
fsck.gitmodulesLarge
fsck.gitmodulesMissing
fsck.gitmodulesName
fsck.gitmodulesParse
fsck.gitmodulesPath
fsck.gitmodulesSymlink
fsck.gitmodulesUpdate
fsck.gitmodulesUrl
fsck.hasDot
fsck.hasDotdot
fsck.hasDotgit
fsck.largePathname
fsck.mailmapSymlink
fsck.missingAuthor
fsck.missingCommitter
fsck.missingEmail
fsck.missingNameBeforeEmail
fsck.missingObject
fsck.missingSpaceBeforeDate
fsck.missingSpaceBeforeEmail
fsck.missingTag
fsck.missingTagEntry
fsck.missingTaggerEntry
fsck.missingTree
fsck.missingType
fsck.missingTypeEntry
fsck.multipleAuthors
fsck.nulInCommit
fsck.nulInHeader
fsck.nullSha1
fsck.packedRefEntryNotTerminated
fsck.packedRefUnsorted
fsck.refMissingNewline
fsck.skipList
fsck.symlinkRef
fsck.symrefTargetIsNotARef
fsck.trailingRefContent
fsck.treeNotSorted
fsck.unknownType
fsck.unterminatedHeader
fsck.zeroPaddedDate
fsck.zeroPaddedFilemode
fsmonitor.allowRemote
fsmonitor.socketDir
gc.<pattern>.reflogExpire
gc.<pattern>.reflogExpireUnreachable
gc.aggressiveDepth
gc.aggressiveWindow
gc.auto
gc.autoDetach
gc.autoPackLimit
gc.bigPackThreshold
gc.cruftPacks
gc.logExpiry
gc.maxCruftSize
gc.packRefs
gc.pruneExpire
gc.recentObjectsHook
gc.reflogExpire
gc.reflogExpireUnreachable
gc.repackFilter
gc.repackFilterTo
gc.rerereResolved
gc.rerereUnresolved
gc.worktreePruneExpire
gc.writeCommitGraph
gitcvs.allBinary
gitcvs.commitMsgAnnotation
gitcvs.dbDriver
gitcvs.dbName
gitcvs.dbPass
gitcvs.dbTableNamePrefix
gitcvs.dbUser
gitcvs.enabled
gitcvs.logFile
gitcvs.usecrlfattr
gitweb.avatar
gitweb.blame
gitweb.category
gitweb.description
gitweb.grep
gitweb.highlight
gitweb.owner
gitweb.patches
gitweb.pickaxe
gitweb.remote_heads
gitweb.showSizes
gitweb.snapshot
gitweb.url
gpg.<format>.program
gpg.format
gpg.minTrustLevel
gpg.program
gpg.ssh.allowedSignersFile
gpg.ssh.defaultKeyCommand
gpg.ssh.revocationFile
grep.column
grep.extendedRegexp
grep.fallbackToNoIndex
grep.fullName
grep.lineNumber
grep.patternType
grep.threads
gui.GCWarning
gui.blamehistoryctx
gui.commitMsgWidth
gui.copyBlameThreshold
gui.diffContext
gui.displayUntracked
gui.encoding
gui.fastCopyBlame
gui.matchTrackingBranch
gui.newBranchTemplate
gui.pruneDuringFetch
gui.spellingDictionary
gui.trustmtime
guitool.<name>.argPrompt
guitool.<name>.cmd
guitool.<name>.confirm
guitool.<name>.needsFile
guitool.<name>.noConsole
guitool.<name>.noRescan
guitool.<name>.prompt
guitool.<name>.revPrompt
guitool.<name>.revUnmerged
guitool.<name>.title
hasconfig:remote.*.url
help.autoCorrect
help.browser
help.format
help.htmlPath
hook.<event>.enabled
hook.<event>.jobs
hook.<friendly-name>.command
hook.<friendly-name>.enabled
hook.<friendly-name>.event
hook.<friendly-name>.parallel
hook.jobs
http.<url>.*
http.cookieFile
http.curloptResolve
http.delegation
http.emptyAuth
http.extraHeader
http.followRedirects
http.keepAliveCount
http.keepAliveIdle
http.keepAliveInterval
http.lowSpeedLimit
http.lowSpeedTime
http.maxRequests
http.maxRetries
http.maxRetryTime
http.minSessions
http.noEPSV
http.pinnedPubkey
http.postBuffer
http.proactiveAuth
http.proxy
http.proxyAuthMethod
http.proxySSLCAInfo
http.proxySSLCert
http.proxySSLCertPasswordProtected
http.proxySSLKey
http.retryAfter
http.saveCookies
http.schannelCheckRevoke
http.schannelUseSSLCAInfo
http.sslBackend
http.sslCAInfo
http.sslCAPath
http.sslCert
http.sslCertPasswordProtected
http.sslCertType
http.sslCipherList
http.sslKey
http.sslKeyType
http.sslTry
http.sslVerify
http.sslVersion
http.userAgent
http.version
i18n.commitEncoding
i18n.logOutputEncoding
imap.authMethod
imap.folder
imap.host
imap.pass
imap.port
imap.preformattedHTML
imap.sslverify
imap.tunnel
imap.user
include.path
includeIf.<condition>.path
index.recordEndOfIndexEntries
index.recordOffsetTable
index.skipHash
index.sparse
index.threads
index.version
init.defaultBranch
init.defaultObjectFormat
init.defaultRefFormat
init.defaultSubmodulePathConfig
init.templateDir
instaweb.browser
instaweb.httpd
instaweb.local
instaweb.modulePath
instaweb.port
interactive.diffFilter
interactive.singleKey
log.abbrevCommit
log.date
log.decorate
log.diffMerges
log.excludeDecoration
log.follow
log.graphColors
log.initialDecorationSet
log.mailmap
log.showRoot
log.showSignature
lsrefs.unborn
mailinfo.scissors
mailmap.blob
mailmap.file
maintenance.<task>.enabled
maintenance.<task>.schedule
maintenance.auto
maintenance.autoDetach
maintenance.commit-graph.auto
maintenance.geometric-repack.auto
maintenance.geometric-repack.splitFactor
maintenance.incremental-repack.auto
maintenance.loose-objects.auto
maintenance.loose-objects.batchSize
maintenance.reflog-expire.auto
maintenance.rerere-gc.auto
maintenance.strategy
maintenance.worktree-prune.auto
man.<tool>.cmd
man.<tool>.path
man.viewer
merge.<driver>.driver
merge.<driver>.name
merge.<driver>.recursive
merge.autoStash
merge.branchdesc
merge.conflictStyle
merge.defaultToUpstream
merge.directoryRenames
merge.ff
merge.guitool
merge.log
merge.renameLimit
merge.renames
merge.renormalize
merge.stat
merge.suppressDest
merge.tool
merge.verbosity
merge.verifySignatures
mergetool.<tool>.cmd
mergetool.<tool>.hideResolved
mergetool.<tool>.path
mergetool.<tool>.trustExitCode
mergetool.<variant>.layout
mergetool.guiDefault
mergetool.hideResolved
mergetool.keepBackup
mergetool.keepTemporaries
mergetool.meld.hasOutput
mergetool.meld.useAutoMerge
mergetool.prompt
mergetool.writeToTemp
notes.<name>.mergeStrategy
notes.displayRef
notes.mergeStrategy
notes.rewrite.<command>
notes.rewriteMode
notes.rewriteRef
pack.allowPackReuse
pack.compression
pack.deltaCacheLimit
pack.deltaCacheSize
pack.depth
pack.indexVersion
pack.island
pack.islandCore
pack.packSizeLimit
pack.preferBitmapTips
pack.readReverseIndex
pack.threads
pack.useBitmapBoundaryTraversal
pack.useBitmaps
pack.usePathWalk
pack.useSparse
pack.window
pack.windowMemory
pack.writeBitmapHashCache
pack.writeBitmapLookupTable
pack.writeReverseIndex
pager.<cmd>
pretty.<name>
promisor.acceptFromServer
promisor.advertise
promisor.checkFields
promisor.quiet
promisor.sendFields
promisor.storeFields
protocol.<name>.allow
protocol.allow
protocol.version
pull.autoStash
pull.ff
pull.octopus
pull.rebase
pull.twohead
push.autoSetupRemote
push.default
push.followTags
push.gpgSign
push.negotiate
push.pushOption
push.recurseSubmodules
push.useBitmaps
push.useForceIfIncludes
rebase.abbreviateCommands
rebase.autoSquash
rebase.autoStash
rebase.backend
rebase.forkPoint
rebase.instructionFormat
rebase.maxLabelLength
rebase.missingCommitsCheck
rebase.rebaseMerges
rebase.rescheduleFailedExec
rebase.stat
rebase.updateRefs
receive.advertiseAtomic
receive.advertisePushOptions
receive.autogc
receive.certNonceSeed
receive.certNonceSlop
receive.denyCurrentBranch
receive.denyDeleteCurrent
receive.denyDeletes
receive.denyNonFastForwards
receive.fsck.badDate
receive.fsck.badDateOverflow
receive.fsck.badEmail
receive.fsck.badFilemode
receive.fsck.badGpgsig
receive.fsck.badHeadTarget
receive.fsck.badHeaderContinuation
receive.fsck.badName
receive.fsck.badObjectSha1
receive.fsck.badPackedRefEntry
receive.fsck.badPackedRefHeader
receive.fsck.badParentSha1
receive.fsck.badRefContent
receive.fsck.badRefFiletype
receive.fsck.badRefName
receive.fsck.badRefOid
receive.fsck.badReferentName
receive.fsck.badReftableTableName
receive.fsck.badTagName
receive.fsck.badTimezone
receive.fsck.badTree
receive.fsck.badTreeSha1
receive.fsck.badType
receive.fsck.duplicateEntries
receive.fsck.emptyName
receive.fsck.emptyPackedRefsFile
receive.fsck.extraHeaderEntry
receive.fsck.fullPathname
receive.fsck.gitattributesBlob
receive.fsck.gitattributesLarge
receive.fsck.gitattributesLineLength
receive.fsck.gitattributesMissing
receive.fsck.gitattributesSymlink
receive.fsck.gitignoreSymlink
receive.fsck.gitmodulesBlob
receive.fsck.gitmodulesLarge
receive.fsck.gitmodulesMissing
receive.fsck.gitmodulesName
receive.fsck.gitmodulesParse
receive.fsck.gitmodulesPath
receive.fsck.gitmodulesSymlink
receive.fsck.gitmodulesUpdate
receive.fsck.gitmodulesUrl
receive.fsck.hasDot
receive.fsck.hasDotdot
receive.fsck.hasDotgit
receive.fsck.largePathname
receive.fsck.mailmapSymlink
receive.fsck.missingAuthor
receive.fsck.missingCommitter
receive.fsck.missingEmail
receive.fsck.missingNameBeforeEmail
receive.fsck.missingObject
receive.fsck.missingSpaceBeforeDate
receive.fsck.missingSpaceBeforeEmail
receive.fsck.missingTag
receive.fsck.missingTagEntry
receive.fsck.missingTaggerEntry
receive.fsck.missingTree
receive.fsck.missingType
receive.fsck.missingTypeEntry
receive.fsck.multipleAuthors
receive.fsck.nulInCommit
receive.fsck.nulInHeader
receive.fsck.nullSha1
receive.fsck.packedRefEntryNotTerminated
receive.fsck.packedRefUnsorted
receive.fsck.refMissingNewline
receive.fsck.skipList
receive.fsck.symlinkRef
receive.fsck.symrefTargetIsNotARef
receive.fsck.trailingRefContent
receive.fsck.treeNotSorted
receive.fsck.unknownType
receive.fsck.unterminatedHeader
receive.fsck.zeroPaddedDate
receive.fsck.zeroPaddedFilemode
receive.fsckObjects
receive.hideRefs
receive.keepAlive
receive.maxInputSize
receive.procReceiveRefs
receive.shallowUpdate
receive.unpackLimit
receive.updateServerInfo
reftable.blockSize
reftable.geometricFactor
reftable.indexObjects
reftable.lockTimeout
reftable.restartInterval
remote.<name>.fetch
remote.<name>.followRemoteHEAD
remote.<name>.mirror
remote.<name>.negotiationInclude
remote.<name>.negotiationRestrict
remote.<name>.partialclonefilter
remote.<name>.promisor
remote.<name>.proxy
remote.<name>.proxyAuthMethod
remote.<name>.prune
remote.<name>.pruneTags
remote.<name>.push
remote.<name>.pushurl
remote.<name>.receivepack
remote.<name>.serverOption
remote.<name>.skipDefaultUpdate
remote.<name>.skipFetchAll
remote.<name>.tagOpt
remote.<name>.uploadpack
remote.<name>.url
remote.<name>.vcs
remote.pushDefault
remotes.<group>
repack.cruftDepth
repack.cruftThreads
repack.cruftWindow
repack.cruftWindowMemory
repack.midxMustContainCruft
repack.midxNewLayerThreshold
repack.midxSplitFactor
repack.packKeptObjects
repack.updateServerInfo
repack.useDeltaBaseOffset
repack.useDeltaIslands
repack.writeBitmaps
replay.refAction
rerere.autoUpdate
rerere.enabled
revert.reference
safe.bareRepository
safe.directory
sendemail.<identity>.*
sendemail.aliasFileType
sendemail.aliasesFile
sendemail.annotate
sendemail.bcc
sendemail.cc
sendemail.ccCmd
sendemail.chainReplyTo
sendemail.confirm
sendemail.envelopeSender
sendemail.forbidSendmailVariables
sendemail.from
sendemail.headerCmd
sendemail.identity
sendemail.imapSentFolder
sendemail.mailmap
sendemail.mailmap.blob
sendemail.mailmap.file
sendemail.multiEdit
sendemail.outlookidfix
sendemail.signedOffByCc
sendemail.smtpBatchSize
sendemail.smtpDomain
sendemail.smtpEncryption
sendemail.smtpPass
sendemail.smtpReloginDelay
sendemail.smtpSSLCertPath
sendemail.smtpSSLClientCert
sendemail.smtpSSLClientKey
sendemail.smtpServer
sendemail.smtpServerOption
sendemail.smtpServerPort
sendemail.smtpUser
sendemail.suppressCc
sendemail.suppressFrom
sendemail.thread
sendemail.to
sendemail.toCmd
sendemail.transferEncoding
sendemail.useImapOnly
sendemail.validate
sendemail.xmailer
sequence.editor
showBranch.default
sideband.<url>.*
sideband.allowControlCharacters
sparse.expectFilesOutsideOfPatterns
splitIndex.maxPercentChange
splitIndex.sharedIndexExpire
ssh.variant
stash.index
stash.showIncludeUntracked
stash.showPatch
stash.showStat
status.aheadBehind
status.branch
status.compareBranches
status.displayCommentPrefix
status.relativePaths
status.renameLimit
status.renames
status.short
status.showStash
status.showUntrackedFiles
status.submoduleSummary
submodule.<name>.active
submodule.<name>.branch
submodule.<name>.fetchRecurseSubmodules
submodule.<name>.gitdir
submodule.<name>.ignore
submodule.<name>.update
submodule.<name>.url
submodule.active
submodule.alternateErrorStrategy
submodule.alternateLocation
submodule.fetchJobs
submodule.propagateBranches
submodule.recurse
tag.forceSignAnnotated
tag.gpgSign
tag.sort
tar.umask
trace2.configParams
trace2.destinationDebug
trace2.envVars
trace2.eventBrief
trace2.eventNesting
trace2.eventTarget
trace2.maxFiles
trace2.normalBrief
trace2.normalTarget
trace2.perfBrief
trace2.perfTarget
trailer.<key-alias>.cmd
trailer.<key-alias>.command
trailer.<key-alias>.ifexists
trailer.<key-alias>.ifmissing
trailer.<key-alias>.key
trailer.<key-alias>.where
trailer.ifexists
trailer.ifmissing
trailer.separators
trailer.where
transfer.advertiseObjectInfo
transfer.advertiseSID
transfer.bundleURI
transfer.credentialsInUrl
transfer.fsckObjects
transfer.hideRefs
transfer.unpackLimit
uploadarchive.allowUnreachable
uploadpack.allowAnySHA1InWant
uploadpack.allowFilter
uploadpack.allowReachableSHA1InWant
uploadpack.allowRefInWant
uploadpack.allowTipSHA1InWant
uploadpack.hideRefs
uploadpack.keepAlive
uploadpack.packObjectsHook
uploadpackfilter.<filter>.allow
uploadpackfilter.allow
uploadpackfilter.tree.maxDepth
url.<base>.insteadOf
url.<base>.pushInsteadOf
user.email
user.name
user.signingKey
user.useConfigOnly
versionsort.suffix
web.browser
worktree.guessRemote
worktree.useRelativePaths
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
    /// `--config-for-completion`, git's `SHOW_CONFIG_VARS`.
    ConfigVars,
    /// `--config-sections-for-completion`, git's `SHOW_CONFIG_SECTIONS`.
    ConfigSections,
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
            Mode::ConfigVars => "--config-for-completion",
            Mode::ConfigSections => "--config-sections-for-completion",
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
            Mode::ConfigVars => "--config-for-completion",
            Mode::ConfigSections => "--config-sections-for-completion",
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
                "all"
                | "guides"
                | "config"
                | "config-for-completion"
                | "config-sections-for-completion"
                | "user-interfaces"
                | "developer-interfaces" => {
                    let m = match long {
                        "all" => Mode::All,
                        "guides" => Mode::Guides,
                        "config" => Mode::Config,
                        "config-for-completion" => Mode::ConfigVars,
                        "config-sections-for-completion" => Mode::ConfigSections,
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
                // git's non-verbose `-a` runs `list_commands()`, which scans the
                // git-core exec-path directory for its `git-*` helper binaries
                // (`checkout--worker`, `merge-octopus`, `sh-i18n--envsubst`, …)
                // and column-formats that on-disk set — not the static
                // `command-list.txt` table used by the verbose form. This port
                // is a single binary with no such exec-path directory of
                // individual helpers, so there is no faithful data source to
                // reproduce that listing byte-for-byte.
                bail!(
                    "`git help --all --no-verbose` is not supported: it column-formats the \
                     git-* helper binaries found in git's exec-path directory, a set this \
                     single-binary port has no notion of"
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
        Some(Mode::Config) => {
            // git's `list_config_help()` puts every name, then a blank line and
            // the "'git help config' for more information" trailer (its `\n%s\n`).
            print!("{CONFIG_VARS}");
            println!("\n'git help config' for more information");
            Ok(ExitCode::SUCCESS)
        }
        Some(Mode::ConfigVars) => {
            for key in config_keys_uniq(set_config_vars) {
                println!("{key}");
            }
            Ok(ExitCode::SUCCESS)
        }
        Some(Mode::ConfigSections) => {
            for key in config_keys_uniq(set_config_sections) {
                println!("{key}");
            }
            Ok(ExitCode::SUCCESS)
        }
        None => {
            let Some(topic) = rest.first() else {
                print!("{COMMON_HELP}");
                return Ok(ExitCode::SUCCESS);
            };
            // git reads `git_help_config` only once it has a topic, then lets
            // the command line override the configured format, then falls back
            // to `DEFAULT_HELP_FORMAT` ("man").
            let config = match help_config() {
                Ok(c) => c,
                Err(code) => return Ok(code),
            };
            let format = match viewer {
                Some("--web") => Format::Web,
                Some("--info") => Format::Info,
                Some("--man") => Format::Man,
                _ => config.format.unwrap_or(Format::Man),
            };
            match format {
                Format::Web => show_html_page(topic, config.html_path.as_deref()),
                Format::Info => show_info_page(topic),
                Format::Man => show_help_for(topic),
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

/// `set_config_vars()` (builtin/help.c): a variable name is truncated at the
/// first `*` or `<`, whichever comes first, so `alias.*` becomes `alias.` and
/// `branch.<name>.remote` becomes `branch.`. A name with neither is emitted
/// whole. Truncation deliberately keeps the trailing `.` — that is the prefix
/// `git-completion.bash` completes against.
fn set_config_vars(var: &str) -> &str {
    let wildcard = var.find('*');
    let tag = var.find('<');
    match (wildcard, tag) {
        (Some(w), Some(t)) => &var[..w.min(t)],
        (Some(w), None) => &var[..w],
        (None, Some(t)) => &var[..t],
        (None, None) => var,
    }
}

/// `set_config_sections()` (builtin/help.c): the section is everything before
/// the first `.`. A name with no `.` at all falls back to
/// [`set_config_vars`], as git's does.
fn set_config_sections(var: &str) -> &str {
    match var.find('.') {
        Some(dot) => &var[..dot],
        None => set_config_vars(var),
    }
}

/// `list_config_help()`'s tail: map every name in [`CONFIG_VARS`] through `f`,
/// then `string_list_sort_u` — sort by bytes and drop duplicates, which is what
/// collapses the many `alias.*`-style truncations into one entry each.
///
/// [`CONFIG_VARS`] is already the post-`slot_expansion` list (`advice.*`,
/// `color.branch.<slot>` and the rest are stored expanded), so the mapping picks
/// up where git's own loop does.
fn config_keys_uniq(f: fn(&str) -> &str) -> BTreeSet<&'static str> {
    CONFIG_VARS.lines().filter(|l| !l.is_empty()).map(f).collect()
}

/// `git-*` executables reachable through `PATH` that are neither known git
/// commands nor shipped in git's exec-path — git lists the latter under their
/// own categories instead.
///
/// `load_command_list()` builds `main_cmds` by listing the `git-*` files in the
/// exec-path directory and then has `exclude_cmds()` subtract that set from the
/// `PATH` scan, so a helper git itself provides never appears as "external" even
/// when a copy of it also sits on `PATH`. This binary's exec-path is its own
/// directory and holds no `git-*` helpers, so the equivalent set is the dispatch
/// table: every verb this binary serves *is* a helper the installation provides.
/// Without it, `git help --all` listed the four helpers Homebrew symlinks into
/// `PATH` — `receive-pack`, `shell`, `upload-archive`, `upload-pack` — under
/// "External commands", where stock lists none of them. [`command_names`] alone
/// does not cover these: they are dispatched but not named in the categorised
/// listing `--all` prints.
fn external_commands() -> BTreeSet<String> {
    let mut known = command_names();
    known.extend(
        crate::dispatch::PORCELAIN_VERBS
            .iter()
            .map(|v| (*v).to_string()),
    );
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

/// One row of the tables `git help -a` and `git help -g` print: the topic as the
/// user types it, the manual page it maps to, its one-line description, and the
/// heading it is listed under.
pub struct Topic {
    /// The topic as `git help <topic>` takes it — `status`, `revisions`, `everyday`.
    pub name: String,
    /// The page [`cmd_to_page`] maps it to — `git-status`, `gitrevisions`.
    pub page: String,
    /// git's one-line description for it.
    pub summary: String,
    /// The category heading it appears under in the listing.
    pub section: String,
    /// The command that lists it: `git help -a` for the command tables,
    /// `git help -g` for the concept guides.
    pub listing: &'static str,
    /// Whether it is a git command rather than a documentation topic. Drives
    /// both the page name and the wording of the generated page.
    pub is_command: bool,
}

/// The heading [`GUIDES`] entries are filed under. The block's own first line is
/// a sentence rather than a category name, so this names the section instead.
const GUIDES_SECTION: &str = "Git concept guides";

/// Every topic git documents, parsed out of the [`ALL_COMMANDS`] and [`GUIDES`]
/// blocks this module already prints — the command list is not restated
/// anywhere, so nothing downstream (the name set below, the HTML documentation
/// set in [`crate::superset::htmldoc`]) can drift from what `git help -a` says.
pub fn topics() -> Vec<Topic> {
    let mut out = Vec::new();
    for (block, guides) in [(ALL_COMMANDS, false), (GUIDES, true)] {
        let mut section = if guides { GUIDES_SECTION } else { "" }.to_string();
        let mut is_command = !guides;
        for line in block.lines() {
            let Some(entry) = line.strip_prefix("   ") else {
                // A category heading — only [`ALL_COMMANDS`] has them. The
                // guides block opens and closes with prose instead, so its
                // section stays [`GUIDES_SECTION`] throughout.
                if !guides && !line.is_empty() && !line.starts_with("See ") {
                    section = line.to_string();
                    is_command = !DOC_CATEGORIES.contains(&line);
                }
                continue;
            };
            let mut fields = entry.splitn(2, char::is_whitespace);
            let (Some(name), rest) = (fields.next(), fields.next()) else {
                continue;
            };
            out.push(Topic {
                page: cmd_to_page(name, is_command),
                name: name.to_string(),
                summary: rest.unwrap_or("").trim().to_string(),
                section: section.clone(),
                listing: if guides { "git help -g" } else { "git help -a" },
                is_command,
            });
        }
    }
    out
}

/// `cmd_to_page()` (builtin/help.c): a topic already spelled `git*` is its own
/// page, a command becomes `git-<cmd>`, and anything else `git<topic>` — which
/// is what turns `revisions` into `gitrevisions` and `add` into `git-add`.
pub fn cmd_to_page(topic: &str, is_command: bool) -> String {
    if topic.starts_with("git") {
        topic.to_string()
    } else if is_command {
        format!("git-{topic}")
    } else {
        format!("git{topic}")
    }
}

/// Every command name in [`topics`] — the documentation topics (the
/// [`DOC_CATEGORIES`] sections and the guides) are not commands and are left out.
fn command_names() -> BTreeSet<String> {
    topics().into_iter().filter(|t| t.is_command).map(|t| t.name).collect()
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
    // This binary IS git: the helper directory is the one holding this
    // executable, never whatever a foreign `git --exec-path` would report.
    std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(PathBuf::from))
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
    // Superset (`z*`) verbs are real commands here but ship no system man page,
    // so generate theirs on demand and open it with `man -M`. Done before the
    // alias check for git's "a real command wins over an alias" precedence.
    if crate::dispatch::SUPERSET_VERBS.contains(&topic) {
        reject_unsupported_viewer_config()?;
        if let Some(root) = crate::superset::manpage::ensure_page(topic)
            .map_err(|e| anyhow::anyhow!("failed to write man page: {e}"))?
        {
            std::io::stdout().flush().ok();
            let status = Command::new("man")
                .arg("-M")
                .arg(&root)
                .arg(format!("git-{topic}"))
                .status()
                .map_err(|e| anyhow::anyhow!("failed to run man: {e}"))?;
            return Ok(ExitCode::from(exit_status_code(status)));
        }
    }

    let Some(page) = resolve_page(topic)? else {
        return Ok(ExitCode::SUCCESS);
    };
    reject_unsupported_viewer_config()?;

    std::io::stdout().flush().ok();
    let status = Command::new("man")
        .arg(&page)
        .status()
        .map_err(|e| anyhow::anyhow!("failed to run man: {e}"))?;
    Ok(ExitCode::from(exit_status_code(status)))
}

/// `cmd_help`'s alias check followed by `check_git_cmd()` + `cmd_to_page()`,
/// the two steps git takes before it picks a viewer.
///
/// `Ok(None)` means the topic was an alias: git prints its expansion and
/// returns 0 without opening anything.
fn resolve_page(topic: &str) -> Result<Option<String>> {
    // git's `is_git_command()` is "a builtin, or a `git-*` in the exec-path or on
    // `PATH`". The superset verbs are builtins of this binary, so they belong in
    // that set regardless of whether their dashed links happen to be installed.
    let is_command = crate::dispatch::SUPERSET_VERBS.contains(&topic)
        || command_names().contains(topic)
        || external_commands().contains(topic);

    // git resolves a real command before consulting aliases, so an alias that
    // shadows a builtin still shows the builtin's manual.
    if !is_command {
        if let Some(value) = alias_list().get(topic) {
            println!("'{topic}' is aliased to '{value}'");
            return Ok(None);
        }
    }

    Ok(Some(cmd_to_page(topic, is_command)))
}

/// `show_info_page()`: point `INFOPATH` at the install's info directory and hand
/// the page to `info gitman <page>`.
///
/// git `execlp`s, so the info viewer replaces the process and its exit status is
/// the command's; running it as a child and propagating that status is the same
/// observable result. `execlp` only *returns* when there is no `info` on `$PATH`,
/// which is the one case git turns into `die(_("no info viewer handled the
/// request"))`.
fn show_info_page(topic: &str) -> Result<ExitCode> {
    let Some(page) = resolve_page(topic)? else {
        return Ok(ExitCode::SUCCESS);
    };

    std::io::stdout().flush().ok();
    match Command::new("info")
        .env("INFOPATH", info_dir())
        .arg("gitman")
        .arg(&page)
        .status()
    {
        Ok(status) => Ok(ExitCode::from(exit_status_code(status))),
        Err(_) => Ok(die("no info viewer handled the request")),
    }
}

/// `system_path(GIT_INFO_PATH)` — `<prefix>/share/info`, the directory
/// `git --info-path` reports and `show_info_page()` points `INFOPATH` at. This
/// port's prefix is `$ZVCS_HOME`, the same one the man pages install under.
pub fn info_dir() -> PathBuf {
    crate::superset::zdaemon::zvcs_home().join("share").join("info")
}

/// The help viewers, git's `enum help_format` minus its `NONE` sentinel (which
/// this port spells `Option<Format>`).
#[derive(Clone, Copy, PartialEq, Eq)]
enum Format {
    Man,
    Info,
    Web,
}

/// The `help.*` variables `git_help_config()` collects. `man.viewer` is read
/// separately, at the point the man viewer would run.
#[derive(Default)]
struct HelpConfig {
    format: Option<Format>,
    html_path: Option<String>,
}

/// `parse_help_format()`: the four spellings git accepts, and its `die()` on
/// anything else.
fn parse_help_format(format: &str) -> Result<Format, ExitCode> {
    match format {
        "man" => Ok(Format::Man),
        "info" => Ok(Format::Info),
        "web" | "html" => Ok(Format::Web),
        other => Err(die(&format!("unrecognized help format '{other}'"))),
    }
}

/// `git_help_config()`'s `help.format` and `help.htmlpath` arms. git runs this
/// after `setup_git_directory_gently()`, so the global files still apply outside
/// a repository — which is why the lookup falls back to them rather than
/// yielding nothing.
fn help_config() -> Result<HelpConfig, ExitCode> {
    let file = match gix::discover(".") {
        Ok(repo) => repo.config_snapshot().plumbing().clone(),
        Err(_) => match gix::config::File::from_globals() {
            Ok(f) => f,
            Err(_) => return Ok(HelpConfig::default()),
        },
    };
    let string = |key: &str| {
        file.string(key)
            .map(|v| String::from_utf8_lossy(&v).into_owned())
            .filter(|v| !v.is_empty())
    };
    Ok(HelpConfig {
        format: string("help.format").map(|f| parse_help_format(&f)).transpose()?,
        html_path: string("help.htmlpath"),
    })
}

/// `show_html_page()`: resolve the page's path, then open it in a browser.
///
/// git's HTML manual is laid down by `make install`, so the stat in
/// [`get_html_page_path`] finds a file that the installation put there. This
/// port is a single binary with no install step, so the page is materialized
/// first, from the same tables the man pages come from — `git zshadow` and
/// `git zdashed` write the whole set up front, and this covers the case where
/// neither has been run. A page that cannot be written (a topic outside the
/// documented set, an unwritable `$ZVCS_HOME`) falls through to git's own stat,
/// which gives exactly the answer stock gives for a missing file. A configured
/// `help.htmlpath` is the user's own tree and is never written to.
fn show_html_page(topic: &str, configured: Option<&str>) -> Result<ExitCode> {
    let Some(page) = resolve_page(topic)? else {
        return Ok(ExitCode::SUCCESS);
    };
    if configured.is_none() {
        let _ = crate::superset::htmldoc::ensure_page(&page);
    }
    let path = match get_html_page_path(&page, configured) {
        Ok(path) => path,
        Err(code) => return Ok(code),
    };
    open_html(&path)
}

/// `get_html_page_path()` verbatim: `help.htmlpath` when set, else
/// `system_path(GIT_HTML_PATH)`. A path holding `://` is a URL git cannot stat,
/// so it is taken on trust; anything else must name a regular file or git dies.
fn get_html_page_path(page: &str, configured: Option<&str>) -> Result<String, ExitCode> {
    let path = match configured {
        Some(p) => p.to_string(),
        None => crate::superset::htmldoc::html_dir().display().to_string(),
    };
    if !path.contains("://") {
        let file = Path::new(&path).join(format!("{page}.html"));
        if !std::fs::metadata(&file).is_ok_and(|m| m.is_file()) {
            return Err(die(&format!("'{path}/{page}.html': documentation file not found.")));
        }
    }
    Ok(format!("{path}/{page}.html"))
}

/// `open_html()`: git `execl_git_cmd`s `git web--browse -c help.browser <path>`,
/// replacing itself with it. One binary here, so the same porcelain is called
/// in-process and its status becomes this command's — what the exec would leave
/// behind.
fn open_html(path: &str) -> Result<ExitCode> {
    std::io::stdout().flush().ok();
    crate::porcelain::web__browse(&[
        "-c".to_string(),
        "help.browser".to_string(),
        path.to_string(),
    ])
}

/// git's `die()`: one `fatal:` line on stderr and exit 128, with no usage block.
fn die(msg: &str) -> ExitCode {
    eprintln!("fatal: {msg}");
    ExitCode::from(128)
}

/// git's `man.viewer` names a program to hand the page to (`konqueror`, `woman`,
/// a `man.<tool>.cmd`). This port only drives plain `man`, so a different viewer
/// is rejected instead of silently ignored. `help.format` needs no gate: all
/// three of git's formats are implemented, and [`parse_help_format`] rejects the
/// rest with git's own message.
fn reject_unsupported_viewer_config() -> Result<()> {
    let Ok(repo) = gix::discover(".") else {
        return Ok(());
    };
    let config = repo.config_snapshot();
    if let Some(viewer) = config.string("man.viewer") {
        if viewer != "man" {
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

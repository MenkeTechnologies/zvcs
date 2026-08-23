//! The ref tips of an alternate object database — `odb_for_each_alternate_ref()`
//! (`odb.c:399-470`), and the two configuration variables that steer it.
//!
//! A repository that borrows objects through `objects/info/alternates` can be
//! asked what its lenders consider reachable. git does not read the lender's ref
//! store directly; it *runs a command in the lender* and reads a list of object
//! ids off its stdout (`odb.c:371-397`):
//!
//! ```c
//! static void fill_alternate_refs_command(struct repository *repo,
//!                                         struct child_process *cmd,
//!                                         const char *repo_path)
//! {
//!         const char *value;
//!
//!         if (!repo_config_get_value(repo, "core.alternateRefsCommand", &value)) {
//!                 cmd->use_shell = 1;
//!
//!                 strvec_push(&cmd->args, value);
//!                 strvec_push(&cmd->args, repo_path);
//!         } else {
//!                 cmd->git_cmd = 1;
//!
//!                 strvec_pushf(&cmd->args, "--git-dir=%s", repo_path);
//!                 strvec_push(&cmd->args, "for-each-ref");
//!                 strvec_push(&cmd->args, "--format=%(objectname)");
//!
//!                 if (!repo_config_get_value(repo, "core.alternateRefsPrefixes", &value)) {
//!                         strvec_push(&cmd->args, "--");
//!                         strvec_split(&cmd->args, value);
//!                 }
//!         }
//!
//!         strvec_pushv(&cmd->env, (const char **)local_repo_env);
//!         cmd->out = -1;
//! }
//! ```
//!
//! Three consequences are load-bearing and reproduced below.
//!
//! * **The two keys are exclusive, in that order.** `core.alternateRefsCommand`
//!   is tested first and its branch never looks at
//!   `core.alternateRefsPrefixes`, which is what
//!   `Documentation/config/core.adoc:312-313` states outright: "If
//!   `core.alternateRefsCommand` is set, setting `core.alternateRefsPrefixes`
//!   has no effect."
//! * **The command is a shell command, and its single argument is the
//!   alternate's git directory** — `repo_path`, which
//!   `refs_from_alternate_cb()` (`odb.c:437-461`) derives by taking the
//!   `realpath` of the alternate's *object* directory and stripping the trailing
//!   `/objects`. `Documentation/config/core.adoc:298-302` says the same: "The
//!   first argument is the absolute path of the alternate."
//! * **The child is repository-scrubbed.** `local_repo_env`
//!   (`environment.c:101-118`) is pushed into the child's environment as bare
//!   names, which `start_command()` treats as *unset* instructions — so
//!   `GIT_DIR`, `GIT_OBJECT_DIRECTORY` and the rest of the borrower's
//!   environment cannot follow the command into the lender.
//!
//! The read side is `read_alternate_refs()` (`odb.c:399-430`): whole lines, each
//! one a bare hex object id, and the first line that is not stops the whole
//! stream after a warning.
//!
//! ```c
//! if (parse_oid_hex_algop(line.buf, &oid, &p, repo->hash_algo) || *p) {
//!         warning(_("invalid line while parsing alternate refs: %s"),
//!                 line.buf);
//!         break;
//! }
//! ```
//!
//! Note what is *not* validated: the ids are never looked up. A command that
//! prints a well-formed id for an object nobody has still gets that id queued,
//! and the failure surfaces later, from whatever tried to use it.

use gix::bstr::ByteSlice;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use gix::hash::ObjectId;

/// `local_repo_env[]` (`environment.c:101-118`) — the repository-local
/// environment `start_command` clears for this child. The order is the array's.
const LOCAL_REPO_ENV: &[&str] = &[
    "GIT_ALTERNATE_OBJECT_DIRECTORIES",
    "GIT_CONFIG",
    "GIT_CONFIG_PARAMETERS",
    "GIT_CONFIG_COUNT",
    "GIT_OBJECT_DIRECTORY",
    "GIT_DIR",
    "GIT_WORK_TREE",
    "GIT_IMPLICIT_WORK_TREE",
    "GIT_GRAFT_FILE",
    "GIT_INDEX_FILE",
    "GIT_NO_REPLACE_OBJECTS",
    "GIT_REPLACE_REF_BASE",
    "GIT_PREFIX",
    "GIT_SHALLOW_FILE",
    "GIT_COMMON_DIR",
];

/// `refs_from_alternate_cb()` (`odb.c:437-461`): the git directory that owns
/// `object_dir`, or `None` when it does not look like one.
///
/// ```c
/// if (!strbuf_realpath(&path, alternate->path, 0))
///         goto out;
/// if (!strbuf_strip_suffix(&path, "/objects"))
///         goto out;
/// base_len = path.len;
///
/// /* Is this a git repository with refs? */
/// strbuf_addstr(&path, "/refs");
/// if (!is_directory(path.buf))
///         goto out;
/// ```
///
/// The `/objects` suffix test is textual and runs on the *resolved* path, so an
/// alternate reached through a symlink is followed first and one that does not
/// end in `objects` is skipped without a word. The `refs` directory test is what
/// keeps a bare object store — an `objects` directory with no repository around
/// it — from being asked for refs it cannot have.
fn git_dir_of_alternate(object_dir: &Path) -> Option<PathBuf> {
    let resolved = std::fs::canonicalize(object_dir).ok()?;
    if resolved.file_name()? != "objects" {
        return None;
    }
    let base = resolved.parent()?;
    base.join("refs").is_dir().then(|| base.to_path_buf())
}

/// `strvec_split()` (`strvec.c`): split on runs of whitespace, dropping empty
/// fields — how `core.alternateRefsPrefixes` becomes `for-each-ref` operands.
fn split_prefixes(value: &str) -> Vec<String> {
    value.split_whitespace().map(str::to_string).collect()
}

/// `fill_alternate_refs_command()` (`odb.c:371-397`) for one alternate.
///
/// The `use_shell` branch goes through [`crate::external::prepare_shell_cmd`],
/// this crate's `prepare_shell_cmd()` — the same transform `start_command`
/// applies to every child with `use_shell = 1`, including the `"%s \"$@\""`
/// wrapper that lets the configured command see the alternate's path as `$1`.
///
/// The `git_cmd` branch re-executes *this* binary, which is what `git_cmd = 1`
/// means: the alternate is listed by the same `for-each-ref` the borrower would
/// run for itself.
fn alternate_refs_command(repo: &gix::Repository, git_dir: &Path) -> Option<Command> {
    let snapshot = repo.config_snapshot();

    let mut cmd = match snapshot.string("core.alternateRefsCommand") {
        Some(value) => crate::external::prepare_shell_cmd(
            gix::path::from_bstr(value.as_bstr()).as_os_str(),
            [git_dir.as_os_str()],
        ),
        None => {
            let mut cmd = Command::new(std::env::current_exe().ok()?);
            let mut git_dir_arg = OsString::from("--git-dir=");
            git_dir_arg.push(git_dir);
            cmd.arg(git_dir_arg);
            cmd.arg("for-each-ref");
            cmd.arg("--format=%(objectname)");
            if let Some(prefixes) = snapshot.string("core.alternateRefsPrefixes") {
                cmd.arg("--");
                cmd.args(split_prefixes(&prefixes.to_string()));
            }
            cmd
        }
    };
    for name in LOCAL_REPO_ENV {
        cmd.env_remove(name);
    }
    Some(cmd)
}

/// `read_alternate_refs()` (`odb.c:399-430`): every line of the child's stdout
/// as an object id, stopping at the first line that is not one.
///
/// `start_command()` failing is silent (`odb.c:410-411`), and so is a child that
/// exits non-zero — git calls `finish_command()` for the reaping and ignores its
/// answer, so ids printed before a failure are still queued. The child's stderr
/// is inherited, exactly as a `child_process` with `.err` left at 0 is.
fn read_alternate_refs(mut cmd: Command, out: &mut Vec<ObjectId>) {
    let Ok(child) = cmd.stdout(Stdio::piped()).stderr(Stdio::inherit()).output() else {
        return;
    };
    // `strbuf_getline_lf`: LF-terminated lines, plus a final unterminated one.
    // An *empty* line is not skipped — it fails to parse and stops the stream,
    // which is what `parse_oid_hex_algop("")` does in the C.
    let mut rest: &[u8] = &child.stdout;
    while !rest.is_empty() {
        let (line, tail) = match rest.iter().position(|b| *b == b'\n') {
            Some(i) => (&rest[..i], &rest[i + 1..]),
            None => (rest, &rest[rest.len()..]),
        };
        rest = tail;
        match ObjectId::from_hex(line) {
            Ok(id) => out.push(id),
            Err(_) => {
                eprintln!(
                    "warning: invalid line while parsing alternate refs: {}",
                    String::from_utf8_lossy(line)
                );
                break;
            }
        }
    }
}

/// `MAX_ALTERNATE_DEPTH` in spirit — `odb_add_alternate_recursively()` refuses to
/// descend past `depth + 1 > 5` (`odb.c:194`).
const MAX_DEPTH: usize = 5;

/// The alternate object directories of `repo`, in `odb->sources` order —
/// `odb_add_alternate_recursively()` (`odb.c:169-205`).
///
/// ```c
/// /* add the alternate entry */
/// *odb->sources_tail = alternate;
/// odb->sources_tail = &(alternate->next);
/// …
/// /* recursively add alternates */
/// odb_source_read_alternates(alternate, &sources);
/// if (sources.nr && depth + 1 > 5) {
///         error(_("%s: ignoring alternate object stores, nesting too deep"),
///               source);
/// } else {
///         for (size_t i = 0; i < sources.nr; i++)
///                 odb_add_alternate_recursively(odb, sources.v[i], depth + 1);
/// }
/// ```
///
/// Each entry is appended *before* its own alternates are read, so the list is a
/// depth-first pre-order walk in file order. That order is the order the tips
/// below come out in, and with equal commit dates it is the order `rev-list`
/// prints them in — which is why this is not taken from gitoxide's
/// `alternate_db_paths()`, whose stack-based traversal answers the same set in a
/// different sequence.
///
/// `seen` is `odb->source_by_path` (`odb.c:79-93`), which both prevents the
/// "common mistake of listing the same thing twice" and terminates a cycle. The
/// primary object directory is seeded into it there too, so an alternate that
/// points back at the borrower is skipped rather than recursed into.
///
/// The two `error()` calls of `odb_is_source_usable()` and the nesting-depth one
/// are not raised here. They belong to *object database preparation*, which this
/// port does through gitoxide for every command that reads an object; emitting
/// them from this call site alone would make `rev-list --alternate-refs` louder
/// about a broken `objects/info/alternates` than `rev-list --all` in the same
/// repository. The entry is skipped either way, which is what the C does after
/// it prints.
fn alternate_object_dirs(repo: &gix::Repository) -> Vec<PathBuf> {
    let primary = repo.objects.store_ref().path().to_path_buf();
    let mut seen: Vec<PathBuf> = vec![std::fs::canonicalize(&primary).unwrap_or(primary.clone())];
    let mut out = Vec::new();
    add_alternates_recursively(&primary, 0, &mut seen, &mut out);
    out
}

fn add_alternates_recursively(
    object_dir: &Path,
    depth: usize,
    seen: &mut Vec<PathBuf>,
    out: &mut Vec<PathBuf>,
) {
    let Ok(content) = std::fs::read(object_dir.join("info").join("alternates")) else {
        return;
    };
    let sources = crate::setup::alternates_from_file(&content, object_dir);
    // `if (sources.nr && depth + 1 > 5)` — the whole level is dropped, not
    // trimmed, once the nesting is too deep.
    if sources.is_empty() || depth + 1 > MAX_DEPTH {
        return;
    }
    for source in sources {
        // `odb_is_source_usable()`: a path that is not a directory, is the
        // primary store, or has already been linked contributes nothing.
        if !source.is_dir() || seen.contains(&source) {
            continue;
        }
        seen.push(source.clone());
        out.push(source.clone());
        add_alternates_recursively(&source, depth + 1, seen, out);
    }
}

/// `odb_for_each_alternate_ref()` (`odb.c:463-470`): the object ids every
/// alternate of `repo` advertises, in the order the alternates are recorded.
///
/// The primary object database is not one of them — git walks
/// `odb->sources->next` (`odb.c:479`), skipping the repository's own store.
pub fn tips(repo: &gix::Repository) -> Vec<ObjectId> {
    let mut out = Vec::new();
    for object_dir in alternate_object_dirs(repo) {
        let Some(git_dir) = git_dir_of_alternate(&object_dir) else {
            continue;
        };
        let Some(cmd) = alternate_refs_command(repo, &git_dir) else {
            continue;
        };
        read_alternate_refs(cmd, &mut out);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("zvcs-alternate-refs-{name}"));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("create scratch dir");
        dir
    }

    /// The `/objects` suffix and the `refs` directory are both required
    /// (`odb.c:444-453`).
    #[test]
    fn only_an_objects_directory_inside_a_repository_names_a_git_dir() {
        let dir = scratch("gitdir");
        let git_dir = dir.join("repo.git");
        let objects = git_dir.join("objects");
        std::fs::create_dir_all(&objects).unwrap();

        // No `refs` yet — `is_directory()` fails and the alternate is skipped.
        assert_eq!(git_dir_of_alternate(&objects), None);

        std::fs::create_dir_all(git_dir.join("refs")).unwrap();
        assert_eq!(
            git_dir_of_alternate(&objects),
            Some(std::fs::canonicalize(&git_dir).unwrap())
        );

        // A directory that is not named `objects` never gets that far.
        let odd = git_dir.join("pack");
        std::fs::create_dir_all(&odd).unwrap();
        assert_eq!(git_dir_of_alternate(&odd), None);
    }

    /// `strvec_split()` collapses runs of whitespace and drops empty fields.
    #[test]
    fn prefixes_split_on_whitespace_runs() {
        assert_eq!(
            split_prefixes("  refs/heads   refs/tags\t"),
            vec!["refs/heads".to_string(), "refs/tags".to_string()]
        );
        assert!(split_prefixes("   ").is_empty());
    }
}

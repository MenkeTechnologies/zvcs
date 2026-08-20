//! The two keys `git_default_config()` validates for every command that reads a
//! repository's configuration: `core.createObject` and
//! `sparse.expectFilesOutsideOfPatterns`.
//!
//! Unlike `crate::repo_settings`, which git builds lazily the first time some
//! code path asks for a setting, these are checked while the config file is being
//! *parsed*: `git_default_config()` dispatches to `git_default_core_config()`
//! (environment.c:508-518) and `git_default_sparse_config()`
//! (environment.c:551-559), and a value either of them rejects kills the command
//! before it does anything. Measured against git 2.55.0 in a repository with one
//! commit — `branch`, `tag`, `symbolic-ref`, `hash-object`, `var`,
//! `count-objects`, `for-each-ref`, `notes list` and `update-ref` all die for
//! these two keys, none of which reaches the settings block at all:
//!
//! ```text
//! $ git -c core.createObject=bogus branch
//! fatal: invalid mode for object creation: bogus
//! $ git -c sparse.expectFilesOutsideOfPatterns=bogus branch
//! fatal: bad boolean config value 'bogus' for 'sparse.expectfilesoutsideofpatterns'
//! ```
//!
//! # What is honored
//!
//! Neither key changes what this port does with a *valid* value, and the reason
//! is different for each:
//!
//! * **`core.createObject`** picks how a finished loose object is moved into
//!   place: `rename` (the default) or `link`, i.e. `link()` + `unlink()` for
//!   filesystems whose `rename()` cannot be trusted to be atomic
//!   (`object-file.c`'s `finalize_object_file`, reading
//!   `object_creation_mode`). gitoxide writes a loose object through
//!   `tempfile::NamedTempFile::persist`
//!   (`gix-odb/src/store_impls/loose/write.rs`), which is `rename(2)` and has no
//!   hard-link variant to select. Both modes therefore produce the same file with
//!   the same bytes; the mode is resolved here — [`ObjectCreationMode`] — and
//!   rejected here, which is the whole of the observable difference.
//! * **`sparse.expectFilesOutsideOfPatterns`** turns *off* the scan
//!   `repo_read_index()` runs over a sparse index (`repository.c:458` →
//!   `clear_skip_worktree_from_present_files`, sparse-index.c:673-685): for every
//!   entry carrying `SKIP_WORKTREE`, git `lstat`s the path and clears the bit if
//!   the file is actually there. This port never runs that scan — no index read
//!   in it clears `SKIP_WORKTREE` from a present file — so it already behaves as
//!   if the key were `true`, and setting it to `true` changes nothing while
//!   setting it to `false` does not restore a scan that was never written. That
//!   is a pre-existing gap in the sparse-checkout port, not something this reader
//!   introduces; what it does add is git's rejection of an unreadable value.



/// `enum object_creation_mode` (`environment.h`): how a finished loose object is
/// moved from its temporary file to its final name.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum ObjectCreationMode {
    /// `OBJECT_CREATION_USES_RENAMES` — git's default.
    Renames,
    /// `OBJECT_CREATION_USES_HARDLINKS` — `core.createObject = link`.
    Hardlinks,
}

/// The resolved values, for callers that want them rather than just the check.
#[derive(Copy, Clone, Debug)]
pub struct DefaultConfig {
    /// `core.createObject`.
    pub object_creation_mode: ObjectCreationMode,
    /// `sparse.expectFilesOutsideOfPatterns`.
    pub sparse_expect_files_outside_of_patterns: bool,
}

/// How git refuses one of these keys.
pub enum Rejection {
    /// A single `die()` line.
    Die(String),
    /// `config_error_nonbool()`: an `error:` line naming the key, then a `fatal:`
    /// line naming where the value came from.
    NonBool {
        /// The key, lowercased the way the config reader has already normalised it.
        var: String,
        /// The `fatal:` clause — `unable to parse '<var>' from command-line config`
        /// for `-c`/environment, `bad config variable '<var>' in file '<path>'`
        /// otherwise.
        origin: String,
    },
}

impl Rejection {
    /// Print whatever precedes the `fatal:` line and return the message the
    /// caller should die with.
    ///
    /// git's file-backed `config_error_nonbool` also names the line the variable
    /// sits on (`… in file '.git/config' at line 9`). gitoxide's config metadata
    /// carries the source path but not the line, so that clause is dropped — the
    /// same limitation, and the same wording, as `porcelain::stripspace`'s port of
    /// this diagnostic.
    pub fn into_fatal(self) -> String {
        match self {
            Rejection::Die(msg) => msg,
            Rejection::NonBool { var, origin } => {
                eprintln!("error: missing value for '{var}'");
                origin
            }
        }
    }
}

/// `git_default_core_config()` + `git_default_sparse_config()` for the two keys
/// this module owns.
///
/// # Every occurrence, in parse order
///
/// This is not a last-value-wins read. `git_default_config()` is a *callback*: the
/// config parser hands it every `<key> <value>` pair from every file and every
/// `-c` in turn, so a value it rejects is fatal even when a later one overrides
/// it. That is the opposite of how `crate::repo_settings`'s keys behave, and both
/// were checked against git 2.55.0:
///
/// ```text
/// $ git -c core.createObject=bogus -c core.createObject=rename status -s
/// fatal: invalid mode for object creation: bogus
/// $ git -c core.packedGitLimit=bogus -c core.packedGitLimit=1m status -s
/// ?? b.bundle
/// ```
///
/// So the config is walked in order and each occurrence checked as it is met,
/// which also settles precedence *between* the two keys: whichever is parsed first
/// is the one that reports.
///
/// The walk is section-then-value-name, which is the order the parser saw (each
/// `-c` becomes its own section).
///
/// # The valueless form
///
/// git distinguishes `createObject` from `createObject =`: the first hands the
/// callback a `NULL` value, the second an empty string, and the two keys here take
/// that differently — `git_config_string` answers `NULL` with
/// `config_error_nonbool`, `git_config_bool` answers it with *true*
/// (`parse.c:168-169`).
///
/// gitoxide preserves that, but only through `value_implicit()`, and only for the
/// **last** occurrence of a name in a section (`key_and_value_range_by_in`,
/// gix-config/src/file/section/body.rs:182-217, scans backwards and reports "no
/// value" when the `Value` event sits directly after the name event). `values()`
/// renders the same occurrence as an empty string. So each occurrence is read
/// positionally out of `values()` and only the final one is allowed to be
/// valueless — which is exactly right for a section that spells a key once, and
/// reads a repeated `createObject = link` / `createObject` pair as two explicit
/// values rather than one of each.
pub fn validate(repo: &gix::Repository) -> Result<DefaultConfig, Rejection> {
    let config = repo.config_snapshot().plumbing().clone();
    let mut resolved = DefaultConfig {
        object_creation_mode: ObjectCreationMode::Renames,
        sparse_expect_files_outside_of_patterns: false,
    };

    for sec in config.sections() {
        let header = sec.header();
        if header.subsection_name().is_some() {
            continue;
        }
        let section = header.name().to_string().to_ascii_lowercase();
        let body = sec.body();
        let create_object = body.values(CREATE_OBJECT_NAME);
        let sparse_expect = body.values(SPARSE_EXPECT_NAME);
        let create_object_ends_valueless = body.value_implicit(CREATE_OBJECT_NAME) == Some(None);
        let sparse_expect_ends_valueless = body.value_implicit(SPARSE_EXPECT_NAME) == Some(None);
        let (mut create_object_at, mut sparse_expect_at) = (0usize, 0usize);

        for name in body.value_names() {
            let name = name.to_ascii_lowercase();
            match (section.as_str(), name.as_str()) {
                ("core", CREATE_OBJECT_NAME) => {
                    let at = create_object_at;
                    create_object_at += 1;
                    let last = at + 1 == create_object.len();
                    if last && create_object_ends_valueless {
                        return Err(Rejection::NonBool {
                            var: CREATE_OBJECT_VAR.to_string(),
                            origin: nonbool_origin(sec.meta(), CREATE_OBJECT_VAR),
                        });
                    }
                    let Some(v) = create_object.get(at) else { continue };
                    resolved.object_creation_mode = parse_object_creation_mode(&v.to_string())?;
                }
                ("sparse", SPARSE_EXPECT_NAME) => {
                    let at = sparse_expect_at;
                    sparse_expect_at += 1;
                    let last = at + 1 == sparse_expect.len();
                    if last && sparse_expect_ends_valueless {
                        // `git_config_bool(var, NULL)` is 1.
                        resolved.sparse_expect_files_outside_of_patterns = true;
                        continue;
                    }
                    let Some(v) = sparse_expect.get(at) else { continue };
                    resolved.sparse_expect_files_outside_of_patterns =
                        match crate::optint::maybe_bool(&v.to_string()) {
                            Some(b) => b,
                            None => {
                                return Err(Rejection::Die(format!(
                                    "bad boolean config value '{v}' for '{SPARSE_EXPECT_VAR}'"
                                )))
                            }
                        };
                }
                _ => {}
            }
        }
    }

    Ok(resolved)
}

/// The variable-name halves of the two keys, lowercased the way gitoxide and git
/// both normalise them before comparison.
const CREATE_OBJECT_NAME: &str = "createobject";
const SPARSE_EXPECT_NAME: &str = "expectfilesoutsideofpatterns";
/// The full dotted names, as they appear in git's diagnostics.
const CREATE_OBJECT_VAR: &str = "core.createobject";
const SPARSE_EXPECT_VAR: &str = "sparse.expectfilesoutsideofpatterns";

/// environment.c:508-518:
///
/// ```c
/// if (!strcmp(var, "core.createobject")) {
///         if (!value)
///                 return config_error_nonbool(var);
///         if (!strcmp(value, "rename"))
///                 object_creation_mode = OBJECT_CREATION_USES_RENAMES;
///         else if (!strcmp(value, "link"))
///                 object_creation_mode = OBJECT_CREATION_USES_HARDLINKS;
///         else
///                 die(_("invalid mode for object creation: %s"), value);
///         return 0;
/// }
/// ```
///
/// The two comparisons are `strcmp`, not `strcasecmp`: `Link` and `RENAME` are
/// invalid modes, not spellings of the valid ones. Verified against git 2.55.0.
fn parse_object_creation_mode(value: &str) -> Result<ObjectCreationMode, Rejection> {
    match value {
        "rename" => Ok(ObjectCreationMode::Renames),
        "link" => Ok(ObjectCreationMode::Hardlinks),
        other => Err(Rejection::Die(format!(
            "invalid mode for object creation: {other}"
        ))),
    }
}

/// The `fatal:` clause `config_error_nonbool` produces, which depends on where the
/// offending value came from: a `-c`/`GIT_CONFIG_PARAMETERS` value is "command-line
/// config", anything else names its file.
fn nonbool_origin(meta: &gix::config::file::Metadata, var: &str) -> String {
    use gix::config::Source;

    match meta.source {
        Source::Cli | Source::Env => format!("unable to parse '{var}' from command-line config"),
        _ => match &meta.path {
            Some(path) => {
                let shown = path.to_string_lossy();
                let shown = shown.strip_prefix("./").unwrap_or(&shown);
                format!("bad config variable '{var}' in file '{shown}'")
            }
            None => format!("bad config variable '{var}'"),
        },
    }
}

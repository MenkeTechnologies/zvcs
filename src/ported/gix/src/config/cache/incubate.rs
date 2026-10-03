#![allow(clippy::result_large_err)]

use super::{Error, util};
use crate::config::{
    cache::util::{ApplyLeniency, ApplyLeniencyDefaultValue},
    tree::{Core, Extensions, gitoxide},
};

/// A utility to deal with the cyclic dependency between the ref store and the configuration. The ref-store needs the
/// object hash kind, and the configuration needs the current branch name to resolve conditional includes with `onbranch`.
pub(crate) struct StageOne {
    pub git_dir_config: gix_config::File,
    pub buf: Vec<u8>,

    pub is_bare: Option<bool>,
    pub lossy: bool,
    pub object_hash: gix_hash::Kind,
    pub reflog: Option<gix_ref::store::WriteReflog>,
    pub precompose_unicode: bool,
    pub protect_windows: bool,
    /// The ref storage format the repository declares, `files` unless
    /// `extensions.refStorage = reftable`.
    ///
    /// `extensions.refStorage` is a repository format version 1 extension:
    /// `verify_repository_format()` refuses it at version 0 and a config
    /// without `core.repositoryformatversion` ignores it (setup.c:866-925,
    /// v2.56.0). `handle_extension()` (:692-709) takes the format from the
    /// part before `://` and resolves it through `ref_storage_format_by_name()`
    /// (refs.c:49-55), which compares with `strcmp`, so the value is
    /// case-*sensitive* even though the key is not. A value no backend
    /// answers to is refused before any store is built.
    pub ref_storage: gix_ref::store::RefStorage,
    /// Whether `extensions.worktreeConfig` is on, so `$GIT_DIR/config.worktree`
    /// was read and `check_repository_format_gently()` cleared `has_common`
    /// (`setup.c:787-796`, v2.55.0): a linked worktree then takes `core.bare`
    /// like the main one instead of ignoring it.
    pub worktree_config: bool,
}

/// Initialization
impl StageOne {
    pub fn new(
        common_dir: &std::path::Path,
        git_dir: &std::path::Path,
        git_dir_trust: gix_sec::Trust,
        lossy: bool,
        lenient: bool,
    ) -> Result<Self, Error> {
        let mut buf = Vec::with_capacity(512);
        let mut config = load_config(
            common_dir.join("config"),
            &mut buf,
            gix_config::Source::Local,
            git_dir_trust,
            lossy,
            lenient,
        )?;

        // `read_repository_format()` (setup.c:866-876): the repository's own
        // `config`, without includes, through `check_repo_format()`
        // (:718-749). Without a `core.repositoryformatversion` the version is
        // -1 and everything read is cleared again: the repository is SHA-1,
        // uses the files backend, has no `extensions.worktreeConfig`, and its
        // `core.bare` is not taken here (`clear_repository_format()`, :878-886,
        // and `read_and_verify_repository_format()` returning early for a
        // negative version, :766-769). Measured with stock 2.56.0: a config of
        // only `[extensions] objectFormat = sha256` is a SHA-1 repository, and
        // one of only `[core] bare = true` has a work tree.
        let version = Core::REPOSITORY_FORMAT_VERSION.try_into_usize(config.integer("core.repositoryFormatVersion"))?;
        let mut is_bare = match version {
            Some(_) => util::config_bool_opt(&config, &Core::BARE, "core.bare", lenient)?,
            None => None,
        };
        let repo_format_version = version.unwrap_or_default();
        // `handle_extension()` (setup.c:653-716) reads `objectformat` and
        // `refstorage` at every version; a version 0 that names one is refused
        // by `verify_repository_format()` (:888-925), a version without the key
        // ignores both.
        let object_format = config.string(Extensions::OBJECT_FORMAT).filter(|_| version.is_some());
        let object_hash = match (repo_format_version, object_format) {
            (1, Some(format)) => object_format_by_name(format.as_ref())?,
            (0, Some(_)) => return Err(Error::ObjectFormatRequiresV1),
            (0 | 1, None) => legacy_object_hash()?,
            (version, _) => return Err(Error::UnsupportedRepositoryFormatVersion { version }),
        };
        // Read next to `objectFormat` and from the same file, before the
        // worktree configuration is appended: `extensions.refStorage` describes
        // the whole repository, and git never looks for it in `config.worktree`.
        let ref_storage = match config.string("extensions.refStorage") {
            Some(value) if repo_format_version == 1 && version.is_some() => ref_storage_by_uri(value.as_ref()),
            _ => gix_ref::store::RefStorage::Files,
        };

        // `handle_extension_v0()` (setup.c:612-633) takes `worktreeconfig` at
        // any version, and like every other value it is cleared without one.
        let extension_worktree = version.is_some()
            && util::config_bool(
                &config,
                &Extensions::WORKTREE_CONFIG,
                "extensions.worktreeConfig",
                false,
                lenient,
            )?;
        if extension_worktree {
            let worktree_config = load_config(
                git_dir.join("config.worktree"),
                &mut buf,
                gix_config::Source::Worktree,
                git_dir_trust,
                lossy,
                lenient,
            )?;
            // `check_repository_format_gently()` (`setup.c:787-801`, v2.55.0)
            // re-reads the per-worktree file through `read_worktree_config()`, so
            // a `core.bare` there replaces the one from the common `config`
            // before discovery decides whether there is a work tree.
            if let Some(bare) = util::config_bool_opt(&worktree_config, &Core::BARE, "core.bare", lenient)? {
                is_bare = Some(bare);
            }
            config.append(worktree_config)?;
        }
        // `git --bare` leaves `is_bare_repository_cfg = 1` (git.c:258, v2.55.0) until a
        // `core.bare` replaces it: setup's `read_worktree_config()` (setup.c:798-801) when there
        // is no common directory, and `git_default_core_config()` (environment.c:339-342) once
        // the configuration is read in any case.
        if crate::open::bare_repository_cfg() {
            is_bare = is_bare.or(Some(true));
        }
        let precompose_unicode = Core::PRECOMPOSE_UNICODE
            .enrich_error(config.boolean(Core::PRECOMPOSE_UNICODE))
            .with_leniency(lenient)
            .map_err(Error::ConfigBoolean)?
            .unwrap_or_default();

        const IS_WINDOWS: bool = cfg!(windows);
        let protect_windows = gitoxide::Core::PROTECT_WINDOWS
            .enrich_error(config.boolean(gitoxide::Core::PROTECT_WINDOWS))
            .with_lenient_default_value(lenient, Some(IS_WINDOWS))?
            .unwrap_or(IS_WINDOWS);

        let reflog = util::query_refupdates(&config, lenient)?;
        Ok(StageOne {
            git_dir_config: config,
            buf,
            is_bare,
            lossy,
            object_hash,
            reflog,
            precompose_unicode,
            protect_windows,
            ref_storage,
            worktree_config: extension_worktree,
        })
    }
}

/// `hash_algo_by_name()` (hash.c:331-339) as `handle_extension()` applies it to
/// `extensions.objectFormat` (setup.c:659-669): only the exact names `sha1`
/// and `sha256` are formats, `SHA1` is refused like any other value.
fn object_format_by_name(value: &crate::bstr::BStr) -> Result<gix_hash::Kind, Error> {
    if value != "sha1" && value != "sha256" {
        return Err(Error::ConfigTypedString(
            crate::config::key::GenericErrorWithValue::from_value(&Extensions::OBJECT_FORMAT, value.to_owned()),
        ));
    }
    Ok(Extensions::OBJECT_FORMAT.try_into_object_format(value)?)
}

/// The backend `extensions.refStorage` names: `parse_reference_uri()`
/// (setup.c:635-648) takes the part before `://` as the format, which
/// `ref_storage_format_by_name()` (refs.c:49-55) compares with `strcmp`.
/// Any other value is one git refuses while reading the configuration, which
/// callers report before a repository is opened; it is the files backend here.
///
/// The payload after `://` relocates the store (`refs_compute_filesystem_location()`,
/// refs.c:3564-3599); that is not ported, the store stays at the git directory.
fn ref_storage_by_uri(value: &crate::bstr::BStr) -> gix_ref::store::RefStorage {
    use crate::bstr::ByteSlice;
    let format = value.find("://").map_or(value.as_bytes(), |end| &value[..end]);
    if format == b"reftable" {
        gix_ref::store::RefStorage::Reftable
    } else {
        gix_ref::store::RefStorage::Files
    }
}

/// Return the object hash for a repository that does not set `extensions.objectFormat`.
///
/// Git interprets a missing objectFormat as the original Sha1 layout, so we return
/// gix_hash::Kind::Sha1 whenever this build can handle it.
/// In Sha256-only builds we cannot open such a repository, so return an error instead.
fn legacy_object_hash() -> Result<gix_hash::Kind, Error> {
    #[cfg(feature = "sha1")]
    {
        Ok(gix_hash::Kind::Sha1)
    }
    #[cfg(not(feature = "sha1"))]
    {
        Err(Error::UnsupportedObjectFormat { name: "sha1".into() })
    }
}

fn load_config(
    config_path: std::path::PathBuf,
    buf: &mut Vec<u8>,
    source: gix_config::Source,
    git_dir_trust: gix_sec::Trust,
    lossy: bool,
    lenient: bool,
) -> Result<gix_config::File, Error> {
    let metadata = gix_config::file::Metadata::from(source)
        .at(&config_path)
        .with(git_dir_trust);
    let mut file = match std::fs::File::open(&config_path) {
        Ok(f) => f,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(gix_config::File::new(metadata)),
        Err(err) => {
            let err = Error::Io {
                source: err,
                path: config_path,
            };
            if lenient {
                gix_trace::warn!("ignoring: {err:#?}");
                return Ok(gix_config::File::new(metadata));
            } else {
                return Err(err);
            }
        }
    };

    buf.clear();
    if let Err(err) = std::io::copy(&mut file, buf) {
        let err = Error::Io {
            source: err,
            path: config_path,
        };
        if lenient {
            gix_trace::warn!("ignoring: {err:#?}");
            buf.clear();
        } else {
            return Err(err);
        }
    }

    let config = gix_config::File::from_bytes_owned(
        buf,
        metadata,
        gix_config::file::init::Options {
            includes: gix_config::file::includes::Options::no_follow(),
            ..util::base_options(lossy, lenient)
        },
    )?;

    Ok(config)
}

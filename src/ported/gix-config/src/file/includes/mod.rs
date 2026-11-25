use std::path::{Path, PathBuf};

use bstr::{BStr, BString, ByteSlice, ByteVec};
use gix_features::threading::OwnShared;
use gix_ref::Category;

use crate::{
    File, file,
    file::{Metadata, SectionId, includes, init},
    path,
};

impl File {
    /// Traverse all `include` and `includeIf` directives found in this instance and follow them, loading the
    /// referenced files from their location and adding their content right past the value that included them.
    ///
    /// # Limitations
    ///
    /// - Note that this method is _not idempotent_ and calling it multiple times will resolve includes multiple
    ///   times. It's recommended use is as part of a multi-step bootstrapping which needs fine-grained control,
    ///   and unless that's given one should prefer one of the other ways of initialization that resolve includes
    ///   at the right time.
    ///
    /// # Deviation
    ///
    /// - included values are added after the _section_ that included them, not directly after the value. This is
    ///   a deviation from how git does it, as it technically adds new value right after the include path itself,
    ///   technically 'splitting' the section. This can only make a difference if the `include` section also has values
    ///   which later overwrite portions of the included file, which seems unusual as these would be related to `includes`.
    ///   We can fix this by 'splitting' the include section if needed so the included sections are put into the right place.
    /// - `hasconfig:remote.*.url` matches the URLs of the whole configuration, includes followed and every
    ///   `hasconfig:remote.*.url` include taken as true, as `populate_remote_urls()` gathers them (config.c:338-350).
    ///   Where git dies on a `[remote "name"]\nurl = x` in a file such an include brings in (`forbid_remote_url()`,
    ///   config.c:352-366), its URL is gathered here like any other.
    pub fn resolve_includes(&mut self, options: init::Options<'_>) -> Result<(), Error> {
        self.resolve_includes_with_remote_urls(options, &[])
    }

    /// Like [`resolve_includes()`](Self::resolve_includes), with `extra_remote_urls`
    /// standing for the remote URLs of configuration git reads after this one in
    /// its sequence, the command line's, which a `hasconfig:remote.*.url:`
    /// condition matches against as well.
    pub fn resolve_includes_with_remote_urls(
        &mut self,
        options: init::Options<'_>,
        extra_remote_urls: &[BString],
    ) -> Result<(), Error> {
        if options.includes.max_depth == 0 {
            return Ok(());
        }
        let mut buf = Vec::new();
        resolve_with_remote_urls(self, &mut buf, options, extra_remote_urls)
    }
}

pub(crate) fn resolve(config: &mut File, buf: &mut Vec<u8>, options: init::Options<'_>) -> Result<(), Error> {
    resolve_with_remote_urls(config, buf, options, &[])
}

/// How `hasconfig:remote.*.url:` conditions are answered during one resolution.
#[derive(Clone, Copy)]
enum RemoteUrls<'a> {
    /// `opts->unconditional_remote_url` (config.c:385-388): every such condition holds.
    Unconditional,
    /// `inc->remote_urls`, gathered by the first condition that asks.
    Gather(&'a GatherRemoteUrls<'a>),
}

/// `populate_remote_urls()` (config.c:338-350), run at most once per resolution:
/// the configuration as it was before any include was followed, read again with
/// every `hasconfig:remote.*.url:` include followed (`opts.unconditional_remote_url`),
/// so a URL configured in a file that `include.path` or another condition brings
/// in counts as well.
struct GatherRemoteUrls<'a> {
    unresolved: File,
    extra: &'a [BString],
    options: init::Options<'a>,
    urls: std::cell::RefCell<Option<Vec<BString>>>,
}

impl GatherRemoteUrls<'_> {
    /// `at_least_one_url_matches_glob()` (config.c:368-383): `wildmatch()` with
    /// `WM_PATHNAME` against each URL.
    fn any_matches(&self, glob: &BStr) -> Result<bool, Error> {
        if self.urls.borrow().is_none() {
            let mut everything = self.unresolved.clone();
            let mut buf = Vec::new();
            resolve_includes_recursive(None, &mut everything, 0, &mut buf, self.options, RemoteUrls::Unconditional)?;
            let mut urls = remote_urls(&everything);
            urls.extend_from_slice(self.extra);
            *self.urls.borrow_mut() = Some(urls);
        }
        let urls = self.urls.borrow();
        Ok(urls.iter().flatten().any(|url| {
            gix_glob::wildmatch(glob, url.as_bstr(), gix_glob::wildmatch::Mode::NO_MATCH_SLASH_LITERAL)
        }))
    }
}

fn resolve_with_remote_urls<'a>(
    config: &mut File,
    buf: &mut Vec<u8>,
    options: init::Options<'a>,
    extra_remote_urls: &'a [BString],
) -> Result<(), Error> {
    let gather = GatherRemoteUrls {
        unresolved: config.clone(),
        extra: extra_remote_urls,
        options,
        urls: Default::default(),
    };
    resolve_includes_recursive(None, config, 0, buf, options, RemoteUrls::Gather(&gather))
}

/// `add_remote_url()` (config.c:322-336): the `url` of every named remote, in order.
fn remote_urls(config: &File) -> Vec<BString> {
    config
        .sections_by_name("remote")
        .into_iter()
        .flatten()
        .filter(|section| section.header().subsection_name().is_some())
        .flat_map(|section| section.values("url"))
        .collect()
}

fn resolve_includes_recursive(
    search_config: Option<&File>,
    target_config: &mut File,
    depth: u8,
    buf: &mut Vec<u8>,
    options: init::Options<'_>,
    remote_urls: RemoteUrls<'_>,
) -> Result<(), Error> {
    if depth == options.includes.max_depth {
        return if options.includes.err_on_max_depth_exceeded {
            Err(Error::IncludeDepthExceeded {
                max_depth: options.includes.max_depth,
            })
        } else {
            Ok(())
        };
    }

    for id in target_config.section_order.clone().into_iter() {
        let section = &target_config.sections[&id];
        let header = &section.header;
        let backing = &target_config.backing;
        let header_name = header.name.as_bstr_in(backing);
        let mut paths = None;
        if header_name == "include" && header.subsection_name.is_none() {
            paths = Some(gather_paths(section, id, backing));
        } else if header_name == "includeIf" {
            if let Some(condition) = &header.subsection_name {
                let target_config_path = section.meta.path.as_deref();
                if include_condition_match(
                    condition.value_in(backing),
                    target_config_path,
                    search_config.unwrap_or(target_config),
                    options.includes,
                    remote_urls,
                )? {
                    paths = Some(gather_paths(section, id, backing));
                }
            }
        }
        if let Some(paths) = paths {
            insert_includes_recursively(paths, target_config, depth, options, buf, remote_urls)?;
        }
    }
    Ok(())
}

fn insert_includes_recursively(
    section_ids_and_include_paths: Vec<(SectionId, crate::Path)>,
    target_config: &mut File,
    depth: u8,
    options: init::Options<'_>,
    buf: &mut Vec<u8>,
    remote_urls: RemoteUrls<'_>,
) -> Result<(), Error> {
    for (section_id, config_path) in section_ids_and_include_paths {
        let meta = OwnShared::clone(&target_config.sections[&section_id].meta);
        let target_config_path = meta.path.as_deref();
        let config_path = match resolve_path(config_path, target_config_path, options.includes)? {
            Some(p) => p,
            None => continue,
        };
        if !config_path.is_file() {
            continue;
        }

        buf.clear();
        std::io::copy(
            &mut std::fs::File::open(&config_path).map_err(|err| Error::Io {
                source: err,
                path: config_path.to_owned(),
            })?,
            buf,
        )
        .map_err(Error::CopyBuffer)?;
        let config_meta = Metadata {
            path: Some(config_path),
            trust: meta.trust,
            level: meta.level + 1,
            source: meta.source,
        };
        let no_follow_options = init::Options {
            includes: includes::Options::no_follow(),
            ..options
        };

        let mut include_config =
            File::from_bytes_owned(buf, config_meta, no_follow_options).map_err(|err| match err {
                init::Error::Parse(err) => Error::Parse(err),
                init::Error::Interpolate(err) => Error::Interpolate(err),
                init::Error::Span(err) => Error::Span(err),
                init::Error::Includes(_) => unreachable!("BUG: {:?} not possible due to no-follow options", err),
            })?;
        resolve_includes_recursive(Some(target_config), &mut include_config, depth + 1, buf, options, remote_urls)?;

        target_config.append_or_insert(include_config, Some(section_id))?;
    }
    Ok(())
}

fn gather_paths(section: &file::SectionData, id: SectionId, backing: &[u8]) -> Vec<(SectionId, crate::Path)> {
    section
        .body
        .values_in(backing, "path")
        .into_iter()
        .map(|path| (id, crate::Path::from(path)))
        .collect()
}

fn include_condition_match(
    condition: &BStr,
    target_config_path: Option<&Path>,
    search_config: &File,
    options: Options<'_>,
    remote_urls: RemoteUrls<'_>,
) -> Result<bool, Error> {
    let mut tokens = condition.splitn(2, |b| *b == b':');
    let (prefix, condition) = match (tokens.next(), tokens.next()) {
        (Some(a), Some(b)) => (a, b),
        _ => return Ok(false),
    };
    let condition = condition.as_bstr();
    match prefix {
        b"gitdir" => gitdir_matches(
            condition,
            target_config_path,
            options,
            gix_glob::wildmatch::Mode::empty(),
        ),
        b"gitdir/i" => gitdir_matches(
            condition,
            target_config_path,
            options,
            gix_glob::wildmatch::Mode::IGNORE_CASE,
        ),
        // `include_condition_is_true()` (config.c:403-408, git 2.56): the same
        // `include_by_path()` as `gitdir:`, against `repo_get_work_tree()`. No
        // work tree is `if (!path) goto done`, false and never an error.
        b"worktree" | b"worktree/i" => {
            let Some(work_tree) = options.conditional.work_tree else {
                return Ok(false);
            };
            let mode = if prefix == b"worktree/i" {
                gix_glob::wildmatch::Mode::IGNORE_CASE
            } else {
                gix_glob::wildmatch::Mode::empty()
            };
            // `strbuf_add_absolute_path()` on a work tree git already holds as a clean
            // absolute path; a relative one here (`..` from a subdirectory) has to be
            // made absolute and have its `..` resolved before it is matched.
            let absolute = std::path::absolute(work_tree).unwrap_or_else(|_| work_tree.to_owned());
            let work_tree = std::env::current_dir()
                .ok()
                .and_then(|cwd| gix_path::normalize(absolute.as_path().into(), &cwd).map(std::borrow::Cow::into_owned))
                .unwrap_or(absolute);
            path_matches(condition, &work_tree, target_config_path, options, mode)
        }
        b"onbranch" => Ok(onbranch_matches(condition, options.conditional).is_some()),
        b"hasconfig" => {
            let mut tokens = condition.splitn(2, |b| *b == b':');
            let (key_glob, value_glob) = match (tokens.next(), tokens.next()) {
                (Some(a), Some(b)) => (a, b),
                _ => return Ok(false),
            };
            if key_glob.as_bstr() != "remote.*.url" {
                return Ok(false);
            }
            // ```c
            // if (inc->opts->unconditional_remote_url)
            //         return 1;
            // if (!inc->remote_urls)
            //         populate_remote_urls(inc);
            // return at_least_one_url_matches_glob(cond, cond_len, inc->remote_urls);
            // ```
            //
            // (`include_by_remote_url()`, config.c:385-394.) The URLs are those of the
            // whole configuration, not of `search_config`, the file being resolved.
            let _ = search_config;
            match remote_urls {
                RemoteUrls::Unconditional => Ok(true),
                RemoteUrls::Gather(gather) => gather.any_matches(value_glob.as_bstr()),
            }
        }
        _ => Ok(false),
    }
}

fn onbranch_matches(
    condition: &BStr,
    conditional::Context { branch_name, .. }: conditional::Context<'_>,
) -> Option<()> {
    let branch_name = branch_name?;
    let (_, branch_name) = branch_name
        .category_and_short_name()
        .filter(|(cat, _)| *cat == Category::LocalBranch)?;

    let condition: BString = if condition.ends_with(b"/") {
        let mut condition: BString = condition.into();
        condition.push_str("**");
        condition
    } else {
        condition.into()
    };

    gix_glob::wildmatch(
        condition.as_bstr(),
        branch_name,
        gix_glob::wildmatch::Mode::NO_MATCH_SLASH_LITERAL,
    )
    .then_some(())
}

fn gitdir_matches(
    condition_path: &BStr,
    target_config_path: Option<&Path>,
    options: Options<'_>,
    wildmatch_mode: gix_glob::wildmatch::Mode,
) -> Result<bool, Error> {
    let git_dir = options.conditional.git_dir;
    if !options.err_on_interpolation_failure && git_dir.is_none() {
        return Ok(false);
    }
    path_matches(
        condition_path,
        git_dir.ok_or(Error::MissingGitDir)?,
        target_config_path,
        options,
        wildmatch_mode,
    )
}

/// `include_by_path()` (config.c:238-292, git 2.56): the pattern of a `gitdir:` or
/// `worktree:` condition matched against `path`, then against its realpath.
fn path_matches(
    condition_path: &BStr,
    path: &Path,
    target_config_path: Option<&Path>,
    Options {
        interpolate: context,
        err_on_interpolation_failure,
        err_on_missing_config_path,
        ..
    }: Options<'_>,
    wildmatch_mode: gix_glob::wildmatch::Mode,
) -> Result<bool, Error> {
    let path_text = gix_path::to_unix_separators_on_windows(gix_path::into_bstr(path));

    let mut pattern_path: BString = {
        let path = match check_interpolation_result(
            err_on_interpolation_failure,
            crate::Path::from(condition_path.to_owned()).interpolate(context),
        )? {
            Some(p) => p,
            None => return Ok(false),
        };
        gix_path::into_bstr(path).into_owned()
    };
    // NOTE: yes, only if we do path interpolation will the slashes be forced to unix separators on windows
    if pattern_path != condition_path {
        pattern_path = gix_path::to_unix_separators_on_windows(pattern_path).into_owned();
    }

    if let Some(relative_pattern_path) = pattern_path.strip_prefix(b"./") {
        if !err_on_missing_config_path && target_config_path.is_none() {
            return Ok(false);
        }
        // git's `prepare_include_condition_pattern()` (config.c:214-219) answers a
        // `./` pattern with no file behind it with `error()`, whose `-1` the `&&`
        // chain in `git_config_include()` (config.c:436-438) reads as true: the
        // `path` is then followed. The `error:` line itself is printed where the
        // command-line configuration is validated, before any of it is loaded.
        if target_config_path.is_none() {
            return Ok(true);
        }
        let parent_dir = target_config_path
            .ok_or(Error::MissingConfigPath)?
            .parent()
            .expect("config path can never be /");
        let mut joined_path = gix_path::to_unix_separators_on_windows(gix_path::into_bstr(parent_dir)).into_owned();
        joined_path.push(b'/');
        joined_path.extend_from_slice(relative_pattern_path);
        pattern_path = joined_path;
    }

    // NOTE: this special handling of leading backslash is needed to do it like git does
    if pattern_path.iter().next() != Some(&(std::path::MAIN_SEPARATOR as u8))
        && !gix_path::from_bstr(pattern_path.clone()).is_absolute()
    {
        pattern_path.insert_str(0, "**/");
    }
    if pattern_path.ends_with(b"/") {
        pattern_path.push_str("**");
    }

    let match_mode = gix_glob::wildmatch::Mode::NO_MATCH_SLASH_LITERAL | wildmatch_mode;
    let is_match = gix_glob::wildmatch(pattern_path.as_bstr(), path_text.as_bstr(), match_mode);
    if is_match {
        return Ok(true);
    }

    let real_path = gix_path::into_bstr(gix_path::realpath(gix_path::from_byte_slice(&path_text))?);
    Ok(gix_glob::wildmatch(
        pattern_path.as_bstr(),
        real_path.as_bstr(),
        match_mode,
    ))
}

fn check_interpolation_result(
    disable: bool,
    res: Result<impl Into<PathBuf>, path::interpolate::Error>,
) -> Result<Option<PathBuf>, path::interpolate::Error> {
    if disable {
        return res.map(|path| Some(path.into()));
    }
    match res {
        Ok(good) => Ok(Some(good.into())),
        Err(err) => match err {
            path::interpolate::Error::Missing { .. } | path::interpolate::Error::UserInterpolationUnsupported => {
                Ok(None)
            }
            path::interpolate::Error::UsernameConversion(_) | path::interpolate::Error::Utf8Conversion { .. } => {
                Err(err)
            }
        },
    }
}

fn resolve_path(
    path: crate::Path,
    target_config_path: Option<&Path>,
    includes::Options {
        interpolate: context,
        err_on_interpolation_failure,
        err_on_missing_config_path,
        ..
    }: includes::Options<'_>,
) -> Result<Option<PathBuf>, Error> {
    let path = match check_interpolation_result(err_on_interpolation_failure, path.interpolate(context))? {
        Some(p) => p,
        None => return Ok(None),
    };
    let path: PathBuf = if path.is_relative() {
        if !err_on_missing_config_path && target_config_path.is_none() {
            return Ok(None);
        }
        target_config_path
            .ok_or(Error::MissingConfigPath)?
            .parent()
            .expect("path is a config file which naturally lives in a directory")
            .join(path)
    } else {
        path
    };
    Ok(Some(path))
}

mod types;
pub use types::{Error, Options, conditional};

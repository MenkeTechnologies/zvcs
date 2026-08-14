use std::path::{Path, PathBuf};

use crate::DOT_GIT_DIR;

pub(crate) fn shorten_path_with_cwd(cursor: PathBuf, cwd: &Path) -> PathBuf {
    fn comp_len(c: std::path::Component<'_>) -> usize {
        use std::path::Component::*;
        match c {
            Prefix(p) => p.as_os_str().len(),
            CurDir => 1,
            ParentDir => 2,
            Normal(p) => p.len(),
            RootDir => 1,
        }
    }

    debug_assert_eq!(cursor.file_name().and_then(std::ffi::OsStr::to_str), Some(DOT_GIT_DIR));
    let parent = cursor.parent().expect(".git appended");
    cwd.strip_prefix(parent)
        .ok()
        .and_then(|path_relative_to_cwd| {
            let relative_path_components = path_relative_to_cwd.components().count();
            let current_component_len = cursor.components().map(comp_len).sum::<usize>();
            (relative_path_components * "..".len() < current_component_len).then(|| {
                std::iter::repeat_n("..", relative_path_components)
                    .chain(Some(DOT_GIT_DIR))
                    .collect()
            })
        })
        .unwrap_or(cursor)
}

/// Find the number of components parenting the `search_dir` before the first directory in `ceiling_dirs` or
/// `verbatim_ceiling_dirs`.
///
/// `search_dir` needs to be normalized. Every entry of `ceiling_dirs` is normalized and made absolute as well, so it
/// matches no matter how it is spelled, while `verbatim_ceiling_dirs` are compared exactly as they are - see
/// [`Options::ceiling_dirs_verbatim`][crate::upwards::Options::ceiling_dirs_verbatim].
///
/// The deepest ceiling out of both lists wins, which is git taking the *longest* ancestor in `longest_ancestor_length()`.
pub(crate) fn find_ceiling_height(
    search_dir: &Path,
    ceiling_dirs: &[PathBuf],
    verbatim_ceiling_dirs: &[PathBuf],
    cwd: &Path,
) -> Option<usize> {
    if ceiling_dirs.is_empty() && verbatim_ceiling_dirs.is_empty() {
        return None;
    }

    let search_realpath;
    let search_dir = if search_dir.is_absolute() {
        search_dir
    } else {
        search_realpath = gix_path::realpath_opts(search_dir, cwd, gix_path::realpath::MAX_SYMLINKS).ok()?;
        search_realpath.as_path()
    };
    let normalized = ceiling_dirs.iter().filter_map(|ceiling_dir| {
        #[cfg(windows)]
        let ceiling_dir = dunce::simplified(ceiling_dir);
        let mut ceiling_dir = gix_path::normalize(ceiling_dir.into(), cwd)?;
        if !ceiling_dir.is_absolute() {
            ceiling_dir = gix_path::normalize(cwd.join(ceiling_dir.as_ref()).into(), cwd)?;
        }
        search_dir
            .strip_prefix(ceiling_dir.as_ref())
            .ok()
            .map(|path_relative_to_ceiling| path_relative_to_ceiling.components().count())
            .filter(|height| *height > 0)
    });
    let verbatim = verbatim_ceiling_dirs
        .iter()
        .filter_map(|ceiling_dir| verbatim_ceiling_height(search_dir, ceiling_dir));
    normalized.chain(verbatim).min()
}

/// Return the height of `search_dir` above `ceiling_dir` if the latter is a proper ancestor of the former *as written*.
///
/// This is git's `longest_ancestor_length()` from `path.c`, with the matched byte length turned into the number of
/// components between the two paths, which is how this crate expresses a ceiling:
///
/// ```text
/// if (len > 0 && ceil[len - 1] == '/')
///         len--;
///
/// if (strncmp(path, ceil, len) ||
///     path[len] != '/' || !path[len + 1])
///         continue; /* no match */
/// ```
///
/// No normalization happens here, on purpose: git compares these entries as the raw strings they were spelled with, so
/// `/parent/child/..` is not an ancestor of `/parent/child/grandchild` even though it names one.
fn verbatim_ceiling_height(search_dir: &Path, ceiling_dir: &Path) -> Option<usize> {
    // git works on paths that use `/` throughout, including on Windows, and compares them byte by byte.
    let search_dir = gix_path::to_unix_separators_on_windows(gix_path::os_str_into_bstr(search_dir.as_os_str()).ok()?);
    let ceiling_dir =
        gix_path::to_unix_separators_on_windows(gix_path::os_str_into_bstr(ceiling_dir.as_os_str()).ok()?);
    if ceiling_dir.is_empty() {
        return None;
    }

    // git drops one trailing slash from the compared length, and its own comment gives the reason: a root directory
    // (`/`, `C:/`, `//server/share/`) is spelled with a trailing slash it cannot lose. The side effect on every other
    // entry is that `/parent/` and `/parent` are the same ceiling - but `/parent//` is neither, since only one slash
    // comes off.
    let len = ceiling_dir.len() - usize::from(ceiling_dir.last() == Some(&b'/'));
    if !search_dir.starts_with(&ceiling_dir[..len]) || search_dir.get(len) != Some(&b'/') {
        return None;
    }

    // `!path[len + 1]`: the ceiling must be a *proper* ancestor, so a trailing separator is not enough.
    let below_ceiling = search_dir.get(len + 1..)?;
    let height = below_ceiling.split(|b| *b == b'/').filter(|c| !c.is_empty()).count();
    (height > 0).then_some(height)
}

/// Returns the device ID of the directory.
#[cfg(target_os = "linux")]
pub(crate) fn device_id(m: &std::fs::Metadata) -> u64 {
    use std::os::linux::fs::MetadataExt;
    m.st_dev()
}

/// Returns the device ID of the directory.
#[cfg(all(unix, not(target_os = "linux")))]
pub(crate) fn device_id(m: &std::fs::Metadata) -> u64 {
    use std::os::unix::fs::MetadataExt;
    m.dev()
}

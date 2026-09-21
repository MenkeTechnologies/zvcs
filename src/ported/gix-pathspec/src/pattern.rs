use std::path::{Component, Path, PathBuf};

use bstr::{BStr, BString, ByteSlice, ByteVec};

use crate::{MagicSignature, Pattern, SearchMode, normalize};

/// Access
impl Pattern {
    /// Returns `true` if this seems to be a pathspec that indicates that 'there is no pathspec'.
    ///
    /// Note that such a spec is `:`.
    pub fn is_nil(&self) -> bool {
        self.nil
    }

    /// Return the prefix-portion of the `path` of this spec, which is a *directory*.
    /// It can be empty if there is no prefix.
    ///
    /// A prefix is effectively the CWD seen as relative to the working tree, and it's assumed to
    /// match case-sensitively. This makes it useful for skipping over large portions of input by
    /// directly comparing them.
    pub fn prefix_directory(&self) -> &BStr {
        self.path[..self.prefix_len].as_bstr()
    }

    /// Return the path of this spec, typically used for matching.
    pub fn path(&self) -> &BStr {
        self.path.as_ref()
    }
}

/// Mutation
impl Pattern {
    /// Normalize the pattern's path by assuring it's relative to the root of the working tree, and contains
    /// no relative path components. Further, it assures that `/` are used as path separator.
    ///
    /// If `self.path` is a relative path, it will be put in front of the pattern path if `self.signature` isn't indicating `TOP` already.
    /// If `self.path` is an absolute path, we will use `root` to make it worktree relative if possible.
    ///
    /// `prefix` can be empty, we will still normalize this pathspec to resolve relative path components, and
    /// it is assumed not to contain any relative path components, e.g. '', 'a', 'a/b' are valid.
    /// `root` is the absolute path to the root of either the worktree or the repository's `git_dir`.
    pub fn normalize(&mut self, prefix: &Path, root: &Path) -> Result<&mut Self, normalize::Error> {
        fn prefix_components_to_subtract(path: &Path) -> usize {
            let parent_component_end_bound = path.components().enumerate().fold(None::<usize>, |acc, (idx, c)| {
                matches!(c, Component::ParentDir).then_some(idx + 1).or(acc)
            });
            let count = path
                .components()
                .take(parent_component_end_bound.unwrap_or(0))
                .map(|c| match c {
                    Component::ParentDir => 1_isize,
                    Component::Normal(_) => -1,
                    _ => 0,
                })
                .sum::<isize>();
            if count > 0 { count as usize } else { Default::default() }
        }

        // ```c
        // if (pathspec_prefix >= 0) {
        //         match = xstrdup(copyfrom);
        //         prefixlen = pathspec_prefix;
        // } else if (magic & PATHSPEC_FROMTOP) {
        //         match = xstrdup(copyfrom);
        //         prefixlen = 0;
        // } else {
        //         match = prefix_path_gently(…);
        // ```
        //
        // (pathspec.c:481-490.) A rooted element's path is the one git never
        // touches: no prefix is joined to it, and — because
        // `prefix_path_gently()` is what calls `normalize_path_copy_len()`
        // (setup.c:119-160) — nothing folds its `.`, `..` or repeated `/`
        // either. Running the normalisation anyway made `:(top)./a.txt` match
        // `a.txt` where git matches nothing, made `:(top)a.txt/..` select the
        // whole tree, and turned `:(top)../x` — which git answers with a plain
        // no-match — into an `OutsideOfWorktree` error that surfaced as exit 128
        // with an empty stderr.
        //
        // This also settles absolute paths: git tests `pathspec_prefix`/
        // `PATHSPEC_FROMTOP` first, so `:(top)/abs` is the literal `/abs` and
        // never reaches the "outside of worktree" rejection below.
        if self.signature.contains(MagicSignature::TOP) || self.prefix_magic.is_some() {
            // `prefixlen = pathspec_prefix` / `= 0`, then `item->prefix =
            // prefixlen` (pathspec.c:484, :487, :507). git's sanity check
            // BUG()s on a prefix past the end of the match (pathspec.c:547-551);
            // with no `BUG()` to raise, an over-long one is clamped so the
            // prefix stays a prefix.
            self.prefix_len = self.prefix_magic.unwrap_or(0).min(self.path.len());
            return Ok(self);
        }

        // `normalize_path_copy_len()` folds a trailing `.` away but *keeps the
        // separator it sat behind* (path.c:1121-1204): `a/.` comes back as `a/`,
        // which is why `git log -- 'a.txt/.'` matches nothing while
        // `git log -- 'a.txt'` matches the file. Parsing sets `MUST_BE_DIR` only
        // for a slash the user wrote last, and the fold below then dropped the
        // `/.` without a trace, so the two spellings became the same pattern.
        if self.path.ends_with(b"/.") {
            self.signature |= MagicSignature::MUST_BE_DIR;
        }

        let mut path = gix_path::from_bstr(self.path.as_bstr());
        let mut num_prefix_components = 0;
        let mut was_absolute = false;
        if gix_path::is_absolute(path.as_ref()) {
            was_absolute = true;
            let rela_path = match path.strip_prefix(root) {
                Ok(path) => path,
                Err(_) => {
                    return Err(normalize::Error::AbsolutePathOutsideOfWorktree {
                        path: path.into_owned(),
                        worktree_path: root.into(),
                    });
                }
            };
            path = rela_path.to_owned().into();
        } else if !prefix.as_os_str().is_empty() {
            debug_assert_eq!(
                prefix
                    .components()
                    .filter(|c| matches!(c, Component::Normal(_)))
                    .count(),
                prefix.components().count(),
                "BUG: prefixes must not have relative path components, or calculations here will be wrong so pattern won't match"
            );
            num_prefix_components = prefix
                .components()
                .count()
                .saturating_sub(prefix_components_to_subtract(path.as_ref()));
            path = prefix.join(path).into();
        }

        let assure_path_cannot_break_out_upwards = Path::new("");
        let path = match gix_path::normalize(path.as_ref().into(), assure_path_cannot_break_out_upwards) {
            Some(path) => {
                if was_absolute {
                    num_prefix_components = path.components().count().saturating_sub(
                        if self.signature.contains(MagicSignature::MUST_BE_DIR) {
                            0
                        } else {
                            1
                        },
                    );
                }
                path
            }
            None => {
                return Err(normalize::Error::OutsideOfWorktree {
                    path: path.into_owned(),
                });
            }
        };

        self.path = if path == Path::new(".") {
            self.nil = true;
            BString::from(".")
        } else {
            let cleaned = PathBuf::from_iter(path.components().filter(|c| !matches!(c, Component::CurDir)));
            let mut out = gix_path::to_unix_separators_on_windows(gix_path::into_bstr(cleaned)).into_owned();
            self.prefix_len = {
                if self.signature.contains(MagicSignature::MUST_BE_DIR) {
                    out.push(b'/');
                }
                let len = out
                    .find_iter(b"/")
                    .take(num_prefix_components)
                    .last()
                    .unwrap_or_default();
                if self.signature.contains(MagicSignature::MUST_BE_DIR) {
                    out.pop();
                }
                len
            };
            out
        };

        Ok(self)
    }
}

/// Access
impl Pattern {
    /// Return `true` if this pathspec is negated, which means it will exclude an item from the result set instead of including it.
    pub fn is_excluded(&self) -> bool {
        self.signature.contains(MagicSignature::EXCLUDE)
    }

    /// Returns `true` is this pattern is supposed to always match, as it's either empty or designated `nil`.
    /// Note that technically the pattern might still be excluded.
    pub fn always_matches(&self) -> bool {
        self.is_nil() || self.path.is_empty()
    }

    /// Translate ourselves to a long display format, that when parsed back will yield the same pattern.
    ///
    /// Note that the
    pub fn to_bstring(&self) -> BString {
        if self.is_nil() {
            ":".into()
        } else {
            let mut buf: BString = ":(".into();
            if self.signature.contains(MagicSignature::TOP) {
                buf.push_str("top,");
            }
            if self.signature.contains(MagicSignature::EXCLUDE) {
                buf.push_str("exclude,");
            }
            if self.signature.contains(MagicSignature::ICASE) {
                buf.push_str("icase,");
            }
            match self.search_mode {
                SearchMode::ShellGlob => {}
                SearchMode::Literal => buf.push_str("literal,"),
                SearchMode::PathAwareGlob => buf.push_str("glob,"),
            }
            if self.attributes.is_empty() {
                if buf.last() == Some(&b',') {
                    buf.pop();
                }
            } else {
                buf.push_str("attr:");
                for attr in &self.attributes {
                    let attr = attr.as_ref().to_string().replace(',', r"\,");
                    buf.push_str(&attr);
                    buf.push(b' ');
                }
                buf.pop(); // trailing ' '
            }
            buf.push(b')');
            buf.extend_from_slice(&self.path);
            if self.signature.contains(MagicSignature::MUST_BE_DIR) {
                buf.push(b'/');
            }
            buf
        }
    }
}

impl std::fmt::Display for Pattern {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.to_bstring().fmt(f)
    }
}

//! `versioncmp.c` and the `--sort` key grammar of `ref-filter.c`, in one place.
//!
//! Four verbs order refs with git's version comparison — `for-each-ref`,
//! `branch`, `tag` and `ls-remote` — and in git all four reach the same
//! `versioncmp()` through the single call site at `ref-filter.c:2506`. The
//! prerelease-suffix list that comparison consults lives in a file static
//! (`versioncmp.c:25-26`) seeded once from `versionsort.suffix`, so a verb
//! cannot opt out of it: whatever `git tag --sort=version:refname` does,
//! `git for-each-ref --sort=version:refname` does too.
//!
//! [`Prereleases`] reproduces that static faithfully, including *when* it is
//! read. git looks at the configuration only after the two strings have already
//! been walked to their first difference (`versioncmp.c:162-172`), which is why
//! `git tag --sort=version:refname` over a single ref prints no
//! `ignoring versionsort.prereleasesuffix` warning even with both keys set —
//! nothing was ever compared. Reading eagerly would move that warning onto runs
//! git leaves silent.

use std::cmp::Ordering;
use std::sync::OnceLock;

/// git's `valid_atom[]` (`ref-filter.c`), the field names `--sort` accepts
/// before any verb narrows them further.
const VALID_SORT_ATOMS: &[&str] = &[
    "refname",
    "objecttype",
    "objectsize",
    "objectname",
    "deltabase",
    "tree",
    "parent",
    "numparent",
    "object",
    "type",
    "tag",
    "author",
    "authorname",
    "authoremail",
    "authordate",
    "committer",
    "committername",
    "committeremail",
    "committerdate",
    "tagger",
    "taggername",
    "taggeremail",
    "taggerdate",
    "creator",
    "creatordate",
    "subject",
    "body",
    "trailers",
    "contents",
    "signature",
    "raw",
    "upstream",
    "push",
    "symref",
    "flag",
    "HEAD",
    "color",
    "worktreepath",
    "align",
    "end",
    "if",
    "then",
    "else",
    "rest",
    "ahead-behind",
    "is-base",
    "describe",
];

/// Split a `--sort` key into its `-` (descending), `version:`/`v:` and `*`
/// (dereference) markers and the remaining field atom.
pub fn parse_sort_key(key: &str) -> (bool, bool, bool, &str) {
    let mut s = key;
    let mut reverse = false;
    if let Some(rest) = s.strip_prefix('-') {
        reverse = true;
        s = rest;
    }
    let mut version = false;
    if let Some(rest) = s.strip_prefix("version:").or_else(|| s.strip_prefix("v:")) {
        version = true;
        s = rest;
    }
    let mut star = false;
    if let Some(rest) = s.strip_prefix('*') {
        star = true;
        s = rest;
    }
    (reverse, version, star, s)
}

/// git's `parse_ref_filter_atom`: an empty atom is a `malformed field name`, and
/// a field name outside `valid_atom[]` is an `unknown field name`.
pub fn sort_error(key: &str) -> Option<String> {
    let (_, _, _, atom) = parse_sort_key(key);
    if atom.is_empty() {
        return Some(format!("malformed field name: {atom}"));
    }
    let name = atom.split(':').next().unwrap_or(atom);
    if !VALID_SORT_ATOMS.contains(&name) {
        return Some(format!("unknown field name: {atom}"));
    }
    None
}

/// `%(refname:lstrip=<n>)` / `%(refname:rstrip=<n>)`.
///
/// A positive `n` drops `n` components from the given end; a negative `n` keeps
/// `-n` components at that end. Over-stripping yields an empty string for
/// positive counts and the full name for negative ones — never an error.
pub fn strip_components(name: &[u8], n: i64, from_left: bool) -> Vec<u8> {
    let parts: Vec<&[u8]> = name.split(|&b| b == b'/').collect();
    let len = parts.len() as i64;
    let kept: &[&[u8]] = if n >= 0 {
        if n >= len {
            &[]
        } else if from_left {
            &parts[n as usize..]
        } else {
            &parts[..(len - n) as usize]
        }
    } else {
        let keep = -n;
        if keep >= len {
            &parts[..]
        } else if from_left {
            &parts[(len - keep) as usize..]
        } else {
            &parts[..keep as usize]
        }
    };
    kept.join(&b'/')
}

/// The prerelease-suffix list `versioncmp()` consults, with git's lazy,
/// once-per-process initialization (`versioncmp.c:25-26`, `:162-172`).
///
/// `versionsort.suffix` wins over the deprecated
/// `versionsort.prereleasesuffix`, and having both set warns — once, and only
/// once a comparison has actually asked for the list.
pub struct Prereleases<'a> {
    repo: Option<&'a gix::Repository>,
    cell: OnceLock<Vec<Vec<u8>>>,
}

impl<'a> Prereleases<'a> {
    /// The list `repo`'s configuration defines, read on first use.
    pub fn new(repo: &'a gix::Repository) -> Self {
        Prereleases {
            repo: Some(repo),
            cell: OnceLock::new(),
        }
    }

    /// An always-empty list, for a comparison with no repository behind it.
    pub fn none() -> Self {
        Prereleases {
            repo: None,
            cell: OnceLock::new(),
        }
    }

    /// git's `prereleases` static: the configured suffixes, in configuration
    /// order. The read — and the deprecation warning it can print — happens
    /// exactly once, here.
    fn get(&self) -> &[Vec<u8>] {
        self.cell.get_or_init(|| {
            let Some(repo) = self.repo else {
                return Vec::new();
            };
            let snapshot = repo.config_snapshot();
            let config = snapshot.plumbing();
            let newl = config.strings("versionsort.suffix");
            let oldl = config.strings("versionsort.prereleasesuffix");
            match (newl, oldl) {
                (Some(new), Some(_)) => {
                    eprintln!(
                        "warning: ignoring versionsort.prereleasesuffix because \
                         versionsort.suffix is set"
                    );
                    new.into_iter().map(|s| s.to_vec()).collect()
                }
                (Some(new), None) => new.into_iter().map(|s| s.to_vec()).collect(),
                (None, Some(old)) => old.into_iter().map(|s| s.to_vec()).collect(),
                (None, None) => Vec::new(),
            }
        })
    }
}

/// A partial match of a configured prerelease suffix within a version string.
struct SuffixMatch {
    conf_pos: i32,
    start: i32,
    len: i32,
}

/// git's `find_better_matching_suffix`: try to improve `match` with an earlier
/// (or same-offset-but-longer) placement of `suffix` in `tagname`.
fn find_better_matching_suffix(
    tagname: &[u8],
    suffix: &[u8],
    conf_pos: i32,
    start: i32,
    m: &mut SuffixMatch,
) {
    let suffix_len = suffix.len() as i32;
    // A better match either starts earlier or starts at the same offset but is
    // longer.
    let end = if m.len < suffix_len { m.start } else { m.start - 1 };
    let mut i = start;
    while i <= end {
        let at = i as usize;
        if at <= tagname.len() && tagname[at..].starts_with(suffix) {
            m.conf_pos = conf_pos;
            m.start = i;
            m.len = suffix_len;
            break;
        }
        i += 1;
    }
}

/// git's `swap_prereleases`: when a configured prerelease suffix straddles the
/// first differing offset `off`, force the string carrying the earlier-ranked
/// suffix to sort on top. Returns `Some(diff)` when it decides the order.
fn swap_prereleases(s1: &[u8], s2: &[u8], off: i32, prereleases: &[Vec<u8>]) -> Option<i32> {
    let mut match1 = SuffixMatch {
        conf_pos: -1,
        start: off,
        len: -1,
    };
    let mut match2 = SuffixMatch {
        conf_pos: -1,
        start: off,
        len: -1,
    };

    for (i, suffix) in prereleases.iter().enumerate() {
        let suffix_len = suffix.len() as i32;
        let start = if suffix_len < off {
            off - suffix_len
        } else {
            0
        };
        find_better_matching_suffix(s1, suffix, i as i32, start, &mut match1);
        find_better_matching_suffix(s2, suffix, i as i32, start, &mut match2);
    }
    if match1.conf_pos == -1 && match2.conf_pos == -1 {
        return None;
    }
    if match1.conf_pos == match2.conf_pos {
        // The same suffix in both (e.g. "-rc" in "v1.0-rcX" and "v1.0-rcY"):
        // let the caller decide from what follows.
        return None;
    }
    let diff = if match1.conf_pos >= 0 && match2.conf_pos >= 0 {
        match1.conf_pos - match2.conf_pos
    } else if match1.conf_pos >= 0 {
        -1
    } else {
        1
    };
    Some(diff)
}

/// git's `versioncmp` (glibc `strverscmp` plus git's prerelease-suffix rule):
/// compare two byte strings as version numbers.
pub fn versioncmp(s1: &[u8], s2: &[u8], prereleases: &Prereleases<'_>) -> Ordering {
    // States S_N=0, S_I=3, S_F=6, S_Z=9; columns x=0 (other), d=1 (1-9), 0=2.
    const NEXT_STATE: [u8; 12] = [
        /* S_N */ 0, 3, 9, //
        /* S_I */ 0, 3, 3, //
        /* S_F */ 0, 6, 6, //
        /* S_Z */ 0, 6, 9, //
    ];
    // CMP=2, LEN=3; every other cell is the literal result (-1 or +1).
    const RESULT_TYPE: [i8; 36] = [
        /* S_N */ 2, 2, 2, 2, 3, 2, 2, 2, 2, //
        /* S_I */ 2, -1, -1, 1, 3, 3, 1, 3, 3, //
        /* S_F */ 2, 2, 2, 2, 2, 2, 2, 2, 2, //
        /* S_Z */ 2, 1, 1, -1, 2, 2, -1, 2, 2, //
    ];

    // git operates on NUL-terminated strings; reads past the end return 0.
    let byte = |s: &[u8], i: usize| -> u8 { s.get(i).copied().unwrap_or(0) };
    let col = |c: u8| -> usize { (c == b'0') as usize + (c.is_ascii_digit() as usize) };

    let mut i1 = 0usize;
    let mut i2 = 0usize;
    let mut c1 = byte(s1, i1);
    i1 += 1;
    let mut c2 = byte(s2, i2);
    i2 += 1;
    // Hint: '0' is a digit too.
    let mut state = col(c1);

    let mut diff = c1 as i32 - c2 as i32;
    while diff == 0 {
        if c1 == 0 {
            return Ordering::Equal;
        }
        state = NEXT_STATE[state] as usize;
        c1 = byte(s1, i1);
        i1 += 1;
        c2 = byte(s2, i2);
        i2 += 1;
        state += col(c1);
        diff = c1 as i32 - c2 as i32;
    }

    // Only now does git read the configuration, and only a configured suffix
    // straddling the first difference can flip the order outright.
    let prereleases = prereleases.get();
    if !prereleases.is_empty() {
        if let Some(d) = swap_prereleases(s1, s2, (i1 - 1) as i32, prereleases) {
            return d.cmp(&0);
        }
    }

    match RESULT_TYPE[state * 3 + col(c2)] {
        2 => diff.cmp(&0), // CMP
        3 => {
            // LEN: the longer run of leading digits is the larger number.
            loop {
                let a = byte(s1, i1);
                i1 += 1;
                if !a.is_ascii_digit() {
                    break;
                }
                let b = byte(s2, i2);
                i2 += 1;
                if !b.is_ascii_digit() {
                    return Ordering::Greater;
                }
            }
            if byte(s2, i2).is_ascii_digit() {
                Ordering::Less
            } else {
                diff.cmp(&0)
            }
        }
        d => (d as i32).cmp(&0),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Ordering of the names stock git 2.55 produced for
    /// `git tag --sort=version:refname` over this exact set, which is the
    /// regression this port has to keep reproducing.
    #[test]
    fn versioncmp_matches_git_version_refname_order() {
        let mut names = vec![
            "v1.0.1", "v1.10", "10", "v1", "a01b2", "v1.0-rc2", "1", "v1.0.0.0", "v1.1", "01",
            "v001", "v1a", "release-2", "v10", "v0.9", "v1.0a", "v1_2", "v1.0", "v01", "0",
            "release-10", "abc", "v2.0", "v1.0.0", "v9", "a1b2", "v1-2", "00", "x", "release-1",
            "refs", "v10.0", "v1.0-rc1", "v2", "v1.2", "y",
        ];
        let pre = Prereleases::none();
        names.sort_by(|a, b| versioncmp(a.as_bytes(), b.as_bytes(), &pre).then_with(|| a.cmp(b)));
        assert_eq!(
            names,
            vec![
                "00", "01", "0", "1", "10", "a01b2", "a1b2", "abc", "refs", "release-1",
                "release-2", "release-10", "v001", "v01", "v0.9", "v1", "v1-2", "v1.0", "v1.0-rc1",
                "v1.0-rc2", "v1.0.0", "v1.0.0.0", "v1.0.1", "v1.0a", "v1.1", "v1.2", "v1.10",
                "v1_2", "v1a", "v2", "v2.0", "v9", "v10", "v10.0", "x", "y",
            ]
        );
    }

    /// `--sort` marker parsing, shared by `branch` and `tag`.
    #[test]
    fn sort_key_markers_and_validation() {
        assert_eq!(parse_sort_key("-version:*refname"), (true, true, true, "refname"));
        assert_eq!(parse_sort_key("v:refname"), (false, true, false, "refname"));
        assert_eq!(parse_sort_key("committerdate"), (false, false, false, "committerdate"));
        assert_eq!(sort_error("refname:short"), None);
        assert_eq!(sort_error("-"), Some("malformed field name: ".to_string()));
        assert_eq!(sort_error("bogus"), Some("unknown field name: bogus".to_string()));
    }

    /// `lstrip`/`rstrip` component arithmetic, shared by `tag` and
    /// `for-each-ref`.
    #[test]
    fn strip_components_counts_from_both_ends() {
        let n = b"refs/tags/v1.0";
        assert_eq!(strip_components(n, 2, true), b"v1.0".to_vec());
        assert_eq!(strip_components(n, 9, true), b"".to_vec());
        assert_eq!(strip_components(n, 1, false), b"refs/tags".to_vec());
        assert_eq!(strip_components(n, -1, true), b"v1.0".to_vec());
        assert_eq!(strip_components(n, -9, false), n.to_vec());
    }
}

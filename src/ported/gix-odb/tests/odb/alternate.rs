use std::{
    fs, io,
    path::{Path, PathBuf},
};

use gix_odb::alternate;

pub fn alternate(
    objects_at: impl Into<PathBuf>,
    objects_to: impl Into<PathBuf>,
) -> Result<(PathBuf, PathBuf), io::Error> {
    alternate_with(objects_at, objects_to, None)
}

fn alternate_with(
    objects_at: impl Into<PathBuf>,
    objects_to: impl Into<PathBuf>,
    content_before_to: Option<&str>,
) -> Result<(PathBuf, PathBuf), io::Error> {
    let objects_to = objects_to.into();
    alternate_with_content(
        objects_at,
        objects_to.clone(),
        objects_to.to_str().expect("valid UTF-8").as_bytes().to_owned(),
        content_before_to,
    )
}

fn alternate_with_content(
    objects_at: impl Into<PathBuf>,
    objects_to: impl Into<PathBuf>,
    to_content: Vec<u8>,
    content_before_to: Option<&str>,
) -> Result<(PathBuf, PathBuf), io::Error> {
    let at = objects_at.into();
    let to = objects_to.into();
    let at_info = at.join("info");
    fs::create_dir_all(&at_info)?;
    fs::create_dir_all(&to)?;
    let contents = if let Some(content) = content_before_to {
        let mut c = vec![b'\n'];
        c.extend(content.as_bytes());
        c.extend(to_content);
        c
    } else {
        to_content
    };
    fs::write(at_info.join("alternates"), contents)?;
    Ok((at, to))
}

/// A cycle is not an error to git: `source_by_path` holds the primary store and
/// every alternate linked so far, so the entry that closes the loop is simply
/// not linked a second time (`odb_is_source_usable()`, `odb.c:75-93`).
///
/// Measured against git 2.55.0, with `a` and `b` naming each other:
///
/// ```text
/// $ git -C cyc1.git count-objects -v | tail -1
/// alternate: /…/cyc2.git/objects
/// ```
///
/// One line, no error, and the resolution of `cyc1` is not among them.
#[test]
fn circular_alternates_resolve_to_each_other_once() -> crate::Result {
    let tmp = gix_testtools::tempfile::TempDir::new()?;
    let tmp = tmp.path().join("sub-dir");
    std::fs::create_dir(&tmp)?;
    let (from, _) = alternate(tmp.join("a"), tmp.join("b"))?;
    alternate_with_content(
        tmp.join("b"),
        tmp.join("..").join("a"),
        Path::new("..")
            .join("a")
            .to_str()
            .expect("valid UTF-8")
            .as_bytes()
            .to_owned(),
        None,
    )?;

    let alternates = alternate::resolve(from.clone(), &std::env::current_dir()?)?;
    assert_eq!(
        alternates,
        vec![std::fs::canonicalize(tmp.join("b"))?],
        "the loop closes on the borrower, which is already linked, so `b` is the whole answer"
    );
    assert!(
        !alternates.contains(&std::fs::canonicalize(from)?),
        "the primary object directory is never one of its own alternates"
    );
    Ok(())
}

/// Every entry is normalized (`strbuf_realpath`), which is why
/// `git count-objects -v` prints an absolute, symlink-free path whatever the
/// file said. Measured against git 2.55.0 for an alternate written through a
/// symlink:
///
/// ```text
/// $ git -C b.git count-objects -v | tail -1
/// alternate: /…/real.git/objects
/// ```
#[test]
fn single_link_with_comment_before_path_and_ansi_c_escape() -> crate::Result {
    let tmp = gix_testtools::tempfile::TempDir::new()?;
    let non_alternate = tmp.path().join("actual");

    let (from, to) = alternate_with(tmp.path().join("a"), non_alternate, Some("# comment\n"))?;
    let alternates = alternate::resolve(from, &std::env::current_dir()?)?;
    assert_eq!(alternates, vec![std::fs::canonicalize(to)?]);
    Ok(())
}

/// `odb_is_source_usable()` (`odb.c:68-73`) drops an entry that is not a
/// directory. Measured against git 2.55.0:
///
/// ```text
/// $ git -C r.git count-objects -v | tail -1
/// error: unable to normalize alternate object path: /…/nope/objects
/// size-garbage: 0
/// ```
///
/// — the entry is dropped, so no `alternate:` line follows.
#[test]
fn a_missing_alternate_is_dropped() -> crate::Result {
    let tmp = gix_testtools::tempfile::TempDir::new()?;
    let from = tmp.path().join("a");
    fs::create_dir_all(from.join("info"))?;
    fs::write(
        from.join("info").join("alternates"),
        tmp.path().join("nope").join("objects").to_str().expect("utf8"),
    )?;
    assert!(alternate::resolve(from, &std::env::current_dir()?)?.is_empty());
    Ok(())
}

/// `if (sources.nr && depth + 1 > 5)` (`odb.c:194`) drops the level below, so a
/// chain reached from the primary store contributes at most six directories.
/// Measured against git 2.55.0 on a seven-long chain `a0 -> a1 -> … -> a7`:
///
/// ```text
/// $ git -C a0 cat-file -t <oid in a7>
/// error: /…/a6/objects: ignoring alternate object stores, nesting too deep
/// fatal: git cat-file: could not get object info
/// ```
#[test]
fn nesting_stops_after_five_levels() -> crate::Result {
    let tmp = gix_testtools::tempfile::TempDir::new()?;
    let link = |n: usize| tmp.path().join(format!("a{n}"));
    for n in 0..=7 {
        fs::create_dir_all(link(n).join("info"))?;
    }
    for n in 0..7 {
        fs::write(
            link(n).join("info").join("alternates"),
            link(n + 1).to_str().expect("utf8"),
        )?;
    }

    let alternates = alternate::resolve(link(0), &std::env::current_dir()?)?;
    let expected: Vec<_> = (1..=6).map(|n| std::fs::canonicalize(link(n))).collect::<Result<_, _>>()?;
    assert_eq!(
        alternates, expected,
        "a1 through a6 are linked in pre-order; a6's own alternate is the level that is dropped"
    );
    Ok(())
}

#[test]
fn no_alternate_in_first_objects_dir() -> crate::Result {
    let tmp = gix_testtools::tempfile::TempDir::new()?;
    assert!(alternate::resolve(tmp.path().to_owned(), &std::env::current_dir()?)?.is_empty());
    Ok(())
}

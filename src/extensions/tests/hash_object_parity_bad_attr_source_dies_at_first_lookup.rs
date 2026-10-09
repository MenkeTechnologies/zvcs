//! `hash-object` on a path converts it, and the first attribute lookup dies on an
//! `--attr-source` that names nothing (`compute_default_attr_source()`). zvcs hashed the
//! earlier files and printed their ids before it noticed.
#![cfg(unix)]

#[path = "support/stock_git.rs"]
mod stock_git;
#[path = "support/twin_repo.rs"]
mod twin_repo;

use twin_repo::Side;

fn world(label: &str) -> Option<(Side, Side)> {
    let stock = stock_git::stock_git()?;
    Some(twin_repo::pair(label, stock))
}

#[test]
fn a_bad_attr_source_dies_before_the_first_filtered_file_is_hashed() {
    let Some((s, z)) = world("hash-object-attr-source") else { return };
    for side in [&s, &z] {
        side.write("plain.txt", "x\n");
    }
    for args in [
        &["--attr-source=nope", "hash-object", "plain.txt"][..],
        &["--attr-source=nope", "hash-object", "plain.txt", "missing"],
        &["--attr-source=nope", "hash-object", "--literally", "plain.txt"],
        &["--attr-source=nope", "hash-object", "-w", "plain.txt"],
        &["--attr-source=nope", "hash-object", "--path=plain.txt", "plain.txt"],
        // No lookup happens for these.
        &["--attr-source=nope", "hash-object", "--no-filters", "plain.txt"],
        &["--attr-source=nope", "hash-object", "missing"],
        &["--attr-source=nope", "hash-object", "-t", "tree", "--literally", "--no-filters", "plain.txt"],
    ] {
        let want = s.git(args);
        assert_eq!(z.git(args), want, "{args:?}");
    }
    let want = s.git(&["--attr-source=nope", "hash-object", "plain.txt"]);
    assert_eq!(
        (want.code, want.stdout.as_str(), want.stderr.as_str()),
        (128, "", "fatal: bad --attr-source or GIT_ATTR_SOURCE\n")
    );
}

//! `handle_deletes()` (builtin/fast-export.c) runs after the refs and tags are written and before
//! the `done` trailer: every refspec whose source is empty is a deletion, printed as
//! `reset <dst>` from the null id, once, in command-line order, whatever else was exported. A
//! refspec with no colon at all has a NULL destination, which `printf("%s")` shows as `(null)`.
#![cfg(unix)]

#[path = "support/stock_git.rs"]
mod stock_git;
#[path = "support/twin_repo.rs"]
mod twin_repo;

#[test]
fn an_empty_source_refspec_prints_its_destination_as_a_deletion() {
    let Some(stock) = stock_git::stock_git() else { return };
    let (s, z) = twin_repo::pair("fast-export-refspec-deletes", stock);
    for side in [&s, &z] {
        let tagged = side.git(&["tag", "-a", "-m", "annotated", "v1", "main~1"]);
        assert_eq!(tagged.code, 0, "{tagged:?}");
    }
    for args in [
        &["fast-export", "--refspec=:gone", "main"][..],
        &["fast-export", "--refspec=", "main"],
        &["fast-export", "--refspec=:", "--refspec=:also", "--all"],
        &["fast-export", "--refspec=+:forced", "--refspec=refs/heads/main:refs/heads/trunk", "--all"],
        &["fast-export", "--refspec=:x", "--refspec=", "main", "side"],
        &["fast-export", "--refspec=:x", "--not", "main"],
        &["fast-export", "--refspec=:x", "--use-done-feature", "main"],
        &["fast-export", "--refspec=:x", "--anonymize", "main"],
        &["fast-export", "--max-count=3", "--not", "--refspec=", "--all", "--first-parent"],
    ] {
        let want = s.git(args);
        assert_eq!(want.code, 0, "{args:?}: {want:?}");
        assert_eq!(z.git(args), want, "{args:?}");
    }
    let want = s.git(&["fast-export", "--refspec=:gone", "--refspec=", "main"]);
    assert!(
        want.stdout.ends_with(&format!(
            "reset gone\nfrom {n}\n\nreset (null)\nfrom {n}\n\n",
            n = "0".repeat(40)
        )),
        "{want:?}"
    );
}

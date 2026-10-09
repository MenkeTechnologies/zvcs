//! `do_lookup_replace_object()` follows `refs/replace/` links at most `MAXREPLACEDEPTH` (5)
//! times and then `die("replace depth too high for object %s")`. An object replaced by itself,
//! or two objects replacing each other, never gets out of the loop, so every command that reads
//! the object dies with 128 - while commands that only resolve a name (`rev-parse`) and
//! `replace -d` still work, and a chain that ends is followed to its end.
#![cfg(unix)]

#[path = "support/stock_git.rs"]
mod stock_git;
#[path = "support/twin_repo.rs"]
mod twin_repo;

fn rev(side: &twin_repo::Side, name: &str) -> String {
    side.git(&["rev-parse", name]).stdout.trim().to_owned()
}

#[test]
fn a_self_replacement_kills_every_read_of_the_object() {
    let Some(stock) = stock_git::stock_git() else { return };
    let (s, z) = twin_repo::pair("replace-self", stock);
    for side in [&s, &z] {
        let made = side.git(&["replace", "--", "HEAD", "HEAD"]);
        assert_eq!(made.code, 0, "{made:?}");
    }
    let head = rev(&s, "HEAD");
    let dies = format!("fatal: replace depth too high for object {head}\n");
    for args in [
        &["reflog"][..],
        &["log", "--oneline"],
        &["cat-file", "-t", "HEAD"],
        &["show", "-s", "HEAD"],
        &["status", "-sb"],
    ] {
        let want = s.git(args);
        assert_eq!((want.code, want.stderr.as_str()), (128, dies.as_str()), "{args:?}");
        assert_eq!(z.git(args), want, "{args:?}");
    }
    for args in [&["rev-parse", "HEAD"][..], &["replace", "-l"], &["replace", "-d", "HEAD"], &["log", "--oneline"]] {
        let want = s.git(args);
        assert_eq!(want.code, 0, "{args:?}: {want:?}");
        assert_eq!(z.git(args), want, "{args:?}");
    }
}

#[test]
fn two_objects_replacing_each_other_loop_and_a_chain_is_followed() {
    let Some(stock) = stock_git::stock_git() else { return };
    let (s, z) = twin_repo::pair("replace-chain", stock);
    for side in [&s, &z] {
        // side -> main~1 -> main: a chain of two links, followed to its end.
        for (from, to) in [("side", "main~1"), ("main~1", "main")] {
            let (from, to) = (rev(side, from), rev(side, to));
            let made = side.git(&["update-ref", &format!("refs/replace/{from}"), &to]);
            assert_eq!(made.code, 0, "{made:?}");
        }
    }
    let side = rev(&s, "side");
    let want = s.git(&["cat-file", "-p", &side]);
    assert_eq!(want.code, 0, "{want:?}");
    assert!(want.stdout.contains("\nthree\n"), "followed to main: {want:?}");
    assert_eq!(z.git(&["cat-file", "-p", &side]), want);

    for side in [&s, &z] {
        // Close the loop: main -> side.
        let (from, to) = (rev(side, "main"), rev(side, "side"));
        let made = side.git(&["update-ref", &format!("refs/replace/{from}"), &to]);
        assert_eq!(made.code, 0, "{made:?}");
    }
    let want = s.git(&["cat-file", "-t", &side]);
    assert_eq!(want.code, 128, "{want:?}");
    assert!(want.stderr.starts_with("fatal: replace depth too high for object "), "{want:?}");
    assert_eq!(z.git(&["cat-file", "-t", &side]), want);
}

//! `refs migrate` stops collecting refs at the first broken one and still succeeds.
//!
//! repo_migrate_ref_storage_format() walks the old store with
//! `REFS_FOR_EACH_INCLUDE_BROKEN` (refs.c:3350-3352), so a loose ref that will not parse
//! reaches migrate_one_ref() with a null object id. ref_transaction_create() answers that
//! with a positive 1, not an error (refs.c:1508-1511); the callback's nonzero return ends
//! the iteration, and `if (ret < 0)` lets it through (refs.c:3442-3444). The refs
//! collected before it are committed, the rest are dropped, and the command exits 0.
//! zvcs failed with gitoxide's own "could not be instantiated" message at exit 1 and left
//! the files store in place.

#[path = "support/stock_git.rs"]
mod stock_git;
#[path = "support/twin_repo.rs"]
mod twin_repo;

use twin_repo::Side;

fn broken_ref(side: &Side) {
    assert_eq!(side.git(&["branch", "a-first", "main~1"]).code, 0);
    side.write(".git/refs/heads/broken", "garbage\n");
}

/// `git <probe>` run by stock git in `side`'s repository.
fn stock_reads(stock: &str, side: &Side, probe: &[&str]) -> (i32, String) {
    let out = std::process::Command::new(stock)
        .args(probe)
        .current_dir(side.repo())
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .output()
        .unwrap();
    (out.status.code().unwrap_or(-1), String::from_utf8_lossy(&out.stdout).into_owned())
}

#[test]
fn broken_ref_ends_the_ref_walk_without_failing() {
    let Some(stock) = stock_git::stock_git() else { return };
    for extra in [&[][..], &["--dry-run"]] {
        let (s, z) = twin_repo::pair("refs-migrate-broken-stop", stock);
        broken_ref(&s);
        broken_ref(&z);
        let mut args = vec!["refs", "migrate", "--ref-format=reftable"];
        args.extend_from_slice(extra);
        let (so, zo) = (s.git(&args), z.git(&args));
        assert_eq!(so.code, 0, "{args:?}: {so:?}");
        // The dry run names its mkdtemp() directory, whose six characters are random.
        let tame = |o: &twin_repo::Out| {
            let mut text = o.stdout.clone();
            if let Some(at) = text.find("ref_migration.") {
                text.replace_range(at + 14..at + 20, "XXXXXX");
            }
            (o.code, text, o.stderr.clone())
        };
        assert_eq!(tame(&so), tame(&zo), "{args:?}");
        // Post-state, read by stock git on both sides.
        for probe in [&["for-each-ref", "--format=%(refname) %(objectname) %(symref)"][..], &["symbolic-ref", "HEAD"]] {
            assert_eq!(stock_reads(stock, &s, probe), stock_reads(stock, &z, probe), "{args:?} then {probe:?}");
        }
    }
}

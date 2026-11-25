//! What `verify-commit` and `verify-tag` print when the signature checker cannot run
//! or the signing configuration is bad.
//!
//! * A checker named without a dir separator that the PATH lookup does not find fails
//!   start_command() in the parent: `error: cannot run gpg: <strerror>`
//!   (run-command.c:432-441, :757-763). zvcs printed the child-side
//!   `fatal: cannot exec 'gpg'` that only a path with a `/` produces.
//! * gpg_interface_lazy_init() rejects a bad `gpg.format` or `gpg.minTrustLevel` with
//!   `error()` inside the config reader, which then dies naming where the value came
//!   from (configset_iter(), config.c:1654-1673). zvcs printed only the first line.
//!   An unsigned commit never reaches the lazy init, so it stays a quiet exit 1.

#[path = "support/stock_git.rs"]
mod stock_git;
#[path = "support/twin_repo.rs"]
mod twin_repo;

use twin_repo::Side;

const SIG: &str = "-----BEGIN PGP SIGNATURE-----\n\niQEzBAABCAAdFiEE\n-----END PGP SIGNATURE-----\n";

/// `refs/heads/sig`, `main` with a `gpgsig` header, and `refs/tags/sig`, an annotated
/// tag with a signature appended; both fake, which is enough to reach the checker.
fn signed_objects(side: &Side) {
    let commit = side.git(&["cat-file", "commit", "main"]).stdout;
    let header: String = SIG.lines().map(|l| format!(" {l}\n")).collect();
    let commit = commit.replacen("\n\n", &format!("\ngpgsig{}\n\n", &header[..header.len() - 1]), 1);
    side.write("sig-commit", &commit);
    let oid = side.git(&["hash-object", "-t", "commit", "-w", "sig-commit"]).stdout;
    assert_eq!(side.git(&["update-ref", "refs/heads/sig", oid.trim()]).code, 0);

    side.write(
        "sig-tag",
        &format!(
            "object {}\ntype commit\ntag sig\ntagger C O Mitter <committer@example.com> 1700000000 +0000\n\nsigned\n{SIG}",
            side.git(&["rev-parse", "main"]).stdout.trim()
        ),
    );
    let oid = side.git(&["hash-object", "-t", "tag", "-w", "sig-tag"]).stdout;
    assert_eq!(side.git(&["update-ref", "refs/tags/sig", oid.trim()]).code, 0);
}

#[test]
fn checker_and_config_failures_match_stock() {
    let Some(stock) = stock_git::stock_git() else { return };
    let (s, z) = twin_repo::pair("verify-gpg-lazy-init", stock);
    signed_objects(&s);
    signed_objects(&z);
    // A PATH holding no gpg, so the default program is not found on any host.
    let empty = s.root.join("empty-path");
    std::fs::create_dir_all(&empty).unwrap();
    let path = [("PATH", empty.to_str().unwrap())];
    for args in [
        &["verify-commit", "refs/heads/sig"][..],
        &["verify-commit", "-v", "refs/heads/sig"],
        &["verify-tag", "refs/tags/sig"],
        &["verify-tag", "--raw", "refs/tags/sig", "main"],
        &["-c", "gpg.format=bogus", "verify-commit", "refs/heads/sig"],
        &["-c", "gpg.format=bogus", "verify-tag", "refs/tags/sig"],
        &["-c", "gpg.format=bogus", "verify-commit", "main"],
        &["-c", "gpg.minTrustLevel=bogus", "verify-commit", "refs/heads/sig"],
    ] {
        assert_eq!(s.git_env(&path, args), z.git_env(&path, args), "{args:?}");
    }
    // From a config file the die names the file and line.
    for side in [&s, &z] {
        let out = side.git(&["config", "gpg.format", "bogus"]);
        assert_eq!(out.code, 0, "{out:?}");
    }
    let args = ["verify-commit", "refs/heads/sig"];
    let so = s.git_env(&path, &args);
    assert!(so.stderr.contains("fatal: bad config variable 'gpg.format'"), "{so:?}");
    assert_eq!(so, z.git_env(&path, &args));
}

//! A null `<old-oid>` asks for a ref that does not exist yet, and `check_old_oid()`
//! refuses any ref that does — even one already holding `<new-oid>`
//! (refs/files-backend.c:2647-2651). gitoxide's `MustNotExist` lets that case
//! through, so `update-ref` makes the check itself, on the command line and for
//! `--stdin`'s `update`, as it already did for `create`.
#![cfg(unix)]

#[path = "support/stock_git.rs"]
mod stock_git;
#[path = "support/twin_repo.rs"]
mod twin_repo;

const ZERO: &str = "0000000000000000000000000000000000000000";

/// `git <args>` run by stock git in `side`'s repository: its stdout.
fn stock_read(stock: &str, side: &twin_repo::Side, args: &[&str]) -> String {
    let out = std::process::Command::new(stock)
        .args(args)
        .current_dir(side.repo())
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .output()
        .unwrap();
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// What the ref store holds afterwards, read by stock git: the refs and the
/// reflogs of `main` and `HEAD`.
fn state(stock: &str, side: &twin_repo::Side) -> [String; 3] {
    [
        stock_read(stock, side, &["for-each-ref"]),
        stock_read(stock, side, &["reflog", "show", "--format=%H %gs", "refs/heads/main"]),
        stock_read(stock, side, &["reflog", "show", "--format=%H %gs", "HEAD"]),
    ]
}

#[test]
fn a_null_old_value_refuses_a_ref_that_already_holds_the_new_one() {
    let Some(stock) = stock_git::stock_git() else { return };
    for (args, stdin) in [
        (&["update-ref", "refs/heads/main", "@new", ZERO][..], None),
        (&["update-ref", "-m", "msg", "refs/heads/main", "@new", ZERO], None),
        (&["update-ref", "HEAD", "@new", ZERO], None),
        (&["update-ref", "--no-deref", "HEAD", "@new", ZERO], None),
        (&["update-ref", "refs/heads/main", "@old", ZERO], None),
        (&["update-ref", "refs/heads/fresh", "@new", ZERO], None),
        (&["update-ref", "--stdin"], Some(format!("update refs/heads/main @new {ZERO}\n"))),
        (&["update-ref", "--stdin", "--batch-updates"], Some(format!("update refs/heads/side @side {ZERO}\nupdate refs/heads/fresh @new {ZERO}\n"))),
        (&["update-ref", "--stdin"], Some("create refs/heads/main @new\n".to_owned())),
    ] {
        let (s, z) = twin_repo::pair("update-ref-null-old", stock);
        let run = |side: &twin_repo::Side| {
            let rev = |r: &str| stock_read(stock, side, &["rev-parse", r]).trim().to_owned();
            let (new, old, side_tip) = (rev("main"), rev("main~1"), rev("side"));
            let fill = |a: &str| a.replace("@new", &new).replace("@old", &old).replace("@side", &side_tip);
            let args: Vec<String> = args.iter().map(|a| fill(a)).collect();
            let args: Vec<&str> = args.iter().map(String::as_str).collect();
            let out = match &stdin {
                None => side.git(&args),
                Some(text) => {
                    let input = side.root.join("input");
                    std::fs::write(&input, fill(text)).unwrap();
                    let mut cmd = std::process::Command::new(&side.bin);
                    cmd.args(&args)
                        .current_dir(side.repo())
                        .env("GIT_CONFIG_GLOBAL", "/dev/null")
                        .env("GIT_CONFIG_NOSYSTEM", "1")
                        .stdin(std::fs::File::open(&input).unwrap());
                    let out = cmd.output().unwrap();
                    twin_repo::Out {
                        code: out.status.code().unwrap_or(-1),
                        stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
                        stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
                    }
                }
            };
            (out, state(stock, side))
        };
        assert_eq!(run(&z), run(&s), "{args:?} {stdin:?}");
    }
}

//! `git var GIT_AUTHOR_IDENT` / `GIT_COMMITTER_IDENT` and the idents `git var -l`
//! lists, against stock git.
//!
//! Both are `fmt_ident()` (ident.c:456-542) over `git_author_info()` /
//! `git_committer_info()` (ident.c:565-589): `IDENT_STRICT` for a single
//! variable, no flag for `-l` (builtin/var.c:240, :181). zvcs read only
//! `GIT_<ROLE>_*` and `user.*`, refused anything else with a message of its own
//! at 1, stripped `.` as crud, and kept `<`/`>` inside a name:
//!
//! * `author.*` / `committer.*` come ahead of `user.*`, and an empty one is not
//!   a value;
//! * `user.useConfigOnly` refuses, under `IDENT_STRICT` only, an identity no
//!   `*.email` / `*.name` key gave — with the `Author identity unknown` advice;
//! * an email left unset is `EMAIL`, trimmed, and an empty name is refused when
//!   strict and replaced by the login name otherwise;
//! * `strbuf_addstr_without_crud()` (ident.c:229-266) keeps `.`, drops `<`, `>`
//!   and `\n` inside, and a strict name of nothing but crud is refused;
//! * an empty `GIT_<ROLE>_DATE` is the current time (ident.c:533-538).
//!
//! Every case sets `EMAIL` or an email, so none depends on whether this host's
//! name carries a domain, and the `<seconds> <tz>` of each ident is dropped
//! before comparing, since two processes need not agree on "now".

#[path = "support/stock_git.rs"]
mod stock_git;
#[path = "support/twin_repo.rs"]
mod twin_repo;

use stock_git::stock_git;
use twin_repo::{Out, Side};

const IDENT_ENV: [&str; 4] = ["GIT_AUTHOR_NAME", "GIT_AUTHOR_EMAIL", "GIT_COMMITTER_NAME", "GIT_COMMITTER_EMAIL"];

/// `git <args>` in `side`'s repository with `Side::run_in`'s environment, `set`
/// added and `unset` removed; only the ident lines of the output are kept, each
/// without its timestamp.
fn var(side: &Side, set: &[(&str, &str)], unset: &[&str], args: &[&str]) -> Out {
    let mut cmd = std::process::Command::new(&side.bin);
    cmd.args(args)
        .current_dir(side.repo())
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("HOME", &side.root)
        .env("GIT_CEILING_DIRECTORIES", side.root.parent().unwrap())
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_AUTHOR_NAME", "A U Thor")
        .env("GIT_AUTHOR_EMAIL", "author@example.com")
        .env("GIT_COMMITTER_NAME", "C O Mitter")
        .env("GIT_COMMITTER_EMAIL", "committer@example.com")
        .env("GIT_AUTHOR_DATE", "1700000000 +0000")
        .env("GIT_COMMITTER_DATE", "1700000000 +0000")
        .env("LC_ALL", "C");
    cmd.envs(set.iter().copied());
    for name in unset {
        cmd.env_remove(name);
    }
    let out = cmd.output().unwrap();
    let listing = args.contains(&"-l");
    let stdout = String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter(|line| !listing || line.contains("_IDENT="))
        .map(|line| line.rsplitn(3, ' ').last().unwrap_or(line).to_string())
        .collect::<Vec<_>>()
        .join("\n");
    Out {
        code: out.status.code().unwrap_or(-1),
        stdout,
        stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
    }
}

#[test]
fn var_formats_idents_as_fmt_ident_does() {
    let Some(stock) = stock_git() else { return };
    let (s, z) = twin_repo::pair("var-fmt-ident", stock);

    let none: &[&str] = &[];
    let em = ("EMAIL", "em@x");
    let cases: &[(&[(&str, &str)], &[&str], &[&str])] = &[
        (&[("GIT_AUTHOR_NAME", ".A."), ("GIT_AUTHOR_EMAIL", ".A.")], none, &["var", "GIT_AUTHOR_IDENT"]),
        (&[("GIT_AUTHOR_NAME", "a<b>c"), ("GIT_AUTHOR_EMAIL", "<x@y>")], none, &["var", "GIT_AUTHOR_IDENT"]),
        (&[("GIT_AUTHOR_NAME", ";;"), ("GIT_AUTHOR_EMAIL", ";;")], none, &["var", "GIT_AUTHOR_IDENT"]),
        (&[("GIT_AUTHOR_NAME", " "), ("GIT_AUTHOR_EMAIL", " ")], none, &["var", "GIT_AUTHOR_IDENT"]),
        (&[("GIT_AUTHOR_NAME", "A. B.")], none, &["var", "GIT_AUTHOR_IDENT"]),
        (&[("GIT_AUTHOR_NAME", "")], none, &["var", "GIT_AUTHOR_IDENT"]),
        (&[em], &IDENT_ENV, &["-c", "author.name=AN", "-c", "author.email=ae", "var", "GIT_AUTHOR_IDENT"]),
        (&[em], &IDENT_ENV, &["-c", "committer.name=CN", "-c", "user.name=UN", "var", "GIT_COMMITTER_IDENT"]),
        (&[em], &IDENT_ENV, &["-c", "author.email=", "-c", "user.email=ue", "-c", "user.name=U", "var", "GIT_AUTHOR_IDENT"]),
        (&[em], &IDENT_ENV, &["-c", "user.email=", "-c", "user.name=U", "var", "GIT_AUTHOR_IDENT"]),
        (&[("EMAIL", "  sp@x  ")], &IDENT_ENV, &["-c", "user.name=U", "var", "GIT_COMMITTER_IDENT"]),
        (&[em], &IDENT_ENV, &["-c", "user.name=", "var", "GIT_AUTHOR_IDENT"]),
        (&[em], &IDENT_ENV, &["-c", "user.useConfigOnly=true", "-c", "user.name=UN", "var", "GIT_AUTHOR_IDENT"]),
        (&[em], &IDENT_ENV, &["-c", "user.useConfigOnly=true", "-c", "user.email=ue", "var", "GIT_COMMITTER_IDENT"]),
        (&[em], &IDENT_ENV, &["-c", "user.useConfigOnly=true", "-c", "author.email=ae", "-c", "author.name=an", "var", "GIT_AUTHOR_IDENT"]),
        (&[em], &IDENT_ENV, &["-c", "user.useConfigOnly=true", "-c", "user.email=ue", "-c", "user.name=U", "var", "-l"]),
        (&[em, ("GIT_AUTHOR_NAME", "")], none, &["var", "-l"]),
        (&[("GIT_AUTHOR_DATE", "")], none, &["var", "GIT_AUTHOR_IDENT"]),
        (&[("GIT_COMMITTER_DATE", "")], none, &["var", "-l"]),
    ];
    for (set, unset, args) in cases {
        assert_eq!(var(&z, set, unset, args), var(&s, set, unset, args), "{set:?} -{unset:?} {args:?}");
    }
}

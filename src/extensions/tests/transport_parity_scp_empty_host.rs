//! A URL that starts with `:` is an ssh target with an empty host.
//!
//! `parse_connect_url()` takes the text before the first `:` as the host whatever it is, so
//! `git push :refs/heads/x` runs `ssh` with `""` for the host and lets it complain. zvcs refused the
//! URL itself (`SCP-like target ":refs/heads/x" can not be parsed as valid URL: Scheme requires
//! host`, exit 1). `ssh` is replaced by a shell function that prints its arguments, so what the
//! test compares is exactly what each side asked it to run.
//!
//! Stock git is the oracle (`support/stock_git.rs`).

#[path = "support/stock_git.rs"]
mod stock_git;
#[path = "support/twin.rs"]
mod twin;

use twin::Twin;

const FAKE_SSH: &str = "f() { printf '<%s>' \"$@\" >&2; echo >&2; exit 255; }; f";

fn same(t: &Twin, args: &[&str]) -> twin::Outcome {
    let (stock, zvcs) = t.run_env_in("work", args, &[("GIT_SSH_COMMAND", FAKE_SSH)]);
    assert_eq!(zvcs, stock, "git {args:?}: left is zvcs, right is stock");
    stock
}

#[test]
fn an_empty_host_reaches_ssh_as_an_empty_argument() {
    let Some(t) = Twin::new("scp-empty-host") else { return };
    let stock = same(&t, &["push", "--", ":refs/heads/div"]);
    assert_eq!(stock.code, 128, "{stock:?}");
    assert!(stock.stderr.starts_with("<"), "ssh was not run: {stock:?}");
    same(&t, &["fetch", ":refs/heads/div"]);
    same(&t, &["ls-remote", ":x"]);
}

#[test]
fn a_named_host_is_unchanged() {
    let Some(t) = Twin::new("scp-named-host") else { return };
    same(&t, &["push", "--", "host:refs/heads/div"]);
    same(&t, &["ls-remote", "user@host:path/to/repo"]);
}

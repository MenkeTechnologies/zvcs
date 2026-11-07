//! `clone -c <key>=<value>` is written into the new repository before
//! `transport_get()` runs (builtin/clone.c:1241, :1345), so `protocol.allow` and
//! `protocol.<type>.allow` given that way decide `transport_check_allowed()`
//! (transport.c:1169-1205) as repository-local configuration: under a `git -c`,
//! over the global files, and not consulted at all when `GIT_ALLOW_PROTOCOL` is
//! set. A refusal is `fatal: transport 'file' not allowed` at 128, after the
//! `Cloning into` banner and the local-clone warnings.
#![cfg(unix)]

#[path = "support/stock_git.rs"]
mod stock_git;
#[path = "support/twin_repo.rs"]
mod twin_repo;

#[test]
fn clone_config_pairs_decide_the_transport_policy() {
    let Some(stock) = stock_git::stock_git() else { return };
    let (s, z) = twin_repo::pair("clone-protocol-allow", stock);
    for (n, (env, args)) in [
        (&[][..], &["clone", "-c", "protocol.file.allow=never", "repo"][..]),
        (&[], &["clone", "-c", "protocol.allow=never", "repo"]),
        (&[], &["clone", "--config", "PROTOCOL.file.ALLOW=never", "--bare", "repo"]),
        (&[], &["clone", "-c", "protocol.file.allow=never", "--depth", "1", "repo"]),
        (&[], &["clone", "-c", "protocol.allow=never", "-c", "protocol.file.allow=always", "repo"]),
        (&[], &["clone", "-c", "protocol.file.allow=bogus", "repo"]),
        (&[], &["-c", "protocol.file.allow=always", "clone", "-c", "protocol.file.allow=never", "repo"]),
        (&[], &["-c", "protocol.file.allow=never", "clone", "-c", "protocol.file.allow=always", "repo"]),
        (&[("GIT_PROTOCOL_FROM_USER", "0")], &["clone", "-c", "protocol.file.allow=always", "repo"]),
        (&[("GIT_PROTOCOL_FROM_USER", "0")], &["clone", "-c", "protocol.allow=user", "repo"]),
        (&[("GIT_PROTOCOL_FROM_USER", "0")], &["clone", "--depth", "1", "repo"]),
        (&[("GIT_ALLOW_PROTOCOL", "file")], &["clone", "-c", "protocol.file.allow=never", "repo"]),
        (&[("GIT_ALLOW_PROTOCOL", "git")], &["clone", "-c", "protocol.file.allow=always", "repo"]),
    ]
    .into_iter()
    .enumerate()
    {
        let dest = format!("dest{n}");
        let args: Vec<&str> = args.iter().copied().chain([dest.as_str()]).collect();
        let run = |side: &twin_repo::Side| {
            let out = side.run_in(&side.root, env, &args);
            // What the clone left behind: nothing, or a repository whose config
            // holds the pairs and no trace of the policy decision.
            let config = std::process::Command::new(stock)
                .args(["config", "--local", "--list"])
                .current_dir(side.root.join(&dest))
                .env("GIT_CONFIG_GLOBAL", "/dev/null")
                .env("GIT_CONFIG_NOSYSTEM", "1")
                .output()
                .map(|o| String::from_utf8_lossy(&o.stdout).replace(&*side.root.to_string_lossy(), "<root>"))
                .unwrap_or_default();
            (out, side.root.join(&dest).exists(), config)
        };
        assert_eq!(run(&z), run(&s), "{env:?} {args:?}");
    }
}

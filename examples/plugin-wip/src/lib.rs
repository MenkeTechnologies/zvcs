//! `git wip` — stage every change and commit it, as one command.
//!
//! The point of the example is what a plugin does *not* have to do: there is no
//! `Command::new("git")` here. `host.run(verb, args)` re-enters the same
//! dispatch table the command line uses, in this process, and hands back the
//! verb's exit status — so a plugin composes the porcelain the way a shell
//! alias does, without paying for a fork or depending on a `git` being on PATH.
//!
//! ```sh
//! git znative add path:examples/plugin-wip
//! git wip                      # commits everything as "wip on <branch>"
//! git wip fixing the parser    # …or with a message of your own
//! ```

use std::os::raw::c_int;

use znative::{declare_plugin, Args, Host};

fn wip(host: &Host, args: &Args) -> c_int {
    // Stage first, then ask whether anything landed. The other order is the
    // tempting one and it is wrong: `diff --quiet HEAD` compares *tracked*
    // content, so a work tree whose only change is a new file reads as clean
    // and `git wip` would refuse the very case it exists for.
    if host.run("add", &["-A"]) != 0 {
        return 1;
    }

    // `diff --quiet --cached HEAD` exits 0 when the index matches HEAD, i.e.
    // nothing was staged. Reading a verb's *status* rather than its output is
    // what `run` is for. An unborn HEAD is not a diff target, and a first
    // commit always has something to say, so the check is skipped there.
    if host.repo_info("head").is_some() && host.run("diff", &["--quiet", "--cached", "HEAD"]) == 0 {
        host.eprint("wip: nothing to commit\n");
        return 1;
    }

    let message = if args.rest().is_empty() {
        match host.repo_info("branch") {
            Some(branch) => format!("wip on {branch}"),
            None => "wip".to_string(),
        }
    } else {
        args.rest().join(" ")
    };

    // `wip.sign` is read through the host, so it resolves the way any git
    // config key does — system, XDG, user, repo, work tree, and `-c` overrides.
    let mut commit: Vec<&str> = vec!["-q", "-m", &message];
    if host.config_get("wip.sign").is_some_and(|v| v == "true") {
        commit.push("-S");
    }
    host.run("commit", &commit)
}

declare_plugin! {
    name: "wip",
    version: "0.1.0",
    verbs: { "wip" => wip },
}

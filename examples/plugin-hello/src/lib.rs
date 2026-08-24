//! The example native zvcs plugin, exercising every part of the `znative` ABI
//! a real plugin uses: an added subcommand, a verb override that delegates to
//! the original, repository reads, config reads, and object read/write.
//!
//! ```sh
//! git znative add path:examples/plugin-hello
//! git hello                 # the added verb
//! git hello --write         # writes a blob through the host
//! git version               # the overridden verb: a prefix line, then the original
//! ```

use std::os::raw::c_int;

use znative::{declare_plugin, Args, Host};

/// `git hello [--write]` — print where we are, and optionally round-trip a blob
/// through the host's object database.
fn hello(host: &Host, args: &Args) -> c_int {
    let workdir = host.repo_info("workdir").unwrap_or_else(|| "(no work tree)".into());
    let head = host.repo_info("head").unwrap_or_else(|| "(unborn)".into());
    let branch = host.repo_info("branch").unwrap_or_else(|| "(detached)".into());
    let user = host.config_get("user.name").unwrap_or_else(|| "(unset)".into());
    host.print(&format!("hello from a native plugin\n"));
    host.print(&format!("  workdir  {workdir}\n"));
    host.print(&format!("  branch   {branch}\n"));
    host.print(&format!("  head     {head}\n"));
    host.print(&format!("  user     {user}\n"));

    if args.rest().iter().any(|a| a == "--write") {
        let Some(id) = host.object_write("blob", b"hello from a plugin\n") else {
            host.eprint("hello: could not write the blob\n");
            return 1;
        };
        let Some((kind, bytes)) = host.object_read(&id) else {
            host.eprint("hello: could not read the blob back\n");
            return 1;
        };
        host.print(&format!(
            "  wrote    {id} ({} bytes, kind {kind})\n",
            bytes.len()
        ));
    }
    0
}

/// An override of the built-in `version`: print one line of our own, then run
/// the original through the host so nothing about it is reimplemented here.
fn version(host: &Host, args: &Args) -> c_int {
    host.print("plugin `hello` is loaded\n");
    host.dispatch_verb("version", &args.rest_str())
}

declare_plugin! {
    name: "hello",
    version: "0.1.0",
    verbs:     { "hello" => hello },
    overrides: { "version" => version },
}

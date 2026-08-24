//! `git znative` — the plugin package manager verb. Ported from the zshrs
//! `znative` builtin (`zshrs/src/extensions/pkg/builtin.rs`), which is an argv
//! dispatcher over [`crate::pkg::commands`]. Errors print as
//! `znative: <reason>` on stderr and the verb exits 1.

use std::process::ExitCode;

use anyhow::Result;

use crate::pkg::commands;

const USAGE: &str = "\
usage: git znative <command> [args]

  load [SOURCE...]   install a source that is not in the store yet, then verify
                     it loads and refresh the verb tables dispatch reads. No
                     args re-verifies everything installed. Zero network once
                     stored. SOURCE: owner/repo, github:o/r, git+URL, path:DIR
  add <SOURCE>       install a plugin (load self-installs, so add is mainly for
                     installing without a bootstrap line)
  remove <NAME>      delete an installed plugin
  list               list installed plugins
  info <NAME>        show details for one plugin
  update [NAME]      re-resolve + reinstall from the recorded source
  gc [--dry-run]     remove orphan store entries + the clone cache
  clean              clear scratch caches (git/, cache/, bin/)
  help               this message

aliases: add=install=i  remove=rm=uninstall  list=ls  info=show
         load=source  update=up=upgrade";

/// Entry point for `git znative`. `args` are the arguments after the verb, so
/// `args[0]` is the subcommand.
pub fn znative(args: &[String]) -> Result<ExitCode> {
    let sub = args.first().map(String::as_str).unwrap_or("");
    let rest = &args[args.len().min(1)..];

    let result = match sub {
        "add" | "install" | "i" => match rest.first() {
            // `add a b c` installs several; the last failure is reported.
            Some(_) => rest.iter().fold(Ok(()), |acc, spec| commands::add(spec).and(acc)),
            None => return Ok(usage_err("add requires a SOURCE")),
        },
        "remove" | "rm" | "uninstall" => match rest.first() {
            Some(_) => rest.iter().fold(Ok(()), |acc, name| commands::remove(name).and(acc)),
            None => return Ok(usage_err("remove requires a NAME")),
        },
        "list" | "ls" => commands::list(),
        "info" | "show" => match rest.first() {
            Some(name) => commands::info(name),
            None => return Ok(usage_err("info requires a NAME")),
        },
        "load" | "source" => {
            if rest.is_empty() {
                commands::load(None)
            } else {
                rest.iter().fold(Ok(()), |acc, spec| commands::load(Some(spec)).and(acc))
            }
        }
        "update" | "upgrade" | "up" => commands::update(rest.first().map(String::as_str)),
        "gc" => commands::gc(rest.iter().any(|a| a == "--dry-run" || a == "-n")),
        "clean" => commands::clean(),
        "help" | "-h" | "--help" | "" => {
            println!("{USAGE}");
            return Ok(ExitCode::SUCCESS);
        }
        other => return Ok(usage_err(&format!("unknown command '{other}'"))),
    };

    match result {
        Ok(()) => Ok(ExitCode::SUCCESS),
        Err(e) => {
            eprintln!("znative: {e}");
            Ok(ExitCode::FAILURE)
        }
    }
}

/// Print a usage error to stderr and answer 1.
fn usage_err(msg: &str) -> ExitCode {
    eprintln!("znative: {msg}");
    eprintln!("{USAGE}");
    ExitCode::FAILURE
}

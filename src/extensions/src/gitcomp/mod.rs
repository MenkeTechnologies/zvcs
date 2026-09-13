//! `git <builtin> --git-completion-helper[-all]`: the option list
//! `git-completion.bash` asks every parse-options builtin for.
//!
//! In git this costs a builtin nothing. `parse_options_step()` recognises the
//! two arguments before any option is parsed (parse-options.c:1052-1059):
//!
//! ```c
//! /*
//!  * lone --git-completion-helper and --git-completion-helper-all
//!  * are asked by git-completion.bash
//!  */
//! if (ctx->total == 1 && !strcmp(arg, "--git-completion-helper"))
//!         return show_gitcomp(options, 0);
//! if (ctx->total == 1 && !strcmp(arg, "--git-completion-helper-all"))
//!         return show_gitcomp(options, 1);
//! ```
//!
//! and `parse_options()` turns the `PARSE_OPT_COMPLETE` that comes back into
//! `exit(0)` (parse-options.c:1202-1203). What is printed is therefore a pure
//! function of the builtin's `struct option` array, which is what this module
//! holds: one table per builtin, ported entry by entry from that array, and one
//! renderer, [`show_gitcomp`].
//!
//! # Why not the resolver tables
//!
//! Most verbs already carry a `porcelain::LongOpt` table, and `git zrepl`
//! renders completion from it at build time. It cannot produce this output:
//! it records only what `parse_long_opt()` reads — name, `PARSE_OPT_NONEG`, value
//! sense — while `show_gitcomp()` also reads `PARSE_OPT_HIDDEN`,
//! `PARSE_OPT_NOCOMPLETE`, `PARSE_OPT_COMP_ARG`, the option *type* (which decides
//! both the `=` suffix and whether a `--no-` form exists), `OPTION_SUBCOMMAND`
//! entries, `OPTION_ALIAS` entries, and the position of every entry including
//! the ones without a long name (the first printed item carries no leading
//! space only when it is the array's first entry). None of that is in `LongOpt`,
//! and its order follows each verb's parser rather than the C array.
//!
//! # Porting a table
//!
//! Entries are written with functions named after the parse-options.h macros
//! they expand, keeping only the arguments `show_gitcomp()` reads: the long name
//! (`NULL` where the C passes `NULL`) and, for the `_F` forms, the flags. A
//! macro that expands to two entries (`OPT__VERBOSITY`, `OPT_IPVERSION`) is
//! written as its two entries. A designated initialiser is written with
//! [`option`]. Entries without a long name stay in the table: they are skipped
//! when printing, but they still decide whether the first printed name is
//! preceded by a space.
#![allow(non_snake_case)]

use std::io::Write;
use std::process::ExitCode;

mod tables;

/// `enum parse_opt_type` (parse-options.h:12-32), less `OPTION_END`, which a
/// Rust slice does not need.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Type {
    Group,
    Number,
    Alias,
    Subcommand,
    Bit,
    NegBit,
    BitOp,
    CountUp,
    SetInt,
    String,
    Integer,
    Unsigned,
    Callback,
    LowlevelCallback,
    Filename,
}

// `enum parse_opt_option_flags` (parse-options.h:45-57).
pub(crate) const PARSE_OPT_OPTARG: u16 = 1 << 0;
pub(crate) const PARSE_OPT_NOARG: u16 = 1 << 1;
pub(crate) const PARSE_OPT_NONEG: u16 = 1 << 2;
pub(crate) const PARSE_OPT_HIDDEN: u16 = 1 << 3;
pub(crate) const PARSE_OPT_LASTARG_DEFAULT: u16 = 1 << 4;
pub(crate) const PARSE_OPT_NODASH: u16 = 1 << 5;
pub(crate) const PARSE_OPT_LITERAL_ARGHELP: u16 = 1 << 6;
pub(crate) const PARSE_OPT_FROM_ALIAS: u16 = 1 << 7;
pub(crate) const PARSE_OPT_NOCOMPLETE: u16 = 1 << 9;
pub(crate) const PARSE_OPT_COMP_ARG: u16 = 1 << 10;
pub(crate) const PARSE_OPT_CMDMODE: u16 = 1 << 11;

/// A `NULL` long name. No option can be spelled `--`, so the empty string
/// cannot collide with a real one.
pub(crate) const NULL: &str = "";

/// The fields of one `struct option` that `show_gitcomp()` and
/// `preprocess_options()` read.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Opt {
    ty: Type,
    long: &'static str,
    flags: u16,
    /// `OPTION_ALIAS`'s `.value`: the long name of the option it aliases.
    source: &'static str,
}

/// A designated initialiser, `{ .type = …, .long_name = …, .flags = … }`.
pub(crate) const fn option(ty: Type, long: &'static str, flags: u16) -> Opt {
    Opt { ty, long, flags, source: NULL }
}

// ---- parse-options.h:171-397 ------------------------------------------------

pub(crate) const fn OPT_BIT_F(l: &'static str, f: u16) -> Opt {
    option(Type::Bit, l, PARSE_OPT_NOARG | f)
}
pub(crate) const fn OPT_COUNTUP_F(l: &'static str, f: u16) -> Opt {
    option(Type::CountUp, l, PARSE_OPT_NOARG | f)
}
pub(crate) const fn OPT_SET_INT_F(l: &'static str, f: u16) -> Opt {
    option(Type::SetInt, l, PARSE_OPT_NOARG | f)
}
pub(crate) const fn OPT_BOOL_F(l: &'static str, f: u16) -> Opt {
    OPT_SET_INT_F(l, f)
}
pub(crate) const fn OPT_CALLBACK_F(l: &'static str, f: u16) -> Opt {
    option(Type::Callback, l, f)
}
pub(crate) const fn OPT_STRING_F(l: &'static str, f: u16) -> Opt {
    option(Type::String, l, f)
}
pub(crate) const fn OPT_INTEGER_F(l: &'static str, f: u16) -> Opt {
    option(Type::Integer, l, f)
}
pub(crate) const fn OPT_GROUP() -> Opt {
    option(Type::Group, NULL, 0)
}
pub(crate) const fn OPT_BIT(l: &'static str) -> Opt {
    OPT_BIT_F(l, 0)
}
pub(crate) const fn OPT_BITOP(l: &'static str) -> Opt {
    option(Type::BitOp, l, PARSE_OPT_NOARG | PARSE_OPT_NONEG)
}
pub(crate) const fn OPT_NEGBIT(l: &'static str) -> Opt {
    option(Type::NegBit, l, PARSE_OPT_NOARG)
}
pub(crate) const fn OPT_COUNTUP(l: &'static str) -> Opt {
    OPT_COUNTUP_F(l, 0)
}
pub(crate) const fn OPT_SET_INT(l: &'static str) -> Opt {
    OPT_SET_INT_F(l, 0)
}
pub(crate) const fn OPT_BOOL(l: &'static str) -> Opt {
    OPT_BOOL_F(l, 0)
}
pub(crate) const fn OPT_HIDDEN_BOOL(l: &'static str) -> Opt {
    option(Type::SetInt, l, PARSE_OPT_NOARG | PARSE_OPT_HIDDEN)
}
pub(crate) const fn OPT_CMDMODE_F(l: &'static str, f: u16) -> Opt {
    option(Type::SetInt, l, PARSE_OPT_CMDMODE | PARSE_OPT_NOARG | PARSE_OPT_NONEG | f)
}
pub(crate) const fn OPT_CMDMODE(l: &'static str) -> Opt {
    OPT_CMDMODE_F(l, 0)
}
pub(crate) const fn OPT_INTEGER(l: &'static str) -> Opt {
    OPT_INTEGER_F(l, 0)
}
pub(crate) const fn OPT_UNSIGNED(l: &'static str) -> Opt {
    option(Type::Unsigned, l, PARSE_OPT_NONEG)
}
pub(crate) const fn OPT_STRING(l: &'static str) -> Opt {
    OPT_STRING_F(l, 0)
}
pub(crate) const fn OPT_STRING_LIST(l: &'static str) -> Opt {
    option(Type::Callback, l, 0)
}
pub(crate) const fn OPT_STRVEC(l: &'static str) -> Opt {
    option(Type::Callback, l, 0)
}
pub(crate) const fn OPT_UYN(l: &'static str) -> Opt {
    option(Type::Callback, l, PARSE_OPT_NOARG)
}
pub(crate) const fn OPT_EXPIRY_DATE(l: &'static str) -> Opt {
    option(Type::Callback, l, 0)
}
pub(crate) const fn OPT_CALLBACK(l: &'static str) -> Opt {
    OPT_CALLBACK_F(l, 0)
}
pub(crate) const fn OPT_NUMBER_CALLBACK() -> Opt {
    option(Type::Number, NULL, PARSE_OPT_NOARG | PARSE_OPT_NONEG)
}
pub(crate) const fn OPT_FILENAME(l: &'static str) -> Opt {
    option(Type::Filename, l, 0)
}
pub(crate) const fn OPT_COLOR_FLAG(l: &'static str) -> Opt {
    option(Type::Callback, l, PARSE_OPT_OPTARG)
}
pub(crate) const fn OPT_NOOP_NOARG(l: &'static str) -> Opt {
    option(Type::Callback, l, PARSE_OPT_HIDDEN | PARSE_OPT_NOARG)
}
pub(crate) const fn OPT_NOOP_ARG(l: &'static str) -> Opt {
    option(Type::Callback, l, PARSE_OPT_HIDDEN)
}
pub(crate) const fn OPT_ALIAS(l: &'static str, source: &'static str) -> Opt {
    Opt { ty: Type::Alias, long: l, flags: 0, source }
}
pub(crate) const fn OPT_SUBCOMMAND_F(l: &'static str, f: u16) -> Opt {
    option(Type::Subcommand, l, f)
}
pub(crate) const fn OPT_SUBCOMMAND(l: &'static str) -> Opt {
    OPT_SUBCOMMAND_F(l, 0)
}

// ---- parse-options.h:543-634 ------------------------------------------------

pub(crate) const fn OPT__VERBOSE() -> Opt {
    OPT_COUNTUP("verbose")
}
pub(crate) const fn OPT__QUIET() -> Opt {
    OPT_COUNTUP("quiet")
}
pub(crate) const fn OPT__DRY_RUN() -> Opt {
    OPT_BOOL("dry-run")
}
pub(crate) const fn OPT__FORCE(f: u16) -> Opt {
    OPT_COUNTUP_F("force", f)
}
pub(crate) const fn OPT__ABBREV() -> Opt {
    option(Type::Callback, "abbrev", PARSE_OPT_OPTARG)
}
pub(crate) const fn OPT__SUPER_PREFIX() -> Opt {
    OPT_STRING_F("super-prefix", PARSE_OPT_HIDDEN)
}
pub(crate) const fn OPT__COLOR() -> Opt {
    OPT_COLOR_FLAG("color")
}
pub(crate) const fn OPT_COLUMN(l: &'static str) -> Opt {
    option(Type::Callback, l, PARSE_OPT_OPTARG)
}
pub(crate) const fn OPT_PASSTHRU(l: &'static str, f: u16) -> Opt {
    option(Type::Callback, l, f)
}
pub(crate) const fn OPT_PASSTHRU_ARGV(l: &'static str, f: u16) -> Opt {
    option(Type::Callback, l, f)
}
const fn _OPT_CONTAINS_OR_WITH(l: &'static str, f: u16) -> Opt {
    option(Type::Callback, l, PARSE_OPT_LASTARG_DEFAULT | f)
}
pub(crate) const fn OPT_CONTAINS() -> Opt {
    _OPT_CONTAINS_OR_WITH("contains", PARSE_OPT_NONEG)
}
pub(crate) const fn OPT_NO_CONTAINS() -> Opt {
    _OPT_CONTAINS_OR_WITH("no-contains", PARSE_OPT_NONEG)
}
pub(crate) const fn OPT_WITH() -> Opt {
    _OPT_CONTAINS_OR_WITH("with", PARSE_OPT_HIDDEN | PARSE_OPT_NONEG)
}
pub(crate) const fn OPT_WITHOUT() -> Opt {
    _OPT_CONTAINS_OR_WITH("without", PARSE_OPT_HIDDEN | PARSE_OPT_NONEG)
}
pub(crate) const fn OPT_CLEANUP() -> Opt {
    OPT_STRING("cleanup")
}
pub(crate) const fn OPT_PATHSPEC_FROM_FILE() -> Opt {
    OPT_FILENAME("pathspec-from-file")
}
pub(crate) const fn OPT_PATHSPEC_FILE_NUL() -> Opt {
    OPT_BOOL("pathspec-file-nul")
}
pub(crate) const fn OPT_AUTOSTASH() -> Opt {
    OPT_BOOL("autostash")
}
pub(crate) const fn OPT_DIFF_UNIFIED() -> Opt {
    OPT_INTEGER_F("unified", PARSE_OPT_NONEG)
}
pub(crate) const fn OPT_DIFF_INTERHUNK_CONTEXT() -> Opt {
    OPT_INTEGER_F("inter-hunk-context", PARSE_OPT_NONEG)
}

// ---- ref-filter.h:119-137, rerere.h:42, list-objects-filter-options.h:126 --

const fn _OPT_MERGED_NO_MERGED(l: &'static str) -> Opt {
    option(Type::Callback, l, PARSE_OPT_LASTARG_DEFAULT | PARSE_OPT_NONEG)
}
pub(crate) const fn OPT_MERGED() -> Opt {
    _OPT_MERGED_NO_MERGED("merged")
}
pub(crate) const fn OPT_NO_MERGED() -> Opt {
    _OPT_MERGED_NO_MERGED("no-merged")
}
pub(crate) const fn OPT_REF_SORT() -> Opt {
    OPT_STRING_LIST("sort")
}
pub(crate) const fn OPT_REF_FILTER_EXCLUDE() -> Opt {
    OPT_STRVEC("exclude")
}
pub(crate) const fn OPT_RERERE_AUTOUPDATE() -> Opt {
    OPT_UYN("rerere-autoupdate")
}
pub(crate) const fn OPT_PARSE_LIST_OBJECTS_FILTER() -> Opt {
    OPT_CALLBACK("filter")
}

/// `preprocess_options()` (parse-options.c:903-971), the half that matters
/// here: every `OPTION_ALIAS` entry becomes a copy of the option it names, with
/// the alias's long name and `PARSE_OPT_FROM_ALIAS` added, in the alias's own
/// position.
fn preprocess_options(options: &[Opt]) -> Vec<Opt> {
    options
        .iter()
        .map(|o| {
            if o.ty != Type::Alias {
                return *o;
            }
            let source = options
                .iter()
                .find(|s| s.long == o.source && s.ty != Type::Alias)
                .unwrap_or_else(|| {
                    panic!("could not find source option '{}' of alias '{}'", o.source, o.long)
                });
            Opt { long: o.long, flags: source.flags | PARSE_OPT_FROM_ALIAS, ..*source }
        })
        .collect()
}

/// `show_negated_gitcomp()` (parse-options.c:795-842).
fn show_negated_gitcomp(out: &mut String, opts: &[Opt], show_all: bool, mut nr_noopts: i32) {
    let mut printed_dashdash = false;
    for o in opts {
        if o.long.is_empty() {
            continue;
        }
        if !show_all && o.flags & (PARSE_OPT_HIDDEN | PARSE_OPT_NOCOMPLETE) != 0 {
            continue;
        }
        if o.flags & PARSE_OPT_NONEG != 0 {
            continue;
        }
        let has_unset_form = matches!(
            o.ty,
            Type::String
                | Type::Filename
                | Type::Integer
                | Type::Unsigned
                | Type::Callback
                | Type::Bit
                | Type::NegBit
                | Type::CountUp
                | Type::SetInt
        );
        if !has_unset_form {
            continue;
        }
        if let Some(name) = o.long.strip_prefix("no-") {
            if nr_noopts < 0 {
                out.push_str(" --");
                out.push_str(name);
            }
        } else if nr_noopts >= 0 {
            if nr_noopts != 0 && !printed_dashdash {
                out.push_str(" --");
                printed_dashdash = true;
            }
            out.push_str(" --no-");
            out.push_str(o.long);
            nr_noopts += 1;
        }
    }
}

/// `show_gitcomp()` (parse-options.c:844-892), returning the line it prints,
/// newline included.
pub(crate) fn show_gitcomp(opts: &[Opt], show_all: bool) -> String {
    let mut out = String::new();
    let mut nr_noopts = 0;
    for (i, o) in opts.iter().enumerate() {
        if o.long.is_empty() {
            continue;
        }
        if !show_all
            && o.flags & (PARSE_OPT_HIDDEN | PARSE_OPT_NOCOMPLETE | PARSE_OPT_FROM_ALIAS) != 0
        {
            continue;
        }
        let mut prefix = "--";
        let mut suffix = "";
        match o.ty {
            Type::Subcommand => prefix = "",
            Type::Group => continue,
            Type::String | Type::Filename | Type::Integer | Type::Unsigned | Type::Callback => {
                if o.flags & (PARSE_OPT_NOARG | PARSE_OPT_OPTARG | PARSE_OPT_LASTARG_DEFAULT) == 0 {
                    suffix = "=";
                }
            }
            _ => {}
        }
        if o.flags & PARSE_OPT_COMP_ARG != 0 {
            suffix = "=";
        }
        if o.long.starts_with("no-") {
            nr_noopts += 1;
        }
        if i != 0 {
            out.push(' ');
        }
        out.push_str(prefix);
        out.push_str(o.long);
        out.push_str(suffix);
    }
    show_negated_gitcomp(&mut out, opts, show_all, -1);
    show_negated_gitcomp(&mut out, opts, show_all, nr_noopts);
    out.push('\n');
    out
}

/// One builtin whose `commands[]` entry (git.c:529-685) lacks `NO_PARSEOPT`.
pub(crate) struct Builtin {
    pub(crate) name: &'static str,
    /// `RUN_SETUP`: `run_builtin()` dies without a repository before the
    /// builtin — and so its `parse_options()` — is ever reached (git.c:472-480).
    pub(crate) run_setup: bool,
    /// The array handed to the `parse_options()` call that sees the lone
    /// argument, as the segments `parse_options_concat()` joined into it.
    pub(crate) options: &'static [&'static [Opt]],
}

impl Builtin {
    fn render(&self, show_all: bool) -> String {
        let joined: Vec<Opt> = self.options.iter().flat_map(|s| s.iter().copied()).collect();
        show_gitcomp(&preprocess_options(&joined), show_all)
    }
}

/// The builtins that answer the helper, in `commands[]` order.
pub(crate) fn builtins() -> &'static [Builtin] {
    tables::BUILTINS
}

/// Answer `git <sub> --git-completion-helper[-all]`, or `None` when `args` is
/// not exactly that lone argument or `sub` has no ported table.
///
/// Called where `run_builtin()` would call the builtin: after the dispatcher's
/// own gates, before the command runs. A `RUN_SETUP` builtin that finds no
/// repository reports the discovery failure the way every such command does.
pub(crate) fn answer(sub: &str, args: &[String]) -> Option<anyhow::Result<ExitCode>> {
    let [arg] = args else { return None };
    let show_all = match arg.as_str() {
        "--git-completion-helper" => false,
        "--git-completion-helper-all" => true,
        _ => return None,
    };
    let builtin = tables::BUILTINS.iter().find(|b| b.name == sub)?;
    Some((|| {
        if builtin.run_setup {
            crate::setup::discover()?;
        }
        std::io::stdout().lock().write_all(builtin.render(show_all).as_bytes())?;
        Ok(ExitCode::SUCCESS)
    })())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn render(opts: &[Opt], all: bool) -> String {
        show_gitcomp(&preprocess_options(opts), all)
    }

    /// With no `no-`-named entry of its own, the counter starts at zero, so the
    /// first `--no-` prints bare and the `--` separator lands after it.
    #[test]
    fn dashdash_follows_the_first_negation() {
        let t = [OPT_BOOL("a"), OPT_BOOL("b")];
        assert_eq!(render(&t, false), "--a --b --no-a -- --no-b\n");
        // A single negatable option prints no separator at all.
        assert_eq!(render(&[OPT_BOOL("a")], false), "--a --no-a\n");
    }

    /// A `no-`-named entry prints its stem in the first negated pass and makes
    /// the separator precede every `--no-` of the second.
    #[test]
    fn no_named_entries_print_their_stem_first() {
        let t = [OPT_BOOL("no-x"), OPT_BOOL("y")];
        assert_eq!(render(&t, false), "--no-x --y --x -- --no-y\n");
    }

    /// The leading space is decided by array position, not by what printed: a
    /// skipped first entry leaves every printed name space-prefixed.
    #[test]
    fn skipped_first_entry_leaves_a_leading_space() {
        let t = [OPT_GROUP(), OPT_STRING("s")];
        assert_eq!(render(&t, false), " --s= --no-s\n");
        let t = [OPT_HIDDEN_BOOL("h"), OPT_BOOL("v")];
        assert_eq!(render(&t, false), " --v --no-v\n");
        assert_eq!(render(&t, true), "--h --v --no-h -- --no-v\n");
    }

    /// `=` only for value-taking types whose value is mandatory, or forced by
    /// `PARSE_OPT_COMP_ARG`; no `--no-` for `NONEG` or for types without an
    /// unset form.
    #[test]
    fn suffix_and_negation_follow_type_and_flags() {
        let t = [
            OPT_STRING("req"),
            OPT__ABBREV(),
            OPT_CONTAINS(),
            OPT_CALLBACK_F("noarg", PARSE_OPT_NOARG),
            OPT_BOOL_F("forced", PARSE_OPT_COMP_ARG),
            OPT_BITOP("bitop"),
            OPT_CMDMODE("mode"),
            OPT_SUBCOMMAND("sub"),
        ];
        assert_eq!(
            render(&t, false),
            "--req= --abbrev --contains --noarg --forced= --bitop --mode sub \
             --no-req -- --no-abbrev --no-noarg --no-forced\n"
        );
    }

    /// An alias is hidden from the plain list but, since the negated passes do
    /// not test `PARSE_OPT_FROM_ALIAS`, still contributes its `--no-` form; with
    /// `-all` it prints with its source's suffix.
    #[test]
    fn aliases_take_their_source_and_keep_their_negation() {
        let t = [OPT_STRING("upload-pack"), OPT_ALIAS("exec", "upload-pack")];
        assert_eq!(render(&t, false), "--upload-pack= --no-upload-pack -- --no-exec\n");
        assert_eq!(render(&t, true), "--upload-pack= --exec= --no-upload-pack -- --no-exec\n");
    }

    /// Every ported table renders: an alias naming no option is a table bug that
    /// git itself reports with `BUG()`.
    #[test]
    fn every_table_renders() {
        for b in builtins() {
            b.render(false);
            b.render(true);
        }
    }
}

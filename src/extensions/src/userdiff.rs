//! Port of `userdiff.c` — git's diff drivers — together with the half of
//! `xdiff-interface.c` that turns a driver's `funcname` pattern into the section
//! heading a hunk header carries (`xdiff_set_find_func()` and `ff_regexp()`).
//!
//! # What a driver is
//!
//! A path's `diff` gitattribute names a driver; `diff.<name>.<key>` configures one.
//! `userdiff_config()` (userdiff.c:452) resolves `<name>` through
//! `userdiff_find_by_namelen()`, which searches the *user* drivers first and then
//! the built-in table — so configuring `diff.markdown.textconv` does not shadow the
//! built-in `markdown` driver, it **mutates that entry in place**. Only the keys
//! actually configured are replaced; everything else the built-in carries survives.
//! Measured against git 2.55.0 on a `*.md diff=markdown` path:
//!
//! ```text
//! $ git -c diff.markdown.textconv=cat diff -U0 HEAD~2 HEAD~1 -- docs/manual.md
//! @@ -3,0 +4 @@ # manual          <- the built-in markdown funcname still applies
//! $ git -c diff.markdown.xfuncname=^zzz diff -U0 HEAD~2 HEAD~1 -- docs/manual.md
//! @@ -3,0 +4 @@                   <- and this one replaced it
//! ```
//!
//! [`Settings::for_driver`] reproduces that merge.
//!
//! # funcname vs xfuncname
//!
//! Both keys write the *same* field (`drv->funcname`), differing only in the
//! `cflags` they attach — `funcname` compiles as a POSIX **basic** regex,
//! `xfuncname` as an **extended** one (userdiff.c:437-441). They therefore do not
//! have a precedence order: the last one in configuration order wins. Verified:
//!
//! ```text
//! $ git -c diff.markdown.funcname=^prose -c diff.markdown.xfuncname='^# man' … -U0
//! @@ -3,0 +4 @@ # man
//! $ git -c diff.markdown.xfuncname='^# man' -c diff.markdown.funcname=^prose … -U0
//! @@ -3,0 +4 @@ prose
//! ```
//!
//! # Where the port's regexes differ from `regcomp`
//!
//! git compiles these with the platform's POSIX `regcomp`; this port uses the
//! `regex` crate over bytes. Three consequences, all documented rather than hidden:
//!
//! * **Leftmost-longest vs leftmost-first.** POSIX picks the longest alternative at
//!   a given start position, the `regex` crate the first one written. Only an
//!   alternation whose branches can match at the same offset can tell them apart.
//! * **Bracket-expression spelling.** POSIX gives three things inside `[...]` a
//!   literal meaning the `regex` crate rejects or reads as syntax: a leading `]`, a
//!   bare `\`, and a `[` that does not open a `[:class:]`. [`posix_class_fixups`]
//!   rewrites exactly those and nothing else, which is what lets the built-in
//!   `csharp`, `java`, `bibtex` and `css` patterns compile with the meaning
//!   `regcomp` gives them.
//! * **Refusal text.** A pattern the `regex` crate rejects and `regcomp` would have
//!   taken (or the reverse) changes *whether* the fatal fires, not its wording:
//!   `Invalid regexp to look for hunk header: <pattern>` is git's own string.

use gix::bstr::{BStr, ByteSlice};

// ---------------------------------------------------------------------------
// the built-in driver table (userdiff.c:45-372)
// ---------------------------------------------------------------------------

/// One entry of `builtin_drivers[]`, reduced to the fields this port reads.
///
/// The `binary` tri-state is the table's remaining column; it is not read here,
/// because every built-in entry carries `.binary = -1` (the `PATTERNS`/`IPATTERN`
/// macros, userdiff.c:14 and :27) — the value that means "work it out from the
/// content", which is what the blob pipeline already does.
struct Builtin {
    name: &'static str,
    /// The `funcname.pattern` field, verbatim from `userdiff.c`.
    pattern: &'static str,
    /// `IPATTERN` adds `REG_ICASE` to `REG_EXTENDED`; `PATTERNS` does not.
    icase: bool,
    /// `word_regex`: the macro's third argument with the tail both macros append,
    /// `"|[^[:space:]]|[\xc0-\xff][\x80-\xbf]+"` (userdiff.c:22).
    ///
    /// git also carries a `word_regex_multi_byte` twin — the same pattern without
    /// the UTF-8 continuation alternative — and `userdiff_find_by_name()`
    /// (userdiff.c:518-527) swaps it in when `regexec_supports_multi_byte_chars()`
    /// says the locale can do it. That swap is what makes `diff.<name>.wordregex`
    /// silently ineffective on a built-in driver under a UTF-8 locale: it overwrites
    /// `word_regex` after `userdiff_config()` has already written the configured
    /// value into it. Measured against git 2.55.0, `-c diff.python.wordregex=...`
    /// changes nothing under `LC_ALL=en_US.UTF-8` and takes effect under `LC_ALL=C`.
    /// This port carries the C-locale spelling only: the `regex` crate matches on
    /// bytes regardless of locale, so the byte-range alternative reproduces the
    /// multi-byte behaviour, and configuration is never clobbered.
    word_regex: &'static str,
}

/// `builtin_drivers[]` (userdiff.c:45-372), in git's order. Only drivers carrying a
/// funcname pattern appear — `default`, `driver_true` (`diff`) and `driver_false`
/// (`-diff`) have none, and answer `None` here as they do in git.
const BUILTIN: &[Builtin] = &[
    Builtin { name: "ada", pattern: "!^(.*[ \t])?(is[ \t]+new|renames|is[ \t]+separate)([ \t].*)?$\n!^[ \t]*with[ \t].*$\n^[ \t]*((procedure|function)[ \t]+.*)$\n^[ \t]*((package|protected|task)[ \t]+.*)$", icase: true, word_regex: "[a-zA-Z][a-zA-Z0-9_]*|[-+]?[0-9][0-9#_.aAbBcCdDeEfF]*([eE][+-]?[0-9_]+)?|=>|\\.\\.|\\*\\*|:=|/=|>=|<=|<<|>>|<>|[^[:space:]]|[\\xc0-\\xff][\\x80-\\xbf]+" },
    Builtin { name: "bash", pattern: "^[ \t]*((([a-zA-Z_][a-zA-Z0-9_]*[ \t]*\\([ \t]*\\))|(function[ \t]+[a-zA-Z_][a-zA-Z0-9_]*(([ \t]*\\([ \t]*\\))|([ \t]+)))).*$)", icase: false, word_regex: "[a-zA-Z_][a-zA-Z0-9_]*|\\$[a-zA-Z0-9_]+|\\$\\{|\\|\\||&&|<<|>>|==|!=|<=|>=|[-+*/%&|^]=|:=|:-|:\\+|:\\?|##|%%|\\^\\^|,,|[-a-zA-Z0-9_]+|\\(|\\)|\\{|\\}|\\[|\\]|[^[:space:]]|[\\xc0-\\xff][\\x80-\\xbf]+" },
    Builtin { name: "bibtex", pattern: "(@[a-zA-Z]{1,}[ \t]*\\{{0,1}[ \t]*[^ \t\"@',\\#}{~%]*).*$", icase: false, word_regex: "[={}\"]|[^={}\" \t]+|[^[:space:]]|[\\xc0-\\xff][\\x80-\\xbf]+" },
    Builtin { name: "cpp", pattern: "!^[ \t]*[A-Za-z_][A-Za-z_0-9]*:[[:space:]]*($|/[/*])\n^((::[[:space:]]*)?[A-Za-z_].*)$", icase: false, word_regex: "[a-zA-Z_][a-zA-Z0-9_]*|[0-9][0-9.]*([Ee][-+]?[0-9]+)?[fFlLuU]*|0[xXbB][0-9a-fA-F]+[lLuU]*|\\.[0-9][0-9]*([Ee][-+]?[0-9]+)?[fFlL]?|[-+*/<>%&^|=!]=|--|\\+\\+|<<=?|>>=?|&&|\\|\\||::|->\\*?|\\.\\*|<=>|[^[:space:]]|[\\xc0-\\xff][\\x80-\\xbf]+" },
    Builtin { name: "csharp", pattern: "!(^|[ \t]+)(do|while|for|foreach|if|else|new|default|return|switch|case|throw|catch|using|lock|fixed)([ \t(]+|$)\n^[ \t]*(([][[:alnum:]@_.](<[][[:alnum:]@_, \t<>]+>)?)+([ \t]+([][[:alnum:]@_.](<[][[:alnum:]@_, \t<>]+>)?)+)+[ \t]*\\([^;]*)$\n^[ \t]*(([][[:alnum:]@_.](<[][[:alnum:]@_, \t<>]+>)?)+([ \t]+([][[:alnum:]@_.](<[][[:alnum:]@_, \t<>]+>)?)+)+[^;=:,()]*)$\n^[ \t]*(((static|public|internal|private|protected|new|unsafe|sealed|abstract|partial)[ \t]+)*(class|enum|interface|struct|record)[ \t]+.*)$\n^[ \t]*(namespace[ \t]+.*)$", icase: false, word_regex: "[a-zA-Z_][a-zA-Z0-9_]*|[-+0-9.e]+[fFlL]?|0[xXbB]?[0-9a-fA-F]+[lL]?|[-+*/<>%&^|=!]=|--|\\+\\+|<<=?|>>=?|&&|\\|\\||::|->|[^[:space:]]|[\\xc0-\\xff][\\x80-\\xbf]+" },
    Builtin { name: "css", pattern: "![:;][[:space:]]*$\n^[:[@.#]?[_a-z0-9].*$", icase: true, word_regex: "-?[_a-zA-Z][-_a-zA-Z0-9]*|-?[0-9]+|\\#[0-9a-fA-F]+|[^[:space:]]|[\\xc0-\\xff][\\x80-\\xbf]+" },
    Builtin { name: "dts", pattern: "!;\n!=\n^[ \t]*((/[ \t]*\\{|&?[a-zA-Z_]).*)", icase: false, word_regex: "[a-zA-Z0-9,._+?#-]+|[-+*/%&^|!~]|>>|<<|&&|\\|\\||[^[:space:]]|[\\xc0-\\xff][\\x80-\\xbf]+" },
    Builtin { name: "elixir", pattern: "^[ \t]*((def(macro|module|impl|protocol|p)?|test)[ \t].*)$", icase: false, word_regex: "[@:]?[a-zA-Z0-9@_?!]+|[-+]?0[xob][0-9a-fA-F]+|[-+]?[0-9][0-9_.]*([eE][-+]?[0-9_]+)?|:?(\\+\\+|--|\\.\\.|~~~|<>|\\^\\^\\^|<?\\|>|<<<?|>?>>|<<?~|~>?>|<~>|<=|>=|===?|!==?|=~|&&&?|\\|\\|\\|?|=>|<-|\\\\\\\\|->)|:?%[A-Za-z0-9_.]\\{\\}?|[^[:space:]]|[\\xc0-\\xff][\\x80-\\xbf]+" },
    Builtin { name: "fortran", pattern: "!^([C*]|[ \t]*!)\n!^[ \t]*MODULE[ \t]+PROCEDURE[ \t]\n^[ \t]*((END[ \t]+)?(PROGRAM|MODULE|BLOCK[ \t]+DATA|([^!'\" \t]+[ \t]+)*(SUBROUTINE|FUNCTION))[ \t]+[A-Z].*)$", icase: true, word_regex: "[a-zA-Z][a-zA-Z0-9_]*|\\.([Ee][Qq]|[Nn][Ee]|[Gg][TtEe]|[Ll][TtEe]|[Tt][Rr][Uu][Ee]|[Ff][Aa][Ll][Ss][Ee]|[Aa][Nn][Dd]|[Oo][Rr]|[Nn]?[Ee][Qq][Vv]|[Nn][Oo][Tt])\\.|[-+]?[0-9.]+([AaIiDdEeFfLlTtXx][Ss]?[-+]?[0-9.]*)?(_[a-zA-Z0-9][a-zA-Z0-9_]*)?|//|\\*\\*|::|[/<>=]=|[^[:space:]]|[\\xc0-\\xff][\\x80-\\xbf]+" },
    Builtin { name: "fountain", pattern: "^((\\.[^.]|(int|ext|est|int\\.?/ext|i/e)[. ]).*)$", icase: true, word_regex: "[^ \t-]+|[^[:space:]]|[\\xc0-\\xff][\\x80-\\xbf]+" },
    Builtin { name: "golang", pattern: "^[ \t]*(func[ \t]*.*(\\{[ \t]*)?)\n^[ \t]*(type[ \t].*(struct|interface)[ \t]*(\\{[ \t]*)?)", icase: false, word_regex: "[a-zA-Z_][a-zA-Z0-9_]*|[-+0-9.eE]+i?|0[xX]?[0-9a-fA-F]+i?|[-+*/<>%&^|=!:]=|--|\\+\\+|<<=?|>>=?|&\\^=?|&&|\\|\\||<-|\\.{3}|[^[:space:]]|[\\xc0-\\xff][\\x80-\\xbf]+" },
    Builtin { name: "html", pattern: "^[ \t]*(<[Hh][1-6]([ \t].*)?>.*)$", icase: false, word_regex: "[^<>= \t]+|[^[:space:]]|[\\xc0-\\xff][\\x80-\\xbf]+" },
    Builtin { name: "ini", pattern: "^[ \t]*\\[[^]]+\\]", icase: false, word_regex: "[^ \t]+|[^[:space:]]|[\\xc0-\\xff][\\x80-\\xbf]+" },
    Builtin { name: "java", pattern: "!^[ \t]*(catch|do|for|if|instanceof|new|return|switch|throw|while)\n^[ \t]*(([a-z-]+[ \t]+)*(class|enum|interface|record)[ \t]+.*)$\n^[ \t]*(([A-Za-z_<>&][][?&<>.,A-Za-z_0-9]*[ \t]+)+[A-Za-z_][A-Za-z_0-9]*[ \t]*\\([^;]*)$", icase: false, word_regex: "[a-zA-Z_][a-zA-Z0-9_]*|[-+0-9.e]+[fFlL]?|0[xXbB]?[0-9a-fA-F]+[lL]?|[-+*/<>%&^|=!]=|--|\\+\\+|<<=?|>>>?=?|&&|\\|\\||[^[:space:]]|[\\xc0-\\xff][\\x80-\\xbf]+" },
    Builtin { name: "kotlin", pattern: "^[ \t]*(([a-z]+[ \t]+)*(fun|class|interface)[ \t]+.*)$", icase: false, word_regex: "[a-zA-Z_][a-zA-Z0-9_]*|0[xXbB][0-9a-fA-F_]+[lLuU]*|[0-9][0-9_]*([.][0-9_]*)?([Ee][-+]?[0-9]+)?[fFlLuU]*|[.][0-9][0-9_]*([Ee][-+]?[0-9]+)?[fFlLuU]?|[-+*/<>%&^|=!]==?|--|\\+\\+|<<=|>>=|&&|\\|\\||->|\\.\\*|!!|[?:.][.:]|[^[:space:]]|[\\xc0-\\xff][\\x80-\\xbf]+" },
    Builtin { name: "markdown", pattern: "^ {0,3}#{1,6}[ \t].*", icase: false, word_regex: "[^<>= \t]+|[^[:space:]]|[\\xc0-\\xff][\\x80-\\xbf]+" },
    Builtin { name: "matlab", pattern: "^[[:space:]]*((classdef|function)[[:space:]].*)$|^(%%%?|##)[[:space:]].*$", icase: false, word_regex: "[a-zA-Z_][a-zA-Z0-9_]*|[-+0-9.e]+|[=~<>]=|\\.[*/\\^']|\\|\\||&&|[^[:space:]]|[\\xc0-\\xff][\\x80-\\xbf]+" },
    Builtin { name: "objc", pattern: "!^[ \t]*(do|for|if|else|return|switch|while)\n^[ \t]*([-+][ \t]*\\([ \t]*[A-Za-z_][A-Za-z_0-9* \t]*\\)[ \t]*[A-Za-z_].*)$\n^[ \t]*(([A-Za-z_][A-Za-z_0-9]*[ \t]+)+[A-Za-z_][A-Za-z_0-9]*[ \t]*\\([^;]*)$\n^(@(implementation|interface|protocol)[ \t].*)$", icase: false, word_regex: "[a-zA-Z_][a-zA-Z0-9_]*|[-+0-9.e]+[fFlL]?|0[xXbB]?[0-9a-fA-F]+[lL]?|[-+*/<>%&^|=!]=|--|\\+\\+|<<=?|>>=?|&&|\\|\\||::|->|[^[:space:]]|[\\xc0-\\xff][\\x80-\\xbf]+" },
    Builtin { name: "pascal", pattern: "^(((class[ \t]+)?(procedure|function)|constructor|destructor|interface|implementation|initialization|finalization)[ \t]*.*)$\n^(.*=[ \t]*(class|record).*)$", icase: false, word_regex: "[a-zA-Z_][a-zA-Z0-9_]*|[-+0-9.e]+|0[xXbB]?[0-9a-fA-F]+|<>|<=|>=|:=|\\.\\.|[^[:space:]]|[\\xc0-\\xff][\\x80-\\xbf]+" },
    Builtin { name: "perl", pattern: "^package .*\n^sub [[:alnum:]_':]+[ \t]*(\\([^)]*\\)[ \t]*)?(:[^;#]*)?(\\{[ \t]*)?(#.*)?$\n^(BEGIN|END|INIT|CHECK|UNITCHECK|AUTOLOAD|DESTROY)[ \t]*(\\{[ \t]*)?(#.*)?$\n^=head[0-9] .*", icase: false, word_regex: "[[:alpha:]_'][[:alnum:]_']*|0[xb]?[0-9a-fA-F_]*|[0-9a-fA-F_]+(\\.[0-9a-fA-F_]+)?([eE][-+]?[0-9_]+)?|=>|-[rwxoRWXOezsfdlpSugkbctTBMAC>]|~~|::|&&=|\\|\\|=|//=|\\*\\*=|&&|\\|\\||//|\\+\\+|--|\\*\\*|\\.\\.\\.?|[-+*/%.^&<>=!|]=|=~|!~|<<|<>|<=>|>>|[^[:space:]]|[\\xc0-\\xff][\\x80-\\xbf]+" },
    Builtin { name: "php", pattern: "^[\t ]*(((public|protected|private|static|abstract|final)[\t ]+)*function.*)$\n^[\t ]*((((final|abstract)[\t ]+)?class|enum|interface|trait).*)$", icase: false, word_regex: "[a-zA-Z_][a-zA-Z0-9_]*|[-+0-9.e]+|0[xXbB]?[0-9a-fA-F]+|[-+*/<>%&^|=!.]=|--|\\+\\+|<<=?|>>=?|===|&&|\\|\\||::|->|[^[:space:]]|[\\xc0-\\xff][\\x80-\\xbf]+" },
    Builtin { name: "python", pattern: "^[ \t]*((class|(async[ \t]+)?def)[ \t].*)$", icase: false, word_regex: "[a-zA-Z_][a-zA-Z0-9_]*|[-+0-9.e]+[jJlL]?|0[xX]?[0-9a-fA-F]+[lL]?|[-+*/<>%&^|=!]=|//=?|<<=?|>>=?|\\*\\*=?|[^[:space:]]|[\\xc0-\\xff][\\x80-\\xbf]+" },
    Builtin { name: "r", pattern: "^[ \t]*([a-zA-z][a-zA-Z0-9_.]*[ \t]*(<-|=)[ \t]*function.*)$", icase: false, word_regex: "[^ \t]+|[^[:space:]]|[\\xc0-\\xff][\\x80-\\xbf]+" },
    Builtin { name: "ruby", pattern: "^[ \t]*((class|module|def)[ \t].*)$", icase: false, word_regex: "(@|@@|\\$)?[a-zA-Z_][a-zA-Z0-9_]*|[-+0-9.e]+|0[xXbB]?[0-9a-fA-F]+|\\?(\\\\C-)?(\\\\M-)?.|//=?|[-+*/<>%&^|=!]=|<<=?|>>=?|===|\\.{1,3}|::|[!=]~|[^[:space:]]|[\\xc0-\\xff][\\x80-\\xbf]+" },
    Builtin { name: "rust", pattern: "^[\t ]*((pub(\\([^\\)]+\\))?[\t ]+)?((async|const|unsafe|extern([\t ]+\"[^\"]+\"))[\t ]+)?(struct|enum|union|mod|trait|fn|impl|macro_rules!)[< \t]+[^;]*)$", icase: false, word_regex: "[a-zA-Z_][a-zA-Z0-9_]*|[0-9][0-9_a-fA-Fiosuxz]*(\\.([0-9]*[eE][+-]?)?[0-9_fF]*)?|[-+*\\/<>%&^|=!:]=|<<=?|>>=?|&&|\\|\\||->|=>|\\.{2}=|\\.{3}|::|[^[:space:]]|[\\xc0-\\xff][\\x80-\\xbf]+" },
    Builtin { name: "scheme", pattern: "^(\\(.*)$\n^[\t ]*(\\(((define|def(struct|syntax|class|method|rules|record|proto|alias)?)[-*/ \t]|(library|module|struct|class)[*+ \t]).*)$\n^  ?(\\([Dd][Ee][Ff].*)$", icase: false, word_regex: "\\|([^|\\\\]|\\\\.)*\\||([^][)(}{ \t])+|[^[:space:]]|[\\xc0-\\xff][\\x80-\\xbf]+" },
    Builtin { name: "tex", pattern: "^(\\\\((sub)*section|chapter|part)\\*{0,1}\\{.*)$", icase: false, word_regex: "\\\\[a-zA-Z@]+|\\\\.|([a-zA-Z0-9]|[^\x01-\x7f])+|[^[:space:]]|[\\xc0-\\xff][\\x80-\\xbf]+" },
];

// ---------------------------------------------------------------------------
// driver settings
// ---------------------------------------------------------------------------

/// Where a driver's funcname pattern came from, which decides how it compiles.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FuncPattern {
    /// The configured or built-in pattern text, before any translation.
    pub pattern: String,
    /// `REG_EXTENDED`: set for `xfuncname` and every built-in, clear for `funcname`.
    pub extended: bool,
    /// `REG_ICASE`: set only by the built-in `IPATTERN` drivers.
    pub icase: bool,
}

/// The fields of `struct userdiff_driver` this port reads, after the built-in
/// defaults for the name have been overlaid with configuration.
#[derive(Clone, Debug, Default)]
pub struct Settings {
    /// `drv->funcname`, written by both `funcname` and `xfuncname`.
    pub funcname: Option<FuncPattern>,
    /// `drv->textconv`.
    pub textconv: Option<String>,
    /// `drv->textconv_want_cache`, i.e. `diff.<name>.cachetextconv`.
    pub cache_textconv: bool,
    /// `drv->external.cmd`, i.e. `diff.<name>.command`.
    pub external: Option<String>,
    /// `drv->external.trust_exit_code`, i.e. `diff.<name>.trustExitCode`.
    pub trust_exit_code: bool,
    /// `drv->word_regex`, i.e. the built-in table column overlaid with
    /// `diff.<name>.wordregex`. Kept as text: git compiles it in
    /// `init_diff_words_data()` (diff.c:2352-2359), at the first pair that is
    /// actually word-diffed, so a pattern that will not compile is only fatal under
    /// `--word-diff`.
    pub word_regex: Option<String>,
}

impl Settings {
    /// `userdiff_find_by_name()` + every `userdiff_config()` assignment that has
    /// been made for `name`: the built-in entry for the name, with each configured
    /// key overlaid in configuration order.
    ///
    /// The walk is over `config_snapshot().sections()` rather than a keyed lookup
    /// because `funcname` and `xfuncname` share one field: only the order the two
    /// keys appear in decides which pattern the driver ends up with.
    pub fn for_driver(repo: &gix::Repository, name: &str) -> Self {
        let mut out = Settings::default();
        if let Some(b) = BUILTIN.iter().find(|b| b.name == name) {
            out.funcname = Some(FuncPattern {
                pattern: b.pattern.to_string(),
                extended: true,
                icase: b.icase,
            });
            out.word_regex = Some(b.word_regex.to_string());
        }
        let snapshot = repo.config_snapshot();
        for section in snapshot.sections() {
            let header = section.header();
            if !header.name().to_string().eq_ignore_ascii_case("diff") {
                continue;
            }
            // git compares the subsection byte for byte; only the section name is
            // case-insensitive.
            if header.subsection_name() != Some(BStr::new(name.as_bytes())) {
                continue;
            }
            for (key, value) in section.body() {
                let text = || value.to_str_lossy().into_owned();
                // `parse_config_key()` lowercases the variable name.
                match key.to_ascii_lowercase().as_str() {
                    "funcname" => {
                        out.funcname = Some(FuncPattern { pattern: text(), extended: false, icase: false });
                    }
                    "xfuncname" => {
                        out.funcname = Some(FuncPattern { pattern: text(), extended: true, icase: false });
                    }
                    "textconv" => out.textconv = Some(text()),
                    "cachetextconv" => out.cache_textconv = config_bool(&text()),
                    "command" => out.external = Some(text()),
                    "trustexitcode" => out.trust_exit_code = config_bool(&text()),
                    "wordregex" => out.word_regex = Some(text()),
                    _ => {}
                }
            }
        }
        out
    }

    /// `xdiff_set_find_func()` over this driver's pattern, or `None` when it has
    /// none — which is what leaves `xecfg->find_func` NULL and `def_ff` in charge.
    pub fn compile_funcname(&self) -> Result<Option<FuncName>, String> {
        match &self.funcname {
            None => Ok(None),
            Some(p) => FuncName::compile(p).map(Some),
        }
    }
}

/// `git_config_bool()` for the two boolean driver keys. An empty value is git's
/// "implicit true" (`-c diff.d.cachetextconv` with no `=`), which reaches the
/// config reader as a valueless key rather than an empty string.
pub fn config_bool(v: &str) -> bool {
    let v = v.trim();
    !(v.eq_ignore_ascii_case("false")
        || v.eq_ignore_ascii_case("no")
        || v.eq_ignore_ascii_case("off")
        || v == "0")
}

// ---------------------------------------------------------------------------
// xdiff-interface.c: xdiff_set_find_func() / ff_regexp()
// ---------------------------------------------------------------------------

/// One compiled element of `struct ff_regs`.
struct Reg {
    re: regex::bytes::Regex,
    /// A pattern written `!<re>`: matching it means "this line is *not* a heading",
    /// and the search stops there rather than falling through to the next pattern.
    negate: bool,
}

/// `struct ff_regs`: the pattern list a driver's funcname compiles to.
pub struct FuncName {
    regs: Vec<Reg>,
}

impl std::fmt::Debug for FuncName {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FuncName").field("patterns", &self.regs.len()).finish()
    }
}

impl FuncName {
    /// `xdiff_set_find_func()` (xdiff-interface.c:277).
    ///
    /// The value is one regex per line. A leading `!` negates, and the last element
    /// may not be negated — git dies on that before compiling anything else.
    /// A compilation failure is `die("Invalid regexp to look for hunk header: %s")`
    /// naming the *element*, not the whole value.
    pub fn compile(p: &FuncPattern) -> Result<FuncName, String> {
        let parts: Vec<&str> = p.pattern.split('\n').collect();
        let mut regs = Vec::with_capacity(parts.len());
        for (i, raw) in parts.iter().enumerate() {
            let negate = raw.starts_with('!');
            if negate && i == parts.len() - 1 {
                return Err(format!("Last expression must not be negated: {}", p.pattern));
            }
            let expr = if negate { &raw[1..] } else { *raw };
            let re = compile_one(expr, p.extended, p.icase)
                .ok_or_else(|| format!("Invalid regexp to look for hunk header: {expr}"))?;
            regs.push(Reg { re, negate });
        }
        Ok(FuncName { regs })
    }

    /// `ff_regexp()` (xdiff-interface.c:246).
    ///
    /// The record is matched without its terminator, the first non-negated pattern
    /// to match wins, and the heading is that match's extent — capture group 1 when
    /// the pattern has one that participated, the whole match otherwise. The result
    /// is clipped to `sz` and only then stripped of trailing whitespace, the same
    /// order [`super::porcelain::diff_pairs::def_ff`] uses.
    pub fn find<'a>(&self, rec: &'a [u8], sz: usize) -> Option<&'a [u8]> {
        // "Exclude terminating newline (and cr) from matching".
        let mut len = rec.len();
        if len > 0 && rec[len - 1] == b'\n' {
            if len > 1 && rec[len - 2] == b'\r' {
                len -= 2;
            } else {
                len -= 1;
            }
        }
        let line = &rec[..len];

        let caps = self.regs.iter().find_map(|reg| {
            let c = reg.re.captures(line)?;
            Some((reg.negate, c))
        })?;
        // `if (reg->negate) return -1;` — a negated hit ends the search with no
        // heading, and the patterns after it are never tried.
        if caps.0 {
            return None;
        }
        let caps = caps.1;
        let m = match caps.get(1) {
            Some(g) => g,
            None => caps.get(0)?,
        };
        let mut result = (m.end() - m.start()).min(sz);
        let text = &line[m.start()..];
        // C's `isspace` in the default locale, which includes the vertical tab that
        // Rust's `is_ascii_whitespace` leaves out.
        while result > 0 && matches!(text[result - 1], b' ' | b'\t' | b'\n' | 0x0b | 0x0c | b'\r') {
            result -= 1;
        }
        Some(&text[..result])
    }
}

/// `regcomp()` over one element, translated into what the `regex` crate accepts.
///
/// `None` is `regcomp()` returning non-zero. An empty expression is one of those:
/// POSIX leaves it undefined and both regex libraries this port is measured
/// against reject it, while the `regex` crate would accept it as "matches
/// everywhere".
fn compile_one(expr: &str, extended: bool, icase: bool) -> Option<regex::bytes::Regex> {
    if expr.is_empty() {
        return None;
    }
    let ere = if extended {
        posix_class_fixups(expr)
    } else {
        posix_class_fixups(&bre_to_ere(expr))
    };
    regex::bytes::RegexBuilder::new(&ere)
        // The patterns are matched against raw record bytes, so `.` and the classes
        // must be byte-oriented; a record is not required to be valid UTF-8.
        .unicode(false)
        .case_insensitive(icase)
        .build()
        .ok()
}

/// POSIX **basic** regular expression to the extended syntax the `regex` crate
/// speaks, which is what `diff.<driver>.funcname` (no `REG_EXTENDED`) needs.
///
/// In a BRE the grouping, alternation and interval metacharacters are the
/// *backslashed* forms and the bare characters are literals; `+` and `?` are
/// literals with no operator spelling at all in POSIX (the `\+` / `\?` GNU
/// extensions are accepted here because both regex libraries this port is measured
/// against implement them). Bracket expressions are copied through untouched —
/// backslash has no special meaning inside one in either dialect.
fn bre_to_ere(bre: &str) -> String {
    let src: Vec<char> = bre.chars().collect();
    let mut out = String::with_capacity(bre.len());
    let mut i = 0;
    let mut in_class = false;
    while i < src.len() {
        let c = src[i];
        if in_class {
            out.push(c);
            // `]` closes the class unless it is the first member, which
            // `posix_class_fixups` is left to sort out.
            if c == ']' {
                in_class = false;
            }
            i += 1;
            continue;
        }
        match c {
            '[' => {
                in_class = true;
                out.push(c);
                i += 1;
                // A leading `^` and then a leading `]` are both members, not syntax.
                if i < src.len() && src[i] == '^' {
                    out.push('^');
                    i += 1;
                }
                if i < src.len() && src[i] == ']' {
                    out.push(']');
                    i += 1;
                }
            }
            '\\' if i + 1 < src.len() => {
                let n = src[i + 1];
                match n {
                    // The backslashed operators become bare ones.
                    '(' | ')' | '{' | '}' | '|' | '+' | '?' => out.push(n),
                    // Everything else keeps its escape.
                    _ => {
                        out.push('\\');
                        out.push(n);
                    }
                }
                i += 2;
            }
            // The bare operator characters are literals in a BRE.
            '(' | ')' | '{' | '}' | '|' | '+' | '?' => {
                out.push('\\');
                out.push(c);
                i += 1;
            }
            _ => {
                out.push(c);
                i += 1;
            }
        }
    }
    out
}

/// The two bracket-expression spellings POSIX allows and the `regex` crate does
/// not, rewritten to the meaning `regcomp()` gives them.
///
/// * `]` immediately after `[` or `[^` is a literal `]`, not an empty class.
///   git's built-in `csharp` and `java` patterns rely on it (`[][[:alnum:]@_.]`).
/// * a `\` inside a bracket expression is a literal backslash, not an escape —
///   git's `bibtex` pattern relies on it (`[^ \t\"@',\\#}{~%]`).
/// * a `[` inside a bracket expression that does not open a `[:class:]` is a
///   literal `[`; the built-in `css` pattern relies on it (`^[:[@.#]?`).
///
/// Everything outside a bracket expression is copied through byte for byte.
pub fn posix_class_fixups(ere: &str) -> String {
    // Walked as characters, not bytes: a pattern is UTF-8 text and re-encoding a
    // multi-byte character one byte at a time would change what it matches.
    let src: Vec<char> = ere.chars().collect();
    let mut out = String::with_capacity(ere.len());
    let mut i = 0;
    while i < src.len() {
        match src[i] {
            '\\' if i + 1 < src.len() => {
                out.push('\\');
                out.push(src[i + 1]);
                i += 2;
            }
            '[' => {
                out.push('[');
                i += 1;
                if src.get(i) == Some(&'^') {
                    out.push('^');
                    i += 1;
                }
                if src.get(i) == Some(&']') {
                    out.push_str("\\]");
                    i += 1;
                }
                // Inside the bracket expression until its closing `]`.
                while i < src.len() && src[i] != ']' {
                    if src[i] == '[' {
                        // `[:name:]`, `[.coll.]` and `[=equiv=]` are the only
                        // constructs a `[` can open here.
                        match find_class_end(&src, i) {
                            Some(end) => {
                                out.extend(&src[i..=end + 1]);
                                i = end + 2;
                            }
                            None => {
                                out.push_str("\\[");
                                i += 1;
                            }
                        }
                        continue;
                    }
                    if src[i] == '\\' {
                        out.push_str("\\\\");
                        i += 1;
                        continue;
                    }
                    out.push(src[i]);
                    i += 1;
                }
                if i < src.len() {
                    out.push(']');
                    i += 1;
                }
            }
            c => {
                out.push(c);
                i += 1;
            }
        }
    }
    out
}

/// The index of the `:`/`.`/`=` that closes the `[:class:]` opened at `at`, or
/// `None` when nothing closes it — in which case the `[` is a literal member.
fn find_class_end(src: &[char], at: usize) -> Option<usize> {
    let kind = *src.get(at + 1)?;
    if !matches!(kind, ':' | '.' | '=') {
        return None;
    }
    let mut j = at + 2;
    while j + 1 < src.len() {
        if src[j] == kind && src[j + 1] == ']' {
            return Some(j);
        }
        j += 1;
    }
    None
}

// ---------------------------------------------------------------------------
// attribute lookup
// ---------------------------------------------------------------------------

/// One driver as a command uses it: [`Settings`] merged out of the built-in table
/// and configuration, with the funcname pattern already through
/// `xdiff_set_find_func()`.
///
/// Shared behind an `Arc` because a run's pairs almost always name the same one or
/// two drivers, and compiling a pattern per pair is exactly the work git's
/// `userdiff_find_by_name()` lookup avoids.
pub struct Driver {
    /// `drv->name` — the value of the path's `diff` attribute. `userdiff_get_textconv()`
    /// (userdiff.c:436) builds the textconv cache's ref out of it
    /// (`refs/notes/textconv/<name>`), which is the only thing that reads it here.
    pub name: String,
    pub settings: Settings,
    /// `xdiff_set_find_func()` over `settings.funcname`.
    pub funcname: Option<FuncName>,
}

/// `userdiff_find_by_path()` for one command: the gitattributes stack, the driver
/// table, and the compiled patterns, resolved once per driver name.
///
/// git keeps one static `attr_check` and one driver list for the whole process; this
/// is the same amortisation, scoped to whoever holds it. Building it reads the index,
/// so a `log -p` that built one per commit would pay that once per patch.
pub struct Lookup<'repo> {
    repo: &'repo gix::Repository,
    names: crate::porcelain::cat_file::Textconv<'repo>,
    /// Driver name to resolved driver; a run touches at most a handful.
    cache: std::collections::HashMap<String, Option<std::sync::Arc<Driver>>>,
}

impl<'repo> Lookup<'repo> {
    pub fn new(repo: &'repo gix::Repository) -> anyhow::Result<Self> {
        Ok(Self {
            repo,
            names: crate::porcelain::cat_file::Textconv::new(repo)?,
            cache: std::collections::HashMap::new(),
        })
    }

    /// `userdiff_find_by_path()` plus `xdiff_set_find_func()`: the driver a path's
    /// `diff` attribute names, with its pattern compiled.
    ///
    /// `None` for a path with no driver — which includes the boolean attribute forms
    /// (`diff` / `-diff`), since git's `driver_true` / `driver_false` carry no
    /// funcname, textconv or command at all, and a driver that configures none of
    /// those either, which behaves identically to having no driver.
    ///
    /// `Err` carries git's `die(_("Invalid regexp to look for hunk header: %s"))`.
    pub fn for_path(&mut self, path: &BStr) -> Result<Option<std::sync::Arc<Driver>>, String> {
        let Some(name) = self.names.driver_name(path).map_err(|e| e.to_string())? else {
            return Ok(None);
        };
        if let Some(hit) = self.cache.get(&name) {
            return Ok(hit.clone());
        }
        let settings = Settings::for_driver(self.repo, &name);
        let funcname = settings.compile_funcname()?;
        let interesting = funcname.is_some()
            || settings.textconv.is_some()
            || settings.external.is_some()
            || settings.word_regex.is_some();
        let drv = interesting
            .then(|| std::sync::Arc::new(Driver { name: name.clone(), settings, funcname }));
        self.cache.insert(name, drv.clone());
        Ok(drv)
    }

    /// `prep_temp_blob()` + `run_textconv()` (diff.c:7758): write `blob`'s worktree
    /// form under its own basename in a private directory, run `program` over it
    /// through the shell, and take its stdout. `None` is git's NULL return — the
    /// program could not be started, or exited non-zero.
    ///
    /// It shares the stack above because both halves need the same attribute and
    /// filter state git's one `userdiff_driver` lookup carries.
    pub fn run_program(
        &mut self,
        program: &str,
        path: &BStr,
        blob: &[u8],
    ) -> anyhow::Result<Option<Vec<u8>>> {
        self.names.run(program, path, blob)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every built-in pattern has to survive the translation to the `regex`
    /// crate's syntax; one that does not would silently lose its driver's hunk
    /// headings.
    #[test]
    fn every_builtin_pattern_compiles() {
        for b in BUILTIN {
            let p = FuncPattern { pattern: b.pattern.to_string(), extended: true, icase: b.icase };
            FuncName::compile(&p).unwrap_or_else(|e| panic!("{}: {e}", b.name));
        }
    }

    /// Every built-in `word_regex` has to survive the same translation, or
    /// `--word-diff` over a path naming that driver would die where git renders.
    /// The byte-range alternative both macros append
    /// (`[\xc0-\xff][\x80-\xbf]+`) is the part most likely to be rejected by an
    /// engine that reads the pattern as UTF-8, so it is what this test is really
    /// asking about.
    #[test]
    fn every_builtin_word_regex_compiles() {
        for b in BUILTIN {
            crate::porcelain::diff_color::compile_word_regex(b.word_regex)
                .unwrap_or_else(|e| panic!("{}: {e}", b.name));
        }
    }

    /// The `python` driver splits `alpha_beta+gamma` into three words, which is what
    /// a driver word regex is *for*: without one the whole run of non-space bytes is
    /// a single word. Measured against git 2.55.0 under `LC_ALL=C`,
    /// `git diff --word-diff=plain` over a `diff=python` path printed
    /// `x = alpha_beta+[-gamma-]{+delta+}`.
    #[test]
    fn a_driver_word_regex_splits_where_whitespace_would_not() {
        let b = BUILTIN.iter().find(|b| b.name == "python").expect("python driver");
        let re = crate::porcelain::diff_color::compile_word_regex(b.word_regex).expect("compiles");
        let words: Vec<&[u8]> =
            re.find_iter(b"alpha_beta+gamma").map(|m| m.as_bytes()).collect();
        assert_eq!(words, vec![&b"alpha_beta"[..], &b"+"[..], &b"gamma"[..]]);
    }

    /// The built-in `markdown` driver's heading is the whole match, since its
    /// pattern has no capture group.
    #[test]
    fn markdown_heading_is_the_atx_line() {
        let b = BUILTIN.iter().find(|b| b.name == "markdown").expect("markdown driver");
        let f = FuncName::compile(&FuncPattern {
            pattern: b.pattern.to_string(),
            extended: true,
            icase: b.icase,
        })
        .expect("compiles");
        assert_eq!(f.find(b"# manual\n", 80).as_deref(), Some(&b"# manual"[..]));
        assert_eq!(f.find(b"prose\n", 80), None);
        // Four spaces of indent is a code block, not a heading.
        assert_eq!(f.find(b"    # manual\n", 80), None);
        // No space after the hashes is not an ATX heading either.
        assert_eq!(f.find(b"#manual\n", 80), None);
    }

    /// Capture group 1 wins over the whole match, which is how git's own patterns
    /// return the declaration without its leading indentation.
    #[test]
    fn group_one_is_preferred_over_the_whole_match() {
        let f = FuncName::compile(&FuncPattern {
            pattern: "^(pro|man)".to_string(),
            extended: true,
            icase: false,
        })
        .expect("compiles");
        assert_eq!(f.find(b"prose\n", 80).as_deref(), Some(&b"pro"[..]));
    }

    /// `funcname` is a basic regex: the backslashed forms are the operators.
    #[test]
    fn basic_regex_operators_are_the_backslashed_ones() {
        let f = FuncName::compile(&FuncPattern {
            pattern: r"^\(pro\|man\)".to_string(),
            extended: false,
            icase: false,
        })
        .expect("compiles");
        assert_eq!(f.find(b"prose\n", 80).as_deref(), Some(&b"pro"[..]));

        // ... and the bare ones are literals.
        let f = FuncName::compile(&FuncPattern {
            pattern: "^a(b)".to_string(),
            extended: false,
            icase: false,
        })
        .expect("compiles");
        assert_eq!(f.find(b"a(b)c\n", 80).as_deref(), Some(&b"a(b)"[..]));
    }

    /// A negated element ends the search: the line is deliberately *not* a heading,
    /// and the patterns after it are never consulted.
    #[test]
    fn a_negated_element_suppresses_the_heading() {
        let f = FuncName::compile(&FuncPattern {
            pattern: "!^skip\n^.*".to_string(),
            extended: true,
            icase: false,
        })
        .expect("compiles");
        assert_eq!(f.find(b"skip me\n", 80), None);
        assert_eq!(f.find(b"keep me\n", 80).as_deref(), Some(&b"keep me"[..]));
    }

    /// git refuses a value whose last element is negated, before compiling.
    #[test]
    fn the_last_element_may_not_be_negated() {
        let err = FuncName::compile(&FuncPattern {
            pattern: "!^skip".to_string(),
            extended: true,
            icase: false,
        })
        .expect_err("refused");
        assert_eq!(err, "Last expression must not be negated: !^skip");
    }

    /// The refusal names the element that failed, with git's wording.
    #[test]
    fn an_uncompilable_element_reports_gits_message() {
        let err = FuncName::compile(&FuncPattern {
            pattern: "^[".to_string(),
            extended: true,
            icase: false,
        })
        .expect_err("refused");
        assert_eq!(err, "Invalid regexp to look for hunk header: ^[");
    }

    /// The heading is clipped to the caller's buffer and only then right-trimmed,
    /// which is the order both this and `def_ff` use.
    #[test]
    fn the_heading_is_clipped_before_it_is_trimmed() {
        let f = FuncName::compile(&FuncPattern {
            pattern: "^.*".to_string(),
            extended: true,
            icase: false,
        })
        .expect("compiles");
        assert_eq!(f.find(b"abc    def\n", 5).as_deref(), Some(&b"abc"[..]));
    }
}

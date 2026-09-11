//! Build-time harvester for `git zrepl` Tab completion.
//!
//! The completion table is generated FROM each verb's own parser rather than
//! maintained as a parallel hand-written list that rots the moment a flag is
//! added. One file : one verb, matching `dispatch.rs`'s file→verb convention
//! (`__`→`--`, `_`→`-`).
//!
//! # Options: the verb's own option table first
//!
//! Most verbs resolve a `--name` through [`porcelain::resolve_long`] — the port
//! of `parse_long_opt()` — over a `const LONG_OPTS` table of `LongOpt { name,
//! neg, arg }`. That table *is* the parser: a name absent from it is refused, a
//! name added to it is accepted the same build. Rendering completion from it is
//! therefore drift-free by construction, and it is rendered in the exact shape
//! `parse-options.c:show_gitcomp()` prints for `--git-completion-helper`, so a
//! verb's list can be diffed byte-for-byte against the stock binary's.
//!
//! Three literal table shapes are read, in this order:
//!
//! * `const LONG_OPTS: &[LongOpt]` — name, negation sense and value sense, so
//!   the full stock rendering (`--x`, `--x=`, `--no-x`) can be produced.
//! * `const …LONG_OPTS: &[&str]` — names only (`upload-pack`, `log`). Rendered
//!   as bare `--name`: sparse next to stock (no `--no-` half, no `=` suffix)
//!   but every entry is one the parser genuinely resolves.
//! * `const OPTS: &[OptDef]` — `pack-objects` declares `{ long, kind,
//!   negatable }` and *derives* its `LONG_OPTS` from it in a `const` block, so
//!   the derived table holds no literals to read; the same three fields are
//!   read here under the same `Kind`→value mapping the file itself writes.
//!
//! Each shape is accepted only when it parses *completely* — every entry
//! matched, no empty name — so a table that is computed rather than written out
//! falls through to the next shape instead of yielding a half-read list.
//!
//! # Options: the regex fallback, and why it is marked
//!
//! A verb with no such table keeps the original whole-file harvest: any *whole*
//! quoted flag literal (`"` flag `"`), so a flag inside a message string
//! (`"unknown option --foo"`) never matches. That harvest is imprecise in both
//! directions — it cannot see a name the parser matches after stripping `--`,
//! and it happily collects a flag literal the verb only ever *rejects* or
//! rewrites (stock refuses `git log --renames`, which the harvest offered). It
//! is kept because a rough list beats none, but every row it produces carries
//! `approximate: true` into the generated file so the imprecision is visible at
//! the point of use rather than silently indistinguishable from a parsed table.
//!
//! # Subcommands
//!
//! Taken from match-arm position (`Some("w")` / `^"w" =>`), first-arg dispatch,
//! for every verb regardless of which option shape it used. The superset `z*`
//! verbs are NOT harvested here (their parsers span multi-verb modules);
//! repl.rs carries their spec by hand.

use std::{env, fs, path::Path};

fn main() {
    emit_zlib_rs_version();

    // Package root is src/extensions/; the porcelain modules live under src/.
    let dir = Path::new("src/porcelain");
    println!("cargo:rerun-if-changed=src/porcelain");

    // Options: a *whole* quoted flag literal, so a flag inside a message string
    // (`"unknown option --foo"`) never matches — only standalone `"--foo"` do.
    let opt_re = regex::Regex::new(r#""(--?[A-Za-z][A-Za-z0-9-]*)""#).unwrap();
    // Subcommands are harvested ONLY from inside a `match` block whose scrutinee is
    // subcommand dispatch — either an explicitly subcommand-named variable
    // (`sub`/`cmd`/…) or a first-positional (`X.first()...`). Both the bare
    // `"word" =>` and `Some("word") =>` arm forms are collected there (depth-1).
    // A GLOBAL `Some("word")` scan is deliberately NOT used: `Some("literal")` is
    // also value dispatch (date formats, `--track` modes, grep pattern types), so
    // scanning it everywhere harvests those values as fake subcommands. Scoping to
    // first-arg/subcommand blocks excludes value matches — their scrutinee is the
    // flag value (`match atom`, `match track_mode`), not `sub`/`first()`.
    let match_hdr = regex::Regex::new(r"match[[:space:]]+([^\n{]*?)[[:space:]]*\{").unwrap();
    let scrut_ok = regex::Regex::new(r"^(sub|subcommand|subcmd|cmd|command|verb)\b|first\(").unwrap();

    let mut entries: Vec<(String, Vec<String>, Vec<String>, bool)> = Vec::new();
    for ent in fs::read_dir(dir).expect("read porcelain dir") {
        let path = ent.unwrap().path();
        if path.extension().and_then(|e| e.to_str()) != Some("rs") {
            continue;
        }
        let stem = path.file_stem().unwrap().to_str().unwrap();
        if stem == "mod" {
            continue;
        }
        let verb = stem.replace("__", "\u{1}").replace('_', "-").replace('\u{1}', "--");
        let full = fs::read_to_string(&path).unwrap();
        // The parser is the source of truth — test modules are not. Drop everything
        // from the first `#[cfg(test)]` so `Some("feature")` in a test assertion
        // never lands in the completion set.
        let text = full.split("#[cfg(test)]").next().unwrap_or(&full).to_string();

        // The verb's own option table if it declares a readable one, else the
        // whole-file regex harvest — flagged, because that one can be wrong.
        let (opts, approximate) = match option_table(&text) {
            Some(table) => (show_gitcomp(&table), false),
            None => (dedup_sorted(opt_re.captures_iter(&text).map(|c| c[1].to_string())), true),
        };

        let mut raw_subs: Vec<String> = Vec::new();
        for hdr in match_hdr.captures_iter(&text) {
            if !scrut_ok.is_match(hdr.get(1).unwrap().as_str().trim()) {
                continue;
            }
            let open = hdr.get(0).unwrap().end() - 1; // index of the block's `{`
            harvest_block_arms(&text, open, &mut raw_subs);
        }
        // Drop the verb's own name (self-referential guard like `if a == "remote"`).
        raw_subs.retain(|s| *s != verb);
        let subs = dedup_sorted(raw_subs.into_iter());

        if opts.is_empty() && subs.is_empty() {
            continue;
        }
        entries.push((verb, opts, subs, approximate));
    }
    entries.sort();

    let approx: Vec<&str> = entries.iter().filter(|e| e.3).map(|e| e.0.as_str()).collect();
    let mut out = String::from("// @generated by build.rs from src/porcelain/*.rs — do not edit.\n");
    out.push_str("//\n// `(verb, options, subcommands, approximate)`.\n");
    out.push_str(
        "//\n// `approximate == false`: the options are the verb's own `parse_long_opt()` table,\n\
         // rendered the way `show_gitcomp()` prints `--git-completion-helper`, and so cannot\n\
         // drift from what the verb accepts.\n\
         //\n// `approximate == true`: the verb declares no readable option table, so the options\n\
         // are a whole-file scan for quoted flag literals. That list can both miss a name the\n\
         // parser matches with `--` already stripped and offer one the verb only ever rejects.\n",
    );
    out.push_str(&format!(
        "//\n// approximate verbs ({} of {}): {}\n",
        approx.len(),
        entries.len(),
        approx.join(", ")
    ));
    out.push_str("pub static PORCELAIN_SPEC: &[(&str, &[&str], &[&str], bool)] = &[\n");
    for (verb, opts, subs, approximate) in &entries {
        out.push_str(&format!("    ({verb:?}, &{opts:?}, &{subs:?}, {approximate}),\n"));
    }
    out.push_str("];\n");

    let dest = Path::new(&env::var("OUT_DIR").unwrap()).join("porcelain_spec.rs");
    fs::write(dest, out).unwrap();
}

/// `git version --build-options` names the flate library the binary links, the
/// way stock prints `zlib:` / `zlib-ng:` for the one *it* links. The version has
/// to be read out of the resolved dependency graph rather than written into the
/// source, or it silently goes stale the first time the lockfile moves.
///
/// The lockfile is the workspace root's (`src/extensions` is a member), and an
/// empty value is emitted rather than a guess when `zlib-rs` is not in the graph
/// — `porcelain::version` drops the line entirely on an empty string, so a
/// dropped dependency removes the claim instead of freezing a false one.
fn emit_zlib_rs_version() {
    let manifest = env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR");
    let lock = Path::new(&manifest).join("..").join("..").join("Cargo.lock");
    println!("cargo:rerun-if-changed={}", lock.display());

    let version = fs::read_to_string(&lock).ok().and_then(|text| lock_version(&text, "zlib-rs"));
    println!("cargo:rustc-env=ZVCS_ZLIB_RS_VERSION={}", version.unwrap_or_default());
}

/// The `version = "…"` of the `[[package]]` block whose `name` is `crate_name`,
/// read straight out of a Cargo.lock. The scan stops at the next `[[package]]`
/// header so a missing `version` key cannot pick up the next package's.
fn lock_version(text: &str, crate_name: &str) -> Option<String> {
    let needle = format!("name = \"{crate_name}\"");
    let mut lines = text.lines().skip_while(|l| l.trim() != needle);
    lines.next()?; // consume the matched `name = …` line itself
    lines
        .take_while(|l| !l.trim_start().starts_with("[["))
        .find_map(|l| Some(l.trim().strip_prefix("version = \"")?.strip_suffix('"')?.to_string()))
}

/// One entry of a verb's own option table, carrying only what
/// `show_gitcomp()` reads: the long name, whether `--no-<name>` resolves
/// (the absence of `PARSE_OPT_NONEG`), and whether a value is mandatory.
struct TableOpt {
    name: String,
    neg: bool,
    /// A value that is neither optional nor defaulted, which is the only case
    /// `show_gitcomp()` marks with a trailing `=`.
    requires_arg: bool,
}

/// The option table a porcelain verb declares, in whichever of the three
/// readable shapes it uses — `None` when it declares none, or when the one it
/// declares is computed rather than written out (so no literal can be read).
fn option_table(text: &str) -> Option<Vec<TableOpt>> {
    if let Some((ty, block)) = const_block(text, r"[A-Z_]*LONG_OPTS") {
        if ty.contains("LongOpt") {
            if let Some(t) = parse_long_opt_table(&block) {
                return Some(t);
            }
        } else if regex::Regex::new(r"\[\s*&(?:'static\s+)?str").unwrap().is_match(&ty) {
            if let Some(t) = parse_name_table(&block) {
                return Some(t);
            }
        }
    }
    let (_, block) = const_block(text, "OPTS")?;
    parse_opt_def_table(&block)
}

/// The `<type>` and the initialiser text of `const <name_re>: <type> = <init>;`.
/// The type is everything up to the `=`, `;` included — a fixed-size array type
/// carries one (`[&str; 5]`) and stopping at it would hide those tables.
///
/// the initialiser taken up to the `;` that closes the item at bracket depth 0
/// so a table spelled `&[…]`, `&{ … &[…] }` or `[…]` all yield their whole body.
/// String contents are skipped, so a `;` or a bracket inside a literal cannot
/// end the scan early.
/// Drop `//` comments, keeping everything else byte-for-byte.
///
/// The recognizers below read every string literal in a block and reject the
/// whole table if one does not look like an option name — which is the right
/// rule for entries and the wrong one for prose. A `//` line that happens to
/// quote a flag, as `GIT_LOG_LONG_OPTS`' does when it explains that git parses
/// `"--graph-lane-limit="` only in its `=` form, silently demoted `log` to the
/// regex fallback and cost it 200-odd completions. Nothing announced it; the
/// table simply stopped being recognised. A string literal inside a comment is
/// not a table entry, so it is removed before parsing rather than left to be
/// mistaken for one.
///
/// Quotes are tracked so a `//` inside a string literal — a URL, a doc example —
/// does not start a comment.
fn strip_line_comments(text: &str) -> String {
    let b = text.as_bytes();
    let (mut out, mut i, mut in_str) = (String::with_capacity(text.len()), 0usize, false);
    while i < b.len() {
        if in_str {
            if b[i] == b'\\' && i + 1 < b.len() {
                out.push(b[i] as char);
                out.push(b[i + 1] as char);
                i += 2;
                continue;
            }
            if b[i] == b'"' {
                in_str = false;
            }
        } else if b[i] == b'"' {
            in_str = true;
        } else if b[i] == b'/' && i + 1 < b.len() && b[i + 1] == b'/' {
            while i < b.len() && b[i] != b'\n' {
                i += 1;
            }
            continue;
        }
        out.push(b[i] as char);
        i += 1;
    }
    out
}

fn const_block(text: &str, name_re: &str) -> Option<(String, String)> {
    let text = &strip_line_comments(text);
    let re = regex::Regex::new(&format!(r"const\s+{name_re}\s*:\s*([^=]*?)=")).unwrap();
    let m = re.captures(text)?;
    let ty = m[1].trim().to_string();
    let start = m.get(0).unwrap().end();
    let b = text.as_bytes();
    let (mut depth, mut i) = (0i32, start);
    while i < b.len() {
        match b[i] {
            b'[' | b'{' | b'(' => depth += 1,
            b']' | b'}' | b')' => depth -= 1,
            b';' if depth <= 0 => return Some((ty, text[start..i].to_string())),
            b'"' => {
                i += 1;
                while i < b.len() && b[i] != b'"' {
                    i += if b[i] == b'\\' { 2 } else { 1 };
                }
            }
            _ => {}
        }
        i += 1;
    }
    None
}

/// `LongOpt { name: "x", neg: <bool>, arg: Arg::<sense> }` entries. Every
/// `LongOpt {` in the block must parse and name something, so `pack-objects`'s
/// table — a `const` loop over another table, whose only literal is the `name:
/// ""` array seed — is rejected here rather than read as one nameless option.
fn parse_long_opt_table(block: &str) -> Option<Vec<TableOpt>> {
    let entry = regex::Regex::new(
        r#"LongOpt\s*\{\s*name:\s*"([^"]*)"\s*,\s*neg:\s*(true|false)\s*,\s*arg:\s*(?:super::)?Arg::(\w+)\s*,?\s*\}"#,
    )
    .unwrap();
    let opened = regex::Regex::new(r"LongOpt\s*\{").unwrap().find_iter(block).count();
    let opts: Vec<TableOpt> = entry
        .captures_iter(block)
        .map(|c| TableOpt {
            name: c[1].to_string(),
            neg: &c[2] == "true",
            requires_arg: &c[3] == "Required",
        })
        .collect();
    (opts.len() == opened && !opts.is_empty() && opts.iter().all(|o| !o.name.is_empty()))
        .then_some(opts)
}

/// A plain `&[&str]` of long-option names. Rendered without a negation half or
/// an `=` suffix: the table says which names resolve and nothing more, and
/// inventing the rest would offer spellings the verb may not take.
fn parse_name_table(block: &str) -> Option<Vec<TableOpt>> {
    let lit = regex::Regex::new(r#""([^"]*)""#).unwrap();
    let names: Vec<&str> = lit.captures_iter(block).map(|c| c.get(1).unwrap().as_str()).collect();
    let ok = regex::Regex::new(r"^[a-z][a-z0-9-]*$").unwrap();
    (!names.is_empty() && names.iter().all(|n| ok.is_match(n))).then(|| {
        names
            .into_iter()
            .map(|n| TableOpt { name: n.to_string(), neg: false, requires_arg: false })
            .collect()
    })
}

/// `pack-objects`'s `OptDef { long, kind, negatable }`, under the same
/// `Kind`→value mapping the file's own `LONG_OPTS` derivation uses: `Bool` has
/// no value, `OptStr` is `PARSE_OPT_OPTARG`, everything else takes one.
fn parse_opt_def_table(block: &str) -> Option<Vec<TableOpt>> {
    let entry = regex::Regex::new(
        r#"OptDef\s*\{\s*long:\s*"([^"]*)"\s*,\s*kind:\s*Kind::(\w+)\s*,\s*negatable:\s*(true|false)\s*,?\s*\}"#,
    )
    .unwrap();
    let opened = regex::Regex::new(r"OptDef\s*\{").unwrap().find_iter(block).count();
    let opts: Vec<TableOpt> = entry
        .captures_iter(block)
        .map(|c| TableOpt {
            name: c[1].to_string(),
            neg: &c[3] == "true",
            requires_arg: !matches!(&c[2], "Bool" | "OptStr"),
        })
        .collect();
    (opts.len() == opened && !opts.is_empty() && opts.iter().all(|o| !o.name.is_empty()))
        .then_some(opts)
}

/// `parse-options.c:show_gitcomp()` (631-679) plus the two
/// `show_negated_gitcomp()` passes (582-629) it calls, which is what a verb
/// prints for `--git-completion-helper`:
///
/// ```c
/// for (; opts->type != OPTION_END; opts++) {
///         ...
///         if (starts_with(opts->long_name, "no-"))
///                 nr_noopts++;
///         printf("%s%s%s%s", opts == original_opts ? "" : " ",
///                prefix, opts->long_name, suffix);
/// }
/// show_negated_gitcomp(original_opts, show_all, -1);
/// show_negated_gitcomp(original_opts, show_all, nr_noopts);
/// ```
///
/// and, in the negated pass:
///
/// ```c
/// if (skip_prefix(opts->long_name, "no-", &name)) {
///         if (nr_noopts < 0)
///                 printf(" --%s", name);
/// } else if (nr_noopts >= 0) {
///         if (nr_noopts && !printed_dashdash) {
///                 printf(" --");
///                 printed_dashdash = 1;
///         }
///         printf(" --no-%s", opts->long_name);
///         nr_noopts++;
/// }
/// ```
///
/// The `nr_noopts++` on the last line is why the bare `--` lands *after* the
/// first `--no-` and not before it whenever the table holds no `no-`-named
/// option of its own: the counter starts at zero, the first negation prints
/// with no separator, and only the second one finds it non-zero. A table with a
/// single negatable option therefore prints no `--` at all (`show-index`).
///
/// `PARSE_OPT_HIDDEN` / `PARSE_OPT_NOCOMPLETE` have no counterpart in
/// [`porcelain::LongOpt`] — it carries what the *resolver* reads, and the
/// resolver resolves a hidden option like any other. The rendering is therefore
/// a superset of stock's by exactly the hidden and no-complete entries, each of
/// which the verb does accept (`git branch --force` works; stock just declines
/// to suggest it).
fn show_gitcomp(table: &[TableOpt]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut nr_noopts = 0usize;
    for o in table {
        out.push(format!("--{}{}", o.name, if o.requires_arg { "=" } else { "" }));
        nr_noopts += usize::from(o.name.starts_with("no-"));
    }
    // `show_negated_gitcomp(…, -1)`: the `no-`-named entries, under their stem.
    for o in table.iter().filter(|o| o.neg) {
        if let Some(stem) = o.name.strip_prefix("no-") {
            out.push(format!("--{stem}"));
        }
    }
    // `show_negated_gitcomp(…, nr_noopts)`: the rest, under `--no-`.
    let (mut n, mut dashdash) = (nr_noopts, false);
    for o in table.iter().filter(|o| o.neg && !o.name.starts_with("no-")) {
        if n != 0 && !dashdash {
            out.push("--".to_string());
            dashdash = true;
        }
        out.push(format!("--no-{}", o.name));
        n += 1;
    }
    out
}

fn dedup_sorted(it: impl Iterator<Item = String>) -> Vec<String> {
    let mut v: Vec<String> = it.collect();
    v.sort();
    v.dedup();
    v
}

/// From the `{` at byte `open`, scan the balanced block and collect the heads of
/// its **depth-1** match arms — a `"word"` literal immediately followed by `=>`
/// or `|`. Depth-1 only, so nested inner matches (value dispatch inside an arm
/// body) don't leak in; string contents (with `\"` escapes) are skipped so their
/// braces/quotes never perturb the depth count.
fn harvest_block_arms(text: &str, open: usize, out: &mut Vec<String>) {
    let b = text.as_bytes();
    let mut depth: i32 = 0;
    let mut i = open;
    while i < b.len() {
        match b[i] {
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    return;
                }
            }
            b'"' => {
                let start = i + 1;
                let mut j = start;
                while j < b.len() {
                    if b[j] == b'\\' {
                        j += 2;
                        continue;
                    }
                    if b[j] == b'"' {
                        break;
                    }
                    j += 1;
                }
                if depth == 1 {
                    let lit = &text[start..j.min(b.len())];
                    let after = text.get(j + 1..).unwrap_or("").trim_start();
                    let before = text.get(..i).unwrap_or("").trim_end();
                    // A subcommand arm head, in either form:
                    //   bare:  `"word" =>` / `"word" |`
                    //   Some:  `Some("word")` (an Option arm, `)` then `=>`/`|`)
                    let bare = after.starts_with("=>") || after.starts_with('|');
                    let some = before.ends_with("Some(") && after.starts_with(')');
                    let word = lit.len() >= 2
                        && lit.as_bytes()[0].is_ascii_lowercase()
                        && lit.bytes().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'-');
                    if word && (bare || some) {
                        out.push(lit.to_string());
                    }
                }
                i = j + 1;
                continue;
            }
            _ => {}
        }
        i += 1;
    }
}

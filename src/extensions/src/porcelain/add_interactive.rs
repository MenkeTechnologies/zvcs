//! The numbered interactive menu — a port of git 2.55.0's `add-interactive.c`.
//!
//! This is `git add -i` and `git commit --interactive`: a main loop offering
//! `status`, `update`, `revert`, `add untracked`, `patch`, `diff`, `quit` and
//! `help`, each of which lists the files it can act on and asks the user to
//! choose from them by number, range, or unique prefix.
//!
//! ### Structure
//!
//! ```text
//!   PrefixItems     — git's `prefix_item_list`: the item strings plus, for each,
//!                     the shortest prefix (1..=4 chars) that identifies it
//!                     uniquely among its sorted neighbours
//!   list_and_choose — the selection prompt: `3`, `3-5`, `2-3,6-9`, `foo`,
//!                     `-2` (deselect), `*` (all), `?` (help), empty (done)
//!   FileItem        — one path with its index-side and worktree-side +N/-M
//! ```
//!
//! The colors, context widths and `interactive.*` settings come from
//! [`super::add_patch::Config`], because git shares one `interactive_config`
//! between the menu and the hunk selector.
//!
//! ### Sub-processes
//!
//! git computes its file lists with in-process `run_diff_index` /
//! `run_diff_files` / `fill_directory` calls and stages with
//! `add_file_to_index`. This port re-executes *this* binary for those, the way
//! [`super::add_patch`] already does for its diffs: `diff-index --cached
//! --numstat -z`, `diff-files --numstat -z`, `ls-files -o --exclude-standard`,
//! `add [-A]` over `:(literal)` pathspecs, and `diff -p --cached` for `diff`.
//! Every one of them is a ported zvcs command, so the numbers and the staging
//! are the ones the rest of the tool would produce.
//!
//! ### Deviations (never faked, always noted)
//!
//! ```text
//!   * git detects unmerged paths from the diffstat's `is_unmerged` flag; this
//!     port reads the conflict stages straight out of the index, which is the
//!     same set of paths.
//!   * git holds `index.lock` across `update`/`revert`/`add untracked`; the
//!     staging children take (and release) that lock one command at a time.
//!   * `repo_refresh_and_write_index` after `revert` is not reproduced: it only
//!     rewrites the stat cache, which is invisible to the logical index state.
//! ```

use anyhow::Result;
use std::io::Write;

use super::add_patch::{color_print, color_println, run_git, Config, Options};
use super::color;

/// git's `prefix_item_list` bounds: a prefix is at least one and at most four
/// characters, or it is not offered at all.
const MIN_PREFIX: usize = 1;
const MAX_PREFIX: usize = 4;

/// `list_and_choose` sentinels.
const CHOOSE_ERROR: isize = -1;
const CHOOSE_QUIT: isize = -2;

// ---------------------------------------------------------------------------
// prefix item lists
// ---------------------------------------------------------------------------

/// A list of items addressed by number *or* by a unique prefix of their name —
/// git's `prefix_item_list`.
struct PrefixItems {
    /// The item names, in display order.
    names: Vec<String>,
    /// Per item, the length of its unique prefix, or `0` when it has none.
    prefix_length: Vec<usize>,
    /// Per item, whether it is currently selected (multi-select mode only).
    selected: Vec<bool>,
    /// Indices into `names`, sorted by name — git's `list->sorted`.
    sorted: Vec<usize>,
}

impl PrefixItems {
    fn new(names: Vec<String>) -> Self {
        let n = names.len();
        Self { names, prefix_length: vec![0; n], selected: vec![false; n], sorted: Vec::new() }
    }

    fn len(&self) -> usize {
        self.names.len()
    }

    /// git's `find_unique_prefixes`: start every item at [`MIN_PREFIX`], then
    /// extend it past whichever of its two sorted neighbours shares it, giving
    /// up (length `0`) at [`MAX_PREFIX`] or at a non-ASCII byte.
    fn find_unique_prefixes(&mut self) {
        if self.sorted.len() == self.names.len() {
            return;
        }
        self.sorted = (0..self.names.len()).collect();
        self.sorted.sort_by(|&a, &b| self.names[a].cmp(&self.names[b]));

        for i in 0..self.sorted.len() {
            let item = self.sorted[i];
            let mut len = 0usize;
            while len < MIN_PREFIX {
                match self.names[item].as_bytes().get(len) {
                    Some(c) if c.is_ascii() => len += 1,
                    _ => {
                        len = 0;
                        break;
                    }
                }
            }
            if i > 0 {
                len = extend_prefix_length(&self.names[item], &self.names[self.sorted[i - 1]], len);
            }
            if i + 1 < self.sorted.len() {
                len = extend_prefix_length(&self.names[item], &self.names[self.sorted[i + 1]], len);
            }
            self.prefix_length[item] = len;
        }
    }

    /// git's `find_unique`: the item `string` addresses, or `None` when it is
    /// ambiguous or matches nothing.
    fn find_unique(&self, string: &str) -> Option<usize> {
        let index = self.sorted.partition_point(|&i| self.names[i].as_str() < string);
        let exact = self.sorted.get(index).is_some_and(|&i| self.names[i] == string);
        if exact {
            return Some(self.sorted[index]);
        }
        // An unambiguous prefix has exactly one neighbour starting with it; git
        // checks both sides of the insertion point and refuses when either the
        // one before or the one after also matches.
        if index > 0 && self.names[self.sorted[index - 1]].starts_with(string) {
            return None;
        }
        if index + 1 < self.sorted.len() && self.names[self.sorted[index + 1]].starts_with(string) {
            return None;
        }
        if index < self.sorted.len() && self.names[self.sorted[index]].starts_with(string) {
            return Some(self.sorted[index]);
        }
        None
    }
}

/// git's `extend_prefix_length`: grow `len` while `name` and `other` still agree
/// on it, and zero it when that would run past the end, past [`MAX_PREFIX`], or
/// into a multi-byte UTF-8 character.
fn extend_prefix_length(name: &str, other: &str, mut len: usize) -> usize {
    let a = name.as_bytes();
    let b = other.as_bytes();
    if len == 0 || a.get(..len) != b.get(..len) {
        return len;
    }
    loop {
        let c = a.get(len).copied().unwrap_or(0);
        len += 1;
        if c == 0 || len > MAX_PREFIX || !c.is_ascii() {
            return 0;
        }
        if Some(&c) != b.get(len - 1) {
            return len;
        }
    }
}

/// git's `is_valid_prefix`: a prefix that `list_and_choose` would read as
/// something else is not offered as a prefix at all.
fn is_valid_prefix(prefix: &str, prefix_len: usize) -> bool {
    let b = prefix.as_bytes();
    prefix_len > 0
        && b[..prefix_len.min(b.len())]
            .iter()
            .all(|c| !matches!(c, b' ' | b'\t' | b'\r' | b'\n' | b','))
        && b.first() != Some(&b'-')
        && !b.first().is_some_and(u8::is_ascii_digit)
        && (prefix_len != 1 || (b[0] != b'*' && b[0] != b'?'))
}

// ---------------------------------------------------------------------------
// the file table
// ---------------------------------------------------------------------------

/// One side (index or worktree) of a path's change — git's `struct adddel`.
#[derive(Default, Clone, Copy)]
struct AddDel {
    add: u64,
    del: u64,
    seen: bool,
    unmerged: bool,
    binary: bool,
}

/// git's `struct file_item`: both sides of one path.
#[derive(Default, Clone, Copy)]
struct FileItem {
    index: AddDel,
    worktree: AddDel,
}

/// Which diffs `get_modified_files` runs, and in which order — git's
/// `enum modified_files_filter`.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Filter {
    /// Both sides, every path either mentions.
    None,
    /// Worktree first; the index pass only fills in paths already listed.
    Worktree,
    /// Index first; the worktree pass only fills in paths already listed.
    Index,
}

/// git's `render_adddel`.
fn render_adddel(ad: &AddDel, no_changes: &str) -> String {
    if ad.binary {
        "binary".to_string()
    } else if ad.seen {
        format!("+{}/-{}", ad.add, ad.del)
    } else {
        no_changes.to_string()
    }
}

// ---------------------------------------------------------------------------
// the menu state
// ---------------------------------------------------------------------------

/// git's `struct add_i_state` plus the display data `run_add_i` keeps on its
/// stack: the prompt-highlight pair and the current file table.
struct State<'a> {
    repo: &'a gix::Repository,
    cfg: Config,
    /// The pathspec `add -i` was invoked with, narrowing every listing.
    pathspecs: Vec<String>,
    /// `HEAD`'s object id, or `None` before the first commit.
    head: Option<gix::ObjectId>,
    /// The per-item highlight pair: the prompt color when color is on, else
    /// git's plain `[`/`]` brackets.
    color: String,
    reset: String,
    /// The file table shared by every command, and its `+N/-M` metadata.
    files: PrefixItems,
    meta: Vec<FileItem>,
    /// Set while `add untracked` is listing, which prints names only.
    only_names: bool,
}

impl State<'_> {
    /// The tree-ish the index is compared against: `HEAD` or the empty tree.
    fn base(&self) -> String {
        match &self.head {
            Some(id) => id.to_string(),
            None => gix::ObjectId::empty_tree(self.repo.object_hash()).to_string(),
        }
    }

    /// git's `error()` in the interactive color.
    fn err(&self, msg: &str) {
        eprintln!("{}{}{}", self.cfg.error_color, msg, self.cfg.reset_color_interactive);
    }

    // -----------------------------------------------------------------------
    // listing
    // -----------------------------------------------------------------------

    /// git's `list()`: the header, then every item, `columns` per line.
    fn list(&self, show_selection: bool, columns: usize, header: Option<&str>) {
        if self.files.len() == 0 {
            return;
        }
        if let Some(h) = header {
            color_println(&self.cfg.header_color, h);
        }
        let mut last_lf = false;
        for i in 0..self.files.len() {
            self.print_file_item(i, show_selection && self.files.selected[i]);
            if columns != 0 && (i + 1) % columns != 0 {
                print!("\t");
                last_lf = false;
            } else {
                println!();
                last_lf = true;
            }
        }
        if !last_lf {
            println!();
        }
    }

    /// The item name with its unique prefix wrapped in the highlight pair, or
    /// plain when it has none — the shared half of git's `print_file_item` and
    /// `print_command_item`.
    fn highlighted(&self, i: usize) -> String {
        let name = &self.files.names[i];
        let len = self.files.prefix_length[i];
        if len > 0 && is_valid_prefix(name, len) {
            format!("{}{}{}{}", self.color, &name[..len], self.reset, &name[len..])
        } else {
            name.clone()
        }
    }

    /// git's `print_file_item`.
    fn print_file_item(&self, i: usize, selected: bool) {
        let marker = if selected { '*' } else { ' ' };
        let name = self.highlighted(i);
        if self.only_names {
            print!("{marker}{:2}: {name}", i + 1);
            return;
        }
        let m = &self.meta[i];
        print!(
            "{marker}{:2}: {:>12} {:>12} {name}",
            i + 1,
            render_adddel(&m.index, "unchanged"),
            render_adddel(&m.worktree, "nothing"),
        );
    }

    // -----------------------------------------------------------------------
    // selection
    // -----------------------------------------------------------------------

    /// git's `list_and_choose`. In `singleton` mode the return value is the
    /// chosen index; otherwise it is the number of selected items, with the
    /// selection itself left in `self.files.selected`.
    fn list_and_choose(
        &mut self,
        prompt: &str,
        singleton: bool,
        immediate: bool,
        columns: usize,
        header: Option<&str>,
        print_help: fn(&Self),
    ) -> isize {
        let mut res: isize = if singleton { CHOOSE_ERROR } else { 0 };
        if !singleton {
            self.files.selected = vec![false; self.files.len()];
        }
        self.files.find_unique_prefixes();

        loop {
            self.list(!singleton, columns, header);
            color_print(&self.cfg.prompt_color.clone(), prompt);
            print!("{}", if singleton { "> " } else { ">> " });
            let _ = std::io::stdout().flush();

            let Some(input) = read_line_interactively() else {
                println!();
                if immediate {
                    res = CHOOSE_QUIT;
                }
                break;
            };
            if input.is_empty() {
                break;
            }
            if input == "?" {
                print_help(self);
                continue;
            }

            // One pass over the comma/whitespace-separated tokens.
            let mut rest = input.as_str();
            loop {
                let sep = rest.find([' ', '\t', '\r', '\n', ',']).unwrap_or(rest.len());
                if sep == 0 {
                    if rest.is_empty() {
                        break;
                    }
                    rest = &rest[1..];
                    continue;
                }
                // The separator, when there is one, is consumed with the token.
                let consumed = if sep < rest.len() { sep + 1 } else { sep };
                let (mut token, mut span) = (rest, sep);
                let choose = if token.starts_with('-') {
                    token = &token[1..];
                    span -= 1;
                    false
                } else {
                    true
                };
                let word = &token[..span];

                // `*`, a number, or a `from-`/`from-to` range; anything else is
                // looked up as a unique prefix below.
                let (mut from, mut to): (isize, isize) = (-1, -1);
                if word == "*" {
                    from = 0;
                    to = self.files.len() as isize;
                } else if word.as_bytes().first().is_some_and(u8::is_ascii_digit) {
                    let digits = word.find(|c: char| !c.is_ascii_digit()).unwrap_or(word.len());
                    from = word[..digits].parse::<isize>().unwrap_or(0) - 1;
                    let tail = &word[digits..];
                    if tail.is_empty() {
                        to = from + 1;
                    } else if let Some(after) = tail.strip_prefix('-') {
                        let d2 = after.find(|c: char| !c.is_ascii_digit()).unwrap_or(after.len());
                        to = if d2 > 0 {
                            after[..d2].parse::<isize>().unwrap_or(0)
                        } else {
                            self.files.len() as isize
                        };
                        // Trailing junk after the range invalidates the whole token.
                        if d2 != after.len() {
                            from = -1;
                        }
                    }
                }
                if from < 0 {
                    match self.files.find_unique(word) {
                        Some(i) => {
                            from = i as isize;
                            to = from + 1;
                        }
                        None => from = -1,
                    }
                }

                if from < 0 || from >= self.files.len() as isize || (singleton && from + 1 != to) {
                    let msg = format!("Huh ({word})?");
                    self.err(&msg);
                    break;
                }
                if singleton {
                    res = from;
                    break;
                }
                if to > self.files.len() as isize {
                    to = self.files.len() as isize;
                }
                for i in from..to {
                    if self.files.selected[i as usize] != choose {
                        self.files.selected[i as usize] = choose;
                        res += if choose { 1 } else { -1 };
                    }
                }
                rest = &rest[consumed..];
            }

            if (immediate && res != CHOOSE_ERROR) || input == "*" {
                break;
            }
        }
        res
    }

    /// The names of everything currently selected.
    fn chosen(&self) -> Vec<String> {
        self.files
            .names
            .iter()
            .enumerate()
            .filter(|(i, _)| self.files.selected[*i])
            .map(|(_, n)| n.clone())
            .collect()
    }

    // -----------------------------------------------------------------------
    // collecting the file table
    // -----------------------------------------------------------------------

    /// git's `get_modified_files`: two `--numstat` diffs (index-vs-`HEAD` and
    /// worktree-vs-index) merged into one path-keyed table, sorted by path.
    fn get_modified_files(&mut self, filter: Filter) -> Result<(usize, usize)> {
        let mut table: Vec<(String, FileItem)> = Vec::new();
        let mut unmerged_count = 0usize;
        let mut binary_count = 0usize;
        let unmerged = self.unmerged_paths()?;

        for pass in 0..2 {
            let from_index = match filter {
                Filter::Index => pass == 0,
                _ => pass == 1,
            };
            let skip_unseen = filter != Filter::None && pass == 1;

            for (path, add, del, binary) in self.numstat(from_index)? {
                let idx = match table.iter().position(|(p, _)| *p == path) {
                    Some(i) => i,
                    None => {
                        if skip_unseen {
                            continue;
                        }
                        table.push((path, FileItem::default()));
                        table.len() - 1
                    }
                };
                // git's `other_adddel`: the far side of the same path decides
                // whether this binary/unmerged path has been counted already.
                let is_unmerged = unmerged.contains(&table[idx].0);
                let item = &mut table[idx].1;
                let other = if from_index { item.worktree } else { item.index };
                let side = if from_index { &mut item.index } else { &mut item.worktree };
                side.seen = true;
                side.add = add;
                side.del = del;
                if binary {
                    if !other.binary {
                        binary_count += 1;
                    }
                    side.binary = true;
                }
                if is_unmerged {
                    if !other.unmerged {
                        unmerged_count += 1;
                    }
                    side.unmerged = true;
                }
            }
        }

        // Two diffs were merged, so the result has to be re-sorted.
        table.sort_by(|a, b| a.0.cmp(&b.0));
        self.meta = table.iter().map(|(_, m)| *m).collect();
        self.files = PrefixItems::new(table.into_iter().map(|(p, _)| p).collect());
        self.only_names = false;
        Ok((unmerged_count, binary_count))
    }

    /// One `--numstat -z` diff as `(path, added, deleted, is_binary)`.
    fn numstat(&self, from_index: bool) -> Result<Vec<(String, u64, u64, bool)>> {
        let mut args: Vec<String> = if from_index {
            vec!["diff-index".into(), "--cached".into(), "--numstat".into(), "-z".into(), self.base()]
        } else {
            vec![
                "diff-files".into(),
                "--numstat".into(),
                "-z".into(),
                "--ignore-submodules=dirty".into(),
            ]
        };
        if !self.pathspecs.is_empty() {
            args.push("--".into());
            args.extend(self.pathspecs.iter().cloned());
        }
        let (ok, out) = run_git(&args, None, true, None)?;
        if !ok {
            crate::git_fatal!("could not read index");
        }
        let mut result = Vec::new();
        for record in out.split(|&b| b == 0) {
            if record.is_empty() {
                continue;
            }
            let text = String::from_utf8_lossy(record);
            let mut fields = text.splitn(3, '\t');
            let (Some(a), Some(d), Some(path)) = (fields.next(), fields.next(), fields.next())
            else {
                continue;
            };
            let binary = a == "-" || d == "-";
            result.push((
                path.to_string(),
                a.parse().unwrap_or(0),
                d.parse().unwrap_or(0),
                binary,
            ));
        }
        Ok(result)
    }

    /// The paths with conflict stages in the index — git reads the same set out
    /// of the diffstat's `is_unmerged` flag.
    fn unmerged_paths(&self) -> Result<std::collections::HashSet<String>> {
        let mut set = std::collections::HashSet::new();
        if !self.repo.index_path().exists() {
            return Ok(set);
        }
        let index = self.repo.open_index()?;
        let backing = index.path_backing();
        for entry in index.entries() {
            if entry.stage() != gix::index::entry::Stage::Unconflicted {
                set.insert(entry.path_in(backing).to_string());
            }
        }
        Ok(set)
    }

    /// git's `get_untracked_files`, as `ls-files --others --exclude-standard`.
    fn get_untracked_files(&mut self) -> Result<()> {
        let mut args: Vec<String> = vec![
            "ls-files".into(),
            "--others".into(),
            "--exclude-standard".into(),
            "-z".into(),
        ];
        if !self.pathspecs.is_empty() {
            args.push("--".into());
            args.extend(self.pathspecs.iter().cloned());
        }
        let (ok, out) = run_git(&args, None, true, None)?;
        if !ok {
            crate::git_fatal!("could not read index");
        }
        let names: Vec<String> = out
            .split(|&b| b == 0)
            .filter(|r| !r.is_empty())
            .map(|r| String::from_utf8_lossy(r).into_owned())
            .collect();
        self.meta = vec![FileItem::default(); names.len()];
        self.files = PrefixItems::new(names);
        Ok(())
    }

    // -----------------------------------------------------------------------
    // the commands
    // -----------------------------------------------------------------------

    /// git's `run_status`.
    fn cmd_status(&mut self, header: &str) -> i32 {
        if self.get_modified_files(Filter::None).is_err() {
            self.err("could not read index");
            return -1;
        }
        self.list(false, 0, Some(header));
        println!();
        0
    }

    /// git's `run_update`: stage the worktree state of the chosen paths.
    fn cmd_update(&mut self, header: &str) -> i32 {
        if self.get_modified_files(Filter::Worktree).is_err() {
            self.err("could not read index");
            return -1;
        }
        if self.files.len() == 0 {
            println!();
            return 0;
        }
        let count = self.list_and_choose("Update", false, false, 0, Some(header), choose_prompt_help);
        if count <= 0 {
            println!();
            return 0;
        }
        // `add_file_to_index` for a path that still exists, `remove_file_from_index`
        // for one that does not — which is exactly what `add -A` does over a
        // literal pathspec (literal, because git stages the names verbatim).
        let mut args: Vec<String> = vec!["add".into(), "-A".into(), "--".into()];
        args.extend(self.chosen().iter().map(|p| format!(":(literal){p}")));
        let res = match run_git(&args, None, false, None) {
            Ok((true, _)) => 0,
            _ => {
                self.err("could not write index");
                -1
            }
        };
        if res == 0 {
            println!("updated {count} path{}", plural(count));
        }
        println!();
        res
    }

    /// git's `run_revert`: reset the chosen paths' index entries to `HEAD`.
    fn cmd_revert(&mut self, header: &str) -> i32 {
        if self.get_modified_files(Filter::Index).is_err() {
            self.err("could not read index");
            return -1;
        }
        if self.files.len() == 0 {
            println!();
            return 0;
        }
        let count = self.list_and_choose("Revert", false, false, 0, Some(header), choose_prompt_help);
        if count <= 0 {
            println!();
            return 0;
        }
        let res = match self.revert_paths(&self.chosen()) {
            Ok(()) => 0,
            Err(e) => {
                self.err(&e.to_string());
                -1
            }
        };
        if res == 0 {
            println!("reverted {count} path{}", plural(count));
        }
        println!();
        res
    }

    /// The index half of `run_revert`'s `revert_from_diff` callback: every
    /// chosen path takes `HEAD`'s mode and blob back, or leaves the index
    /// entirely when `HEAD` does not have it.
    fn revert_paths(&self, paths: &[String]) -> Result<()> {
        use gix::bstr::{BStr, BString};
        let mut index = if self.repo.index_path().exists() {
            self.repo.open_index()?
        } else {
            gix::index::File::from_state(
                gix::index::State::new(self.repo.object_hash()),
                self.repo.index_path(),
            )
        };
        // `HEAD`'s tree flattened into path -> (mode, id), which is what
        // `do_diff_cache` walks in git.
        let base = match &self.head {
            Some(id) => {
                let tree = self.repo.find_commit(*id)?.tree_id()?.detach();
                let head_index = self.repo.index_from_tree(&tree)?;
                let backing = head_index.path_backing();
                head_index
                    .entries()
                    .iter()
                    .map(|e| (e.path_in(backing).to_owned(), (e.mode, e.id)))
                    .collect::<std::collections::HashMap<BString, _>>()
            }
            None => std::collections::HashMap::new(),
        };

        let chosen: std::collections::HashSet<BString> =
            paths.iter().map(|p| BString::from(p.as_str())).collect();
        index.remove_entries(|_, path, _| chosen.contains(&path.to_owned()));
        for path in paths {
            let key = BString::from(path.as_str());
            match base.get(&key) {
                Some((mode, id)) => index.dangerously_push_entry(
                    gix::index::entry::Stat::default(),
                    *id,
                    gix::index::entry::Flags::empty(),
                    *mode,
                    BStr::new(path.as_str()),
                ),
                // Not in `HEAD` at all: git drops the entry and says so.
                None => println!("note: {path} is untracked now."),
            }
        }
        index.sort_entries();
        // Every entry above was replaced with `HEAD`'s version or dropped, so any
        // cached tree id for the directories they live in now describes content the
        // index no longer holds. git invalidates per entry from inside
        // `add_index_entry_with_check()` (read-cache.c:1273-1274); dropping the whole
        // extension is the conservative equivalent — it costs the next `write-tree` a
        // recomputation and cannot leave a stale node behind.
        index.remove_tree();
        // The interactive `revert` command writes the real index; options come
        // from the repository as they do for every writer (read-cache.c:2830-2831).
        crate::index_racy::write(self.repo, &mut index)?;
        Ok(())
    }

    /// git's `run_add_untracked`.
    fn cmd_add_untracked(&mut self, header: &str) -> i32 {
        if self.get_untracked_files().is_err() {
            self.err("could not read index");
            return -1;
        }
        if self.files.len() == 0 {
            println!("No untracked files.");
            println!();
            return 0;
        }
        self.only_names = true;
        let count =
            self.list_and_choose("Add untracked", false, false, 0, Some(header), choose_prompt_help);
        self.only_names = false;
        if count <= 0 {
            println!();
            return 0;
        }
        // git's `add_file_to_index` over the names it just listed, verbatim.
        let mut args: Vec<String> = vec!["add".into(), "--".into()];
        args.extend(self.chosen().iter().map(|p| format!(":(literal){p}")));
        let res = match run_git(&args, None, false, None) {
            Ok((true, _)) => 0,
            _ => {
                self.err("could not write index");
                -1
            }
        };
        if res == 0 {
            println!("added {count} path{}", plural(count));
        }
        println!();
        res
    }

    /// git's `run_patch`: drop binary and unmerged paths, then hand the chosen
    /// ones to the hunk selector.
    fn cmd_patch(&mut self, header: &str) -> i32 {
        let (unmerged_count, binary_count) = match self.get_modified_files(Filter::Worktree) {
            Ok(counts) => counts,
            Err(_) => {
                self.err("could not read index");
                return -1;
            }
        };
        if unmerged_count > 0 || binary_count > 0 {
            let mut names = Vec::new();
            let mut meta = Vec::new();
            for (i, name) in std::mem::take(&mut self.files.names).into_iter().enumerate() {
                let m = self.meta[i];
                if m.index.binary || m.worktree.binary {
                    continue;
                }
                if m.index.unmerged || m.worktree.unmerged {
                    let msg = format!("ignoring unmerged: {name}");
                    self.err(&msg);
                    continue;
                }
                names.push(name);
                meta.push(m);
            }
            self.meta = meta;
            self.files = PrefixItems::new(names);
        }
        if self.files.len() == 0 {
            if binary_count > 0 {
                eprintln!("Only binary files changed.");
            } else {
                eprintln!("No changes.");
            }
            return 0;
        }

        let count =
            self.list_and_choose("Patch update", false, false, 0, Some(header), choose_prompt_help);
        if count <= 0 {
            return 0;
        }
        let opts = Options {
            context: self.cfg.context,
            interhunk: self.cfg.interhunk,
            auto_advance: self.cfg.auto_advance,
            disallow_edit: false,
        };
        match super::add_patch::run_status(
            self.repo,
            super::add_patch::Mode::Add,
            None,
            opts,
            &self.chosen(),
        ) {
            Ok(0) => 0,
            Ok(_) | Err(_) => -1,
        }
    }

    /// git's `run_diff`: `git diff -p --cached <base> -- <chosen>`.
    fn cmd_diff(&mut self, header: &str) -> i32 {
        if self.get_modified_files(Filter::Index).is_err() {
            self.err("could not read index");
            return -1;
        }
        if self.files.len() == 0 {
            println!();
            return 0;
        }
        let count =
            self.list_and_choose("Review diff", false, true, 0, Some(header), choose_prompt_help);
        let mut res = 0;
        if count > 0 {
            let mut args: Vec<String> =
                vec!["diff".into(), "-p".into(), "--cached".into()];
            if self.cfg.context != -1 {
                args.push(format!("--unified={}", self.cfg.context));
            }
            if self.cfg.interhunk != -1 {
                args.push(format!("--inter-hunk-context={}", self.cfg.interhunk));
            }
            args.push(self.base());
            args.push("--".into());
            args.extend(self.chosen());
            if !matches!(run_git(&args, None, false, None), Ok((true, _))) {
                res = -1;
            }
        }
        println!();
        res
    }

    /// git's `run_help`.
    fn cmd_help(&mut self) -> i32 {
        let c = self.cfg.help_color.clone();
        color_println(&c, "status        - show paths with changes");
        color_println(&c, "update        - add working tree state to the staged set of changes");
        color_println(&c, "revert        - revert staged set of changes back to the HEAD version");
        color_println(&c, "patch         - pick hunks and update selectively");
        color_println(&c, "diff          - view diff between HEAD and index");
        color_println(
            &c,
            "add untracked - add contents of untracked files to the staged set of changes",
        );
        0
    }
}

/// git's `choose_prompt_help`, shown by `?` at a file-selection prompt.
fn choose_prompt_help(s: &State<'_>) {
    let c = s.cfg.help_color.clone();
    color_println(&c, "Prompt help:");
    color_println(&c, "1          - select a single item");
    color_println(&c, "3-5        - select a range of items");
    color_println(&c, "2-3,6-9    - select multiple ranges");
    color_println(&c, "foo        - select item based on unique prefix");
    color_println(&c, "-...       - unselect specified items");
    color_println(&c, "*          - choose all items");
    color_println(&c, "           - (empty) finish selecting");
}

/// git's `command_prompt_help`, shown by `?` at the main menu.
fn command_prompt_help(s: &State<'_>) {
    let c = s.cfg.help_color.clone();
    color_println(&c, "Prompt help:");
    color_println(&c, "1          - select a numbered item");
    color_println(&c, "foo        - select item based on unique prefix");
    color_println(&c, "           - (empty) select nothing");
}

/// git's `Q_()` plural for the `updated %d path` / `added %d path` lines.
fn plural(n: isize) -> &'static str {
    if n == 1 {
        ""
    } else {
        "s"
    }
}

/// git's `git_read_line_interactively`: one line without its trailing newline
/// (and one optional CR). `None` is EOF.
fn read_line_interactively() -> Option<String> {
    let _ = std::io::stdout().flush();
    let mut line = String::new();
    match std::io::stdin().read_line(&mut line) {
        Ok(0) | Err(_) => None,
        Ok(_) => {
            if line.ends_with('\n') {
                line.pop();
                if line.ends_with('\r') {
                    line.pop();
                }
            }
            Some(line)
        }
    }
}

// ---------------------------------------------------------------------------
// entry point
// ---------------------------------------------------------------------------

/// The eight main-menu entries, in git's `command_list` order.
const COMMANDS: [&str; 8] =
    ["status", "update", "revert", "add untracked", "patch", "diff", "quit", "help"];

/// Run the numbered interactive menu — git's `run_add_i`, as its raw status.
pub(crate) fn run_status(
    repo: &gix::Repository,
    opts: Options,
    pathspecs: &[String],
) -> Result<u8> {
    let cfg = match Config::init(repo, &opts) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("fatal: {e}");
            return Ok(128);
        }
    };
    // With color on, the unique prefix is painted in the prompt color; with it
    // off, git falls back to wrapping the prefix in square brackets.
    let (color, reset) = if color::want_color_stdout(repo, "interactive") {
        (cfg.prompt_color.clone(), cfg.reset_color_interactive.clone())
    } else {
        ("[".to_string(), "]".to_string())
    };
    let head = repo.head_id().ok().map(|id| id.detach());
    let header = format!("     {:>12} {:>12} {}", "staged", "unstaged", "path");

    let mut state = State {
        repo,
        cfg,
        pathspecs: pathspecs.to_vec(),
        head,
        color,
        reset,
        files: PrefixItems::new(Vec::new()),
        meta: Vec::new(),
        only_names: false,
    };

    let mut res = state.cmd_status(&header);

    // The main loop runs over its own item list (the command names), so the file
    // table is swapped out for it and restored by the next command that fills it.
    loop {
        let files = std::mem::replace(
            &mut state.files,
            PrefixItems::new(COMMANDS.iter().map(|s| s.to_string()).collect()),
        );
        let meta = std::mem::take(&mut state.meta);
        state.meta = vec![FileItem::default(); COMMANDS.len()];
        state.only_names = true;
        let choice = state.list_and_choose(
            "What now",
            true,
            true,
            4,
            Some("*** Commands ***"),
            command_prompt_help,
        );
        state.files = files;
        state.meta = meta;
        state.only_names = false;

        // `quit` and EOF both leave; an unrecognised answer re-lists the menu.
        if choice == CHOOSE_QUIT || COMMANDS.get(choice as usize) == Some(&"quit") {
            println!("Bye.");
            res = 0;
            break;
        }
        res = match COMMANDS.get(choice as usize) {
            Some(&"status") => state.cmd_status(&header),
            Some(&"update") => state.cmd_update(&header),
            Some(&"revert") => state.cmd_revert(&header),
            Some(&"add untracked") => state.cmd_add_untracked(&header),
            Some(&"patch") => state.cmd_patch(&header),
            Some(&"diff") => state.cmd_diff(&header),
            Some(&"help") => state.cmd_help(),
            _ => res,
        };
    }

    Ok(if res == 0 { 0 } else { 1 })
}

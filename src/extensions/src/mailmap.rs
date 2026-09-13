//! git's `mailmap.c` (v2.55.0): reading `.mailmap` sources and mapping an
//! identity through them — the one implementation every consumer shares
//! (`check-mailmap`, `log`/`show`/`rev-list` pretty formats, `shortlog`, `blame`,
//! `cat-file --use-mailmap`, `for-each-ref`'s `:mailmap` atoms, `request-pull`,
//! `range-diff`, `commit --author`).
//!
//! The structure is git's: a string list of old emails compared with
//! `strcasecmp()` (`namemap_cmp`, mailmap.c:56-59), each carrying the simple
//! replacement plus a second case-insensitive list keyed by old name. Keys are
//! stored ASCII-lowercased, which orders and matches exactly as `strcasecmp()`
//! does in the C locale.
//!
//! Line parsing is `read_mailmap_line()`/`parse_name_and_email()` byte for byte:
//! the name is trimmed, the address inside `<...>` is not, text after the second
//! address is ignored, and an unqualified line only overrides the fields it
//! carries (`add_mapping()`), so `<new@> <old@>` changes the address of an
//! earlier `Name <x@> <old@>` and keeps its name.

use std::collections::BTreeMap;
use std::io::Read;
use std::path::{Path, PathBuf};

use gix::bstr::ByteSlice;

/// `struct mailmap_info` (mailmap.c:12-15): the replacement a match supplies.
#[derive(Default)]
struct Info {
    name: Option<Vec<u8>>,
    email: Option<Vec<u8>>,
}

/// `struct mailmap_entry` (mailmap.c:17-24): the simple mapping for one old
/// email and the name-qualified ones under it.
#[derive(Default)]
struct Entry {
    simple: Info,
    namemap: BTreeMap<Vec<u8>, Info>,
}

/// A read mailmap: git's `struct string_list` of [`Entry`] keyed by old email.
#[derive(Default)]
pub struct Mailmap {
    map: BTreeMap<Vec<u8>, Entry>,
}

/// The `strcasecmp()` key: ASCII-lowercased bytes.
fn fold(s: &[u8]) -> Vec<u8> {
    s.to_ascii_lowercase()
}

/// `isspace()` in the C locale.
fn is_space(b: u8) -> bool {
    matches!(b, b' ' | b'\t' | b'\n' | 0x0b | 0x0c | b'\r')
}

/// A C string view of `buf`: everything before its first NUL.
fn c_str(buf: &[u8]) -> &[u8] {
    buf.find_byte(0).map_or(buf, |nul| &buf[..nul])
}

/// `parse_name_and_email()` (mailmap.c:102-129): `(name, email, rest)`, with
/// `email` `None` when the line carries no acceptable `<...>`. `rest` is what
/// follows the `>`, `None` when nothing does.
fn parse_name_and_email(
    buffer: &[u8],
    allow_empty_email: bool,
) -> (Option<&[u8]>, Option<&[u8]>, Option<&[u8]>) {
    let Some(left) = buffer.find_byte(b'<') else {
        return (None, None, None);
    };
    let Some(right) = buffer[left + 1..].find_byte(b'>').map(|at| left + 1 + at) else {
        return (None, None, None);
    };
    if !allow_empty_email && left + 1 == right {
        return (None, None, None);
    }

    // `while (isspace(*nstart) && nstart < left) ++nstart;` and
    // `nend = left-1; while (nend > nstart && isspace(*nend)) --nend;`
    let mut nstart = 0usize;
    while nstart < left && is_space(buffer[nstart]) {
        nstart += 1;
    }
    let mut nend = left as isize - 1;
    while nend > nstart as isize && is_space(buffer[nend as usize]) {
        nend -= 1;
    }
    let name = (nstart as isize <= nend).then(|| &buffer[nstart..=nend as usize]);
    let email = &buffer[left + 1..right];
    let rest = &buffer[right + 1..];
    (name, Some(email), (!rest.is_empty()).then_some(rest))
}

impl Mailmap {
    /// Whether no mapping was read — `map->nr == 0`.
    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }

    /// `add_mapping()` (mailmap.c:61-100).
    fn add_mapping(
        &mut self,
        new_name: Option<&[u8]>,
        new_email: Option<&[u8]>,
        old_name: Option<&[u8]>,
        old_email: Option<&[u8]>,
    ) {
        let (new_email, old_email) = match old_email {
            Some(old) => (new_email, old),
            None => match new_email {
                Some(only) => (None, only),
                None => return,
            },
        };
        let me = self.map.entry(fold(old_email)).or_default();
        match old_name {
            None => {
                // Replace current name and new email for simple entry.
                if let Some(name) = new_name {
                    me.simple.name = Some(name.to_vec());
                }
                if let Some(email) = new_email {
                    me.simple.email = Some(email.to_vec());
                }
            }
            Some(old_name) => {
                me.namemap.insert(
                    fold(old_name),
                    Info {
                        name: new_name.map(<[u8]>::to_vec),
                        email: new_email.map(<[u8]>::to_vec),
                    },
                );
            }
        }
    }

    /// `read_mailmap_line()` (mailmap.c:131-143) over one C-string line.
    fn read_line(&mut self, buffer: &[u8]) {
        if buffer.first() == Some(&b'#') {
            return;
        }
        let (name1, email1, rest) = parse_name_and_email(buffer, false);
        let (name2, email2) = match rest {
            Some(rest) => {
                let (name2, email2, _) = parse_name_and_email(rest, true);
                (name2, email2)
            }
            None => (None, None),
        };
        if email1.is_some() {
            self.add_mapping(name1, email1, name2, email2);
        }
    }

    /// `read_mailmap_string()` (mailmap.c:173-184): the buffer as a C string,
    /// one mapping per `\n`-separated line.
    pub fn read_string(&mut self, buf: &[u8]) {
        let mut buf = c_str(buf);
        while !buf.is_empty() {
            let (line, next) = match buf.find_byte(b'\n') {
                Some(nl) => (&buf[..nl], &buf[nl + 1..]),
                None => (buf, &b""[..]),
            };
            self.read_line(line);
            buf = next;
        }
    }

    /// `read_mailmap_file()` (mailmap.c:145-171). `display` is the name git
    /// reports in its error, `path` where the file is opened from.
    fn read_file_at(&mut self, display: &str, path: &Path, nofollow: bool) {
        let mut options = std::fs::OpenOptions::new();
        options.read(true);
        if nofollow {
            use std::os::unix::fs::OpenOptionsExt;
            options.custom_flags(libc::O_NOFOLLOW);
        }
        let mut file = match options.open(path) {
            Ok(file) => file,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return,
            Err(e) => {
                eprintln!(
                    "error: unable to open mailmap at {display}: {}",
                    crate::external::strerror(&e)
                );
                return;
            }
        };
        // `fgets()` stops at a read error with whatever it had; a directory
        // opens and then reads nothing.
        let mut data = Vec::new();
        let _ = file.read_to_end(&mut data);

        // `while (fgets(buffer, sizeof(buffer), f) != NULL)` with a 1024-byte
        // buffer: each record ends after a `\n` or 1023 bytes, and is then read
        // as a C string.
        let mut rest = &data[..];
        while !rest.is_empty() {
            let limit = rest.len().min(1023);
            let take = rest[..limit].find_byte(b'\n').map_or(limit, |nl| nl + 1);
            self.read_line(c_str(&rest[..take]));
            rest = &rest[take..];
        }
    }

    /// `read_mailmap_file(map, filename, 0)` for a path named on the command
    /// line (`check-mailmap --mailmap-file`), opened as given.
    pub fn read_file(&mut self, filename: &str) {
        self.read_file_at(filename, Path::new(filename), false);
    }

    /// `read_mailmap_blob()` (mailmap.c:186-212).
    pub fn read_blob(&mut self, repo: &gix::Repository, name: &str) {
        let Some(oid) = crate::objname::resolve(repo, name) else {
            return;
        };
        let Ok(object) = repo.find_object(oid) else {
            eprintln!("error: unable to read mailmap object at {name}");
            return;
        };
        if object.kind != gix::object::Kind::Blob {
            eprintln!("error: mailmap is not a blob: {name}");
            return;
        }
        self.read_string(&object.data);
    }

    /// `read_mailmap()` (mailmap.c:214-243): the worktree `.mailmap`, then
    /// `mailmap.blob` (`HEAD:.mailmap` by default in a bare repository), then
    /// `mailmap.file`. `repo` is `None` outside a repository, where only the
    /// current directory's `.mailmap` and the global `mailmap.file` apply.
    ///
    /// git has already moved to the top of the work tree by the time it reads
    /// these, so relative names resolve from there ([`crate::setup::setup_cwd`]);
    /// from inside a `.git` directory, or a bare repository, they resolve from
    /// the directory the command was started in.
    pub fn read(repo: Option<&gix::Repository>) -> Mailmap {
        let mut map = Mailmap::default();

        let cwd: PathBuf = match repo {
            Some(repo) => crate::setup::setup_cwd(repo),
            None => std::env::current_dir().ok(),
        }
        .unwrap_or_default();
        // `repo_config_get_pathname(repo, "mailmap.file", …)` then
        // `repo_config_get_string(repo, "mailmap.blob", …)` (mailmap.c:216-217):
        // a valueless key dies through `git_die_config()`, the file first.
        let mailmap_file = crate::config::config_get_pathname(repo, "mailmap.file", &cwd)
            .map(|path| path.to_string_lossy().into_owned());
        let mailmap_blob = crate::config::config_get_string(repo, "mailmap.blob");

        // `is_bare_repository()` (environment.c:131-135): `core.bare` not false,
        // and no work tree.
        let bare = repo.is_some_and(|repo| {
            repo.workdir().is_none()
                && repo.config_snapshot().boolean("core.bare").unwrap_or(true)
        });
        let mailmap_blob = match mailmap_blob {
            None if bare => Some("HEAD:.mailmap".to_string()),
            other => other,
        };

        if repo.is_none() || !bare {
            map.read_file_at(".mailmap", &cwd.join(".mailmap"), repo.is_some());
        }
        if let (Some(repo), Some(blob)) = (repo, &mailmap_blob) {
            map.read_blob(repo, blob);
        }
        if let Some(file) = &mailmap_file {
            map.read_file_at(file, &cwd.join(file), false);
        }
        map
    }

    /// The mailmap `%aN`/`%aE`/`%aL` (and `%gN`/`%gE`) read: `mailmap_name()`
    /// (pretty.c:777-785) keeps its own `static` list, loaded on first use and
    /// independent of `--use-mailmap`/`log.mailmap`.
    pub fn for_pretty(repo: &gix::Repository) -> &'static Mailmap {
        static PRETTY: std::sync::OnceLock<Mailmap> = std::sync::OnceLock::new();
        PRETTY.get_or_init(|| Mailmap::read(Some(repo)))
    }

    /// `map_user()` (mailmap.c:300-336): replace `email` and/or `name` with the
    /// mapping for them, answering whether one applied. A matching entry with
    /// neither a name nor an email is no match.
    pub fn map_user<'a>(&'a self, email: &mut &'a [u8], name: &mut &'a [u8]) -> bool {
        let Some(me) = self.map.get(&fold(email)) else {
            return false;
        };
        let info = if me.namemap.is_empty() {
            &me.simple
        } else {
            // Look up on name too; if the name is not found, the simple entry.
            me.namemap.get(&fold(name)).unwrap_or(&me.simple)
        };
        if info.name.is_none() && info.email.is_none() {
            return false;
        }
        if let Some(mapped) = &info.email {
            *email = mapped;
        }
        if let Some(mapped) = &info.name {
            *name = mapped;
        }
        true
    }

    /// The identity after [`Mailmap::map_user`], as owned `(name, email)`.
    pub fn mapped(&self, name: &[u8], email: &[u8]) -> (Vec<u8>, Vec<u8>) {
        let (mut name, mut email) = (name, email);
        self.map_user(&mut email, &mut name);
        (name.to_vec(), email.to_vec())
    }
}

/// `split_ident_line()` (ident.c:275-309) reduced to what `rewrite_ident_line()`
/// uses: the name span (from the start of the line to its last non-space byte
/// before `<`) and the mail span. `None` is git's `-1`.
fn split_ident_spans(line: &[u8]) -> Option<(usize, usize, usize)> {
    let lt = c_str(line).find_byte(b'<')?;
    let mail_begin = lt + 1;
    let name_end = line[..lt].iter().rposition(|&b| !is_space(b)).map_or(0, |at| at + 1);
    let mail_end = mail_begin + line[mail_begin..].find_byte(b'>')?;
    Some((name_end, mail_begin, mail_end))
}

/// `apply_mailmap_to_header()` + `rewrite_ident_line()` (ident.c:355-428):
/// rewrite every `header` line's identity in an object buffer in place.
///
/// Ported with its bookkeeping: `rewrite_ident_line()` returns
/// `newlen - (mail_end - name_begin)` for a splice that replaced one byte more
/// than that, so after a rewritten line the scan resumes one byte late — past
/// the `\n` — and a blank line there is stepped over as if it ended the line.
/// A rewritten last header therefore carries the scan into the message, where a
/// line starting with a header name is rewritten too.
pub fn apply_mailmap_to_header(buf: &mut Vec<u8>, headers: &[&[u8]], mailmap: &Mailmap) {
    let mut offset = 0usize;
    loop {
        // `if (!*line || *line == '\n') return;` — the strbuf is NUL-terminated.
        match buf.get(offset) {
            None | Some(0) | Some(b'\n') => return,
            Some(_) => {}
        }
        let line_len = c_str(&buf[offset..]).find_byte(b'\n').map_or_else(
            || c_str(&buf[offset..]).len(),
            |nl| nl,
        );

        let mut found = false;
        for header in headers {
            if !buf[offset..].starts_with(header) || header.len() > line_len {
                continue;
            }
            found = true;
            let person_at = offset + header.len();
            let person_len = line_len - header.len();
            offset += line_len;
            let delta = rewrite_ident_line(buf, person_at, person_len, mailmap);
            offset = (offset as isize + delta) as usize;
            if buf.get(offset) == Some(&b'\n') {
                offset += 1;
            }
            break;
        }
        if !found {
            offset += line_len;
            if buf.get(offset) == Some(&b'\n') {
                offset += 1;
            }
        }
    }
}

/// `rewrite_ident_line()` (ident.c:355-390) over `buf[at..at + len]`.
fn rewrite_ident_line(buf: &mut Vec<u8>, at: usize, len: usize, mailmap: &Mailmap) -> isize {
    let person = &buf[at..at + len];
    let Some((name_end, mail_begin, mail_end)) = split_ident_spans(person) else {
        return 0;
    };
    let (mut name, mut mail) = (&person[..name_end], &person[mail_begin..mail_end]);
    if !mailmap.map_user(&mut mail, &mut name) {
        return 0;
    }
    let mut namemail = Vec::with_capacity(name.len() + mail.len() + 3);
    namemail.extend_from_slice(name);
    namemail.extend_from_slice(b" <");
    namemail.extend_from_slice(mail);
    namemail.push(b'>');
    let newlen = namemail.len();
    buf.splice(at..at + mail_end + 1, namemail);
    newlen as isize - mail_end as isize
}

#[cfg(test)]
mod tests {
    use super::*;

    fn map_of(text: &str) -> Mailmap {
        let mut map = Mailmap::default();
        map.read_string(text.as_bytes());
        map
    }

    fn check(map: &Mailmap, name: &str, email: &str) -> String {
        let (name, email) = map.mapped(name.as_bytes(), email.as_bytes());
        format!("{} <{}>", name.to_str_lossy(), email.to_str_lossy())
    }

    #[test]
    fn unqualified_line_overrides_only_its_own_fields() {
        let map = map_of("Proper Name <proper@x> <old@x>\n<form-two@x> <old@x>\n");
        assert_eq!(check(&map, "Old", "old@x"), "Proper Name <form-two@x>");
    }

    #[test]
    fn text_after_the_second_address_is_ignored() {
        let map = map_of("Hash Name <hash@x> <old@x> # trailing\nA <a@x> B <b@x> C <c@x>\n");
        assert_eq!(check(&map, "Old", "OLD@x"), "Hash Name <hash@x>");
        assert_eq!(check(&map, "b", "b@x"), "A <a@x>");
        assert_eq!(check(&map, "C", "c@x"), "C <c@x>");
    }

    #[test]
    fn addresses_keep_their_padding() {
        let map = map_of("Inner <  pad@x  > <  old@x  >\n");
        assert_eq!(check(&map, "Old", "old@x"), "Old <old@x>");
        assert_eq!(check(&map, "Old", "  old@x  "), "Inner <  pad@x  >");
    }

    #[test]
    fn name_only_entry_keeps_the_recorded_address_casing() {
        let map = map_of("Nick <nick@x>\n");
        assert_eq!(check(&map, "n", "NICK@x"), "Nick <NICK@x>");
    }

    #[test]
    fn rewriting_the_last_header_carries_the_scan_into_the_message() {
        let map = map_of("New <new@x> <old@x>\n");
        let mut buf = b"tree t\ncommitter O <old@x> 1 +0000\n\nauthor O <old@x> 2 +0000\n".to_vec();
        apply_mailmap_to_header(&mut buf, &[b"author ", b"committer "], &map);
        assert_eq!(
            buf.to_str_lossy(),
            "tree t\ncommitter New <new@x> 1 +0000\n\nauthor New <new@x> 2 +0000\n"
        );
    }
}

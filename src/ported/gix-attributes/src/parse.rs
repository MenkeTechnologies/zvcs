use std::borrow::Cow;

use bstr::{BStr, ByteSlice};

use crate::{AssignmentRef, Name, NameRef, StateRef, name};

/// The kind of attribute that was parsed.
#[derive(PartialEq, Eq, Debug, Hash, Ord, PartialOrd, Clone)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum Kind {
    /// A pattern to match paths against
    Pattern(gix_glob::Pattern),
    /// The name of the macro to define, always a valid attribute name
    Macro(Name),
}

mod error {
    use bstr::BString;
    /// The error returned by [`parse::Lines`][crate::parse::Lines].
    #[derive(thiserror::Error, Debug)]
    #[expect(missing_docs)]
    pub enum Error {
        #[error(r"Line {line_number} has a negative pattern, for literal characters use \!: {line}")]
        PatternNegation { line_number: usize, line: BString },
        #[error("Attribute in line {line_number} has non-ascii characters or starts with '-': {attribute}")]
        AttributeName { line_number: usize, attribute: BString },
        #[error("Macro in line {line_number} has non-ascii characters or starts with '-': {macro_name}")]
        MacroName { line_number: usize, macro_name: BString },
        #[error("Macro in line {line_number} is not allowed outside the top-level attributes files: {name}")]
        MacroNotAllowed { line_number: usize, name: BString },
        #[error("Could not unquote attributes line")]
        Unquote(#[from] gix_quote::ansi_c::undo::Error),
    }
}
pub use error::Error;

/// An iterator over attribute assignments, parsed line by line.
pub struct Lines<'a> {
    lines: bstr::Lines<'a>,
    line_no: usize,
    macros_ok: bool,
}

/// An iterator over attribute assignments in a single line.
pub struct Iter<'a> {
    attrs: bstr::Fields<'a>,
}

impl<'a> Iter<'a> {
    /// Create a new instance to parse attribute assignments from `input`.
    pub fn new(input: &'a BStr) -> Self {
        Iter { attrs: input.fields() }
    }

    fn parse_attr(&self, attr: &'a [u8]) -> Result<AssignmentRef<'a>, name::Error> {
        let mut tokens = attr.splitn(2, |b| *b == b'=');
        let attr = tokens.next().expect("attr itself").as_bstr();
        let possibly_value = tokens.next();
        let (attr, state) = if attr.first() == Some(&b'-') {
            (&attr[1..], StateRef::Unset)
        } else if attr.first() == Some(&b'!') {
            (&attr[1..], StateRef::Unspecified)
        } else {
            (attr, possibly_value.map_or(StateRef::Set, StateRef::from_bytes))
        };
        Ok(AssignmentRef::new(check_attr(attr)?, state))
    }
}

fn check_attr(attr: &BStr) -> Result<NameRef<'_>, name::Error> {
    fn attr_valid(attr: &BStr) -> bool {
        if attr.first() == Some(&b'-') {
            return false;
        }

        attr.bytes()
            .all(|b| matches!(b, b'-' | b'.' | b'_' | b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9'))
    }

    attr_valid(attr)
        .then(|| NameRef(attr.to_str().expect("no illformed utf8")))
        .ok_or_else(|| name::Error { attribute: attr.into() })
}

impl<'a> Iterator for Iter<'a> {
    type Item = Result<AssignmentRef<'a>, name::Error>;

    fn next(&mut self) -> Option<Self::Item> {
        let attr = self.attrs.next().filter(|a| !a.is_empty())?;
        self.parse_attr(attr).into()
    }
}

/// Instantiation
impl<'a> Lines<'a> {
    /// Create a new instance to parse all attributes in all lines of the input `bytes`.
    pub fn new(bytes: &'a [u8]) -> Self {
        let bom = unicode_bom::Bom::from(bytes);
        Lines {
            lines: bytes[bom.len()..].lines(),
            line_no: 0,
            macros_ok: true,
        }
    }

    /// Parse without `READ_ATTR_MACRO_OK` (attr.c:266), as git reads every
    /// `.gitattributes` below the top of the work tree: a `[attr]` line is then
    /// [`Error::MacroNotAllowed`] rather than a macro definition.
    pub fn macros_not_allowed(mut self) -> Self {
        self.macros_ok = false;
        self
    }
}

impl<'a> Iterator for Lines<'a> {
    type Item = Result<(Kind, Iter<'a>, usize), Error>;

    fn next(&mut self) -> Option<Self::Item> {
        fn skip_blanks(line: &BStr) -> &BStr {
            line.find_not_byteset(BLANKS).map_or(line, |pos| &line[pos..])
        }
        for line in self.lines.by_ref() {
            self.line_no += 1;
            let line = skip_blanks(line.into());
            if line.first() == Some(&b'#') {
                continue;
            }
            match parse_line(line, self.line_no, self.macros_ok) {
                None => continue,
                Some(res) => return Some(res),
            }
        }
        None
    }
}

fn parse_line(line: &BStr, line_number: usize, macros_ok: bool) -> Option<Result<(Kind, Iter<'_>, usize), Error>> {
    if line.is_empty() {
        return None;
    }

    let raw_line = line;
    let quoted = line.starts_with(b"\"");
    let (line, attrs): (Cow<'_, _>, _) = if quoted {
        let (unquoted, consumed) = match gix_quote::ansi_c::undo(line) {
            Ok(res) => res,
            Err(err) => return Some(Err(err.into())),
        };
        (unquoted, &line[consumed..])
    } else {
        line.find_byteset(BLANKS)
            .map(|pos| (line[..pos].as_bstr().into(), line[pos..].as_bstr()))
            .unwrap_or((line.into(), [].as_bstr()))
    };

    // `parse_attr_line()` (attr.c:348-354): a name is a macro only when it is
    // longer than the `[attr]` prefix, so a bare `[attr]` stays a pattern; and
    // without `READ_ATTR_MACRO_OK` it is refused before its name is checked,
    // reported as `name` — a C string, so for an unquoted line the rest of the
    // line from the name on, states included.
    let macro_name = line.strip_prefix(b"[attr]").filter(|name| !name.is_empty());
    if macro_name.is_some() && !macros_ok {
        let name: &BStr = if quoted { &line } else { raw_line };
        return Some(Err(Error::MacroNotAllowed {
            line_number,
            name: name.to_owned(),
        }));
    }
    let kind_res = match macro_name {
        Some(macro_name) => check_attr(macro_name.into())
            .map_err(|err| Error::MacroName {
                line_number,
                macro_name: err.attribute,
            })
            .map(|name| Kind::Macro(name.to_owned())),
        None => {
            let pattern = gix_glob::Pattern::from_bytes(line.as_ref())?;
            if pattern.mode.contains(gix_glob::pattern::Mode::NEGATIVE) {
                Err(Error::PatternNegation {
                    line: line.into_owned(),
                    line_number,
                })
            } else {
                Ok(Kind::Pattern(pattern))
            }
        }
    };
    let kind = match kind_res {
        Ok(kind) => kind,
        Err(err) => return Some(Err(err)),
    };
    Ok((kind, Iter::new(attrs), line_number)).into()
}

const BLANKS: &[u8] = b" \t\r";

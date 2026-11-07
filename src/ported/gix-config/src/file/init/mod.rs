use gix_features::threading::OwnShared;

use crate::{
    File,
    file::{Metadata, includes, section},
    parse,
};

mod types;
pub use types::{Error, Options};

mod comfort;
///
pub mod from_env;
///
pub mod from_paths;

impl File {
    /// Return an empty `File` with the given `meta`-data to be attached to all new sections.
    pub fn new(meta: impl Into<OwnShared<Metadata>>) -> Self {
        Self {
            backing: Default::default(),
            frontmatter_events: Default::default(),
            frontmatter_post_section: Default::default(),
            section_lookup_tree: Default::default(),
            sections: Default::default(),
            next_section_id: 0,
            section_order: Default::default(),
            meta: meta.into(),
        }
    }

    /// Instantiate a new `File` from given `input`, associating each section and their values with
    /// `meta`-data, while respecting `options`.
    pub fn from_bytes_no_includes(
        input: &[u8],
        meta: impl Into<OwnShared<Metadata>>,
        options: Options<'_>,
    ) -> Result<Self, Error> {
        let meta = meta.into();
        Ok(Self::from_parse_events_no_includes(
            parse::Events::from_bytes(input, options.to_event_filter())?,
            meta,
        ))
    }

    /// Instantiate a new `File` from given `events`, associating each section and their values with
    /// `meta`-data.
    pub fn from_parse_events_no_includes(
        parse::Events {
            backing,
            frontmatter,
            sections,
        }: parse::Events,
        meta: impl Into<OwnShared<Metadata>>,
    ) -> Self {
        let meta = meta.into();
        let mut this = File::new(OwnShared::clone(&meta));

        this.backing = backing;

        this.sections.reserve(sections.len() + 1);
        this.section_order.reserve(sections.len() + 1);
        // ```c
        // strbuf_setlen(var, baselen);
        // strbuf_addch(var, tolower(c));
        // if (get_value(cs, kvi, fn, data, var) < 0)
        // ```
        //
        // (`git_parse_source()`, config.c:1124-1135.) `baselen` is 0 until a `[...]` line
        // sets a stem, so an entry above the first header is a key with an empty section,
        // listed by `git config --list` as its bare name. Such entries become a section of
        // their own, which carries this file's metadata and keeps their place among the
        // sections when files are appended or included. Front matter of comments and blank
        // lines alone stays front matter.
        if frontmatter.iter().any(|event| matches!(event, parse::Event::SectionValueName(_))) {
            this.push_section_internal(crate::file::SectionData {
                header: parse::section::HeaderData {
                    name: Default::default(),
                    separator: None,
                    subsection_name: None,
                    headerless: true,
                },
                body: section::BodyData(frontmatter.into_vec()),
                meta: OwnShared::clone(&meta),
                id: Default::default(),
            });
        } else {
            this.frontmatter_events = frontmatter;
        }
        for section in sections {
            this.push_section_internal(crate::file::SectionData {
                header: section.header,
                body: section::BodyData(section.events),
                meta: OwnShared::clone(&meta),
                id: Default::default(),
            });
        }
        this
    }
}

impl File {
    /// Instantiate a new fully-owned `File` from given `input` (later reused as buffer when resolving includes),
    /// associating each section and their values with `meta`-data, while respecting `options`, and
    /// following includes as configured there.
    pub fn from_bytes_owned(
        input_and_buf: &mut Vec<u8>,
        meta: impl Into<OwnShared<Metadata>>,
        options: Options<'_>,
    ) -> Result<Self, Error> {
        let mut config = Self::from_parse_events_no_includes(
            parse::Events::from_bytes(input_and_buf, options.to_event_filter()).map_err(Error::from)?,
            meta,
        );

        includes::resolve(&mut config, input_and_buf, options).map_err(Error::from)?;
        Ok(config)
    }
}

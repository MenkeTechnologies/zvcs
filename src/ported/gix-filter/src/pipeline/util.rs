use std::path::Path;

use bstr::{BStr, BString};
use gix_attributes::StateRef;
use smallvec::SmallVec;

use crate::{
    Driver, driver, eol,
    eol::AttributesDigest,
    pipeline::{Context, CrlfRoundTripCheck, convert::configuration},
};

pub(crate) struct Configuration<'a> {
    pub(crate) driver: Option<&'a Driver>,
    /// What attributes say about CRLF handling.
    pub(crate) _attr_digest: Option<eol::AttributesDigest>,
    /// The final digest that includes configuration values
    pub(crate) digest: eol::AttributesDigest,
    /// The `working-tree-encoding` as spelled in the attribute, which is what git carries in
    /// `conv_attrs::working_tree_encoding` and hands to `iconv` — a name, not a resolved encoding.
    /// Resolving it here would lose the distinction between `UTF-16`, `UTF-16LE` and `UTF-16BE`,
    /// which `encoding_rs` folds onto one value, and would turn a name the platform happens not to
    /// have into a hard failure where git carries on.
    pub(crate) encoding: Option<BString>,
    /// Whether or not to apply the `ident` filter
    pub(crate) apply_ident_filter: bool,
}

impl<'driver> Configuration<'driver> {
    pub(crate) fn at_path(
        rela_path: &BStr,
        drivers: &'driver [Driver],
        attrs: &mut gix_attributes::search::Outcome,
        attributes: &mut dyn FnMut(&BStr, &mut gix_attributes::search::Outcome),
        config: eol::Configuration,
    ) -> Result<Configuration<'driver>, configuration::Error> {
        fn extract_driver<'a>(drivers: &'a [Driver], attr: &gix_attributes::search::Match<'_>) -> Option<&'a Driver> {
            if let StateRef::Value(name) = attr.assignment.state {
                drivers.iter().find(|d| d.name == name.as_bstr())
            } else {
                None
            }
        }

        /// This is `git_path_check_encoding()` in the git codebase — `convert.c:1251-1267`, v2.55.0.
        ///
        /// The value is kept as it was spelled and never resolved here: git only refuses the two
        /// boolean forms and drops a name that already means UTF-8, then hands the rest to
        /// `iconv_open()` at conversion time. Whether the platform has that encoding is not a
        /// question the attribute lookup answers.
        fn extract_encoding(
            attr: &gix_attributes::search::Match<'_>,
        ) -> Result<Option<BString>, configuration::Error> {
            match attr.assignment.state {
                StateRef::Set | StateRef::Unset => Err(configuration::Error::InvalidEncoding),
                StateRef::Value(name) => {
                    let name = name.as_bstr();
                    // The working-tree-encoding is the encoding we have to expect in the working tree.
                    // If the specified one is the default encoding, there is nothing to do.
                    Ok(
                        if name.is_empty() || crate::worktree::utf::same_encoding(name, "UTF-8") {
                            None
                        } else {
                            Some(name.to_owned())
                        },
                    )
                }
                StateRef::Unspecified => Ok(None),
            }
        }

        /// This is based on `git_path_check_crlf` in the git codebase.
        fn extract_crlf(attr: &gix_attributes::search::Match<'_>) -> Option<eol::AttributesDigest> {
            match attr.assignment.state {
                StateRef::Unspecified => None,
                StateRef::Set => Some(eol::AttributesDigest::Text),
                StateRef::Unset => Some(eol::AttributesDigest::Binary),
                StateRef::Value(v) => {
                    if v.as_bstr() == "input" {
                        Some(eol::AttributesDigest::TextInput)
                    } else if v.as_bstr() == "auto" {
                        Some(eol::AttributesDigest::TextAuto)
                    } else {
                        None
                    }
                }
            }
        }

        fn extract_eol(attr: &gix_attributes::search::Match<'_>) -> Option<eol::Mode> {
            match attr.assignment.state {
                StateRef::Unspecified | StateRef::Unset | StateRef::Set => None,
                StateRef::Value(v) => {
                    if v.as_bstr() == "lf" {
                        Some(eol::Mode::Lf)
                    } else if v.as_bstr() == "crlf" {
                        Some(eol::Mode::CrLf)
                    } else {
                        None
                    }
                }
            }
        }

        attributes(rela_path, attrs);
        let attrs: SmallVec<[_; crate::pipeline::ATTRS.len()]> = attrs.iter_selected().collect();
        let apply_ident_filter = attrs[1].assignment.state.is_set();
        let driver = extract_driver(drivers, &attrs[2]);
        let encoding = extract_encoding(&attrs[5])?;

        let mut digest = extract_crlf(&attrs[4]);
        if digest.is_none() {
            digest = extract_crlf(&attrs[0]);
        }

        if digest != Some(AttributesDigest::Binary) {
            let eol = extract_eol(&attrs[3]);
            digest = match digest {
                Some(AttributesDigest::TextAuto) if eol == Some(eol::Mode::Lf) => Some(AttributesDigest::TextAutoInput),
                Some(AttributesDigest::TextAuto) if eol == Some(eol::Mode::CrLf) => {
                    Some(AttributesDigest::TextAutoCrlf)
                }
                _ => match eol {
                    Some(eol::Mode::CrLf) => Some(AttributesDigest::TextCrlf),
                    Some(eol::Mode::Lf) => Some(AttributesDigest::TextInput),
                    _ => digest,
                },
            };
        }

        let attr_digest = digest;
        digest = match digest {
            None => Some(config.auto_crlf.into()),
            Some(AttributesDigest::Text) => Some(config.to_eol().into()),
            _ => digest,
        };

        Ok(Configuration {
            driver,
            _attr_digest: attr_digest,
            digest: digest.expect("always set by now"),
            encoding,
            apply_ident_filter,
        })
    }
}

impl Context {
    pub(crate) fn with_path<'a>(&self, rela_path: &'a BStr) -> driver::apply::Context<'a, '_> {
        driver::apply::Context {
            rela_path,
            ref_name: self.ref_name.as_ref().map(AsRef::as_ref),
            treeish: self.treeish,
            blob: self.blob,
        }
    }
}

impl CrlfRoundTripCheck {
    pub(crate) fn to_eol_roundtrip_check(self, rela_path: &Path) -> Option<eol::convert_to_git::RoundTripCheck<'_>> {
        match self {
            CrlfRoundTripCheck::Fail => Some(eol::convert_to_git::RoundTripCheck::Fail { rela_path }),
            CrlfRoundTripCheck::Warn => Some(eol::convert_to_git::RoundTripCheck::Warn { rela_path }),
            CrlfRoundTripCheck::Skip => None,
        }
    }
}

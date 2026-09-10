use bstr::BStr;
use encoding_rs::Encoding;

///
pub mod for_label {
    use bstr::BString;

    /// The error returned by [for_label()][super::for_label()].
    #[derive(Debug, thiserror::Error)]
    #[expect(missing_docs)]
    pub enum Error {
        #[error("An encoding named '{name}' is not known")]
        Unknown { name: BString },
    }
}

/// Try to produce a new `Encoding` for `label` or report an error if it is not known.
///
/// This is the `encoding_rs` half of `working-tree-encoding` and answers for the legacy encodings
/// only. The UTF-16 and UTF-32 family never reaches it: `encoding_rs` folds the byte-order variants
/// onto one value, knows no `-BOM` suffixed label at all, and encodes the whole family as UTF-8, so
/// [`worktree::utf`][crate::worktree::utf] carries those and the byte-order-mark rules that go with
/// them, and both conversion directions consult it first.
pub fn for_label<'a>(label: impl Into<&'a BStr>) -> Result<&'static Encoding, for_label::Error> {
    let mut label = label.into();
    if label == "latin-1" {
        label = "ISO-8859-1".into();
    }
    let enc = Encoding::for_label(label.as_ref()).ok_or_else(|| for_label::Error::Unknown { name: label.into() })?;
    Ok(enc)
}

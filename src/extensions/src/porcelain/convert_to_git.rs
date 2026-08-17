//! `convert_to_git()` (convert.c): the content filters a worktree file passes
//! through on its way into the object database.
//!
//! `.gitattributes` `clean` drivers, `working-tree-encoding`, `ident` and the EOL
//! normalization `text`/`core.autocrlf` ask for all run here, so a repository that
//! normalizes line endings gets the same blob out of this port that it gets out of
//! git. Every command that hashes worktree content — `add`, `stage` — goes through
//! one [`WorktreeFilter`], because a blob that differs by a single `\r` is a
//! different object id and the two commands are the same command in git.

use anyhow::Result;

/// The filter pipeline plus the attribute stack and index state it needs.
///
/// This is `gix::filter::Pipeline` taken apart with `into_parts()`: the plumbing
/// pipeline has to be driven directly because [`WorktreeFilter::convert`] needs to
/// vary the `index_object` callback, which the facade hard-codes.
pub(super) struct WorktreeFilter {
    pipeline: gix::filter::plumbing::Pipeline,
    attrs: gix::worktree::Stack,
    index: gix::index::State,
    /// `HASH_RENORMALIZE`: see [`WorktreeFilter::convert`].
    renormalize: bool,
}

impl WorktreeFilter {
    /// Build the pipeline for `repo`.
    ///
    /// `roundtrip_check` turns on the `core.safecrlf` round-trip test — the
    /// `in the working copy of '<p>', CRLF will be replaced by LF …` warning and,
    /// under `core.safecrlf=true`, the refusal that goes with it.
    ///
    /// In git that test rides on `INDEX_WRITE_OBJECT` (`get_conv_flags()`), which
    /// `add_to_index()` computes as `pretend ? 0 : INDEX_WRITE_OBJECT`
    /// (read-cache.c:723) — so a dry run and an intent-to-add never raise it, and
    /// `INDEX_RENORMALIZE` bypasses it outright by returning `CONV_EOL_RENORMALIZE`
    /// on its own. That is a property of the *caller's mode*, not of who happens to
    /// deposit the blob: git hashes and stores in one `index_path()` call, while
    /// this port splits the run into a scan that only computes ids and a later pass
    /// that writes them. Tying the check to "this pipeline writes objects" put it on
    /// the wrong half — the scan is the pass that stands in for `index_path()`, so
    /// the flag is the caller's to set and is deliberately independent of any write.
    pub(super) fn new(
        repo: &gix::Repository,
        roundtrip_check: bool,
        renormalize: bool,
    ) -> Result<Self> {
        let (pipeline, index) = repo.filter_pipeline(None)?;
        let (mut pipeline, attrs) = pipeline.into_parts();
        if !roundtrip_check || renormalize {
            pipeline.options_mut().crlf_roundtrip_check =
                gix::filter::plumbing::pipeline::CrlfRoundTripCheck::Skip;
        }
        Ok(WorktreeFilter {
            pipeline,
            attrs,
            // `gix_index::File` derefs to the `State` the pipeline reads.
            index: index.into_owned().into(),
            renormalize,
        })
    }

    /// Convert the worktree bytes of `rela_path` to their stored form.
    ///
    /// Under `--renormalize` the `text=auto` guard is switched off: git normally
    /// leaves a file's line endings alone when the blob already in the index holds
    /// CRLF, so that a mixed-ending file is not rewritten behind the user's back
    /// (`crlf_to_git()` consults `has_crlf_in_index()`). `CONV_EOL_RENORMALIZE`
    /// skips that check — it is the whole purpose of the flag — which here means
    /// telling the pipeline that no index version of the file is available.
    pub(super) fn convert(
        &mut self,
        repo: &gix::Repository,
        rela_path: &std::path::Path,
        bytes: &[u8],
    ) -> std::result::Result<Vec<u8>, ConvertError> {
        use std::io::Read;
        let entry = self
            .attrs
            .at_path(rela_path, None, &repo.objects)
            .map_err(|e| ConvertError(e.to_string()))?;
        let renormalize = self.renormalize;
        let index = &self.index;
        let mut index_object =
            |buf: &mut Vec<u8>| -> std::result::Result<Option<()>, Box<dyn std::error::Error + Send + Sync>> {
                if renormalize {
                    return Ok(None);
                }
                use gix::objs::Find as _;
                let unix = gix::path::to_unix_separators_on_windows(gix::path::into_bstr(rela_path));
                let Some(entry) = index.entry_by_path(unix.as_ref()) else {
                    return Ok(None);
                };
                let obj = repo.objects.try_find(&entry.id, buf)?;
                Ok(obj.filter(|obj| obj.kind == gix::object::Kind::Blob).map(|_| ()))
            };
        let mut outcome = self
            .pipeline
            .convert_to_git(
                bytes,
                rela_path,
                &mut |_, attrs| {
                    entry.matching_attributes(attrs);
                },
                &mut index_object,
            )
            .map_err(|e| ConvertError(e.to_string()))?;
        let mut converted = Vec::with_capacity(bytes.len());
        outcome.read_to_end(&mut converted).map_err(|e| ConvertError(e.to_string()))?;
        Ok(converted)
    }
}

/// A conversion that could not be performed — in practice `core.safecrlf=true`
/// refusing a round trip, which git reports as a `fatal:` naming the path and
/// exits 128 with nothing staged.
pub(super) struct ConvertError(String);

impl std::fmt::Display for ConvertError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

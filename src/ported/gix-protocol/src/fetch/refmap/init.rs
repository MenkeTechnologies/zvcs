use bstr::{BString, ByteSlice};
use gix_transport::client::Capabilities;

use crate::{
    fetch::{
        RefMap,
        refmap::{Mapping, Source, SpecIndex},
    },
    handshake::Ref,
};

/// The error returned by [`crate::Handshake::prepare_lsrefs_or_extract_refmap()`].
#[derive(Debug, thiserror::Error)]
#[expect(missing_docs)]
pub enum Error {
    #[error("The object format {format:?} as used by the remote is unsupported")]
    UnknownObjectFormat { format: BString },
    #[error(transparent)]
    MappingValidation(#[from] gix_refspec::match_group::validate::Error),
    #[error(transparent)]
    ListRefs(#[from] crate::ls_refs::Error),
}

/// For use in [`RefMap::from_refs()`].
#[derive(Debug, Clone)]
pub struct Context {
    /// All explicit refspecs to identify references on the remote that you are interested in.
    /// Note that these are copied to [`RefMap::refspecs`] for convenience, as `RefMap::mappings` refer to them by index.
    pub fetch_refspecs: Vec<gix_refspec::RefSpec>,
    /// A list of refspecs to use as implicit refspecs which won't be saved or otherwise be part of the remote in question.
    ///
    /// This is useful for handling `remote.<name>.tagOpt` for example.
    pub extra_refspecs: Vec<gix_refspec::RefSpec>,
    /// Refspecs for the *second* stage of git's two-stage match (`get_ref_map()` in `builtin/fetch.c`).
    ///
    /// git applies these to the refs already selected by `fetch_refspecs` — never to the full advertisement —
    /// to derive the local tracking refs that should be updated opportunistically. They are the remote's
    /// configured fetch refspecs, or whatever `--refmap` supplied.
    ///
    /// Leave empty unless `fetch_refspecs` came from the command line; git only computes opportunistic updates
    /// when the user named refspecs explicitly.
    pub opportunistic_refspecs: Vec<gix_refspec::RefSpec>,
}

impl Context {
    /// The refspecs the remote is asked to pre-filter its advertisement by.
    ///
    /// Opportunistic refspecs are excluded on purpose: they never select refs of their own, so a prefix derived
    /// from them would widen the advertisement beyond what git asks for.
    pub(crate) fn aggregate_refspecs(&self) -> Vec<gix_refspec::RefSpec> {
        let mut all_refspecs = self.fetch_refspecs.clone();
        all_refspecs.extend(self.extra_refspecs.iter().cloned());
        all_refspecs
    }
}

impl RefMap {
    /// Create a ref-map from already obtained `remote_refs`. Use `context` to pass in refspecs.
    /// `capabilities` are used to determine the object format.
    pub fn from_refs(remote_refs: Vec<Ref>, capabilities: &Capabilities, context: Context) -> Result<RefMap, Error> {
        let all_refspecs = context.aggregate_refspecs();
        let Context {
            fetch_refspecs,
            mut extra_refspecs,
            opportunistic_refspecs,
        } = context;
        let num_explicit_specs = fetch_refspecs.len();
        let group = gix_refspec::MatchGroup::from_fetch_specs(all_refspecs.iter().map(gix_refspec::RefSpec::to_ref));
        let object_hash = extract_object_hash(capabilities)?;
        let null = object_hash.null();
        let (res, fixes) = group
            .match_lhs(remote_refs.iter().map(|r| {
                let (full_ref_name, target, object) = r.unpack();
                gix_refspec::match_group::Item {
                    full_ref_name,
                    target: target.unwrap_or(&null),
                    object,
                }
            }))
            .validated()?;

        let mappings = res.mappings;
        // Remote refs that the *explicit* refspecs selected, in the order they were matched. They are the input
        // to the second matching stage below, mirroring git passing the stage-one `ref_map` list into
        // `get_fetch_map()` again.
        let mut selected: Vec<usize> = Vec::new();
        let mut mappings: Vec<Mapping> = mappings
            .into_iter()
            .map(|m| {
                let spec_index = if m.spec_index < num_explicit_specs {
                    if let Some(idx) = m.item_index {
                        if !selected.contains(&idx) {
                            selected.push(idx);
                        }
                    }
                    SpecIndex::ExplicitInRemote(m.spec_index)
                } else {
                    SpecIndex::Implicit(m.spec_index - num_explicit_specs)
                };
                Mapping {
                    remote: m.item_index.map_or_else(
                        || {
                            Source::ObjectId(match m.lhs {
                                gix_refspec::match_group::SourceRef::ObjectId(id) => id,
                                _ => unreachable!("no item index implies having an object id"),
                            })
                        },
                        |idx| Source::Ref(remote_refs[idx].clone()),
                    ),
                    local: m.rhs.map(std::borrow::Cow::into_owned),
                    spec_index,
                }
            })
            .collect();

        // Second stage: map only the refs stage one selected onto their tracking refs. git computes these
        // "opportunistic" updates in `get_ref_map()` and appends them after the stage-one entries, so a local
        // destination already claimed by stage one wins (`ref_remove_duplicates()` keeps the first entry).
        let mut opportunistic_specs_offset = None;
        if !opportunistic_refspecs.is_empty() && !selected.is_empty() {
            let offset = extra_refspecs.len();
            let taken: Vec<_> = mappings.iter().filter_map(|m| m.local.clone()).collect();
            let mut opportunistic = Vec::new();
            {
                let group = gix_refspec::MatchGroup::from_fetch_specs(
                    opportunistic_refspecs.iter().map(gix_refspec::RefSpec::to_ref),
                );
                let (res, _fixes) = group
                    .match_lhs(selected.iter().map(|&idx| {
                        let (full_ref_name, target, object) = remote_refs[idx].unpack();
                        gix_refspec::match_group::Item {
                            full_ref_name,
                            target: target.unwrap_or(&null),
                            object,
                        }
                    }))
                    .validated()?;
                for m in res.mappings {
                    // Without a destination there is nothing to update, and git suppresses the duplicate
                    // `FETCH_HEAD` row such a mapping would otherwise produce.
                    let (Some(local), Some(item_index)) = (m.rhs.map(std::borrow::Cow::into_owned), m.item_index)
                    else {
                        continue;
                    };
                    if taken.contains(&local) {
                        continue;
                    }
                    opportunistic.push(Mapping {
                        remote: Source::Ref(remote_refs[selected[item_index]].clone()),
                        local: Some(local),
                        spec_index: SpecIndex::Implicit(offset + m.spec_index),
                    });
                }
            }
            mappings.extend(opportunistic);
            extra_refspecs.extend(opportunistic_refspecs);
            opportunistic_specs_offset = Some(offset);
        }

        Ok(Self {
            mappings,
            refspecs: fetch_refspecs,
            extra_refspecs,
            opportunistic_specs_offset,
            fixes,
            remote_refs,
            object_hash,
        })
    }
}

/// Resolve the object format advertised by the server through the `object-format` capability.
///
/// When the capability is absent, the server is implicitly speaking Sha1 - older servers
/// don't advertise it at all, and even newer ones may omit it for empty repositories.
/// In builds whose `gix-hash` lacks the `sha1` feature, it's treated as unknown object format error.
fn extract_object_hash(capabilities: &Capabilities) -> Result<gix_hash::Kind, Error> {
    let object_format = match capabilities.capability("object-format").and_then(|c| c.value()) {
        Some(object_format) => object_format.to_str().map_err(|_| Error::UnknownObjectFormat {
            format: object_format.into(),
        })?,
        None => "sha1",
    };
    object_format
        .parse::<gix_hash::Kind>()
        .map_err(|_| Error::UnknownObjectFormat {
            format: object_format.into(),
        })
}

#[cfg(test)]
mod tests {
    use bstr::ByteSlice;
    use gix_transport::client::Capabilities;

    use super::Context;
    use crate::{
        fetch::{RefMap, refmap::SpecIndex},
        handshake::Ref,
    };

    fn spec(s: &str) -> gix_refspec::RefSpec {
        gix_refspec::parse(s.into(), gix_refspec::parse::Operation::Fetch)
            .expect("valid refspec")
            .to_owned()
    }

    fn direct(name: &str, hex: &str) -> Ref {
        Ref::Direct {
            full_ref_name: name.into(),
            object: gix_hash::ObjectId::from_hex(hex.as_bytes()).expect("valid hex"),
        }
    }

    fn caps() -> Capabilities {
        Capabilities::from_lines("version 2\nobject-format=sha1\n".into()).expect("valid")
    }

    const MAIN: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    const FEATURE: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

    fn refs() -> Vec<Ref> {
        vec![
            direct("refs/heads/main", MAIN),
            direct("refs/heads/feature", FEATURE),
        ]
    }

    /// The whole point of the second stage: `git fetch origin main` puts `main` in `FETCH_HEAD` *and*
    /// moves `refs/remotes/origin/main`, without the configured refspec ever selecting `feature`.
    #[test]
    fn opportunistic_specs_map_only_what_the_explicit_specs_selected() {
        let map = RefMap::from_refs(
            refs(),
            &caps(),
            Context {
                fetch_refspecs: vec![spec("main")],
                extra_refspecs: Vec::new(),
                opportunistic_refspecs: vec![spec("+refs/heads/*:refs/remotes/origin/*")],
            },
        )
        .expect("mapping succeeds");

        let mapped: Vec<_> = map
            .mappings
            .iter()
            .map(|m| {
                (
                    m.remote.as_name().expect("named").to_str_lossy().into_owned(),
                    m.local.as_ref().map(|l| l.to_str_lossy().into_owned()),
                    map.is_opportunistic(m),
                )
            })
            .collect();
        assert_eq!(
            mapped,
            vec![
                ("refs/heads/main".into(), None, false),
                (
                    "refs/heads/main".into(),
                    Some("refs/remotes/origin/main".into()),
                    true
                ),
            ],
            "`feature` is never touched even though the configured refspec is a wildcard"
        );
        assert_eq!(map.opportunistic_specs_offset, Some(0));
        assert!(
            matches!(map.mappings[1].spec_index, SpecIndex::Implicit(0)),
            "the opportunistic spec is resolvable through `extra_refspecs` so ref updates see its `+`"
        );
        assert_eq!(map.extra_refspecs, vec![spec("+refs/heads/*:refs/remotes/origin/*")]);
    }

    /// git's `ref_remove_duplicates()` keeps the first entry per destination, and the opportunistic ones
    /// are appended last, so an explicit `<src>:<dst>` that already claims the tracking ref wins.
    #[test]
    fn an_explicit_destination_suppresses_the_opportunistic_duplicate() {
        let map = RefMap::from_refs(
            refs(),
            &caps(),
            Context {
                fetch_refspecs: vec![spec("refs/heads/main:refs/remotes/origin/main")],
                extra_refspecs: Vec::new(),
                opportunistic_refspecs: vec![spec("+refs/heads/*:refs/remotes/origin/*")],
            },
        )
        .expect("mapping succeeds");

        assert_eq!(map.mappings.len(), 1);
        assert!(!map.is_opportunistic(&map.mappings[0]));
        assert!(
            map.extra_refspecs.is_empty() || map.opportunistic_specs_offset == Some(0),
            "the spec list is only extended when the stage actually ran"
        );
    }

    /// Without explicit refspecs there is no first stage to feed the second, so nothing changes at all —
    /// this is the plain `git fetch` path that must stay byte-identical.
    #[test]
    fn no_second_stage_without_a_first_one() {
        let map = RefMap::from_refs(
            refs(),
            &caps(),
            Context {
                fetch_refspecs: vec![spec("+refs/heads/*:refs/remotes/origin/*")],
                extra_refspecs: Vec::new(),
                opportunistic_refspecs: Vec::new(),
            },
        )
        .expect("mapping succeeds");

        assert_eq!(map.opportunistic_specs_offset, None);
        assert_eq!(map.mappings.len(), 2);
        assert!(map.mappings.iter().all(|m| !map.is_opportunistic(m)));
    }
}

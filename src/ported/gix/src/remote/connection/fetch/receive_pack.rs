use std::{ops::DerefMut, path::PathBuf, sync::atomic::AtomicBool};

use gix_odb::store::RefreshMode;
use gix_protocol::fetch::{Arguments, negotiate};
#[cfg(feature = "async-network-client")]
use gix_transport::client::async_io::Transport;
#[cfg(feature = "blocking-network-client")]
use gix_transport::client::blocking_io::Transport;

use crate::{
    config::{
        cache::util::ApplyLeniency,
        tree::{Clone, Fetch},
    },
    remote::{
        connection::fetch::{PrepareDetached, config, connected},
        fetch,
        fetch::{
            Error, Outcome, Prepare, RefLogMessage, Status, negotiate::Algorithm, outcome, refs, shallow,
        },
    },
};

impl<T> Prepare<'_, '_, T>
where
    T: Transport,
{
    /// Receive the pack and perform the operation as configured by git via `git-config` or overridden by various builder methods.
    /// Return `Ok(Outcome)` with an [`Outcome::status`] indicating if a change was made or not.
    ///
    /// Note that when in dry-run mode, we don't read the pack the server prepared, which leads the server to be hung up on unexpectedly.
    ///
    /// ### Negotiation
    ///
    /// "fetch.negotiationAlgorithm" describes algorithms `git` uses currently, with the default being `consecutive` and `skipping` being
    /// experimented with.
    ///
    /// ### Pack `.keep` files
    ///
    /// That packs that are freshly written to the object database are vulnerable to garbage collection for the brief time that
    /// it takes between them being placed and the respective references to be written to disk which binds their objects to the
    /// commit graph, making them reachable.
    ///
    /// To circumvent this issue, a `.keep` file is created before any pack related file (i.e. `.pack` or `.idx`) is written,
    /// which indicates the garbage collector (like `git maintenance`, `git gc`) to leave the corresponding pack file alone.
    ///
    /// If there were any ref updates or the received pack was empty, the `.keep` file will be deleted automatically leaving
    /// in its place at `write_pack_bundle.keep_path` a `None`.
    /// However, if no ref-update happened the path will still be present in `write_pack_bundle.keep_path` and is expected to be handled by the caller.
    /// A known application for this behaviour is in `remote-helper` implementations which should send this path via `lock <path>` to stdout
    /// to inform git about the file that it will remove once it updated the refs accordingly.
    ///
    /// ### Deviation
    ///
    /// When **updating refs**, the `git-fetch` docs state the following:
    ///
    /// > Unlike when pushing with git-push, any updates outside of refs/{tags,heads}/* will be accepted without + in the refspec (or --force),
    /// whether that’s swapping e.g. a tree object for a blob, or a commit for another commit that’s doesn’t have the previous commit
    /// as an ancestor etc.
    ///
    /// We explicitly don't special case those refs and expect the caller to take control. Note that by its nature,
    /// force only applies to refs pointing to commits and if they don't, they will be updated either way in our
    /// implementation as well.
    ///
    /// ### Async Mode Shortcoming
    ///
    /// Currently, the entire process of resolving a pack is blocking the executor. This can be fixed using the `blocking` crate, but it
    /// didn't seem worth the tradeoff of having more complex code.
    ///
    /// ### Configuration
    ///
    /// - `gitoxide.userAgent` is read to obtain the application user agent for git servers and for HTTP servers as well.
    ///
    #[gix_protocol::maybe_async::maybe_async]
    pub async fn receive<P>(self, progress: P, should_interrupt: &AtomicBool) -> Result<Outcome, Error>
    where
        P: gix_features::progress::NestedProgress,
        P::SubProgress: 'static,
    {
        let Prepare { inner, repo } = self;
        inner.receive(repo, progress, should_interrupt).await
    }
}

impl<T> PrepareDetached<'_, T>
where
    T: Transport,
{
    #[gix_protocol::maybe_async::maybe_async]
    pub(crate) async fn receive<P>(
        mut self,
        repo: &crate::Repository,
        progress: P,
        should_interrupt: &AtomicBool,
    ) -> Result<Outcome, Error>
    where
        P: gix_features::progress::NestedProgress,
        P::SubProgress: 'static,
    {
        let ref_map = &self.ref_map;
        if ref_map.is_missing_required_mapping() {
            let mut specs = ref_map.refspecs.clone();
            specs.extend(ref_map.extra_refspecs.clone());
            return Err(Error::NoMapping {
                refspecs: specs,
                num_remote_refs: ref_map.remote_refs.len(),
            });
        }

        let mut con = self.con.take().expect("receive() can only be called once");
        let mut handshake = con.handshake.take().expect("receive() can only be called once");

        let expected_object_hash = repo.object_hash();
        if ref_map.object_hash != expected_object_hash {
            return Err(Error::IncompatibleObjectHash {
                local: expected_object_hash,
                remote: ref_map.object_hash,
            });
        }

        let fetch_options = gix_protocol::fetch::Options {
            shallow_file: repo.shallow_file(),
            shallow: &self.shallow,
            tags: con.remote.fetch_tags,
            reject_shallow_remote: Clone::REJECT_SHALLOW
                .enrich_error(
                    repo.config
                        .resolved
                        .boolean_filter("clone.rejectShallow", &mut repo.filter_config_section()),
                )?
                .unwrap_or(false),
            filter: self.filter.as_ref().map(gix_protocol::fetch::filter::Filter::as_str),
        };
        let context = gix_protocol::fetch::Context {
            handshake: &mut handshake,
            transport: &mut con.transport.inner,
            user_agent: repo.config.user_agent_tuple(),
            trace_packetlines: con.trace,
            server_options: con.server_options.clone(),
        };

        let negotiator = repo
            .config
            .resolved
            .string(Fetch::NEGOTIATION_ALGORITHM)
            .map(|n| Fetch::NEGOTIATION_ALGORITHM.try_into_negotiation_algorithm(n))
            .transpose()
            .with_leniency(repo.config.lenient_config)?
            .unwrap_or(Algorithm::Consecutive)
            .into_negotiator();
        let graph_repo = {
            let mut r = repo.clone();
            // assure that checking for unknown server refs doesn't trigger ODB refreshes.
            r.objects.refresh = RefreshMode::Never;
            // we cache everything of importance in the graph and thus don't need an object cache.
            r.objects.unset_object_cache();
            r
        };
        let cache = graph_repo.commit_graph_if_enabled().ok().flatten();
        let mut graph = graph_repo.revision_graph(cache.as_ref());
        let alternates = repo.objects.store_ref().alternate_db_paths()?;
        let mut negotiate = Negotiate {
            objects: &graph_repo.objects,
            refs: &graph_repo.refs,
            graph: &mut graph,
            alternates,
            ref_map,
            shallow: &self.shallow,
            tags: con.remote.fetch_tags,
            negotiator,
            open_options: repo.options.clone(),
            restrictions: std::mem::take(&mut self.negotiation),
            refetch: self.refetch,
        };

        let write_pack_options = gix_pack::bundle::write::Options {
            thread_limit: config::index_threads(repo)?,
            index_version: config::pack_index_version(repo)?,
            iteration_mode: gix_pack::data::input::Mode::Verify,
            object_hash: repo.object_hash(),
            alloc_limit_bytes: repo.config.alloc_limit_bytes,
            compression: repo.config.loose_compression,
        };
        let mut write_pack_bundle = None;

        let res = gix_protocol::fetch(
            &mut negotiate,
            |reader, progress, should_interrupt| -> Result<bool, gix_pack::bundle::write::Error> {
                let mut may_read_to_end = false;
                write_pack_bundle = if matches!(self.dry_run, fetch::DryRun::No) {
                    let res = gix_pack::Bundle::write_to_directory(
                        reader,
                        Some(&repo.objects.store_ref().path().join("pack")),
                        progress,
                        should_interrupt,
                        Some(Box::new({
                            let repo = repo.clone();
                            repo.objects
                        })),
                        write_pack_options,
                    )?;
                    may_read_to_end = true;
                    Some(res)
                } else {
                    None
                };
                Ok(may_read_to_end)
            },
            progress,
            should_interrupt,
            context,
            fetch_options,
        )
        .await?;
        // The `shallow <oid>` lines the remote sent without us having asked for a depth change. They
        // are what git's `receive_shallow_info()` collects into `shallow_info`, and the input to the
        // `update_shallow()` decision below.
        let remote_shallow: Vec<gix_hash::ObjectId> = res
            .as_ref()
            .map(|v| {
                v.last_response
                    .shallow_updates()
                    .iter()
                    .filter_map(|update| match update {
                        gix_protocol::fetch::response::ShallowUpdate::Shallow(id) => Some(*id),
                        gix_protocol::fetch::response::ShallowUpdate::Unshallow(_) => None,
                    })
                    .collect()
            })
            .unwrap_or_default();
        let negotiate = res.map(|v| outcome::Negotiate {
            graph: graph.detach(),
            rounds: v.negotiate.rounds,
        });

        if matches!(handshake.server_protocol_version, gix_protocol::transport::Protocol::V2) {
            gix_protocol::indicate_end_of_interaction(&mut con.transport.inner, con.trace)
                .await
                .ok();
        }

        // git runs `update_shallow()` right after the pack was indexed and before any ref is
        // touched: only then is it known which of the remote's shallow roots the pack actually
        // brought along, and hence which of the fetched refs may be updated at all. A fetch that
        // asked for a depth of its own took the early return there, as its boundary was already
        // settled while negotiating.
        let mut rejected_shallow = Vec::new();
        if !remote_shallow.is_empty() && matches!(self.shallow, gix_protocol::fetch::Shallow::NoChange) {
            let ref_tips: Vec<_> = self
                .ref_map
                .mappings
                .iter()
                .map(|m| m.remote.as_id().map_or_else(|| repo.object_hash().null(), ToOwned::to_owned))
                .collect();
            let outcome = shallow::update(repo, self.shallow_update, remote_shallow, &ref_tips)?;
            // git skips rejected refs both when reporting updates and when writing `FETCH_HEAD`, so
            // they leave the ref map entirely and are handed to the caller to warn about.
            for index in outcome.rejected.into_iter().rev() {
                rejected_shallow.push(self.ref_map.mappings.remove(index));
            }
            rejected_shallow.reverse();
        }

        // A shallow repository's remote advertises refs whose objects lie outside the
        // boundary and were deliberately never sent - an old tag, most often. git's
        // `remove_nonexistent_theirs_shallow()` drops those refs from the ref map instead of
        // updating them, which is also what keeps them out of the connectivity check below;
        // feeding one to that check reports a short pack for a fetch that brought everything
        // it was supposed to. They are dropped silently: git never announces a ref it was
        // never going to write.
        //
        // The condition is what the *fetch* did, not what the repository looks like afterwards:
        // `remove_nonexistent_theirs_shallow()` runs off `si->shallow`, the shallow info this
        // exchange carried (fetch-pack.c:1611). `--unshallow` is the case that separates the two —
        // it deletes the shallow file, so a repository test would be false by the time it is asked,
        // and every tag whose object the remote left out would go to the connectivity check.
        if matches!(self.dry_run, fetch::DryRun::No)
            && (repo.shallow_commits().ok().flatten().is_some()
                || !matches!(self.shallow, gix_protocol::fetch::Shallow::NoChange))
        {
            let outside: Vec<usize> = self
                .ref_map
                .mappings
                .iter()
                .enumerate()
                .filter(|(_, m)| {
                    m.remote
                        .as_id()
                        .is_some_and(|id| repo.find_header(id).is_err())
                })
                .map(|(index, _)| index)
                .collect();
            for index in outside.into_iter().rev() {
                self.ref_map.mappings.remove(index);
            }
        }

        // git's `fetch-pack.c` calls `write_promisor_file()` whenever a filter was in play, marking the
        // pack as one that deliberately lacks objects. Everything that later walks for missing objects
        // (`fsck`, `gc`, `rev-list --missing`) keys off the presence of that file - and so does the
        // connectivity check right below, which is why index-pack is told `--promisor` before it runs.
        if let Some((bundle, _filter)) = write_pack_bundle.as_ref().zip(self.filter.as_ref()) {
            if let Some(index_path) = bundle.index_path.as_deref() {
                write_promisor_file(index_path, &self.ref_map)?;
            }
        }

        // git's `store_updated_refs()` opens with `check_connected()` over the very ref map it is
        // about to store, and abandons the whole fetch if the walk fails - which is what keeps a
        // short pack from leaving refs pointing at objects nobody has. Rejected shallow refs are
        // already out of `mappings`, matching `iterate_ref_map()` skipping `REF_STATUS_REJECT_SHALLOW`.
        // A dry run never wrote the pack, so there is nothing to be connected to.
        if matches!(self.dry_run, fetch::DryRun::No) {
            let tips: Vec<_> = self
                .ref_map
                .mappings
                .iter()
                // `iterate_ref_map()` skips "anything missing a peer_ref, which we are not
                // actually going to write a ref for". Without that filter the check demands
                // objects for refs this fetch never asked for - every tag outside a shallow
                // clone's window, for one - and fails a fetch git completes.
                .filter(|m| m.local.is_some())
                .filter_map(|m| m.remote.as_id().map(ToOwned::to_owned))
                .collect();
            let options = connected::Options {
                from_promisor: self.filter.is_some(),
                // `opt.is_deepening_fetch = args->deepen` (fetch-pack.c:1050): a fetch that moves a
                // shallow boundary must not hide what is already local. Its own refs point into the
                // history the pack just extended, and excluding them makes the walk start below the
                // new boundary and demand the parents the graft exists to cut off.
                is_deepening_fetch: !matches!(self.shallow, gix_protocol::fetch::Shallow::NoChange),
                ..Default::default()
            };
            if !connected::check_connected(repo, &tips, options)? {
                return Err(Error::NotConnected);
            }
        }

        let update_refs = refs::update(
            repo,
            self.reflog_message
                .take()
                .unwrap_or_else(|| RefLogMessage::Prefixed { action: "fetch".into() }),
            &self.ref_map.mappings,
            con.remote.fetch_refspecs(),
            &self.ref_map.extra_refspecs,
            con.remote.fetch_tags,
            self.dry_run,
            self.write_packed_refs,
            self.atomic,
        )?;

        if let Some(bundle) = write_pack_bundle.as_mut() {
            if !update_refs.edits.is_empty() || bundle.index.num_objects == 0 {
                if let Some(path) = bundle.keep_path.take() {
                    std::fs::remove_file(&path).map_err(|err| Error::RemovePackKeepFile { path, source: err })?;
                }
            }
        }

        let out = Outcome {
            handshake,
            rejected_shallow,
            ref_map: std::mem::take(&mut self.ref_map),
            status: match write_pack_bundle {
                Some(write_pack_bundle) => Status::Change {
                    write_pack_bundle,
                    update_refs,
                    negotiate: negotiate.expect("if we have a pack, we always negotiated it"),
                },
                None => Status::NoPackReceived {
                    dry_run: matches!(self.dry_run, fetch::DryRun::Yes),
                    negotiate,
                    update_refs,
                },
            },
        };
        Ok(out)
    }
}

/// Write `<pack>.promisor` next to the pack index at `index_path`, listing the refs this fetch asked for
/// as `<oid> <ref>` lines - the format `write_promisor_file()` produces in git's `fetch-pack.c`.
///
/// A lazy fetch by object id asks for no refs and so leaves the file empty, exactly as git does.
///
/// ### Deviation
///
/// The lines come out in ref-map order rather than in the order git's `sought` array happens to hold
/// them. Nothing reads the contents - `is_promisor_object()` only tests that the file exists - so this
/// only shows up when comparing the files byte for byte.
fn write_promisor_file(index_path: &std::path::Path, ref_map: &gix_protocol::fetch::RefMap) -> Result<(), Error> {
    use crate::bstr::ByteSlice;

    let path = index_path.with_extension("promisor");
    let mut body = String::new();
    for mapping in &ref_map.mappings {
        if let Some((name, id)) = mapping.remote.as_name().zip(mapping.remote.as_id()) {
            body.push_str(&format!("{id} {}\n", name.to_str_lossy()));
        }
    }
    std::fs::write(&path, body).map_err(|err| Error::WritePromisorFile { path, source: err })
}

struct Negotiate<'a, 'b, 'c> {
    objects: &'a crate::OdbHandle,
    refs: &'a gix_ref::file::Store,
    graph: &'a mut gix_negotiate::Graph<'b, 'c>,
    alternates: Vec<PathBuf>,
    ref_map: &'a gix_protocol::fetch::RefMap,
    shallow: &'a gix_protocol::fetch::Shallow,
    tags: gix_protocol::fetch::Tags,
    negotiator: Box<dyn gix_negotiate::Negotiator>,
    open_options: crate::open::Options,
    restrictions: negotiate::Restrictions,
    refetch: bool,
}

impl gix_protocol::fetch::Negotiate for Negotiate<'_, '_, '_> {
    fn mark_complete_and_common_ref(&mut self) -> Result<negotiate::Action, negotiate::Error> {
        if self.refetch {
            // git's `--refetch` skips negotiation outright, so nothing is known to be present on our
            // side and every mapping turns into a `want`. Claiming no target is known also keeps the
            // "nothing changed" short-circuits from firing, which is the point: a refetch must reach
            // the remote even when our tracking refs are already up to date.
            return Ok(negotiate::Action::MustNegotiate {
                remote_ref_target_known: vec![false; self.ref_map.mappings.len()],
            });
        }
        negotiate::mark_complete_and_common_ref(
            &self.objects,
            self.refs,
            {
                let alternates = std::mem::take(&mut self.alternates);
                let open_options = self.open_options.clone();
                move || -> Result<_, std::convert::Infallible> {
                    Ok(alternates
                        .into_iter()
                        .filter_map(move |path| {
                            path.ancestors()
                                .nth(1)
                                .and_then(|git_dir| crate::open_opts(git_dir, open_options.clone()).ok())
                        })
                        .map(|repo| (repo.refs, repo.objects)))
                }
            },
            self.negotiator.deref_mut(),
            &mut *self.graph,
            self.ref_map,
            self.shallow,
            negotiate::make_refmapping_ignore_predicate(self.tags, self.ref_map),
            &self.restrictions,
        )
    }

    fn add_wants(&mut self, arguments: &mut Arguments, remote_ref_target_known: &[bool]) -> bool {
        negotiate::add_wants(
            self.objects,
            arguments,
            self.ref_map,
            remote_ref_target_known,
            self.shallow,
            negotiate::make_refmapping_ignore_predicate(self.tags, self.ref_map),
        )
    }

    fn one_round(
        &mut self,
        state: &mut negotiate::one_round::State,
        arguments: &mut Arguments,
        previous_response: Option<&gix_protocol::fetch::Response>,
    ) -> Result<(negotiate::Round, bool), negotiate::Error> {
        if self.refetch {
            // Nothing to offer and nothing left to ask about: the single request carries the wants
            // and `done`, which is exactly what stock git sends under `--refetch`.
            return Ok((
                negotiate::Round {
                    haves_sent: 0,
                    in_vain: 0,
                    haves_to_send: state.haves_to_send,
                    previous_response_had_at_least_one_in_common: false,
                },
                true,
            ));
        }
        negotiate::one_round(
            self.negotiator.deref_mut(),
            &mut *self.graph,
            state,
            arguments,
            previous_response,
            &self.restrictions.always_have,
        )
    }
}

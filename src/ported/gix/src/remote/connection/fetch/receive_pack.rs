use std::{ops::DerefMut, path::PathBuf, sync::atomic::AtomicBool};

use gix_object::Exists;
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

        // A band-3 message ends `recv_sideband()` with `SIDEBAND_REMOTE_ERROR` once it has been shown
        // (sideband.c:369-377), so the end of the stream after it is not reported as a disconnect.
        let remote_error = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let sideband = con.sideband.clone().map(|inner| {
            let remote_error = remote_error.clone();
            gix_protocol::fetch::Sideband(std::sync::Arc::new(std::sync::Mutex::new(
                move |is_error: bool, data: &[u8]| {
                    if is_error {
                        remote_error.store(true, std::sync::atomic::Ordering::Relaxed);
                    }
                    if let Ok(mut handler) = inner.0.lock() {
                        (&mut *handler)(is_error, data);
                    }
                },
            )))
        });
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
            no_progress: con.no_progress,
            sideband,
            keep_pack: self.keep_pack,
        };
        let protocol = handshake.server_protocol_version;
        // `stateless_rpc`: remote-curl ends every response with a response-end packet (remote-curl.c:1086-1087).
        let stateless = !con.transport.inner.connection_persists_across_multiple_requests();
        let stateless_v2 = stateless && matches!(protocol, gix_protocol::transport::Protocol::V2);
        let mut demux = DemuxState::new(config::unpack_limit(repo), remote_error, stateless_v2);
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
                // `--dry-run` receives the pack too: git's dry run stops short of the
                // ref updates and `FETCH_HEAD`, not of the transfer, which is why its
                // forced-update test has both commits to compare.
                let mut pack = Demux {
                    inner: reader,
                    state: &mut demux,
                };
                write_pack_bundle = Some(gix_pack::Bundle::write_to_directory(
                    &mut pack,
                    Some(&repo.objects.store_ref().path().join("pack")),
                    progress,
                    should_interrupt,
                    Some(Box::new({
                        let repo = repo.clone();
                        repo.objects
                    })),
                    write_pack_options,
                )?);
                // The demultiplexer reads on to the packet that ends the stream after the child is done with it
                // (`recv_sideband()`, pkt-line.c:585-603), so whatever follows the pack is consumed here.
                std::io::copy(&mut pack, &mut std::io::sink())?;
                Ok(false)
            },
            progress,
            should_interrupt,
            context,
            fetch_options,
        )
        .await;
        // `get_pack()`'s ways of dying of a stream that ended early, and `do_check_stateless_delimiter()`'s
        // (fetch-pack.c:1068-1100, :1846-1847).
        let end = demux.end;
        // `finish_async()` fails for a demultiplexer that hit the end of the stream or a band-3 message.
        let demux_failed =
            end == Some(DemuxEnd::Disconnect) || demux.remote_error.load(std::sync::atomic::Ordering::Relaxed);
        let response_end_consumed = end == Some(DemuxEnd::Special) && stateless_v2;
        let mut pack_stream = |consumer_failed: bool, bundle: Option<gix_pack::bundle::write::Outcome>| {
            Error::PackStream(Box::new(fetch::PackStream {
                protocol,
                header: std::mem::take(&mut demux.header),
                retained: demux.retained.take(),
                disconnected: demux_failed,
                consumer_failed,
                response_end_consumed: !consumer_failed && response_end_consumed,
                bundle,
            }))
        };
        let res = match res {
            Err(gix_protocol::fetch::Error::ConsumePack(_)) if end.is_some() => {
                return Err(pack_stream(true, None));
            }
            Ok(Some(_)) if demux_failed || response_end_consumed => {
                return Err(pack_stream(false, write_pack_bundle.take()));
            }
            res => res?,
        };
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

        if negotiate.is_some() {
            // `fetch_refs_via_pack()` clears `finished_handshake` once `fetch_pack()` has run
            // (transport.c:564), so `disconnect_git()` writes no closing flush after a fetch
            // (transport.c:1024-1025): the server just sees the connection close.
            con.transport.skip_end_of_interaction();
        } else if matches!(handshake.server_protocol_version, gix_protocol::transport::Protocol::V2) {
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
            // Which refs this fetch is actually going to write, decided before the check so the
            // check can be limited to them. git reaches the same set the other way round: by the
            // time `store_updated_refs()` runs, `filter_refs()` has already dropped the updates it
            // will not perform, so a *rejected* one — a tag that would be overwritten, a
            // non-fast-forward — never reaches `check_connected()`. Deciding them here is a plain
            // dry run: it reads refs and writes nothing.
            let planned = refs::update(
                repo,
                self.reflog_message
                    .clone()
                    .unwrap_or_else(|| RefLogMessage::Prefixed { action: "fetch".into() }),
                &self.ref_map.mappings,
                con.remote.fetch_refspecs(),
                &self.ref_map.extra_refspecs,
                con.remote.fetch_tags,
                fetch::DryRun::Yes,
                self.show_forced_updates,
                self.write_packed_refs,
                self.atomic,
            )?;
            // With `tagOpt` left at its default, every `refs/tags/*` the server advertised is in
            // the ref map, including the ones that point outside the history being fetched. git
            // drops those before the connectivity check: `find_non_local_tags()` only follows a
            // tag whose object `has_object()` or is in `fetch_oids`, so an unrelated tag never
            // reaches `iterate_ref_map()`. gitoxide drops them later instead, in `update_refs()`
            // as `ImplicitTagNotSentByRemote` - and that classification needs the 'object exists'
            // check which `DryRun::Yes` deliberately skips, so `planned` above cannot report it
            // and the rule has to be applied here, against the pack that is now on disk.
            let implicit_tag_spec = con
                .remote
                .fetch_tags
                .to_refspec()
                .filter(|_| matches!(con.remote.fetch_tags, fetch::Tags::Included));
            let tips: Vec<_> = planned
                .updates
                .iter()
                .zip(self.ref_map.mappings.iter())
                // `iterate_ref_map()` skips "anything missing a peer_ref, which we are not
                // actually going to write a ref for". Without that filter the check demands
                // objects for refs this fetch never asked for - every tag outside a shallow
                // clone's window, for one - and fails a fetch git completes.
                //
                // The same reasoning covers the updates that move no ref: a rejected one is
                // not written, and one that needs no change already points at an object that
                // is here. Neither can be a reason to demand something from the pack.
                .filter(|(update, m)| {
                    m.local.is_some()
                        && !update.mode.is_rejected()
                        && !matches!(update.mode, refs::update::Mode::NoChangeNeeded)
                })
                .filter_map(|(_, m)| {
                    let id = m.remote.as_id()?;
                    let is_implicit_tag = implicit_tag_spec.is_some_and(|tag_spec| {
                        m.spec_index
                            .get(con.remote.fetch_refspecs(), &self.ref_map.extra_refspecs)
                            .is_some_and(|spec| spec.to_ref() == tag_spec)
                    });
                    if is_implicit_tag && !repo.objects.exists(id) {
                        return None;
                    }
                    Some(id.to_owned())
                })
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
            self.show_forced_updates,
            self.write_packed_refs,
            self.atomic,
        )?;

        if let Some(bundle) = write_pack_bundle.as_mut() {
            // A dry run applied none of its edits, but git unlocks the pack all the same.
            if !update_refs.edits.is_empty()
                || bundle.index.num_objects == 0
                || matches!(self.dry_run, fetch::DryRun::Yes)
            {
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
            negotiate::make_refmapping_ignore_predicate(self.tags, self.ref_map, self.objects, self.refs),
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
            negotiate::make_refmapping_ignore_predicate(self.tags, self.ref_map, self.objects, self.refs),
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

    fn next_have(&mut self) -> Option<Result<gix_hash::ObjectId, negotiate::Error>> {
        // `--refetch` runs the noop negotiator (fetch-pack.c:1724-1725, :1149-1150), which never
        // offers anything.
        if self.refetch {
            return None;
        }
        self.negotiator
            .next_have(&mut *self.graph)
            .map(|res| res.map_err(Into::into))
    }

    fn ack(&mut self, id: gix_hash::ObjectId) -> Result<bool, negotiate::Error> {
        if self.refetch {
            return Ok(false);
        }
        Ok(self.negotiator.in_common_with_remote(id, &mut *self.graph)?)
    }

    fn always_have(&self) -> Vec<gix_hash::ObjectId> {
        if self.refetch {
            return Vec::new();
        }
        self.restrictions.always_have.clone()
    }
}

/// Where `recv_sideband()` stopped reading the pack's sideband stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DemuxEnd {
    /// The flush that ends the pack, or a delimiter the reader stops at.
    Flush,
    /// A delimiter or response-end packet in a reader that does not stop at it, which `demultiplex_sideband()`
    /// treats as a flush (sideband.c:333-346).
    Special,
    /// The end of the stream, or a packet cut short by it: `PACKET_READ_EOF` under `PACKET_READ_GENTLE_ON_EOF`
    /// (pkt-line.c:364-372, :585-589), or a band-3 message before it. Either fails `finish_async()`.
    Disconnect,
}

/// What [`Demux`] learned of the pack as it went by.
struct DemuxState {
    end: Option<DemuxEnd>,
    /// The first bytes of the pack, up to the 12 of its header.
    header: Vec<u8>,
    /// Every byte of the pack, while its header announces fewer objects than `retain_below`, or for every pack when
    /// that is `0`: `get_pack()` then reads no header and gives any pack it does not keep to `unpack-objects`
    /// (fetch-pack.c:991-998, :1007).
    retained: Option<Vec<u8>>,
    retain_below: u64,
    /// A protocol v2 response over remote-curl, which ends every response it took whole with a response-end
    /// packet (remote-curl.c:1086-1087). The vendored transport ends such a response with the end of its body.
    stateless_v2: bool,
    /// Set once the remote sent a band-3 message.
    remote_error: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

impl DemuxState {
    fn new(
        retain_below: u64,
        remote_error: std::sync::Arc<std::sync::atomic::AtomicBool>,
        stateless_v2: bool,
    ) -> Self {
        DemuxState {
            end: None,
            header: Vec::with_capacity(12),
            retained: None,
            retain_below,
            stateless_v2,
            remote_error,
        }
    }

    fn record(&mut self, bytes: &[u8]) {
        if self.header.len() < 12 {
            let missing = 12 - self.header.len();
            self.header.extend_from_slice(&bytes[..missing.min(bytes.len())]);
            if self.header.len() == 12 {
                let nr_objects = u32::from_be_bytes(self.header[8..12].try_into().expect("four bytes"));
                if self.retain_below == 0 || u64::from(nr_objects) < self.retain_below {
                    self.retained = Some(Vec::new());
                }
            }
        }
        if let Some(retained) = self.retained.as_mut() {
            retained.extend_from_slice(bytes);
        }
    }
}

/// `sideband_demux()` (fetch-pack.c:897-905): the pack's sideband stream as the child that takes the pack reads
/// it. `recv_sideband()` stops at any of the packets that end a stream (sideband.c:333-346) and at the end of the
/// stream itself, which it reports before closing its output:
///
/// ```c
/// if (status == PACKET_READ_EOF) {
///         strbuf_addf(scratch,
///                     "%s%s: unexpected disconnect while reading sideband packet",
///                     scratch->len ? "\n" : "", me);
///         *sideband_type = SIDEBAND_PROTOCOL_ERROR;
///         goto cleanup;
/// }
/// ```
///
/// Either way the child sees the end of its input there.
struct Demux<'a, 'r> {
    inner: &'r mut dyn std::io::BufRead,
    state: &'a mut DemuxState,
}

impl std::io::BufRead for Demux<'_, '_> {
    fn fill_buf(&mut self) -> std::io::Result<&[u8]> {
        if self.state.end.is_none() {
            let available = match self.inner.fill_buf() {
                Ok(buf) => buf.len(),
                Err(err) => {
                    let non_data_line = err.get_ref().is_some_and(|inner| {
                        matches!(
                            inner.downcast_ref::<gix_protocol::transport::packetline::decode::band::Error>(),
                            Some(gix_protocol::transport::packetline::decode::band::Error::NonDataLine)
                        )
                    });
                    if non_data_line {
                        self.state.end = Some(DemuxEnd::Special);
                    } else if err.kind() == std::io::ErrorKind::UnexpectedEof
                        && self.state.stateless_v2
                        && gix_protocol::transport::client::RpcEnded::find(&err).is_none()
                    {
                        // A v2 response whose body ended between packets without a failure is followed by
                        // the response-end packet remote-curl writes after every response, which stops
                        // `recv_sideband()` as a flush does. A failed one carries its own `0002`.
                        self.state.end = Some(DemuxEnd::Special);
                    } else if err.kind() == std::io::ErrorKind::UnexpectedEof {
                        if !self.state.remote_error.load(std::sync::atomic::Ordering::Relaxed) {
                            eprintln!("fetch-pack: unexpected disconnect while reading sideband packet");
                        }
                        self.state.end = Some(DemuxEnd::Disconnect);
                    } else {
                        return Err(err);
                    }
                    0
                }
            };
            if available == 0 {
                self.state.end.get_or_insert(DemuxEnd::Flush);
            }
        }
        if self.state.end.is_some() {
            return Ok(&[]);
        }
        self.inner.fill_buf()
    }

    fn consume(&mut self, amt: usize) {
        if amt == 0 {
            return;
        }
        if let Ok(buf) = self.inner.fill_buf() {
            self.state.record(&buf[..amt.min(buf.len())]);
        }
        self.inner.consume(amt);
    }
}

impl std::io::Read for Demux<'_, '_> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let available = std::io::BufRead::fill_buf(self)?;
        let n = available.len().min(buf.len());
        buf[..n].copy_from_slice(&available[..n]);
        std::io::BufRead::consume(self, n);
        Ok(n)
    }
}

//! Promisor remotes: the half of *partial clone* that makes the objects a clone skipped reachable again.
//!
//! A partial clone deliberately arrives without some objects, and records the remote it came from as a
//! *promisor* - a remote that promised to hand those objects over later. Reading one of them makes the
//! repository go back and fetch it on demand.
//!
//! See git's `Documentation/technical/partial-clone.adoc` and `promisor-remote.c`. This module is the
//! port of the client half: [`remotes()`] is `promisor_remote_init()`, and the hook installed by
//! [`install_hook()`] plays the role of `promisor_remote_get_direct()`, which `oid_object_info_extended()`
//! calls when an object is missing locally.
//!
//! [`capability_reply()`] is the other client half: the protocol-v2 `promisor-remote` capability, by
//! which a server offers the promisor remotes *it* uses and the client says which of them it will use
//! itself. That is `promisor_remote_reply()` and the `filter_promisor_remote()` it drives, and it is
//! what `promisor.acceptFromServer`, `promisor.checkFields` and `promisor.storeFields` govern.

use crate::bstr::ByteSlice;

/// A remote that a partial clone may fall back on for objects it does not have.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Remote {
    /// The name of the remote, as in `remote.<name>.url`.
    pub name: String,
    /// The filter that was in effect when the partial clone was made, from `remote.<name>.partialclonefilter`.
    ///
    /// It is repeated on every lazy fetch so the remote doesn't start sending objects that were skipped
    /// on purpose, exactly as git's `--filter=blob:none` on the fetch it spawns.
    pub filter: Option<String>,
}

/// The promisor remotes configured for `repo`, in configuration order.
///
/// Two spellings are recognized, both of which `git` still reads:
///
/// * `remote.<name>.promisor = true` - what `partial_clone_register()` writes today.
/// * `extensions.partialClone = <name>` - the original, single-remote form.
pub fn remotes(repo: &crate::Repository) -> Vec<Remote> {
    let config = &repo.config.resolved;
    let filter_of = |name: &str| {
        config
            .string_by("remote", Some(crate::bstr::BStr::new(name)), "partialclonefilter")
            .map(|v| v.to_str_lossy().into_owned())
    };

    let mut remotes: Vec<Remote> = config
        .sections_by_name("remote")
        .into_iter()
        .flatten()
        .filter_map(|section| {
            let name = section.header().subsection_name()?.to_str().ok()?;
            config
                .boolean_by("remote", Some(crate::bstr::BStr::new(name)), "promisor")
                .ok()
                .flatten()
                .filter(|promisor| *promisor)
                .map(|_| Remote {
                    name: name.to_owned(),
                    filter: filter_of(name),
                })
        })
        .collect();

    if let Some(name) = config
        .string("extensions.partialClone")
        .map(|v| v.to_str_lossy().into_owned())
        .filter(|name| !name.is_empty())
    {
        if !remotes.iter().any(|r| r.name == name) {
            let filter = filter_of(&name);
            remotes.push(Remote { name, filter });
        }
    }
    remotes
}

/// Give `repo`'s object database a way to fetch objects it doesn't have from a promisor remote.
///
/// Does nothing when no promisor remote is configured, which is the case for every ordinary repository.
///
/// The hook re-opens the repository from `git_dir` when it fires rather than capturing this one: the
/// object database outlives the `Repository` handle, and capturing it would keep the two alive through
/// each other. git achieves the same separation by running the fetch in a subprocess.
#[cfg(feature = "blocking-network-client")]
pub(crate) fn install_hook(repo: &crate::Repository) {
    install_hook_for(repo, remotes(repo));
}

/// Like [`install_hook()`], but for promisor remotes the caller knows about.
///
/// A clone needs this: its object database is opened before `remote.<name>.promisor` is written, so
/// the checkout that immediately follows would otherwise find no way to obtain the blobs the filter
/// left behind.
#[cfg(feature = "blocking-network-client")]
pub(crate) fn install_hook_for(repo: &crate::Repository, promisors: Vec<Remote>) {
    if promisors.is_empty() {
        return;
    }
    let git_dir = repo.git_dir().to_owned();
    let open_options = repo.options.clone();
    repo.objects.store_ref().set_promisor(Box::new(move |ids| {
        let Ok(repo) = crate::open_opts(&git_dir, open_options.clone()) else {
            return false;
        };
        promisors
            .iter()
            .any(|promisor| fetch(&repo, promisor, ids).unwrap_or(false))
    }));
}

/// Obtain every object in `ids` that is not present locally, in a single fetch from the promisor remote.
///
/// Returns `true` if a fetch was made. This is the prefetch git performs in `check_updates()` before
/// writing a worktree: without it, each missing blob would cost its own round trip.
#[cfg(feature = "blocking-network-client")]
pub fn prefetch(repo: &crate::Repository, ids: impl IntoIterator<Item = gix_hash::ObjectId>) -> bool {
    use gix_object::Exists;

    let store = repo.objects.store_ref();
    if !store.has_promisor() {
        return false;
    }
    let mut missing: Vec<_> = ids.into_iter().filter(|id| !repo.objects.exists(id)).collect();
    missing.sort_unstable();
    missing.dedup();
    store.fetch_from_promisor(&missing)
}

/// The error produced by [`fetch()`].
#[cfg(feature = "blocking-network-client")]
#[derive(Debug, thiserror::Error)]
#[expect(missing_docs)]
pub enum Error {
    #[error(transparent)]
    FindRemote(#[from] crate::remote::find::existing::Error),
    #[error(transparent)]
    Connect(#[from] crate::remote::connect::Error),
    #[error(transparent)]
    Handshake(#[from] gix_protocol::handshake::Error),
    #[error(transparent)]
    Transport(#[from] gix_protocol::transport::client::Error),
    #[error(transparent)]
    ConfigureTransport(#[from] Box<dyn std::error::Error + Send + Sync + 'static>),
    #[error(transparent)]
    GatherTransportConfig(#[from] crate::config::transport::Error),
    #[error(transparent)]
    Fetch(#[from] gix_protocol::fetch::Error),
    #[error("Failed to mark the pack of a lazy fetch as promisor at \"{}\"", path.display())]
    WritePromisorFile {
        path: std::path::PathBuf,
        source: std::io::Error,
    },
}

/// Fetch exactly the objects named by `ids` from `promisor` into `repo`, returning `true` if a pack arrived.
///
/// This is git's
/// `git -c fetch.negotiationAlgorithm=noop fetch <remote> --no-tags --no-write-fetch-head --recurse-submodules=no --filter=<spec> --stdin`,
/// as `promisor-remote.c`'s `fetch_objects()` spawns it, done in-process:
///
/// * no negotiation - the wants are sent with `done` in a single round, which is what `noop` amounts to,
/// * no tags,
/// * no reference is updated, so there is no `FETCH_HEAD` and no tracking ref to write,
/// * the same filter the partial clone used, so the remote doesn't re-send what was skipped on purpose,
/// * the resulting pack is marked `.promisor`, because it too may lack objects.
#[cfg(feature = "blocking-network-client")]
pub fn fetch(repo: &crate::Repository, promisor: &Remote, ids: &[gix_hash::ObjectId]) -> Result<bool, Error> {
    use std::sync::atomic::AtomicBool;

    if ids.is_empty() {
        return Ok(false);
    }
    let remote = repo.find_remote(promisor.name.as_str())?;
    let mut con = remote.connect(crate::remote::Direction::Fetch)?;

    let mut credentials_storage;
    let url = con.transport.inner.to_url();
    let remote_name = crate::bstr::BStr::new(promisor.name.as_str());
    let transport_options = repo.transport_options(url.as_ref(), Some(remote_name))?;
    let authenticate = match con.authenticate.as_mut() {
        Some(f) => f,
        None => {
            credentials_storage = con.configured_credentials_for_current_url();
            &mut credentials_storage
        }
    };
    if let Some(config) = transport_options.as_ref() {
        con.transport.inner.configure(&**config)?;
    }

    // No `ls-refs`: protocol v2 lets the `fetch` command stand on its own, and we know the object ids
    // already. A v1 server sends its advertisement as part of the handshake, which is harmless here.
    let mut progress = crate::progress::Discard;
    let mut handshake = gix_protocol::handshake(
        &mut con.transport.inner,
        gix_transport::Service::UploadPack,
        authenticate,
        Vec::new(),
        &mut progress,
    )?;

    let write_pack_options = gix_pack::bundle::write::Options {
        thread_limit: None,
        index_version: gix_pack::index::Version::V2,
        iteration_mode: gix_pack::data::input::Mode::Verify,
        object_hash: repo.object_hash(),
        alloc_limit_bytes: repo.config.alloc_limit_bytes,
        compression: repo.config.loose_compression,
    };
    let should_interrupt = AtomicBool::new(false);
    let mut write_pack_bundle = None;
    let mut negotiate = WantOnly { ids };

    let outcome = gix_protocol::fetch(
        &mut negotiate,
        |reader, progress, should_interrupt| -> Result<bool, gix_pack::bundle::write::Error> {
            write_pack_bundle = Some(gix_pack::Bundle::write_to_directory(
                reader,
                Some(&repo.objects.store_ref().path().join("pack")),
                progress,
                should_interrupt,
                Some(Box::new(repo.objects.clone())),
                write_pack_options,
            )?);
            Ok(true)
        },
        &mut progress,
        &should_interrupt,
        gix_protocol::fetch::Context {
            handshake: &mut handshake,
            transport: &mut con.transport.inner,
            user_agent: repo.config.user_agent_tuple(),
            trace_packetlines: con.trace,
            server_options: Vec::new(),
        },
        gix_protocol::fetch::Options {
            shallow_file: repo.shallow_file(),
            shallow: &gix_protocol::fetch::Shallow::NoChange,
            tags: gix_protocol::fetch::Tags::None,
            reject_shallow_remote: false,
            filter: promisor.filter.as_deref(),
        },
    )?;

    if matches!(handshake.server_protocol_version, gix_protocol::transport::Protocol::V2) {
        gix_protocol::indicate_end_of_interaction(&mut con.transport.inner, con.trace).ok();
    }

    let Some(bundle) = write_pack_bundle.filter(|_| outcome.is_some()) else {
        return Ok(false);
    };
    // `write_promisor_file()` with no sought refs, i.e. an empty marker - the same file git's lazy
    // fetch leaves behind. The `.keep` file may go, as the objects are needed right now and nothing
    // is going to collect a pack we are about to read from.
    if let Some(index_path) = bundle.index_path.as_deref() {
        let path = index_path.with_extension("promisor");
        std::fs::write(&path, "").map_err(|err| Error::WritePromisorFile { path, source: err })?;
    }
    if let Some(keep_path) = bundle.keep_path.as_deref() {
        std::fs::remove_file(keep_path).ok();
    }
    Ok(bundle.index.num_objects > 0)
}

/// A [negotiator](gix_protocol::fetch::Negotiate) that asks for a fixed set of objects and nothing else.
///
/// It is git's `fetch.negotiationAlgorithm=noop`, which is what `fetch_objects()` forces for a promisor
/// fetch: sending `have` lines would be pointless, because what we are missing has nothing to do with
/// which commits we already share.
#[cfg(feature = "blocking-network-client")]
struct WantOnly<'a> {
    ids: &'a [gix_hash::ObjectId],
}

#[cfg(feature = "blocking-network-client")]
impl gix_protocol::fetch::Negotiate for WantOnly<'_> {
    fn mark_complete_and_common_ref(
        &mut self,
    ) -> Result<gix_protocol::fetch::negotiate::Action, gix_protocol::fetch::negotiate::Error> {
        Ok(gix_protocol::fetch::negotiate::Action::MustNegotiate {
            remote_ref_target_known: Vec::new(),
        })
    }

    fn add_wants(&mut self, arguments: &mut gix_protocol::fetch::Arguments, _remote_ref_target_known: &[bool]) -> bool {
        for id in self.ids {
            arguments.want(id);
        }
        true
    }

    fn one_round(
        &mut self,
        _state: &mut gix_protocol::fetch::negotiate::one_round::State,
        _arguments: &mut gix_protocol::fetch::Arguments,
        _previous_response: Option<&gix_protocol::fetch::Response>,
    ) -> Result<(gix_protocol::fetch::negotiate::Round, bool), gix_protocol::fetch::negotiate::Error> {
        Ok((
            gix_protocol::fetch::negotiate::Round {
                haves_sent: 0,
                in_vain: 0,
                haves_to_send: 0,
                previous_response_had_at_least_one_in_common: false,
            },
            true,
        ))
    }
}

// ---------------------------------------------------------------------------
// The protocol-v2 `promisor-remote` capability, client half.
//
// A server configured to advertise offers the promisor remotes it uses as
// `promisor-remote=<pr-info>` in its capability advertisement. The client
// decides which of them it is willing to use itself and answers with
// `promisor-remote=<pr-names>` as a capability line on its next command
// request. This is `promisor-remote.c`'s `promisor_remote_reply()` and the
// `filter_promisor_remote()` underneath it. Only the client half lives here:
// the serving side needs a protocol-v2 `serve` loop, which this tree has not
// got, so the two server-side keys are left unread rather than half-honoured.
// ---------------------------------------------------------------------------

/// The mandatory field naming the remote, first in every `pr-fields` group.
const FIELD_NAME: &str = "name";
/// The mandatory field naming the remote's URL, second in every `pr-fields` group.
const FIELD_URL: &str = "url";
/// `remote.<name>.partialCloneFilter`, an optional field.
const FIELD_FILTER: &str = "partialCloneFilter";
/// `remote.<name>.token`, an optional field.
const FIELD_TOKEN: &str = "token";

/// The optional fields the three field-name lists in the `promisor` config section may name.
///
/// git's `known_fields[]`: anything else in one of those lists is a configuration mistake and is
/// dropped with a warning, and anything else on the wire is ignored silently.
const KNOWN_FIELDS: &[&str] = &[FIELD_FILTER, FIELD_TOKEN];

/// git's `allow_unsanitized()`: which bytes survive [`urlencode()`] as themselves.
///
/// `,` and `;` separate the fields and the groups, and `%` introduces an escape, so all three have to
/// go even though they are printable.
fn allow_unsanitized(byte: u8) -> bool {
    byte != b',' && byte != b';' && byte != b'%' && byte > 32 && byte < 127
}

/// `strbuf_addstr_urlencode(sb, s, allow_unsanitized)`.
fn urlencode(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    for byte in input.as_bytes() {
        if allow_unsanitized(*byte) {
            out.push(*byte as char);
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

/// `url_percent_decode()`: `%XX` becomes the byte it names, anything else is kept verbatim.
fn percent_decode(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let decoded = (bytes[i] == b'%' && i + 2 < bytes.len())
            .then(|| std::str::from_utf8(&bytes[i + 1..i + 3]).ok())
            .flatten()
            .and_then(|hex| u8::from_str_radix(hex, 16).ok());
        match decoded {
            Some(byte) => {
                out.push(byte);
                i += 3;
            }
            None => {
                out.push(bytes[i]);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// git's `struct promisor_info`: one promisor remote, from the wire or from the configuration.
///
/// Every member but `name` mirrors a `remote.<name>.<member>` configuration variable.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
struct Info {
    name: String,
    url: String,
    filter: Option<String>,
    token: Option<String>,
}

impl Info {
    /// The value this remote advertises (or has configured) for `field`, if any.
    fn field(&self, field: &str) -> Option<&str> {
        if field.eq_ignore_ascii_case(FIELD_FILTER) {
            self.filter.as_deref()
        } else if field.eq_ignore_ascii_case(FIELD_TOKEN) {
            self.token.as_deref()
        } else {
            None
        }
    }
}

/// git's `promisor.acceptFromServer` policy, `enum accept_promisor`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Accept {
    /// Accept nothing, the default.
    None,
    /// Accept a remote configured locally under the same name *and* the same URL.
    KnownUrl,
    /// Accept a remote configured locally under the same name, whatever its URL.
    KnownName,
    /// Accept every advertised remote, configured locally or not.
    All,
}

/// `accept_from_server()`: read `promisor.acceptFromServer`.
///
/// An unrecognized value is a warning and means [`Accept::None`], so a typo never widens what is
/// accepted.
fn accept_from_server(repo: &crate::Repository) -> Accept {
    let Some(value) = repo.config.resolved.string("promisor.acceptFromServer") else {
        return Accept::None;
    };
    let value = value.to_str_lossy();
    if value.is_empty() || value.eq_ignore_ascii_case("None") {
        Accept::None
    } else if value.eq_ignore_ascii_case("KnownUrl") {
        Accept::KnownUrl
    } else if value.eq_ignore_ascii_case("KnownName") {
        Accept::KnownName
    } else if value.eq_ignore_ascii_case("All") {
        Accept::All
    } else {
        eprintln!("warning: unknown '{value}' value for 'promisor.acceptfromserver' config option");
        Accept::None
    }
}

/// `fields_from_config()`: the field names `config_key` lists, minus the ones we do not know.
///
/// git splits on `,` only - despite what the documentation says about spaces - trims each element and
/// drops the empty ones, then warns about every name outside [`KNOWN_FIELDS`].
fn fields_from_config(repo: &crate::Repository, config_key: &str) -> Vec<String> {
    let Some(value) = repo.config.resolved.string(config_key) else {
        return Vec::new();
    };
    value
        .to_str_lossy()
        .split(',')
        .map(str::trim)
        .filter(|field| !field.is_empty())
        .filter(|field| {
            let known = KNOWN_FIELDS.iter().any(|known| known.eq_ignore_ascii_case(field));
            if !known {
                eprintln!("warning: unsupported field '{field}' in '{config_key}' config");
            }
            known
        })
        .map(ToOwned::to_owned)
        .collect()
}

/// `promisor_config_info_list()`: the locally configured promisor remotes that have a URL.
///
/// Only the fields named by `field_names` are read on top of the mandatory `name` and `url`, which is
/// what keeps `promisor.checkFields` and `promisor.storeFields` from reading each other's variables.
fn config_info_list(repo: &crate::Repository, field_names: &[String]) -> Vec<Info> {
    let config = &repo.config.resolved;
    remotes(repo)
        .into_iter()
        .filter_map(|remote| {
            let name = crate::bstr::BStr::new(remote.name.as_str());
            let url = config.string_by("remote", Some(name), "url")?.to_str_lossy().into_owned();
            if url.is_empty() {
                return None;
            }
            let mut info = Info {
                name: remote.name.clone(),
                url,
                ..Info::default()
            };
            for field in field_names {
                let value = config
                    .string_by("remote", Some(name), field.as_str())
                    .map(|v| v.to_str_lossy().into_owned())
                    .filter(|v| !v.is_empty());
                if let Some(value) = value {
                    if field.eq_ignore_ascii_case(FIELD_FILTER) {
                        info.filter = Some(value);
                    } else if field.eq_ignore_ascii_case(FIELD_TOKEN) {
                        info.token = Some(value);
                    }
                }
            }
            Some(info)
        })
        .collect()
}

/// `parse_one_advertised_remote()`: one `pr-fields` group.
///
/// Unknown field names are ignored, as the protocol requires, and a group without both a `name` and a
/// `url` is refused with the warning git prints.
fn parse_one_advertised_remote(remote_info: &str) -> Option<Info> {
    let mut info = Info::default();
    for elem in remote_info.split(',') {
        let Some((field, value)) = elem.split_once('=') else {
            eprintln!("warning: invalid element '{elem}' from remote info");
            continue;
        };
        if field == FIELD_NAME {
            info.name = percent_decode(value);
        } else if field == FIELD_URL {
            info.url = percent_decode(value);
        } else if field == FIELD_FILTER {
            info.filter = Some(percent_decode(value));
        } else if field == FIELD_TOKEN {
            info.token = Some(percent_decode(value));
        }
    }
    if info.name.is_empty() || info.url.is_empty() {
        eprintln!(
            "warning: server advertised a promisor remote without a name or URL: '{remote_info}', ignoring this remote"
        );
        return None;
    }
    Some(info)
}

/// `all_fields_match()`: every field in `promisor.checkFields` matches the local configuration.
///
/// With `entry` set the comparison is against that one remote; without it - the `all` policy - a field
/// may match any configured promisor remote. A field the server did not advertise never matches, so a
/// `checkFields` naming something the server is silent about rejects the remote outright.
fn all_fields_match(advertised: &Info, config_info: &[Info], entry: Option<&Info>, checked: &[String]) -> bool {
    checked.iter().all(|field| {
        let Some(value) = advertised.field(field) else {
            return false;
        };
        match entry {
            Some(entry) => entry.field(field) == Some(value),
            None => config_info.iter().any(|info| info.field(field) == Some(value)),
        }
    })
}

/// `should_accept_remote()`: apply the `promisor.acceptFromServer` policy to one advertised remote.
fn should_accept_remote(accept: Accept, advertised: &Info, config_info: &[Info], checked: &[String]) -> bool {
    if accept == Accept::All {
        return all_fields_match(advertised, config_info, None, checked);
    }
    let Some(entry) = config_info.iter().find(|info| info.name == advertised.name) else {
        // We don't know about that remote.
        return false;
    };
    if accept == Accept::KnownUrl && entry.url != advertised.url {
        eprintln!(
            "warning: known remote named '{}' but with URL '{}' instead of '{}', ignoring this remote",
            advertised.name, entry.url, advertised.url
        );
        return false;
    }
    all_fields_match(advertised, config_info, Some(entry), checked)
}

/// `valid_filter()`: a filter-spec is only stored if it parses.
fn valid_filter(filter: &str, remote_name: &str) -> bool {
    match gix_protocol::fetch::filter::parse(filter) {
        Ok(_) => true,
        Err(err) => {
            eprintln!("warning: invalid filter '{filter}' for remote '{remote_name}' will not be stored: {err}");
            false
        }
    }
}

/// `valid_token()`: a token is only stored if it would not smuggle control characters into the config.
fn valid_token(token: &str, remote_name: &str) -> bool {
    if token.chars().any(char::is_control) {
        eprintln!("warning: invalid token '{token}' for remote '{remote_name}' will not be stored");
        return false;
    }
    true
}

/// `store_one_field()`: write `remote.<remote_name>.<field_key>` when the server named something new.
///
/// Returns `true` when the configuration was changed. The message on stderr is git's, and is the only
/// notice a user gets that a server just rewrote one of their remotes.
fn store_one_field(
    repo: &crate::Repository,
    remote_name: &str,
    field_name: &str,
    field_key: &str,
    advertised: &str,
    current: Option<&str>,
) -> bool {
    if current == Some(advertised) {
        return false;
    }
    eprintln!(
        "Storing new {field_name} from server for remote '{remote_name}'.\n    '{}' -> '{advertised}'",
        current.unwrap_or_default()
    );
    set_local_config(repo, remote_name, field_key, advertised).is_ok()
}

/// `repo_config_set_gently()` for a single `remote.<name>.<key>`, written to the repository's own config.
fn set_local_config(repo: &crate::Repository, remote_name: &str, key: &str, value: &str) -> std::io::Result<()> {
    let path = repo.common_dir().join("config");
    let mut file = gix_config::File::from_path_no_includes(path.clone(), gix_config::Source::Local)
        .map_err(std::io::Error::other)?;
    file.set_raw_value_by("remote", Some(crate::bstr::BStr::new(remote_name)), key, value)
        .map_err(std::io::Error::other)?;
    let tmp = path.with_extension("promisor-tmp");
    std::fs::write(&tmp, file.to_bstring())?;
    std::fs::rename(&tmp, &path)
}

/// `filter_promisor_remote()` and `promisor_remote_reply()`: decide what to accept from `info`.
///
/// `info` is the `<pr-info>` value of the server's `promisor-remote` capability. The return value is
/// the `<pr-names>` the client answers with, or `None` when it accepts nothing - in which case git
/// does not send the capability at all.
///
/// Storing a field here really does rewrite the local configuration, which is why
/// `promisor.storeFields` only ever touches remotes that already exist locally.
pub fn capability_reply(repo: &crate::Repository, info: &str) -> Option<String> {
    let accept = accept_from_server(repo);
    if accept == Accept::None {
        return None;
    }

    let checked = fields_from_config(repo, "promisor.checkFields");
    let config_info = config_info_list(repo, &checked);

    let stored = fields_from_config(repo, "promisor.storeFields");
    let store_filter = stored.iter().any(|field| field.eq_ignore_ascii_case(FIELD_FILTER));
    let store_token = stored.iter().any(|field| field.eq_ignore_ascii_case(FIELD_TOKEN));
    // git builds this second list only when something was accepted; it is separate from `config_info`
    // because `checkFields` and `storeFields` need not name the same fields.
    let store_config_info = (store_filter || store_token).then(|| config_info_list(repo, &stored));

    let mut accepted: Vec<String> = Vec::new();
    for group in info.split(';') {
        let Some(advertised) = parse_one_advertised_remote(group) else {
            continue;
        };
        if !should_accept_remote(accept, &advertised, &config_info, &checked) {
            continue;
        }
        if let Some(store_config_info) = store_config_info.as_ref() {
            if let Some(local) = store_config_info.iter().find(|info| info.name == advertised.name) {
                if store_filter {
                    if let Some(filter) = advertised.filter.as_deref().filter(|f| valid_filter(f, &advertised.name)) {
                        store_one_field(
                            repo,
                            &advertised.name,
                            "filter",
                            FIELD_FILTER,
                            filter,
                            local.filter.as_deref(),
                        );
                    }
                }
                if store_token {
                    if let Some(token) = advertised.token.as_deref().filter(|t| valid_token(t, &advertised.name)) {
                        store_one_field(repo, &advertised.name, "token", FIELD_TOKEN, token, local.token.as_deref());
                    }
                }
            }
        }
        accepted.push(advertised.name);
    }

    (!accepted.is_empty()).then(|| {
        accepted
            .iter()
            .map(|name| urlencode(name))
            .collect::<Vec<_>>()
            .join(";")
    })
}

#[cfg(test)]
mod capability_tests {
    use super::{percent_decode, urlencode};

    /// The three separators the protocol reserves have to survive a round trip, which is the whole
    /// point of `allow_unsanitized()` refusing them.
    #[test]
    fn separators_round_trip() {
        let raw = "a,b;c%d e";
        assert_eq!(urlencode(raw), "a%2Cb%3Bc%25d%20e");
        assert_eq!(percent_decode(&urlencode(raw)), raw);
    }

    /// A stray `%` that does not introduce two hex digits is data, not an escape.
    #[test]
    fn malformed_escape_is_kept_verbatim() {
        assert_eq!(percent_decode("100%"), "100%");
        assert_eq!(percent_decode("%zz"), "%zz");
    }
}

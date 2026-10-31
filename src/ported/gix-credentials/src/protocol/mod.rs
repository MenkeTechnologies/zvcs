use bstr::BString;

use crate::helper;

/// The outcome of the credentials top-level functions to obtain a complete identity.
#[derive(Debug, Clone, Eq, PartialEq)]
pub struct Outcome {
    /// The identity provide by the helper.
    pub identity: gix_sec::identity::Account,
    /// A handle to the action to perform next in another call to [`helper::invoke()`][crate::helper::invoke()].
    pub next: helper::NextAction,
    /// `<authtype> <credential>`, the value of the `Authorization` header to send instead of one built from the
    /// identity, when the helper answered with both (http.c's `http_append_auth_header()`, :625-636).
    pub authorization: Option<String>,
}

/// The Result type used in credentials top-level functions to obtain a complete identity.
pub type Result = std::result::Result<Option<Outcome>, Error>;

/// The error returned top-level credential functions.
#[derive(Debug, thiserror::Error)]
#[expect(missing_docs)]
pub enum Error {
    #[error(transparent)]
    UrlParse(#[from] gix_url::parse::Error),
    #[error("Either 'url' field or both 'protocol' and 'host' fields must be provided")]
    UrlMissing,
    #[error(transparent)]
    ContextDecode(#[from] context::decode::Error),
    #[error(transparent)]
    InvokeHelper(#[from] helper::Error),
    #[error("Could not configure credential helpers")]
    ConfigureCredentialHelpers {
        #[source]
        source: Box<dyn std::error::Error + Send + Sync + 'static>,
    },
    #[error("Could not obtain identity for context: {}", { let mut buf = Vec::<u8>::new(); context.write_to(&mut buf).ok(); String::from_utf8_lossy(&buf).into_owned() })]
    IdentityMissing { context: Context },
    #[error("The handler asked to stop trying to obtain credentials")]
    Quit,
    #[error("Couldn't obtain {prompt}")]
    Prompt { prompt: String, source: gix_prompt::Error },
    /// The fill ended the process it ran in with `code`, as `credential_fill()` does with its `die()`s
    /// (credential.c:504-547, prompt.c:73). Only a caller that runs the fill in-process where git runs it in a child, as git
    /// does in remote-curl, can see this rather than its own exit.
    #[error("The credential fill exited with status {code}")]
    Exited { code: i32 },
}

/// Additional context to be passed to the credentials helper.
#[derive(Debug, Default, Clone, Eq, PartialEq)]
pub struct Context {
    /// Options controlling how this context is encoded and decoded.
    pub options: ContextOptions,
    /// The protocol over which the credential will be used (e.g., https).
    pub protocol: Option<String>,
    /// The remote hostname for a network credential. This includes the port number if one was specified (e.g., "example.com:8088").
    pub host: Option<String>,
    /// The path with which the credential will be used. E.g., for accessing a remote https repository, this will be the repository’s path on the server.
    /// It can also be a path on the file system.
    pub path: Option<BString>,
    /// The credential’s username, if we already have one (e.g., from a URL, the configuration, the user, or from a previously run helper).
    pub username: Option<String>,
    /// The credential’s password, if we are asking it to be stored.
    pub password: Option<String>,
    /// An OAuth refresh token that may accompany a password. It is to be treated confidentially, just like the password.
    pub oauth_refresh_token: Option<String>,
    /// The expiry date of OAuth tokens as seconds from Unix epoch.
    pub password_expiry_utc: Option<gix_date::SecondsSinceUnixEpoch>,
    /// When this special attribute is read by git credential, the value is parsed as a URL and treated as if its constituent
    /// parts were read (e.g., url=<https://example.com> would behave as if
    /// protocol=https and host=example.com had been provided). This can help callers avoid parsing URLs themselves.
    pub url: Option<BString>,
    /// If true, the caller should stop asking for credentials immediately without calling more credential helpers in the chain.
    pub quit: Option<bool>,
    /// `capability[]=authtype`, credential.h's `capa_authtype`: whether the caller and the helper said they
    /// understand `authtype` and `credential`.
    pub capa_authtype: Capability,
    /// `capability[]=state`, credential.h's `capa_state`: whether the caller and the helper said they
    /// understand `state[]` and `continue`.
    pub capa_state: Capability,
    /// The authentication scheme `credential` is for, such as `Bearer` (credential.h's `authtype`).
    pub authtype: Option<String>,
    /// A pre-encoded credential sent as `Authorization: <authtype> <credential>`. It is as secret as a password.
    pub credential: Option<String>,
    /// `ephemeral=1`: the credential is not to be stored.
    pub ephemeral: bool,
    /// One `WWW-Authenticate` header value of the server's last response per entry, in order
    /// (credential.h's `wwwauth_headers`, written as `wwwauth[]`).
    pub wwwauth: Vec<BString>,
    /// The `state[]` lines the last helper answered with (credential.h's `state_headers`).
    pub state: Vec<BString>,
    /// The `state[]` lines to hand to the next helper invocation (credential.h's `state_headers_to_send`),
    /// filled from [`state`][Self::state] by [`next_state()`][Self::next_state()].
    pub state_to_send: Vec<BString>,
    /// `continue=1`: the helper takes part in a multi-stage exchange (credential.h's `multistage`).
    pub multistage: bool,
}

/// Which side of the credential protocol a [`Context`] is read from or written to, credential.h's
/// `enum credential_op_type`. It decides which `capability[]` announcements apply.
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum CredentialOp {
    /// `CREDENTIAL_OP_INITIAL`: what the caller of `git credential` (or the transport) asked for.
    Initial,
    /// `CREDENTIAL_OP_HELPER`: a `get` written to a helper, or a helper's answer read back.
    Helper,
    /// `CREDENTIAL_OP_RESPONSE`: a `store`/`erase` written to a helper, or the answer written to the caller.
    Response,
}

/// One capability's state along the chain, credential.h's `struct credential_capability`.
#[derive(Debug, Default, Clone, Copy, Eq, PartialEq)]
pub struct Capability {
    /// The caller announced it.
    pub request_initial: bool,
    /// A helper announced it in its answer.
    pub request_helper: bool,
    /// The answer to the caller announced it.
    pub response: bool,
}

impl Capability {
    /// `credential_set_capability()` (credential.c:56-69).
    pub fn set(&mut self, op: CredentialOp) {
        match op {
            CredentialOp::Initial => self.request_initial = true,
            CredentialOp::Helper => self.request_helper = true,
            CredentialOp::Response => self.response = true,
        }
    }

    /// `credential_has_capability()` (credential.c:296-310): pass a capability along only if every step before
    /// `op` had it.
    pub fn has(&self, op: CredentialOp) -> bool {
        match op {
            CredentialOp::Helper => self.request_initial,
            CredentialOp::Response => self.request_initial && self.request_helper,
            CredentialOp::Initial => false,
        }
    }
}

/// Options for encoding and decoding a [`Context`].
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub struct ContextOptions {
    /// If true, carriage returns in credential values are rejected to protect credential-protocol parsing.
    ///
    /// NUL bytes and newlines are always rejected.
    pub protect_protocol: bool,
}

impl Default for ContextOptions {
    fn default() -> Self {
        ContextOptions { protect_protocol: true }
    }
}

/// Convert the outcome of a helper invocation to a helper result, assuring that the identity is complete in the process.
#[expect(
    clippy::result_large_err,
    reason = "will be removed once `gix-error` is used consistently"
)]
pub fn helper_outcome_to_result(outcome: Option<helper::Outcome>, action: helper::Action) -> Result {
    match (action, outcome) {
        (helper::Action::Get(ctx), None) => Err(Error::IdentityMissing {
            context: ctx.redacted(),
        }),
        (helper::Action::Get(ctx), Some(mut outcome)) => {
            let authorization = ctx
                .authtype
                .as_ref()
                .zip(ctx.credential.as_ref())
                .map(|(authtype, credential)| format!("{authtype} {credential}"));
            match outcome.consume_identity() {
                Some(identity) => Ok(Some(Outcome {
                    identity,
                    next: outcome.next,
                    authorization,
                })),
                // `credential_fill()` is done once a helper produced a `credential` (credential.c:533-536); the
                // request is then signed with it alone.
                None if authorization.is_some() && !outcome.quit => Ok(Some(Outcome {
                    identity: gix_sec::identity::Account {
                        username: ctx.username.clone().unwrap_or_default(),
                        password: String::new(),
                        oauth_refresh_token: None,
                    },
                    next: outcome.next,
                    authorization,
                })),
                None => Err(if outcome.quit {
                    Error::Quit
                } else {
                    Error::IdentityMissing {
                        context: ctx.redacted(),
                    }
                }),
            }
        }
        (helper::Action::Store(_) | helper::Action::Erase(_), _ignore) => Ok(None),
    }
}

///
pub mod context;

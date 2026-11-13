use bstr::ByteSlice;

use crate::{File, KeyRef, file, file::init, parse::section, path::interpolate};

/// Represents the errors that may occur when calling [`File::from_env()`].
#[derive(Debug, thiserror::Error)]
#[expect(missing_docs)]
pub enum Error {
    #[error("Configuration {kind} at index {index} contained illformed UTF-8")]
    IllformedUtf8 { index: usize, kind: &'static str },
    #[error("GIT_CONFIG_COUNT was not a positive integer: {}", .input)]
    InvalidConfigCount { input: String },
    #[error("GIT_CONFIG_KEY_{} was not set", .key_id)]
    InvalidKeyId { key_id: usize },
    #[error("GIT_CONFIG_KEY_{} was set to an invalid value: {}", .key_id, .key_val)]
    InvalidKeyValue { key_id: usize, key_val: String },
    #[error("GIT_CONFIG_VALUE_{} was not set", .value_id)]
    InvalidValueId { value_id: usize },
    #[error(transparent)]
    PathInterpolationError(#[from] interpolate::Error),
    #[error(transparent)]
    Includes(#[from] init::includes::Error),
    #[error(transparent)]
    Section(#[from] section::header::Error),
    #[error(transparent)]
    SectionValue(#[from] file::section::value::Error),
}

/// Instantiation from environment variables
impl File {
    /// Generates a config from `GIT_CONFIG_*` environment variables or returns `Ok(None)` if no configuration was found.
    /// See [`git-config`'s documentation] for more information on the environment variables in question.
    ///
    /// With `options` configured, it's possible to resolve `include.path` or `includeIf.<condition>.path` directives as well.
    ///
    /// This is `git_config_from_parameters()` (config.c:731-790, git v2.56.0): the
    /// `GIT_CONFIG_COUNT` / `GIT_CONFIG_KEY_<n>` / `GIT_CONFIG_VALUE_<n>` entries first,
    /// then every entry of `GIT_CONFIG_PARAMETERS`, the list `git -c` and `--config-env`
    /// append to (`git_config_push_split_parameter()`, config.c:449-467) and every child
    /// inherits. Reading both here is what lets each command-line setting reach the
    /// configuration exactly once, in the order git hands it to a config callback, with
    /// its bytes as typed and a valueless `-c <key>` as the implicit `true`.
    ///
    /// [`git-config`'s documentation]: https://git-scm.com/docs/git-config#Documentation/git-config.txt-GITCONFIGCOUNT
    pub fn from_env(options: init::Options<'_>) -> Result<Option<File>, Error> {
        use std::env;
        let count: usize = match env::var("GIT_CONFIG_COUNT") {
            Ok(v) => v.parse().map_err(|_| Error::InvalidConfigCount { input: v })?,
            Err(_) => 0,
        };
        let parameters = parameters_from_env();

        if count == 0 && parameters.is_empty() {
            return Ok(None);
        }

        let meta = file::Metadata {
            path: None,
            source: crate::Source::Env,
            level: 0,
            trust: gix_sec::Trust::Full,
        };
        let mut config = File::new(meta);
        for i in 0..count {
            let key = gix_path::os_string_into_bstring(
                env::var_os(format!("GIT_CONFIG_KEY_{i}")).ok_or(Error::InvalidKeyId { key_id: i })?,
            )
            .map_err(|_| Error::IllformedUtf8 { index: i, kind: "key" })?;
            let value = env::var_os(format!("GIT_CONFIG_VALUE_{i}")).ok_or(Error::InvalidValueId { value_id: i })?;
            let key = KeyRef::parse_unvalidated(key.as_ref()).ok_or_else(|| Error::InvalidKeyValue {
                key_id: i,
                key_val: key.to_string(),
            })?;

            // One section per entry, in `GIT_CONFIG_KEY_<n>` order: git hands the
            // triple to the callback one entry at a time (`git_config_from_parameters()`,
            // config.c:731-790), and an include is read where its line is. Folding a
            // key into an earlier section of the same name reordered the walk and put
            // an included file behind every later entry of the including section.
            config
                .new_section(key.section_name, key.subsection_name.map(ToOwned::to_owned))?
                .push(
                    key.value_name,
                    Some(
                        gix_path::os_str_into_bstr(&value)
                            .map_err(|_| Error::IllformedUtf8 {
                                index: i,
                                kind: "value",
                            })?
                            .as_bytes()
                            .into(),
                    ),
                )?;
        }
        for (key, value) in &parameters {
            let key = KeyRef::parse_unvalidated(key.as_bytes().as_bstr())
                .expect("BUG: parameters_from_env() keeps only keys that parse");
            config
                .new_section(key.section_name, key.subsection_name.map(ToOwned::to_owned))?
                .push(key.value_name, value.as_deref().map(|v| v.as_bytes().as_bstr()))?;
        }

        let mut buf = Vec::new();
        init::includes::resolve(&mut config, &mut buf, options)?;
        Ok(Some(config))
    }
}

/// The entries of `GIT_CONFIG_PARAMETERS`, as `(key, value)` with `None` for git's
/// implicit-bool `NULL` value: `parse_config_env_list()` (config.c:679-725, git v2.56.0).
///
/// Each entry is a single-quoted run (`sq_dequote_step()`, quote.c:123-167). Followed by
/// whitespace or the end it is the old style, `'key=value'`, split at its first `=`
/// (`git_config_parse_parameter()`, config.c:651-677); followed by `=` it is the new
/// style, `'key'='value'` or the bare `'key'=`, whose key is kept whole and may contain
/// an `=`. git `error()`s on a malformed list or an empty key and dies before it uses
/// any of it; that diagnosis is the caller's, so reading stops there and keeps only the
/// entries before it.
fn parameters_from_env() -> Vec<(String, Option<String>)> {
    let mut out = Vec::new();
    let Some(raw) = std::env::var_os("GIT_CONFIG_PARAMETERS") else {
        return out;
    };
    let Some(raw) = raw.to_str() else { return out };
    let mut rest = raw;
    while !rest.is_empty() {
        let Some((key, after)) = sq_dequote_step(rest) else { break };
        let (key, value, next) = match after.as_bytes().first() {
            // old-style 'key=value'
            None => old_style(key, after),
            Some(c) if c.is_ascii_whitespace() => old_style(key, after),
            // new-style 'key'='value'
            Some(b'=') => {
                let tail = &after[1..];
                match tail.as_bytes().first() {
                    Some(b'\'') => match sq_dequote_step(tail) {
                        Some((value, next)) if next.is_empty() || next.as_bytes()[0].is_ascii_whitespace() => {
                            (key, Some(value), next)
                        }
                        _ => break,
                    },
                    // implicit bool: 'key'=
                    None => (key, None, tail),
                    Some(c) if c.is_ascii_whitespace() => (key, None, tail),
                    Some(_) => break,
                }
            }
            Some(_) => break,
        };
        // `config_parse_pair()` (config.c:609-626): an empty or unparsable key fails the
        // whole read.
        if key.is_empty() || KeyRef::parse_unvalidated(key.as_bytes().as_bstr()).is_none() {
            break;
        }
        out.push((key, value));
        rest = next.trim_start_matches(|c: char| c.is_ascii_whitespace());
    }
    out
}

/// `string_list_split(&pair, text, "=", 1)` (config.c:661): the old-style entry is split
/// at its first `=`, and one with none is the implicit bool.
fn old_style(key: String, next: &str) -> (String, Option<String>, &str) {
    match key.split_once('=') {
        Some((k, v)) => (k.to_owned(), Some(v.to_owned()), next),
        None => (key, None, next),
    }
}

/// `sq_dequote_step()` (quote.c:123-167): one single-quoted run, unquoted, and the input
/// after it. Outside the quotes only `\'` and `\!` may appear, each directly re-opening
/// the run (`need_bs_quote()`). `None` when `src` does not start with a quote or the run
/// is never closed.
fn sq_dequote_step(src: &str) -> Option<(String, &str)> {
    let mut rest = src.strip_prefix('\'')?;
    let mut out = String::new();
    loop {
        let end = rest.find('\'')?;
        out.push_str(&rest[..end]);
        rest = &rest[end + 1..];
        let b = rest.as_bytes();
        if b.len() >= 3 && b[0] == b'\\' && matches!(b[1], b'\'' | b'!') && b[2] == b'\'' {
            out.push(b[1] as char);
            rest = &rest[3..];
        } else {
            return Some((out, rest));
        }
    }
}

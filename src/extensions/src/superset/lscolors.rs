//! eza-compatible color theme, parsed from `LS_COLORS` + `EXA_COLORS`/`EZA_COLORS`.
//!
//! eza colors its output from three sources, applied in order: its built-in
//! defaults, then `LS_COLORS` (file kinds and `*.ext` extensions), then
//! `EXA_COLORS`/`EZA_COLORS` (eza's own two-letter keys for permissions, size,
//! date, git, punctuation, …). [`Theme`] reproduces that so `git zls` colors
//! identically to the user's eza — including their perms/git palette — rather
//! than a hardcoded guess.
//!
//! Keys used by the listing: file kinds `di ln ex fi pi so bd cd or`; permission
//! bits `ur uw ux gr gw gx tr tw tx`; `da` date; `sn`/`sb` size number/unit; git
//! `ga gm gd gv gt gi gc`; `xx` punctuation. `*.ext` keys color by extension.

use std::collections::HashMap;

/// A resolved color theme: SGR parameter strings keyed by eza/LS_COLORS code.
pub struct Theme {
    on: bool,
    keys: HashMap<String, String>,
    exts: HashMap<String, String>,
}

impl Theme {
    /// Build the theme: eza-like defaults, overlaid by `LS_COLORS`, then
    /// `EXA_COLORS`, then `EZA_COLORS` (later sources win, as eza resolves them).
    /// `on` is whether color is emitted at all (tty and not `NO_COLOR`).
    pub fn from_env(on: bool) -> Theme {
        let mut keys = default_keys();
        let mut exts = HashMap::new();
        for var in ["LS_COLORS", "EXA_COLORS", "EZA_COLORS"] {
            if let Ok(val) = std::env::var(var) {
                overlay(&val, &mut keys, &mut exts);
            }
        }
        Theme { on, keys, exts }
    }

    /// The SGR parameters for a code (e.g. `"32;1"`), or `""` if unset.
    pub fn sgr(&self, key: &str) -> &str {
        self.keys.get(key).map(String::as_str).unwrap_or("")
    }

    /// The SGR parameters for `name`'s extension (`*.ext`), if any.
    pub fn ext_sgr(&self, name: &str) -> Option<&str> {
        let ext = name.rsplit_once('.').map(|(_, e)| e.to_ascii_lowercase())?;
        self.exts.get(&ext).map(String::as_str)
    }

    /// Wrap `text` in the SGR escape for `params`, or return it unchanged when
    /// color is off or `params` is empty.
    pub fn paint(&self, params: &str, text: &str) -> String {
        if self.on && !params.is_empty() {
            format!("\x1b[{params}m{text}\x1b[0m")
        } else {
            text.to_string()
        }
    }

    pub fn enabled(&self) -> bool {
        self.on
    }
}

/// Parse a `key=value:key=value` list into the key and extension maps. `*.ext`
/// keys populate `exts` (lowercased, without the `*.`); everything else is a
/// plain key. Tokens without `=` (e.g. a leading `reset`) are ignored.
fn overlay(spec: &str, keys: &mut HashMap<String, String>, exts: &mut HashMap<String, String>) {
    for item in spec.split(':') {
        let Some((key, val)) = item.split_once('=') else {
            continue;
        };
        if let Some(ext) = key.strip_prefix("*.") {
            exts.insert(ext.to_ascii_lowercase(), val.to_string());
        } else {
            keys.insert(key.to_string(), val.to_string());
        }
    }
}

/// eza-like built-in defaults, used for any key the environment does not set.
fn default_keys() -> HashMap<String, String> {
    [
        // File kinds.
        ("di", "34"),   // directory — blue
        ("ln", "36"),   // symlink — cyan
        ("ex", "32;1"), // executable — bold green
        ("fi", ""),     // regular file — default
        ("pi", "33"),   // fifo
        ("so", "35"),   // socket
        ("bd", "33"),   // block device
        ("cd", "33"),   // char device
        ("or", "31"),   // broken symlink — red
        // Permission bits: read yellow, write red, execute green.
        ("ur", "33"), ("uw", "31"), ("ux", "32"),
        ("gr", "33"), ("gw", "31"), ("gx", "32"),
        ("tr", "33"), ("tw", "31"), ("tx", "32"),
        // Size, date, inode.
        ("sn", "32"), ("sb", "32"),
        ("da", "34"),
        ("in", "35"),
        // Git status columns.
        ("ga", "32"),   // new — green
        ("gm", "33"),   // modified — yellow
        ("gd", "31"),   // deleted — red
        ("gv", "35"),   // renamed — magenta
        ("gt", "36"),   // type-change — cyan
        ("gi", "90"),   // ignored — grey
        ("gc", "1;31"), // conflicted — bold red
        // Punctuation (the `-` fillers).
        ("xx", "90"),
    ]
    .iter()
    .map(|(k, v)| (k.to_string(), v.to_string()))
    .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn overlay_parses_keys_and_extensions() {
        let mut keys = default_keys();
        let mut exts = HashMap::new();
        // A leading token without `=` (eza allows `reset`) is ignored.
        overlay("reset:di=32;1:ur=32:da=34:*.png=35", &mut keys, &mut exts);
        assert_eq!(keys.get("di").unwrap(), "32;1");
        assert_eq!(keys.get("ur").unwrap(), "32");
        assert_eq!(keys.get("da").unwrap(), "34");
        assert_eq!(exts.get("png").unwrap(), "35");
    }

    #[test]
    fn paint_respects_on_and_empty_params() {
        let on = Theme { on: true, keys: default_keys(), exts: HashMap::new() };
        assert_eq!(on.paint("34", "x"), "\x1b[34mx\x1b[0m");
        assert_eq!(on.paint("", "x"), "x"); // no params → no escape
        let off = Theme { on: false, keys: default_keys(), exts: HashMap::new() };
        assert_eq!(off.paint("34", "x"), "x"); // color disabled → plain
    }

    #[test]
    fn extension_lookup_is_case_insensitive() {
        let mut keys = default_keys();
        let mut exts = HashMap::new();
        overlay("*.png=35", &mut keys, &mut exts);
        let t = Theme { on: true, keys, exts };
        assert_eq!(t.ext_sgr("Photo.PNG"), Some("35"));
        assert_eq!(t.ext_sgr("no-extension"), None);
    }
}

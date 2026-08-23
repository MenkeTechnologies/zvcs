use gix_hash::ObjectId;
use gix_object::bstr::BString;

/// A parsed ref log line that can be changed
#[derive(PartialEq, Eq, Debug, Hash, Ord, PartialOrd, Clone)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Line {
    /// The previous object id. Can be a null-sha to indicate this is a line for a new ref.
    pub previous_oid: ObjectId,
    /// The new object id. Can be a null-sha to indicate this ref is being deleted.
    pub new_oid: ObjectId,
    /// The signature of the currently configured committer.
    pub signature: gix_actor::Signature,
    /// The message providing details about the operation performed in this log line.
    pub message: BString,
}

/// Normalize `message` the way git normalizes every reflog message it is handed.
///
/// Port of `copy_reflog_msg()` (refs.c:1031-1045) as `normalize_reflog_message()`
/// (refs.c:1047-1054) applies it: `ref_transaction_add_update()` runs *every*
/// message through it (refs.c:1342), so a reflog line never carries a run of
/// whitespace, a leading blank, a trailing blank — or a tab, which would otherwise
/// split the line at the very separator the format uses between the committer and
/// the message.
///
/// Each maximal run of whitespace collapses to a single space, a leading run is
/// dropped outright (`wasspace` starts set), and `strbuf_rtrim()` takes the
/// trailing one. The walk is `while ((c = *msg++))`, so it stops at a NUL exactly
/// as the C does.
pub fn normalize_message(message: &gix_object::bstr::BStr) -> BString {
    // C's `isspace()` in the "C" locale: space, \t, \n, \v, \f, \r. Rust's
    // `is_ascii_whitespace()` is the same set minus the vertical tab.
    fn is_space(c: u8) -> bool {
        c.is_ascii_whitespace() || c == 0x0b
    }
    let mut out = BString::default();
    let mut wasspace = true;
    for &c in message.iter() {
        if c == 0 {
            break;
        }
        if wasspace && is_space(c) {
            continue;
        }
        wasspace = is_space(c);
        out.push(if wasspace { b' ' } else { c });
    }
    while out.last().is_some_and(|&c| is_space(c)) {
        out.pop();
    }
    out
}

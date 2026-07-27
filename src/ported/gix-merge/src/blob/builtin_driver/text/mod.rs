use std::num::NonZeroU8;

use bstr::BStr;

/// The way the built-in [text driver](crate::blob::BuiltinDriver::Text) will express
/// merge conflicts in the resulting file.
#[derive(Default, Copy, Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub enum ConflictStyle {
    /// Only show the zealously minified conflicting lines of the local changes and the incoming (other) changes,
    /// hiding the base version entirely.
    ///
    /// ```text
    /// line1-changed-by-both
    /// <<<<<<< local
    /// line2-to-be-changed-in-incoming
    /// =======
    /// line2-changed
    /// >>>>>>> incoming
    /// ```
    #[default]
    Merge,
    /// Show non-minimized hunks of local changes, the base, and the incoming (other) changes.
    ///
    /// This mode does not hide any information.
    ///
    /// ```text
    /// <<<<<<< local
    /// line1-changed-by-both
    /// line2-to-be-changed-in-incoming
    /// ||||||| 9a8d80c
    /// line1-to-be-changed-by-both
    /// line2-to-be-changed-in-incoming
    /// =======
    /// line1-changed-by-both
    /// line2-changed
    /// >>>>>>> incoming
    /// ```
    Diff3,
    /// Like [`Diff3](Self::Diff3), but will show *minimized* hunks of local change and the incoming (other) changes,
    /// as well as non-minimized hunks of the base.
    ///
    /// ```text
    /// line1-changed-by-both
    /// <<<<<<< local
    /// line2-to-be-changed-in-incoming
    /// ||||||| 9a8d80c
    /// line1-to-be-changed-by-both
    /// line2-to-be-changed-in-incoming
    /// =======
    /// line2-changed
    /// >>>>>>> incoming
    /// ```
    ZealousDiff3,
}

/// The set of labels to annotate conflict markers with.
///
/// That way it becomes clearer where the content of conflicts are originating from.
#[derive(Default, Copy, Clone, Debug, Eq, PartialEq)]
pub struct Labels<'a> {
    /// The label for the common *ancestor*.
    pub ancestor: Option<&'a BStr>,
    /// The label for the *current* (or *our*) side.
    pub current: Option<&'a BStr>,
    /// The label for the *other* (or *their*) side.
    pub other: Option<&'a BStr>,
}

/// How hard the merge tries to shrink a conflict down to the lines that really differ.
///
/// These are git's `XDL_MERGE_*` levels from `xdiff/xdiff.h`. Git never derives
/// the level from configuration; each caller picks one and keeps it, which is why
/// `git merge-file` and `git merge` can disagree on the same inputs.
#[derive(Default, Copy, Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub enum Level {
    /// `XDL_MERGE_MINIMAL`: mark every overlapping change as a conflict.
    Minimal,
    /// `XDL_MERGE_EAGER`: mark overlapping changes as a conflict only if they are not identical.
    ///
    /// The diff3 styles clamp any higher level down to this, because refining a
    /// conflict against the other side would desynchronize it from the base
    /// section they print.
    Eager,
    /// `XDL_MERGE_ZEALOUS`: additionally diff the two conflicting postimages against
    /// each other and conflict only over the lines that differ, then fold runs of at
    /// most three unchanged lines between two conflicts into them.
    ///
    /// This is what `merge-ll.c` uses, so it covers `merge`, `merge-tree`,
    /// `cherry-pick`, `rebase`, `revert` and `stash apply`.
    #[default]
    Zealous,
    /// `XDL_MERGE_ZEALOUS_ALNUM`: like [`Zealous`](Self::Zealous), but a run of unchanged
    /// lines between two conflicts is folded into them regardless of length when it holds
    /// no letter or digit.
    ///
    /// This is what `builtin/merge-file.c` uses, so it applies to `git merge-file` only.
    ZealousAlnum,
}

/// Options for the builtin [text driver](crate::blob::BuiltinDriver::Text).
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct Options {
    /// Determine of the diff will be performed.
    /// Defaults to [`imara_diff::Algorithm::Myers`].
    pub diff_algorithm: imara_diff::Algorithm,
    /// Decide what to do to automatically resolve conflicts, or to keep them.
    pub conflict: Conflict,
    /// git's `xmp.style`, which it keeps independent of `xmp.favor`: the style decides
    /// how far conflicting regions are refined, and that still shapes the output when
    /// the conflicts are then resolved automatically.
    ///
    /// `None` takes the style from [`Conflict::Keep`], and uses [`ConflictStyle::Merge`]
    /// for the automatic resolutions — which is what git does when no style was configured.
    pub style: Option<ConflictStyle>,
    /// How hard to try to shrink conflicts, defaulting to [`Level::Zealous`] like git's
    /// low-level merge driver.
    pub level: Level,
}

impl Default for Options {
    fn default() -> Self {
        Options {
            conflict: Default::default(),
            diff_algorithm: imara_diff::Algorithm::Myers,
            style: None,
            level: Level::default(),
        }
    }
}

/// Everything [`Merge::run_with()`] needs besides the input: git's `xmparam_t`, minus
/// the diff algorithm that was already fixed when the [`Merge`] was prepared.
#[derive(Default, Copy, Clone, Debug, Eq, PartialEq)]
pub struct Rendering {
    /// Whether to keep conflicts, and how, or which side to resolve them with.
    pub conflict: Conflict,
    /// See [`Options::style`].
    pub style: Option<ConflictStyle>,
    /// See [`Options::level`].
    pub level: Level,
    /// Overrides the marker size in `conflict`.
    ///
    /// git keeps the size in an `int` and only substitutes its default for a non-positive
    /// value, so `git merge-file --marker-size=300` really does draw 300 markers — more
    /// than [`Conflict::Keep`] can hold.
    pub marker_size: Option<usize>,
}

impl Rendering {
    /// The `xmp.style` this renders with: the explicit override, else the one in
    /// `conflict`, else [`ConflictStyle::Merge`].
    pub fn style(&self) -> ConflictStyle {
        self.style.unwrap_or(match self.conflict {
            Conflict::Keep { style, .. } => style,
            Conflict::ResolveWithOurs | Conflict::ResolveWithTheirs | Conflict::ResolveWithUnion => {
                ConflictStyle::Merge
            }
        })
    }

    /// The number of marker characters to draw.
    pub fn marker_size(&self) -> usize {
        self.marker_size.unwrap_or(match self.conflict {
            Conflict::Keep { marker_size, .. } => usize::from(marker_size.get()),
            Conflict::ResolveWithOurs | Conflict::ResolveWithTheirs | Conflict::ResolveWithUnion => 0,
        })
    }
}

/// What to do to resolve a conflict.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub enum Conflict {
    /// Keep the conflict by marking it in the source file.
    Keep {
        /// How to visualize conflicts in merged files.
        style: ConflictStyle,
        /// The amount of markers to draw, defaults to 7, i.e. `<<<<<<<`
        marker_size: NonZeroU8,
    },
    /// Chose our side to resolve a conflict.
    ResolveWithOurs,
    /// Chose their side to resolve a conflict.
    ResolveWithTheirs,
    /// Place our and their lines one after another, in any order
    ResolveWithUnion,
}

impl Conflict {
    /// The amount of conflict marker characters to print by default.
    pub const DEFAULT_MARKER_SIZE: u8 = 7;

    /// The amount of conflict markers to print if this instance contains them, or `None` otherwise
    pub fn marker_size(&self) -> Option<u8> {
        match self {
            Conflict::Keep { marker_size, .. } => Some(marker_size.get()),
            Conflict::ResolveWithOurs | Conflict::ResolveWithTheirs | Conflict::ResolveWithUnion => None,
        }
    }
}

impl Default for Conflict {
    fn default() -> Self {
        Conflict::Keep {
            style: Default::default(),
            marker_size: Conflict::DEFAULT_MARKER_SIZE.try_into().unwrap(),
        }
    }
}
///
/// Prepared merge state for rendering the same merge with multiple conflict strategies.
///
/// Construct this with [`Merge::new()`] to compute the expensive diff state once, then call
/// [`Merge::run()`] repeatedly with different [`Conflict`] values. It keeps a reference to the
/// [`imara_diff::InternedInput`] used to construct it so rendering cannot accidentally use a
/// different interner than the one the stored tokens were derived from.
#[derive(Clone)]
pub struct Merge<'input, 'data> {
    /// `before` holds the ancestor tokens and `after` the other side's, both interned
    /// together with [`current_tokens`](Self::current_tokens).
    input: &'input imara_diff::InternedInput<&'data [u8]>,
    /// Kept so `xdl_merge()`'s "the other side did not change" shortcut can copy the
    /// winning side verbatim.
    current: &'data [u8],
    other: &'data [u8],
    current_tokens: Vec<imara_diff::Token>,
    /// The `xdchange_t` script from the ancestor to our side, git's `xscr1`.
    ours: Vec<xdl::Change>,
    /// The same to their side, git's `xscr2`.
    theirs: Vec<xdl::Change>,
    /// Needed again to refine conflicts, which re-diffs the two postimages.
    diff_algorithm: imara_diff::Algorithm,
}

pub(super) mod function;
mod xdl;

#!/usr/bin/env perl
# Codegen: src/parity/grammars/*.json -> src/parity/src/grammars_generated.rs
#
# The grammars are extracted per-command by reading git's own man pages. They
# say only WHAT to test; stock git remains the oracle that says whether zvcs
# agrees. So a thin grammar narrows coverage but can never turn a failure into
# a pass — which is why generating them is safe to fan out.
#
# Mutating commands ARE included. Each case runs in a pristine copy of the
# fixture and the harness pins GIT_EDITOR/GIT_SEQUENCE_EDITOR to `true`, so the
# behavior that made them ambiguous to fuzz — blocking on an editor or a prompt
# — no longer occurs. They are marked so the report can distinguish them, since
# a mutating command is judged mostly on resulting repository state rather than
# on stdout.
#
# Commands with no offline-testable surface (daemons, credential helpers,
# network verbs) still drop out, and every exclusion is printed: a silently
# dropped command reads as covered when it is not.
#
# ENTRY FORMS. A flag or positional is written either way:
#
#   "--since=1 year ago"        one argv token, however many spaces it holds
#   ["HEAD", "--", "README.md"] three argv tokens
#
# The distinction is not cosmetic. Every entry becomes exactly one token today,
# so an entry that meant several arrived as one malformed argument -- `git
# shortlog 'HEAD -- README.md'` is `fatal: ambiguous argument`, `git
# symbolic-ref '-q --short'` is ``error: unknown switch ` '`` (both verified
# against stock 2.55.0). Both sides reject it identically, so the case passes
# while measuring nothing. A blind whitespace split is the wrong repair:
# `--author=A U Thor <a@example.invalid>`, `--format=%h %s`, `path with spaces`
# and `refs/heads/foo bar` are single tokens whose spaces are the point, and
# splitting them would destroy the quoting cases most likely to catch a bug.
# Hence the two forms: the string form is unchanged and still means one token.
#
# SHAPES. A grammar's `shapes` list is what the fuzzer draws the fixture from,
# and a shape belongs in a list only where it CHANGES AN ANSWER. Two rules,
# both learned the expensive way:
#
#  * *A shape that only makes the verb refuse buys nothing.* Both sides refuse,
#    the case passes, and it has spent a share of that grammar's budget. That is
#    why `commit-graph` is in `gc`'s list (a `gc` rewrites the graph file, so the
#    state probe has something to disagree about) and not in `fsck`'s, where
#    `fsck` on a valid graph exits 0 exactly as it does with no graph at all
#    (verified against stock 2.55.0 on the shape).
#  * *A shape pays only if the positionals can name what is in it.* The refs and
#    paths a shape carries exist in that shape alone -- `cc-left`/`cc-right`,
#    `alien`, `topic`, `link-to-file`, `refs/heads/dangling` -- so adding the
#    shape without adding its names draws the fixture and then asks it about
#    `main` and `feature`, which resolve to nothing. Every shape added here
#    comes with the names that make it reachable.
#
# WIRE FORMAT. `Grammar::flags` and `Grammar::positionals` are
# `&'static [&'static str]` -- one `&str` per entry -- and `sample_argv` pushes
# each drawn entry into a `Vec<String>` with one `push`. No `&str` can become
# two argv tokens through that, so the array form is emitted as one string whose
# tokens are separated by U+001F (a byte no git argument in this corpus
# contains). The Rust type is therefore unchanged and fuzz.rs still compiles as
# it stands, but the split has to happen on the consuming side: until
# `sample_argv` splits on U+001F before returning, a multi-token entry reaches
# git as a single argument, exactly as the space-joined form did.

use strict;
use warnings;
use JSON::PP;
use File::Basename;

my $root = dirname(__FILE__) . '/..';
my $dir  = "$root/src/parity/grammars";
my $out  = "$root/src/parity/src/grammars_generated.rs";

opendir(my $dh, $dir) or die "cannot read $dir: $!\n";
my @files = sort grep { /\.json$/ } readdir($dh);
closedir($dh);

die "no grammar JSON files in $dir\n" unless @files;

# Only the shapes fixture.rs actually builds. An unknown shape is a typo, not a
# new fixture, so it is rejected loudly rather than silently dropped.
#
# The other direction fails quietly, and has twice: a shape fixture.rs builds but
# this map does not name is unreachable to every generated grammar, and nothing
# says so -- the JSON simply never spells it. `Shape::ALL` gained 14 shapes and
# then 7 more before this map caught up. So the map is cross-checked against
# `Shape::name`'s arms below and any shape it is missing is printed.
my %SHAPE = (
    'linear'           => 'Shape::Linear',
    'branched'         => 'Shape::Branched',
    'merged'           => 'Shape::Merged',
    'dirty'            => 'Shape::Dirty',
    'conflicted'       => 'Shape::Conflicted',
    'detached'         => 'Shape::Detached',
    'awkward-paths'    => 'Shape::AwkwardPaths',
    'submodule'        => 'Shape::Submodule',
    'attributes'       => 'Shape::Attributes',
    'renamed'          => 'Shape::Renamed',
    'whitespace'       => 'Shape::Whitespace',
    'packed'           => 'Shape::Packed',
    'patches'          => 'Shape::Patches',
    'sparse'           => 'Shape::Sparse',
    'mergeable-dirty'  => 'Shape::MergeableDirty',
    'mergeable-staged' => 'Shape::MergeableStaged',
    'stashed'          => 'Shape::Stashed',
    'behind-remote'    => 'Shape::BehindRemote',
    'worktree'         => 'Shape::Worktree',
    'octopus'          => 'Shape::Octopus',
    'no-index-trees'   => 'Shape::NoIndexTrees',
    'decomposed-paths' => 'Shape::DecomposedPaths',
    'hooked'           => 'Shape::Hooked',
    'unrelated'        => 'Shape::Unrelated',
    'criss-cross'      => 'Shape::CrissCross',
    'cherry'           => 'Shape::Cherry',
    'symlinks'         => 'Shape::Symlinks',
    'commit-graph'     => 'Shape::CommitGraph',
    'damaged'          => 'Shape::Damaged',
    'intent-to-add'    => 'Shape::IntentToAdd',
    'pending-rename'   => 'Shape::PendingRename',
    'notes-replace'    => 'Shape::NotesReplace',
    'hooks-fail'       => 'Shape::HooksFail',
    'rerere'           => 'Shape::Rerere',
    'worktree-locked'  => 'Shape::WorktreeLocked',
    'tag-chain'        => 'Shape::TagChain',
    'shallow'          => 'Shape::Shallow',
    'promisor'         => 'Shape::Promisor',
);

# The shapes fixture.rs names, read out of `Shape::name`'s match arms -- the one
# place every variant must appear, since the compiler rejects a non-exhaustive
# match. Read for reporting only: nothing here reaches the generated text, so
# the output stays a pure function of the JSON.
sub fixture_shape_names {
    my $src = "$root/src/parity/src/fixture.rs";
    open(my $sfh, '<', $src) or return ();
    my $body = do { local $/; <$sfh> };
    close($sfh);
    my @names;
    while ($body =~ /Shape::\w+\s*=>\s*"([a-z0-9-]+)"/g) { push @names, $1 }
    my %seen;
    return grep { !$seen{$_}++ } @names;
}

my @unmapped = grep { !$SHAPE{$_} } fixture_shape_names();
printf STDERR "warning: %d shape(s) fixture.rs builds are unreachable to every grammar (not in %%SHAPE): %s\n",
    scalar(@unmapped), join(' ', @unmapped) if @unmapped;

# The separator between the tokens of a multi-token entry. See WIRE FORMAT
# above for why the tokens travel inside one string rather than as a nested
# slice.
my $SEP = "\x{1f}";

sub rs_str {
    my $s = shift;
    $s =~ s/\\/\\\\/g;
    $s =~ s/"/\\"/g;
    # Emitted as an escape rather than a raw byte: a control character sitting
    # invisibly in a source literal is unreadable and unreviewable.
    $s =~ s/\x{1f}/\\u{1f}/g;
    return "\"$s\"";
}

# One grammar entry -> the one string that carries it. A plain string is that
# string; an array is its tokens joined by $SEP. Rejected loudly rather than
# silently flattened: an empty array, a nested array, a non-string token, or a
# token already holding the separator would each produce an entry that does not
# mean what it reads as.
sub entry_str {
    my ($e, $what, $bad) = @_;
    return $e unless ref $e;
    unless (ref $e eq 'ARRAY') { push @$bad, "$what: entry is neither a string nor an array"; return undef }
    unless (@$e)               { push @$bad, "$what: empty array entry"; return undef }
    for my $t (@$e) {
        if (ref $t)            { push @$bad, "$what: nested array in a multi-token entry"; return undef }
        unless (length $t)     { push @$bad, "$what: empty token in a multi-token entry"; return undef }
        if (index($t, $SEP) >= 0) { push @$bad, "$what: token already contains the U+001F separator"; return undef }
    }
    return join($SEP, @$e);
}

my (@entries, @mutating, @skipped_empty, @bad);

for my $f (@files) {
    my $path = "$dir/$f";
    open(my $fh, '<', $path) or die "cannot read $path: $!\n";
    my $raw = do { local $/; <$fh> };
    close($fh);

    my $g = eval { JSON::PP->new->decode($raw) };
    if ($@) { push @bad, "$f: malformed JSON ($@)"; next; }

    my $cmd = $g->{command};
    unless (defined $cmd && length $cmd) { push @bad, "$f: missing \"command\""; next; }

    push @mutating, $cmd if $g->{mutating};

    my @flags = grep { defined } map { entry_str($_, "$f: flag", \@bad) } @{ $g->{flags}       // [] };
    my @pos   = grep { defined } map { entry_str($_, "$f: positional", \@bad) } @{ $g->{positionals} // [] };
    my @sh    = @{ $g->{shapes}      // [] };

    # A grammar with no flags AND no positionals generates nothing but the bare
    # subcommand; that is the honest answer for daemons and network verbs, but
    # it is not fuzzable, so it is recorded and dropped.
    unless (@flags || @pos) { push @skipped_empty, $cmd; next; }

    my @shapes;
    for my $s (@sh) {
        if (my $v = $SHAPE{$s}) { push @shapes, $v }
        else { push @bad, "$f: unknown shape \"$s\""; }
    }
    @shapes = ('Shape::Linear') unless @shapes;

    push @entries, {
        cmd    => $cmd,
        flags  => \@flags,
        pos    => \@pos,
        shapes => \@shapes,
    };
}

die "grammar errors:\n  " . join("\n  ", @bad) . "\n" if @bad;

my $text = <<'HEADER';
//! Generated by scripts/gen_grammars.pl from src/parity/grammars/*.json.
//!
//! Do not edit by hand: regenerate instead. The grammars are extracted from
//! git's own documentation per command and say only which invocations to try.
//! Stock git remains the oracle for whether zvcs agrees, so widening a grammar
//! can surface failures but can never manufacture a pass.
//!
//! **`\u{1f}` inside an entry separates argv tokens.** A grammar entry is one
//! argv token by default, spaces and all — `"--since=1 year ago"` is a single
//! argument and has to stay one. Entries that mean *several* arguments are
//! written in the JSON as arrays (`["HEAD", "--", "README.md"]`) and arrive here
//! as one string with U+001F between the tokens, because `Grammar`'s fields are
//! `&[&str]` and `sample_argv` turns each drawn entry into exactly one
//! `String`. Splitting on that separator is the consumer's job: an entry that
//! is not split reaches git as one malformed argument (`fatal: ambiguous
//! argument 'HEAD -- README.md'`), which both sides reject identically — a case
//! that passes while measuring nothing.

use crate::fixture::Shape;
use crate::fuzz::Grammar;

/// Fuzz grammars for the ported subcommands, read-only ones only.
pub fn generated() -> Vec<Grammar> {
    vec![
HEADER

my $multi = 0;
for my $e (@entries) {
    $multi += grep { index($_, $SEP) >= 0 } @{ $e->{flags} }, @{ $e->{pos} };
    $text .= "        Grammar {\n";
    $text .= sprintf "            cmd: %s,\n", rs_str($e->{cmd});
    $text .= sprintf "            flags: &[%s],\n", join(', ', map { rs_str($_) } @{ $e->{flags} });
    $text .= sprintf "            positionals: &[%s],\n", join(', ', map { rs_str($_) } @{ $e->{pos} });
    $text .= sprintf "            shapes: &[%s],\n", join(', ', @{ $e->{shapes} });
    $text .= "        },\n";
}
$text .= "    ]\n}\n";

# `--check` is for a build gate: it regenerates into memory and reports whether
# the committed file is what the JSON says it should be, without writing.
if (grep { $_ eq '--check' } @ARGV) {
    open(my $ifh, '<', $out) or die "cannot read $out: $!\n";
    my $have = do { local $/; <$ifh> };
    close($ifh);
    if ($have eq $text) { print "up to date: $out\n"; exit 0 }
    print STDERR "stale: $out does not match the grammars; run scripts/gen_grammars.pl\n";
    exit 1;
}

open(my $ofh, '>', $out) or die "cannot write $out: $!\n";
print $ofh $text;
close($ofh);

printf "generated %d grammars -> %s\n", scalar(@entries), $out;
printf "  of which %d multi-token entries (tokens joined by U+001F)\n", $multi if $multi;
printf "  of which %d mutating (fuzzed in pristine copies, non-interactive editors)\n",
    scalar(@mutating) if @mutating;
printf "skipped %d with no fuzzable surface: %s\n", scalar(@skipped_empty), join(' ', sort @skipped_empty)
    if @skipped_empty;

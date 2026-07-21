#!/usr/bin/env perl
# Regenerate porcelain/mod.rs and the porcelain half of dispatch.rs from the
# set of modules actually present on disk.
#
# The file list is the single source of truth. Hand-maintaining two parallel
# lists across 180 subcommands guarantees they drift; deriving both from the
# directory means a module that exists is always reachable, and one that does
# not exist can never be claimed.
#
# Module name <-> subcommand name is a total mapping in both directions:
#   cat_file.rs        <-> cat-file
#   checkout__worker.rs <-> checkout--worker
# i.e. underscore <-> hyphen, one for one.
#
# The superset verbs and the file's prose are preserved verbatim; only the
# generated regions between the BEGIN/END markers are rewritten.

use strict;
use warnings;
use File::Basename;

my $root = dirname(__FILE__) . '/..';
my $porc = "$root/src/extensions/src/porcelain";
my $dispatch = "$root/src/extensions/src/dispatch.rs";
my $modrs = "$porc/mod.rs";

opendir(my $dh, $porc) or die "cannot read $porc: $!\n";
my @mods = sort
           grep { $_ ne 'mod' }
           map  { /^(.+)\.rs$/ ? $1 : () }
           readdir($dh);
closedir($dh);

die "no porcelain modules found in $porc\n" unless @mods;

# Verify every module exposes the expected entry point before wiring it in.
# A module missing its fn would otherwise fail the build with an error that
# points at dispatch.rs instead of at the module actually at fault.
my @bad;
for my $m (@mods) {
    open(my $fh, '<', "$porc/$m.rs") or die "cannot read $porc/$m.rs: $!\n";
    my $src = do { local $/; <$fh> };
    close($fh);
    push @bad, $m unless $src =~ /pub\s+fn\s+\Q$m\E\s*\(/;
}
if (@bad) {
    die "modules missing `pub fn <name>(args: &[String])`:\n  "
      . join("\n  ", map { "$_.rs" } @bad) . "\n";
}

my $sub_of = sub { my $s = shift; $s =~ tr/_/-/; $s };

# ---- porcelain/mod.rs -------------------------------------------------------
my $mod_body = join('', map { "mod $_;\n" } @mods)
             . "\n"
             . join('', map { "pub use ${_}::${_};\n" } @mods);

open(my $mfh, '<', $modrs) or die "cannot read $modrs: $!\n";
my $mod_src = do { local $/; <$mfh> };
close($mfh);

# Keep the leading //! doc block, replace everything after it.
my ($mod_doc) = $mod_src =~ /\A((?:\s*\/\/!.*\n)+)/;
$mod_doc //= "//! git-compatible porcelain, served natively via vendored gitoxide.\n";

open($mfh, '>', $modrs) or die "cannot write $modrs: $!\n";
print $mfh $mod_doc, "\n", $mod_body;
close($mfh);

# ---- dispatch.rs ------------------------------------------------------------
open(my $dfh, '<', $dispatch) or die "cannot read $dispatch: $!\n";
my $disp = do { local $/; <$dfh> };
close($dfh);

my $arms = join('', map { sprintf(qq{        "%s" => porcelain::%s(args),\n}, $sub_of->($_), $_) } @mods);

# The verb-name list backing dispatch::is_verb — same source (the module set),
# so alias expansion's builtin-precedence check can never disagree with the
# arms about which verbs actually dispatch.
my $verbs = join('', map { sprintf(qq{    "%s",\n}, $sub_of->($_)) } @mods);
my $vbegin = '    // ---- BEGIN generated porcelain verbs (scripts/wire_dispatch.pl) ----';
my $vend   = '    // ---- END generated porcelain verbs ----';
$disp =~ /\Q$vbegin\E.*?\Q$vend\E/s
    or die "dispatch.rs: could not locate the generated porcelain verbs region\n";
$disp =~ s/\Q$vbegin\E.*?\Q$vend\E/$vbegin\n$verbs$vend/s;

my $begin = '        // ---- BEGIN generated porcelain arms (scripts/wire_dispatch.pl) ----';
my $end   = '        // ---- END generated porcelain arms ----';

if ($disp =~ /\Q$begin\E.*?\Q$end\E/s) {
    $disp =~ s/\Q$begin\E.*?\Q$end\E/$begin\n$arms$end/s;
} else {
    # First run: replace the hand-written porcelain block, identified as the
    # arms between the superset comment and the catch-all.
    my $marker = qr{        // ---- git-compat porcelain \(gitoxide-backed\) ----\n};
    $disp =~ s{$marker.*?(?=\n        // Not yet ported)}{$begin\n$arms$end\n}s
        or die "dispatch.rs: could not locate the porcelain arm block to replace\n";
}

open($dfh, '>', $dispatch) or die "cannot write $dispatch: $!\n";
print $dfh $disp;
close($dfh);

printf "wired %d porcelain modules into dispatch.rs and mod.rs\n", scalar(@mods);

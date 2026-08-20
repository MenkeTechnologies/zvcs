#!/usr/bin/env perl
# Split a `zvcs-parity --verbose` report into one failure brief per subcommand.
#
# Each brief is the evidence a fixer needs and nothing else: the exact
# invocation, what stock git produced, and what zvcs produced. Handing an agent
# the raw report instead would bury its own command's failures among a thousand
# unrelated ones.
#
#   perl scripts/split_failures.pl <report.txt> <out-dir>
#
# Prints a ranked summary so the worst subcommands can be fanned out first.

use strict;
use warnings;
use File::Path qw(make_path);

my ($report, $outdir) = @ARGV;
die "usage: split_failures.pl <report.txt> <out-dir>\n" unless $report && $outdir;

open(my $fh, '<', $report) or die "cannot read $report: $!\n";
my @lines = <$fh>;
close($fh);

make_path($outdir);

# Per-subcommand parity, straight from the report's own table, so the ranking
# matches what the harness reported rather than being recomputed here.
# The number of per-verdict columns between `match` and `parity` is not fixed —
# a `flaky` column joined `out`/`exit`/`state` when the harness started reporting
# cases zvcs would not reproduce — so the row is matched as "a name, one or more
# counts, a percentage" and only the first two counts and the percentage are
# read. Pinning the column count meant a new bucket silently emptied %parity and
# ranked every subcommand at the same default.
my %parity;
for my $l (@lines) {
    next unless $l =~ /^(\S+)((?:\s+\d+)+)\s+([\d.]+)%\s*$/;
    my ($cmd, $counts, $pct) = ($1, $2, $3);
    next if $cmd eq 'cmd';
    my ($total, $match) = ($counts =~ /(\d+)/g)[0, 1];
    $parity{$cmd} = { total => $total, match => $match, pct => $pct };
}

# Failure blocks: "[VERDICT] shape::cmd::args" followed by indented detail.
my (%blocks, $cur, @buf);
my $flush = sub {
    return unless $cur;
    push @{ $blocks{$cur} }, join('', @buf);
    ($cur, @buf) = (undef);
};

for my $l (@lines) {
    if ($l =~ /^\[([A-Z-]+)\]\s+\S+?::(\S+?)::/) {
        $flush->();
        ($cur, @buf) = ($2, $l);
    } elsif ($cur) {
        # A blank line does not end a block; the next header or EOF does.
        push @buf, $l;
    }
}
$flush->();

my @ranked = sort {
    ($parity{$a}{pct} // 100) <=> ($parity{$b}{pct} // 100)
      || ($blocks{$b} ? scalar @{ $blocks{$b} } : 0) <=> ($blocks{$a} ? scalar @{ $blocks{$a} } : 0)
} keys %blocks;

for my $cmd (@ranked) {
    my $safe = $cmd; $safe =~ tr/-/_/;
    open(my $ofh, '>', "$outdir/$safe.txt") or die "cannot write $outdir/$safe.txt: $!\n";
    my $p = $parity{$cmd};
    printf $ofh "subcommand: %s\n", $cmd;
    printf $ofh "parity: %s/%s cases (%.1f%%)\n", $p->{match}, $p->{total}, $p->{pct} if $p;
    printf $ofh "failing cases: %d\n\n", scalar @{ $blocks{$cmd} };
    print $ofh $_ for @{ $blocks{$cmd} };
    close($ofh);
}

printf "%-20s %6s %8s %s\n", 'cmd', 'parity', 'failures', 'brief';
for my $cmd (@ranked) {
    my $safe = $cmd; $safe =~ tr/-/_/;
    printf "%-20s %5.1f%% %8d  %s/%s.txt\n",
        $cmd, ($parity{$cmd}{pct} // 0), scalar @{ $blocks{$cmd} }, $outdir, $safe;
}
printf "\n%d subcommands with failures\n", scalar @ranked;

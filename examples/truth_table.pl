#!/usr/bin/env perl
# Print the truth table LibPerlRs::Inspect derives for a subroutine.
#
#   perl -Mblib examples/truth_table.pl
#   perl -Mblib examples/truth_table.pl 'sub { my ($x, $y) = @_; if ($x && !$y) { "a" } elsif ($x) { "b" } else { "c" } }'
#
# The argument is Perl source that evaluates to a code reference; with no
# argument a built-in sample is used. Columns are the atomic conditions
# found by the `logic` pass, rows are every truth assignment, and the last
# column is the outcome reached under that assignment (`return EXPR`,
# `die MSG`, or an implicit final value). A `-` marks an assignment for
# which no path condition holds (semantically unreachable under the
# analysis' approximations, or not covered).
use strict;
use warnings;
use LibPerlRs::Inspect;

my $src = @ARGV ? shift : 'sub { my ($a, $b, $c) = @_; return ($a && $b) || !$c }';

my $code = eval $src;
die "source did not compile: $@" if $@;
die "source did not return a code reference\n" unless ref $code eq 'CODE';

my $logic = LibPerlRs::Inspect::analyze($code)->{logic};
my @conds = @{$logic->{conds}};
my @paths = @{$logic->{paths}};

print "source: $src\n\n";

if (!@conds) {
    print "no conditions found; outcomes:\n";
    print "  ", outcome($_), "\n" for @paths;
    exit 0;
}

if (!defined $logic->{table}) {
    print scalar(@conds), " conditions exceed the truth-table limit; paths:\n";
    for my $p (@paths) {
        my $when = join ' && ',
            map { ($_->{value} ? '' : '!') . $conds[$_->{cond}] } @{$p->{when}};
        printf "  %-40s when %s\n", outcome($p), ($when eq '' ? '(always)' : $when);
    }
    exit 0;
}

# Column widths: at least the width of the condition text (T/F needs 1).
my @w = map { length } @conds;
my $header = join ' | ', map { sprintf "%-*s", $w[$_], $conds[$_] } 0 .. $#conds;
print "$header | outcome\n";
print join('-+-', map { '-' x $_ } @w), "-+--------\n";

for my $row (@{$logic->{table}}) {
    my @in = @{$row->{inputs}};
    my $cells = join ' | ', map { sprintf "%-*s", $w[$_], ($in[$_] ? 'T' : 'F') } 0 .. $#conds;
    my $out = defined $row->{path} ? outcome($paths[$row->{path}]) : '-';
    print "$cells | $out\n";
}

# "return EXPR" / "die MSG" / "implicit EXPR" for a path record.
sub outcome {
    my ($p) = @_;
    my $exprs = join ', ', @{$p->{exprs}};
    $p->{kind} eq 'implicit' ? "implicit $exprs"
        : $exprs eq ''        ? $p->{kind}
        :                       "$p->{kind} $exprs";
}

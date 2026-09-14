#!/usr/bin/env perl
# Print the truth table LibPerlRs::Inspect derives for a subroutine.
#
#   perl -Mblib examples/truth_table.pl
#   perl -Mblib examples/truth_table.pl 'sub { my ($x, $y) = @_; if ($x && !$y) { "a" } elsif ($x) { "b" } else { "c" } }'
#
# The argument is Perl source that evaluates to a code reference; with no
# argument a built-in sample is used. Columns are the atomic conditions
# found by the `logic` pass, rows are every truth assignment, and the
# outcome column is `#<path> L<line> <kind> <expr>`: the index of the path
# reached under that assignment, its source line, and what happens there
# (`return EXPR`, `die MSG`, or `implicit EXPR` for the last evaluated
# expression, i.e. the sub's implicit return value). A `-` marks an
# assignment for which no path condition holds. Paths that execute other
# statements on the way (branch-arm side effects) are listed below the
# table.
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
    print "  ", outcome($_, $paths[$_]), "\n" for 0 .. $#paths;
    print_stmts();
    exit 0;
}

if (!defined $logic->{table}) {
    print scalar(@conds), " conditions exceed the truth-table limit; paths:\n";
    for my $i (0 .. $#paths) {
        my $p = $paths[$i];
        my $when = join ' && ',
            map { ($_->{value} ? '' : '!') . $conds[$_->{cond}] } @{$p->{when}};
        printf "  %-44s when %s\n", outcome($i, $p), ($when eq '' ? '(always)' : $when);
    }
    print_stmts();
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
    my $out = defined $row->{path} ? outcome($row->{path}, $paths[$row->{path}]) : '-';
    print "$cells | $out\n";
}
print_stmts();
print "\nkinds: return = explicit return; die/croak/confess = exception; ",
      "implicit = last evaluated expression (the sub's implicit return value)\n";

# "#N L<line> return EXPR" / "#N L<line> die MSG" / "#N L<line> implicit EXPR"
sub outcome {
    my ($i, $p) = @_;
    my $exprs = join ', ', @{$p->{exprs}};
    my $what = $exprs eq '' ? $p->{kind} : "$p->{kind} $exprs";
    my $line = defined $p->{line} ? " L$p->{line}" : '';
    "#$i$line $what";
}

# Paths that execute further statements before their outcome.
sub print_stmts {
    my @with = grep { @{$paths[$_]{stmts}} } 0 .. $#paths;
    return unless @with;
    print "\npaths with side effects:\n";
    for my $i (@with) {
        print "  ", outcome($i, $paths[$i]), "\n";
        for my $s (@{$paths[$i]{stmts}}) {
            my $line = defined $s->{line} ? "L$s->{line} " : '';
            print "      $line$s->{text}\n";
        }
    }
}

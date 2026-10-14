#!/usr/bin/env perl
# Print the module dependency graph of a program in Graphviz DOT format.
#
#   perl -Mblib examples/module_graph.pl [--static] [--prefix=DIR]... -- PERL-ARGS... | dot -Tsvg > deps.svg
#   perl -Mblib examples/module_graph.pl [--static] [--prefix=DIR]... deps.json
#
# Arguments (including --module=NAME) as for examples/module_cycles.pl. Edge styles: solid = use,
# dashed = require that ran, bold = use parent / base, dotted =
# require inside a sub (--static); red = followed while the target was
# still compiling.
use strict;
use warnings;
use Getopt::Long qw(:config require_order);
use LibPerlRs::Inspect::Deps;

GetOptions('static' => \my $static, 'prefix=s' => \my @prefixes, 'module=s' => \my $module)
    or die "bad options\n";
@ARGV = ('--', @ARGV, '-e', "use $module") if defined $module;
@ARGV or die "usage: $0 [--static] [--prefix=DIR]... [--module=NAME] (-- PERL-ARGS... | deps.json)\n";

my $deps = (@ARGV == 1 && $ARGV[0] =~ /\.json\z/)
    ? LibPerlRs::Inspect::Deps->read_json($ARGV[0])
    : LibPerlRs::Inspect::Deps->run_perl($ARGV[0] eq '--' ? @ARGV[1 .. $#ARGV] : @ARGV);

my $nodes = $deps->{nodes};
my $keep  = sub {
    my ($id) = @_;
    return 1 unless @prefixes;
    my $n = $nodes->{$id} or return 0;
    return 1 unless defined $n->{inc};
    my $file = $n->{file} // return 0;
    grep { index($file, $_) == 0 } @prefixes;
};

my %style = (
    use            => 'solid',
    no             => 'solid',
    require        => 'dashed',
    parent         => 'bold',
    require_static => 'dotted',
);

my %seen;
my @lines;
for my $e (@{ $deps->{edges} }) {
    next if $e->{kind} eq 'require_static' && !$static;
    next unless $keep->($e->{from}) && $keep->($e->{to});
    my $hot = ($e->{status} // '') eq 'in_progress';
    my $key = join "\0", $e->{from}, $e->{to}, $e->{kind}, $hot;
    next if $seen{$key}++;
    push @lines, sprintf '  %s -> %s [style=%s%s];',
        quote($e->{from}), quote($e->{to}), $style{ $e->{kind} } // 'solid',
        $hot ? ', color=red' : '';
}

print "digraph deps {\n  node [shape=box];\n", map("$_\n", @lines), "}\n";

sub quote {
    my ($s) = @_;
    $s =~ s/(["\\])/\\$1/g;
    qq{"$s"};
}

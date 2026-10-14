#!/usr/bin/env perl
# Show how a program's modules were loaded, as a tree.
#
#   perl -Mblib examples/load_tree.pl [--all] [--prefix=DIR]... [--module=NAME | -- PERL-ARGS... | deps.json]
#
#   perl -Mblib examples/load_tree.pl --module=Cyc::Main -- -It/lib-deps
#   perl -Mblib examples/load_tree.pl -- -It/lib-deps t/lib-deps/Cyc/Main.pm
#
# Each line is a require that ran (`use`, `require`, or a class loaded by
# `use parent` / `use base`) with where it was written, indented under the
# file being compiled at that moment. Marks:
#   !! failed           the file did not finish loading
#   !! still compiling  the target had not finished compiling (a cycle)
#   !! reload           the file was compiled already (as the main
#                       program, or under another name)
# A package that inherits from another package of its own file shows
# "(same file)" instead (harmless). Requires of modules loaded already are
# left out unless --all is given.
# --prefix keeps only modules whose file starts with one of the given
# directories (with everything they load hidden too). --module=NAME is a
# shorthand for `-- -e 'use NAME'`, followed by further perl arguments
# such as -I. A compile error is printed first; the exit status is then 2.
use strict;
use warnings;
use Getopt::Long qw(:config require_order);
use LibPerlRs::Inspect::Deps;

GetOptions('all' => \my $all, 'prefix=s' => \my @prefixes, 'module=s' => \my $module)
    or die "bad options\n";
@ARGV = ('--', @ARGV, '-e', "use $module") if defined $module;
@ARGV or die "usage: $0 [--all] [--prefix=DIR]... [--module=NAME] (-- PERL-ARGS... | deps.json)\n";

my $deps;
if (@ARGV == 1 && $ARGV[0] =~ /\.json\z/) {
    $deps = LibPerlRs::Inspect::Deps->read_json($ARGV[0]);
} else {
    shift @ARGV if $ARGV[0] eq '--';
    if (my $m = LibPerlRs::Inspect::Deps->program_module(@ARGV)) {
        warn "note: the program is the module $m; compiled as the main program it can\n"
            . "be loaded a second time. Try --module=$m to load it as a module.\n";
    }
    $deps = LibPerlRs::Inspect::Deps->run_perl(@ARGV);
}

my $failed = defined $deps->{compile_error};
if ($failed) {
    (my $msg = $deps->{compile_error}) =~ s/^/  /mg;
    print "compile failed:\n$msg\n";
}

my $nodes = $deps->{nodes};
my $keep  = sub {
    my ($id) = @_;
    return 1 unless @prefixes;
    my $n = $nodes->{$id} or return 0;
    return 1 if $n->{program} || !defined $n->{inc};
    my $file = $n->{file} // return 0;
    grep { index($file, $_) == 0 } @prefixes;
};

my ($root) = grep { $nodes->{$_}{program} } sort keys %$nodes;
print $root // $deps->{program}, "\n";

# edges from require_events, in the order they ran
my %hidden;    # %INC keys whose subtree is hidden
for my $e (grep { $_->{stack} } @{ $deps->{edges} }) {
    my @stack = @{ $e->{stack} };
    next if grep { $hidden{$_} } @stack;
    if (!$keep->($e->{to})) {
        $hidden{ $e->{inc} } = 1 if $e->{status} eq 'load';
        next;
    }
    next if $e->{status} eq 'cached' && !$all;
    my @marks;
    push @marks, 'reload' if defined $e->{reload_of};
    push @marks, 'failed' if $e->{failed};
    if ($e->{status} eq 'in_progress') {
        # a package inheriting from another one of its own file is harmless
        if ($e->{from} eq $e->{to}) {
            push @marks, '(same file)';
        } else {
            push @marks, 'still compiling';
        }
    }
    push @marks, '(loaded already)' if $e->{status} eq 'cached';
    my $mark = join ', ', @marks;
    $mark = "!! $mark" if $mark =~ /^[a-z]/;
    printf "%s%s %s  (%s line %d)%s\n", '  ' x (@stack + 1), $e->{kind}, $e->{to},
        $e->{file}, $e->{line}, length $mark ? "  $mark" : '';
}
exit($failed ? 2 : 0);

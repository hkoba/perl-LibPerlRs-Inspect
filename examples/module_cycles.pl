#!/usr/bin/env perl
# Find dependency cycles between the modules a program loads.
#
#   perl -Mblib examples/module_cycles.pl [--static] [--prefix=DIR]... -- PERL-ARGS...
#   perl -Mblib examples/module_cycles.pl [--static] [--prefix=DIR]... deps.json
#   perl -Mblib examples/module_cycles.pl --module=NAME [-- PERL-ARGS...]
#
#   perl -Mblib examples/module_cycles.pl --prefix=lib -- -Ilib bin/app.pl
#   perl -Mblib examples/module_cycles.pl -- -It/lib-deps -e 'use Deps::A'
#
# PERL-ARGS are compiled (perl -c) in a child perl with
# LibPerlRs::Inspect::Deps loaded first; a single *.json argument is read
# as the output of `perl -MLibPerlRs::Inspect::Deps=deps.json -c ...`
# instead. --module=NAME is a shorthand for `-- -e 'use NAME'`, followed
# by further perl arguments such as -I. When the program given is a *.pm
# file under one of the -I directories, a note suggests --module: a
# module compiled as the main program can be loaded a second time.
#
# First the requires that usually cause trouble are explained, each with
# the chain of loads that led to it and the line where the target's own
# compilation was suspended:
#   - "reload": a file compiled a second time: typically a module given
#     as the main program and reached again through a cycle (it is not in
#     %INC under its module name), or one file loaded under two names.
#     Subs are redefined, and anything that cannot run twice breaks.
#   - "still compiling": a require of a module whose compilation has not
#     finished. Code in the requiring module that runs at that point
#     (import, BEGIN, a use of the target's subs or @ISA) sees the target
#     only partly defined.
# For both, the state of the target package at that moment is shown, what
# it set up only later, and the names a `use` asked for that it could not
# provide yet (Exporter-style import).
# Then each cycle (strongly connected component of the dependency graph)
# is printed with the edges inside it; the edges above are marked "!!".
#
# Edges: use / no / require that ran, use parent / base. With --static,
# also the constant `require`s inside subs (lazy loading, which is a
# common way to break a cycle, so they are left out by default).
# --prefix keeps only modules whose file starts with one of the given
# directories (plus the main program). Exit status is 1 when a cycle or a
# reload is found, 2 when the program failed to compile (the error is printed and
# the dependencies seen until then are still analyzed).
use strict;
use warnings;
use Getopt::Long qw(:config require_order);
use LibPerlRs::Inspect::Deps;

GetOptions('static' => \my $static, 'prefix=s' => \my @prefixes, 'module=s' => \my $module)
    or die "bad options\n";
@ARGV = ('--', @ARGV, '-e', "use $module") if defined $module;
@ARGV or die "usage: $0 [--static] [--prefix=DIR]... [--module=NAME] (-- PERL-ARGS... | deps.json)\n";

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

my (%succ, %edges_of);
for my $e (@{ $deps->{edges} }) {
    next if $e->{kind} eq 'require_static' && !$static;
    next if $e->{from} eq $e->{to};
    next unless $keep->($e->{from}) && $keep->($e->{to});
    $succ{ $e->{from} }{ $e->{to} } = 1;
    $succ{ $e->{to} } //= {};
    push @{ $edges_of{ $e->{from} } }, $e;
}

my @flagged = grep {
    $_->{from} ne $_->{to}    # e.g. a package inheriting from another of its file
        && (defined $_->{reload_of} || ($_->{status} // '') eq 'in_progress')
        && $keep->($_->{from}) && $keep->($_->{to})
} @{ $deps->{edges} };
explain($_) for @flagged;

my @cycles = grep { @$_ > 1 } tarjan_scc(\%succ);
if (!@cycles) {
    print "no cycles\n";
    exit($failed ? 2 : @flagged ? 1 : 0);
}

my $n = 0;
for my $scc (@cycles) {
    my %in = map { $_ => 1 } @$scc;
    printf "cycle %d: %s\n", ++$n, join ', ', sort @$scc;
    for my $from (sort @$scc) {
        for my $e (grep { $in{ $_->{to} } } @{ $edges_of{$from} || [] }) {
            printf "  %s -> %s  (%s)%s\n", $from, $e->{to}, where($e), mark($e);
        }
    }
}
exit($failed ? 2 : 1);

sub where {
    my ($e) = @_;
    "$e->{kind} at $e->{file} line $e->{line}";
}

sub mark {
    my ($e) = @_;
    defined $e->{reload_of}                 ? '  !! reload'
    : ($e->{status} // '') eq 'in_progress' ? '  !! still compiling'
    :                                         '';
}

# A flagged edge with the chain of loads that led to it
sub explain {
    my ($e) = @_;
    my $target = $nodes->{ $e->{to} } || {};
    if (defined $e->{reload_of}) {
        my $how = $e->{reload_of} eq ($deps->{program} // '')
            ? 'as the main program'
            : "as $e->{reload_of}";
        printf "reload: %s (%s) is compiled a second time; it was compiled %s\n",
            $e->{to}, $target->{file} // $e->{inc}, $how;
    } else {
        printf "still compiling: %s requires %s (%s)\n", $e->{from}, $e->{to}, where($e);
    }
    my @chain = LibPerlRs::Inspect::Deps->chain($deps, $e);
    my $root  = $chain[0]{from};
    printf "  %s%s\n", $root, $nodes->{$root}{program} ? ' (main program)' : '';
    printf "    -> %s  (%s)%s\n", $_->{to}, where($_), mark($_) for @chain;
    # where the target's own compilation stood
    my ($paused) = grep { $_->{from} eq $e->{to} } @chain;
    printf "  %s was compiled up to %s line %d at that point\n",
        $e->{to}, $paused->{file}, $paused->{line}
        if $paused;
    if (my $snap = $e->{snapshot}) {
        printf "  %s provided then: %s\n", $snap->{package}, describe($snap) || 'nothing';
        printf "  set up only later: %s\n", describe($e->{later}) if $e->{later};
    }
    if (my $missing = $e->{missing}) {
        if (@{ $e->{args} }) {
            printf "  !! requested but not available then: %s\n", join ' ', @$missing;
        } else {
            printf "  !! `use %s` imported nothing: \@EXPORT was still empty (later: %s)\n",
                $e->{to}, join ' ', @$missing;
        }
    }
    print "\n";
}

# "import Exporter::import; @EXPORT_OK (a b); subs: x y" for a snapshot
# or its `later` part
sub describe {
    my ($state) = @_;
    my @parts;
    push @parts, "import $state->{import}" if defined $state->{import};
    for (['isa', '@ISA'], ['export', '@EXPORT'], ['export_ok', '@EXPORT_OK']) {
        my ($field, $label) = @$_;
        push @parts, "$label (@{ $state->{$field} })" if @{ $state->{$field} || [] };
    }
    push @parts, "subs: @{ $state->{subs} }" if @{ $state->{subs} || [] };
    join '; ', @parts;
}

# Tarjan's strongly connected components; iterative, so that long
# dependency chains do not hit deep recursion.
sub tarjan_scc {
    my ($succ) = @_;
    my ($index, %idx, %low, %on, @stack, @sccs) = (0);
    for my $root (sort keys %$succ) {
        next if exists $idx{$root};
        my @work = ([$root, [sort keys %{ $succ->{$root} }]]);
        $idx{$root} = $low{$root} = $index++;
        push @stack, $root;
        $on{$root} = 1;
        while (@work) {
            my ($v, $kids) = @{ $work[-1] };
            if (@$kids) {
                my $w = shift @$kids;
                if (!exists $idx{$w}) {
                    $idx{$w} = $low{$w} = $index++;
                    push @stack, $w;
                    $on{$w} = 1;
                    push @work, [$w, [sort keys %{ $succ->{$w} }]];
                } elsif ($on{$w}) {
                    $low{$v} = $idx{$w} if $idx{$w} < $low{$v};
                }
                next;
            }
            pop @work;
            if (@work) {
                my $parent = $work[-1][0];
                $low{$parent} = $low{$v} if $low{$v} < $low{$parent};
            }
            if ($low{$v} == $idx{$v}) {
                my @scc;
                while (1) {
                    my $w = pop @stack;
                    $on{$w} = 0;
                    push @scc, $w;
                    last if $w eq $v;
                }
                push @sccs, \@scc;
            }
        }
    }
    @sccs;
}

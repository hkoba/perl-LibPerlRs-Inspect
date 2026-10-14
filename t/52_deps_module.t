use strict;
use warnings;
use Test::More;
use FindBin;
use File::Spec;

my $lib = "$FindBin::Bin/lib-deps";
my $examples = File::Spec->catdir($FindBin::Bin, File::Spec->updir, 'examples');

use LibPerlRs::Inspect::Deps;

sub edges {
    my ($deps, @kinds) = @_;
    my %kind = map { $_ => 1 } @kinds;
    [ sort map { "$_->{from} -> $_->{to} $_->{kind}" . ($_->{status} ? " $_->{status}" : '') }
        grep { $kind{ $_->{kind} } && $_->{to} =~ /^Deps::/ } @{ $deps->{edges} } ];
}

subtest 'run_perl (perl -c in a child)' => sub {
    my $deps = LibPerlRs::Inspect::Deps->run_perl("-I$lib", '-e', 'use Deps::A; use Deps::Child');
    is_deeply edges($deps, qw(use require parent require_static)), [
        '-e -> Deps::A use load',
        '-e -> Deps::Child use load',
        'Deps::A -> Deps::B use load',
        'Deps::B -> Deps::A use in_progress',
        'Deps::B -> Deps::C require_static',
        'Deps::Child -> Deps::C require load',
    ], 'edges';
    like $deps->{nodes}{'Deps::B'}{file}, qr{lib-deps/Deps/B\.pm\z}, 'node file';
    is $deps->{nodes}{'Deps::B'}{inc}, 'Deps/B.pm', 'node %INC key';
    my ($static) = grep { $_->{kind} eq 'require_static' && $_->{from} eq 'Deps::B' }
        @{ $deps->{edges} };
    is $static->{sub}, 'Deps::B::b', 'sub of a static require';
    is $static->{line}, 5, 'its line';
};

subtest 'use parent' => sub {
    my $deps = LibPerlRs::Inspect::Deps->run_perl("-I$lib", '-e',
        'package Kid; use parent "Deps::C"; package Orphan; use parent -norequire, "Deps::A"');
    is_deeply edges($deps, 'parent'), ['-e -> Deps::C parent'], 'no edge with -norequire';
};

subtest 'compile error' => sub {
    eval { LibPerlRs::Inspect::Deps->run_perl("-I$lib", '-e', 'use Deps::Broken') };
    like $@, qr/perl -c failed:.*refuses to load/s, 'stderr of the child';
};

# (output, exit status) of examples/module_cycles.pl
sub module_cycles {
    my @cmd = ($^X, (map { "-I$_" } grep { !ref } @INC),
        "$examples/module_cycles.pl", @_);
    open my $fh, '-|', @cmd or die "Can't run @cmd: $!";
    my $out = do { local $/; <$fh> };
    close $fh;
    ($out, $? >> 8);
}

subtest 'examples/module_cycles.pl' => sub {
    my ($out, $status) = module_cycles('--', "-I$lib", '-e', 'use Deps::A');
    is $status, 1, 'exit status 1 on a cycle';
    like $out, qr/^cycle 1: Deps::A, Deps::B$/m, 'cycle';
    like $out, qr/Deps::B -> Deps::A .*line 3\)  !! still compiling/, 'closing edge flagged';

    ($out, $status) = module_cycles('--', "-I$lib", '-e', 'use Deps::C');
    is $status, 0, 'exit status 0 without cycles';
    is $out, "no cycles\n", 'output';
};

done_testing;

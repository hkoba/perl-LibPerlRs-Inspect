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
    is_deeply edges($deps, 'parent'), ['-e -> Deps::C parent load'],
        'the require parent.pm did, attributed to the use line; no edge with -norequire';
};

subtest 'compile error' => sub {
    my $deps = LibPerlRs::Inspect::Deps->run_perl("-I$lib", '-e', 'use Deps::Broken');
    like $deps->{compile_error}, qr/refuses to load/, 'stderr of the child';
    is_deeply edges($deps, 'use'), ['-e -> Deps::Broken use load'],
        'data collected up to the failure';
};

subtest 'declared-only subs' => sub {
    # Cyc::Gate has `use subs qw(declared_only)`: a CV without an op tree
    my $deps = LibPerlRs::Inspect::Deps->run_perl("-I$lib", '-e', 'use Cyc::Main');
    ok !defined $deps->{compile_error}, 'compiles';
    ok !defined $deps->{collect_error}, 'collected';
    ok scalar(grep { $_->{to} eq 'Cyc::Hub' } @{ $deps->{edges} }), 'edges';
};

subtest 'main program reloaded as a module' => sub {
    my $deps = LibPerlRs::Inspect::Deps->run_perl("-I$lib", "$lib/Cyc/Main.pm");
    like $deps->{compile_error}, qr/Subroutine early redefined/, 'the reload broke it';
    is $deps->{program}, "$lib/Cyc/Main.pm", 'program';
    ok $deps->{nodes}{'Cyc::Main'}{program}, 'program and module are one node';
    my ($reload) = grep { defined $_->{reload_of} } @{ $deps->{edges} };
    is_deeply [ @$reload{qw(from to kind line reload_of)} ],
        ['Cyc::Router', 'Cyc::Main', 'use', 3, "$lib/Cyc/Main.pm"], 'reload edge';
    is_deeply [ map { "$_->{from} -> $_->{to} $_->{kind} $_->{line}" }
            LibPerlRs::Inspect::Deps->chain($deps, $reload) ], [
        'Cyc::Main -> Cyc::Gate parent 9',    # base.pm's require, at the use base line
        'Cyc::Gate -> Cyc::Hub use 4',
        'Cyc::Hub -> Cyc::Router require 3',
        'Cyc::Router -> Cyc::Main use 3',
    ], 'chain';
    ok !grep({ $_->{from} eq 'base' && $_->{to} =~ /^Cyc::/ } @{ $deps->{edges} }),
        'no edge from base.pm to the class it loaded';
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

    ($out, $status) = module_cycles('--', "-I$lib", "$lib/Cyc/Main.pm");
    is $status, 2, 'exit status 2: compile failed';
    like $out, qr{^reload: Cyc::Main \(\S+/Cyc/Main\.pm\) is compiled a second time; it was compiled as the main program\n  Cyc::Main \(main program\)\n    -> Cyc::Gate  \(parent at \S+ line 9\)\n}m,
        'reload explained';
    like $out, qr{^  Cyc::Main was compiled up to \S+/Cyc/Main\.pm line 9 at that point$}m,
        'where the main program stood';
    unlike $out, qr/Cyc::Hub requires Cyc::Hub/, 'no self edge';

    ($out, $status) = module_cycles('--', "-I$lib", '-e', 'use Deps::Broken');
    is $status, 2, 'exit status 2 on a compile error';
    like $out, qr/^compile failed:\n  Deps::Broken refuses to load/, 'error printed';
};

done_testing;

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

subtest 'what a module still compiling was missing' => sub {
    my $deps = LibPerlRs::Inspect::Deps->run_perl("-I$lib", '-e', 'use Imp::Quiet');
    ok !defined $deps->{compile_error}, 'compiles (silently imports nothing)';
    my ($e) = grep { $_->{snapshot} } @{ $deps->{edges} };
    is "$e->{from} -> $e->{to}", 'Imp::Listener -> Imp::Quiet', 'edge';
    is_deeply $e->{args}, [], 'use without arguments';
    is_deeply $e->{later}, { export => ['shout'], subs => ['shout'] }, 'set up later';
    is_deeply $e->{missing}, ['shout'], '@EXPORT names it failed to import';

    $deps = LibPerlRs::Inspect::Deps->run_perl("-I$lib", '-e', 'use Imp::Lib');
    like $deps->{compile_error}, qr/"helper" is not exported/, 'Exporter refused';
    ($e) = grep { $_->{snapshot} } @{ $deps->{edges} };
    is_deeply $e->{args}, ['helper'], 'requested';
    is_deeply $e->{missing}, ['helper'], 'not exportable at that moment';
};

subtest 'program_module' => sub {
    my $pm = sub { LibPerlRs::Inspect::Deps->program_module(@_) };
    is $pm->("-I$lib", "$lib/Cyc/Main.pm"), 'Cyc::Main', '-Idir';
    is $pm->('-I', $lib, "$lib/Cyc/Main.pm"), 'Cyc::Main', '-I dir';
    is $pm->("-I$lib", '-e', 'use Cyc::Main'), undef, '-e';
    is $pm->("-I$lib/Cyc", "$lib/Cyc/Main.pm"), 'Main', 'innermost -I';
    is $pm->("-I$lib/Imp", "$lib/Cyc/Main.pm"), undef, 'outside of -I';
    is $pm->("-I$lib", 'bin/app.pl'), undef, 'not a .pm';
};

# (output, exit status) of an example script; STDERR is kept apart
sub example {
    my ($script, @args) = @_;
    my @cmd = ($^X, (map { "-I$_" } grep { !ref } @INC), "$examples/$script", @args);
    open my $fh, '-|', @cmd or die "Can't run @cmd: $!";
    my $out = do { local $/; <$fh> };
    close $fh;
    ($out, $? >> 8);
}
sub module_cycles { example('module_cycles.pl', @_) }

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

    ($out, $status) = module_cycles('--', "-I$lib", '-e', 'use Imp::Quiet');
    is $status, 1, 'exit status 1: cycle';
    like $out, qr/^  Imp::Quiet provided then: import Exporter::import; subs: import\n  set up only later: \@EXPORT \(shout\); subs: shout\n  !! `use Imp::Quiet` imported nothing: \@EXPORT was still empty \(later: shout\)$/m,
        'snapshot explained';

    ($out, $status) = module_cycles('--', "-I$lib", '-e', 'use Deps::Broken');
    is $status, 2, 'exit status 2 on a compile error';
    like $out, qr/^compile failed:\n  Deps::Broken refuses to load/, 'error printed';
};

subtest 'examples/load_tree.pl' => sub {
    my ($out, $status) = example('load_tree.pl', '--', "-I$lib", "$lib/Cyc/Main.pm");
    is $status, 2, 'exit status 2 on a compile error';
    my ($tree) = $out =~ /\n(Cyc::Main\n.*)/s;
    $tree =~ s{\Q$lib/\E}{}g;
    is $tree, <<'END', 'tree';
Cyc::Main
  use base  (Cyc/Main.pm line 9)
  parent Cyc::Gate  (Cyc/Main.pm line 9)  !! failed
    use subs  (Cyc/Gate.pm line 3)
    use Cyc::Hub  (Cyc/Gate.pm line 4)  !! failed
      parent Cyc::Hub  (Cyc/Hub.pm line 8)  (same file)
      require Cyc::Router  (Cyc/Hub.pm line 3)  !! failed
        use Cyc::Main  (Cyc/Router.pm line 3)  !! reload, failed
END

    ($out, $status) = example('load_tree.pl', "--prefix=$lib", '--module=Cyc::Main', '--', "-I$lib");
    is $status, 0, 'loads as a module';
    like $out, qr/^-e\n  use Cyc::Main  \(-e line 1\)\n    parent Cyc::Gate /, '--module';
    unlike $out, qr/use (?:base|subs)/, '--prefix hides modules outside';
    like $out, qr/^          use Cyc::Main  \(\S+ line 3\)  !! still compiling$/m, 'cycle';
};

done_testing;

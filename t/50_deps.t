use strict;
use warnings;
use Test::More;
use FindBin;
use lib "$FindBin::Bin/lib-deps";
use LibPerlRs::Inspect;

# Load the fixtures with BEGIN capture on: Deps::A and Deps::B use each
# other (a cycle), Deps::Child uses parent / File::Basename and has an
# opaque BEGIN.
BEGIN { LibPerlRs::Inspect::begin_capture(1) }
use Deps::A;
use Deps::Child;
BEGIN { LibPerlRs::Inspect::begin_capture(0) }

my @begins = @{ LibPerlRs::Inspect::begins() };
my $here   = __FILE__;
sub begins_in {
    my ($suffix) = @_;
    grep { defined $_->{file} && $_->{file} =~ /\Q$suffix\E\z/ } @begins;
}
sub uses_of {
    my ($suffix) = @_;
    map { "$_->{kind} $_->{module}" } grep { $_->{kind} ne 'opaque' } begins_in($suffix);
}

subtest 'begins: use statements of every file' => sub {
    is_deeply [uses_of($here)], ['use Deps::A', 'use Deps::Child'],
        'the test file itself (capture was off for earlier ones)';
    is_deeply [uses_of('Deps/A.pm')], ['use strict', 'use Deps::B'], 'Deps::A';
    # the use that closes the cycle is recorded although its require was a no-op
    is_deeply [uses_of('Deps/B.pm')], ['use strict', 'use Deps::A'], 'Deps::B';
    my ($a_use) = grep { ($_->{module} // '') eq 'Deps::A' } begins_in('Deps/B.pm');
    is $a_use->{line}, 3, 'line';
    ok !exists $a_use->{args}, 'use Foo () has no args';
    is ref $a_use->{code}, 'CODE', 'code is the BEGIN itself';
    is_deeply $a_use->{requires}, [{ file => 'Deps/A.pm', module => 'Deps::A', line => 3 }],
        'requires';
};

subtest 'begins: version, args, opaque' => sub {
    my %by_line = map { $_->{line} => $_ } begins_in('Deps/Child.pm');
    is_deeply $by_line{3}{args}, ['-norequire', 'Deps::C'], 'use parent args';
    is $by_line{4}{module}, 'File::Basename', 'module';
    is $by_line{4}{version}, '2', 'version (constant-folded)';
    is_deeply $by_line{4}{args}, ['basename', 'dirname'], 'import args';
    is $by_line{5}{kind}, 'opaque', 'hand-written BEGIN';
    is_deeply [map { $_->{module} } @{ $by_line{5}{requires} }], ['Deps::C'],
        'require inside an opaque BEGIN';
};

subtest 'begin_stmt' => sub {
    my ($use) = grep { ($_->{module} // '') eq 'Deps::B' } begins_in('Deps/A.pm');
    is LibPerlRs::Inspect::begin_stmt($use->{code}), 'use Deps::B;', 'use statement';
    my ($strict) = grep { ($_->{module} // '') eq 'strict' } begins_in('Deps/A.pm');
    is LibPerlRs::Inspect::begin_stmt($strict->{code}), '', 'hint pragma';
    my ($opaque) = grep { $_->{kind} eq 'opaque' } begins_in('Deps/Child.pm');
    is LibPerlRs::Inspect::begin_stmt($opaque->{code}), undef, 'opaque';
};

subtest 'require_sites' => sub {
    my $sites = LibPerlRs::Inspect::require_sites(\&Deps::B::b);
    is_deeply $sites, [
        { file => 'Deps/C.pm', module => 'Deps::C', line => 5 },
        { dynamic => 1, line => 7 },
    ], 'static and dynamic';
    my ($code, $line) = (sub { require 5.010; require "x/y.pl" }, __LINE__);
    is_deeply LibPerlRs::Inspect::require_sites($code),
        [{ version => '5.01', line => $line }, { file => 'x/y.pl', line => $line }],
        'version and file';
};

subtest 'packages / inheritance' => sub {
    my %pkgs = map { $_ => 1 } LibPerlRs::Inspect::packages();
    ok $pkgs{$_}, "$_ listed" for qw(main Deps::A Deps::B Deps::C Deps::Child);
    is_deeply LibPerlRs::Inspect::inheritance('Deps::Child'),
        { isa => ['Deps::C'], linear_isa => ['Deps::Child', 'Deps::C'], mro => 'dfs' },
        'Deps::Child';
    is LibPerlRs::Inspect::inheritance('Deps::C'), undef, 'no @ISA';
};

done_testing;

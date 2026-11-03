use strict;
use warnings;
use Test::More;
use FindBin;
use lib "$FindBin::Bin/lib";
use LibPerlRs::Inspect;
use Fixtures;

my %fixture = map { $_->[0] => $_->[1] } @Fixtures::FIXTURES;

sub report_of {
    my ($name) = @_;
    LibPerlRs::Inspect::analyze(Fixtures::compile($name, $fixture{$name}));
}

subtest 'call sites of calls_mixed' => sub {
    my $c = report_of('calls_mixed')->{calls};
    is scalar @$c, 7, '7 calls';

    is_deeply $c->[0]{callee}, {named => {name => 'Fixtures::foo'}}, 'foo(1, 2)';
    is_deeply $c->[0]{argc}, {min => 2, max => 2}, 'argc 2..2';

    is_deeply $c->[1]{callee}, {named => {name => 'Bar::baz'}}, 'Bar::baz($o, @list)';
    is_deeply $c->[1]{argc}, {min => 1, max => undef}, 'argc 1..';

    is $c->[2]{kind}, 'method', '$o->meth(...)';
    is_deeply $c->[2]{callee}{method}{invocant}, {self_var => '$o'}, 'invocant $o';
    is_deeply [map { $_->[0] } @{$c->[2]{pairs}}], ['a', 'b'], 'pairs a, b';

    is_deeply $c->[3]{callee}{method}{invocant}, {class => 'Klass'}, 'Klass->new';
    is_deeply $c->[3]{argc}, {min => 0, max => undef}, 'argc 0.. (%h)';

    is $c->[4]{kind}, 'amper', '&foo;';
    ok $c->[4]{forwards_args}, '&foo; passes @_';
    ok $c->[5]{forwards_args}, 'foo(@_) passes @_';
    is_deeply $c->[6]{callee}, {dynamic => '$cb'}, '&$cb(3)';
};

subtest 'check_call' => sub {
    my $spec = report_of('args_checks')->{args};
    my $call = sub {
        my ($min, $max, %o) = @_;
        +{node => 0, callee => {named => {name => 'main::f'}}, args => [],
          argc => {min => $min, max => $max}, forwards_args => 0, kind => 'call', %o};
    };
    my $kinds = sub { [map { $_->{kind} } @{LibPerlRs::Inspect::check_call(@_)}] };
    is_deeply $kinds->($spec, $call->(1, 1)), ['too_few'], 'too few';
    is_deeply $kinds->($spec, $call->(3, 3)), ['too_many'], 'too many';
    is_deeply $kinds->($spec, $call->(2, 2)), [], 'exact';

    my $opts = report_of('args_opts_hash')->{args};
    my $pairs = sub { [map { [$_, '1'] } @_] };
    is_deeply $kinds->($opts, $call->(4, 4, kind => 'method', pairs => $pairs->(qw(foo bar)))),
        [], 'known keys';
    is_deeply $kinds->($opts, $call->(4, 4, kind => 'method', pairs => $pairs->(qw(bar qux)))),
        ['unknown_key'], 'unknown key';
    is_deeply $kinds->($opts, $call->(2, 2, kind => 'method', pairs => $pairs->(qw(foo)))),
        ['missing_key'], 'missing required key';
};

done_testing;

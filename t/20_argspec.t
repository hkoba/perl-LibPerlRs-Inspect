use strict;
use warnings;
use Test::More;
use FindBin;
use lib "$FindBin::Bin/lib";
use LibPerlRs::Inspect;
use Fixtures;

sub args_of {
    my ($src) = @_;
    my $code = eval $src;
    die "fixture failed: $@" if $@ or ref $code ne 'CODE';
    LibPerlRs::Inspect::analyze($code)->{args};
}

subtest 'signature' => sub {
    my $a = args_of('use v5.36; sub ($x, $y = 5, @rest) { $x + $y }');
    is $a->{style}, 'signature', 'style';
    is $a->{min_arity}, 1, 'min_arity (required only)';
    is $a->{max_arity}, undef, 'max_arity unbounded (slurpy @rest)';
    is scalar @{$a->{params}}, 3, '3 params';
    is $a->{params}[0]{name}, '$x', 'param 0 name';
    is $a->{params}[1]{name}, '$y', 'param 1 name';
    is $a->{params}[1]{default}, '5', 'param 1 default';
    is $a->{params}[2]{name}, '@rest', 'param 2 slurpy name';
};

subtest 'signature without slurpy' => sub {
    my $a = args_of('use v5.36; sub ($x, $y) { $x . $y }');
    is $a->{style}, 'signature', 'style';
    is $a->{min_arity}, 2, 'min';
    is $a->{max_arity}, 2, 'max';
};

subtest 'signature with named parameters (perl 5.44+)' => sub {
    plan skip_all => 'named parameters need perl 5.44' if $] < 5.044;
    # experimental in 5.44
    my $a = args_of('use v5.36; no warnings;'
        . ' sub ($self, :$alpha, :$beta = 2) { $alpha + $beta }');
    is $a->{style}, 'signature', 'style';
    is $a->{min_arity}, 3, 'min_arity: $self + one required key/value pair';
    is $a->{max_arity}, undef, 'max unbounded (named)';
    is_deeply [map { $_->{key} } @{$a->{params}}], [undef, 'alpha', 'beta'], 'keys';
    is_deeply [map { $_->{name} } @{$a->{params}}], ['$self', '$alpha', '$beta'], 'names';
    is $a->{params}[2]{default}, '2', 'named default';
};

subtest 'unpack: my (...) = @_' => sub {
    my $a = args_of('sub { my ($x, $y) = @_; $x * $y }');
    is $a->{style}, 'unpack', 'style';
    is_deeply [map { $_->{name} } @{$a->{params}}], ['$x', '$y'], 'names';
    is $a->{max_arity}, 2, 'max = 2 (no slurpy)';
};

subtest 'unpack with slurpy hash' => sub {
    my $a = args_of('sub { my ($x, %opts) = @_; $x }');
    is $a->{style}, 'unpack', 'style';
    is_deeply [map { $_->{name} } @{$a->{params}}], ['$x', '%opts'], 'names';
    is $a->{max_arity}, undef, 'max unbounded (slurpy %opts)';
};

subtest 'shift style' => sub {
    my $a = args_of('sub { my $self = shift; my $n = shift; $self + $n }');
    is $a->{style}, 'shift', 'style';
    is_deeply [map { $_->{name} } @{$a->{params}}], ['$self', '$n'], 'names';
    is $a->{invocant_guess}, '$self', 'invocant guess';
};

subtest 'positional $_[n]' => sub {
    my $a = args_of('sub { $_[0] + $_[1] }');
    is $a->{style}, 'positional', 'style';
    is $a->{min_arity}, 2, 'min_arity from max index';
};

subtest 'mixed: shift then unpack' => sub {
    my $a = args_of('sub { my $self = shift; my ($a, $b) = @_; $self->go($a, $b) }');
    is $a->{style}, 'mixed', 'style';
    is_deeply [map { $_->{name} } @{$a->{params}}], ['$self', '$a', '$b'], 'names in order';
    is $a->{invocant_guess}, '$self', 'invocant';
};

subtest 'no args' => sub {
    my $a = args_of('sub { 42 }');
    is $a->{style}, 'none', 'style';
    is scalar @{$a->{params}}, 0, 'no params';
};

# argspec v2: the whole body is executed symbolically (t/lib/Fixtures.pm)
my %fixture = map { $_->[0] => $_->[1] } @Fixtures::FIXTURES;

sub fixture_args {
    my ($name) = @_;
    LibPerlRs::Inspect::analyze(Fixtures::compile($name, $fixture{$name}))->{args};
}

sub param {
    my ($a, $name) = @_;
    my ($p) = grep { ($_->{name} // '') eq $name } @{$a->{params}};
    $p;
}

sub key {
    my ($a, $k) = @_;
    my ($n) = grep { $_->{key} eq $k } @{$a->{named}};
    $n;
}

subtest 'v2: shift, then loop over the rest' => sub {
    my $a = fixture_args('args_shift_loop');
    is_deeply [map { $_->{name} } @{$a->{params}}], ['$sum', '@_'], 'names';
    is_deeply param($a, '$sum')->{alts}, [{front => 0}], '$sum = $_[0]';
    is_deeply param($a, '@_')->{alts}, [{rest => 1}], 'loop over @_[1..]';
    is param($a, '@_')->{source}, 'list', 'source list';
};

subtest 'v2: hash of options' => sub {
    my $a = fixture_args('args_opts_hash');
    is_deeply param($a, '%args')->{alts}, [{rest => 1}], '%args = @_[1..]';
    is key($a, 'foo')->{default}, '"dflt"', 'foo default';
    is key($a, 'foo')->{via}, '%args', 'foo via %args';
    ok key($a, 'bar')->{required}, 'bar required';
    ok !key($a, 'baz')->{required}, 'baz only tested with exists';
};

subtest 'v2: hash with default pairs' => sub {
    my $a = fixture_args('args_hash_defaults');
    is_deeply param($a, '%args')->{alts}, [{rest => 0}], '%args = (..., @_)';
    is key($a, 'color')->{default}, '"red"', 'color default';
    ok key($a, 'size'), 'size';
};

subtest 'v2: hashref argument' => sub {
    my $a = fixture_args('args_hashref');
    is_deeply param($a, '$opts')->{alts}, [{front => 1}], '$opts = $_[1]';
    is key($a, 'verbose')->{via}, '$opts', 'via $opts';
    is key($a, 'verbose')->{default}, '0', 'verbose default';
};

subtest 'v2: pop, splice, count' => sub {
    my $a = fixture_args('args_pop_splice');
    is_deeply param($a, '$last')->{alts}, [{back => 0}], '$last = last arg';
    is_deeply param($a, '$first')->{alts}, [{front => 0}], '$first = $_[0]';
    is_deeply param($a, '$n')->{alts}, ['count'], '$n = scalar(@_)';
};

subtest 'v2: five forms of default' => sub {
    my $a = fixture_args('args_defaults');
    my @d = (['$a', 'da'], ['$b', 'db'], ['$c', 'dc'], ['$d', 'dd'], ['$e', 'de']);
    for my $i (0 .. $#d) {
        my ($name, $dflt) = @{$d[$i]};
        is_deeply param($a, $name)->{alts}, [{front => $i}], "$name slot";
        is param($a, $name)->{default}, qq{"$dflt"}, "$name default";
    }
};

subtest 'v2: forwards' => sub {
    my $a = fixture_args('args_forward');
    is_deeply [map { [$_->{callee}, $_->{kind}, $_->{from}] } @{$a->{forwards}}],
        [['SUPER::new', 'rest', 1], ['other', 'rest', 1], ['other2', 'amper', 1], ['other3', 'goto', 1]],
        'forwards';
};

subtest 'v2: arity checks' => sub {
    my $a = fixture_args('args_checks');
    is_deeply [map { [$_->{cmp}, $_->{n}, $_->{enforced} ? 1 : 0] } @{$a->{checks}}],
        [['==', 2, 1], ['<=', 5, 1]], 'checks (as required conditions)';
    is $a->{min_arity}, 2, 'min';
    is $a->{max_arity}, 2, 'max';
};

subtest 'v2: conditional shift' => sub {
    my $a = fixture_args('args_conditional');
    ok !param($a, '$x')->{conditional}, '$x unconditional';
    is_deeply param($a, '$opt')->{alts}, [{front => 1}], '$opt = $_[1]';
    ok param($a, '$opt')->{conditional}, '$opt conditional';
    is_deeply param($a, '$z')->{alts}, [{front => 1}, {front => 2}], '$z = $_[1] or $_[2]';
    ok param($a, '$z')->{conditional}, '$z conditional';
};

subtest 'v2: elements after shift' => sub {
    my $a = fixture_args('args_elem_after_shift');
    is_deeply [map { $_->{name} } @{$a->{params}}], ['$first', '$_[0]', '$_[1]'], 'names';
    is_deeply param($a, '$_[0]')->{alts}, [{front => 1}], '$_[0] is the 2nd arg';
    is $a->{min_arity}, 3, 'min_arity';
};

subtest 'v2: helper m4i_args inlined' => sub {
    # the helper fixtures define Fixtures::m4i_opts / m4i_opts_alike / m4i_args
    Fixtures::compile($_, $fixture{$_})
        for qw(args_helper_m4i_opts args_helper_m4i_opts_alike args_helper_m4i_args);
    my $code = Fixtures::compile('args_helper_m4i', $fixture{args_helper_m4i});

    my $plain = LibPerlRs::Inspect::analyze($code)->{args};
    is_deeply [map { [$_->{callee}, $_->{kind}] } @{$plain->{forwards}}],
        [['m4i_args', 'all']], 'without helpers: a forward';

    my $a = LibPerlRs::Inspect::analyze_with_helpers($code, 'Fixtures::m4i_args')->{args};
    is $a->{style}, 'helper', 'style';
    is_deeply [map { $_->{name} } @{$a->{params}}], ['$pack', '$opts', '@rest'], 'names';
    is_deeply param($a, '$opts')->{alts}, [{front => 1}], '$opts from $_[1]';
    ok param($a, '$opts')->{conditional}, '$opts: an Opts object or $_[1]';
    is_deeply param($a, '@rest')->{alts}, [{rest => 2}], '@rest = @_[2..]';
    is_deeply [map { $_->{callee} } @{$a->{helpers}}], ['m4i_args'], 'helpers';
};

subtest 'v2: MOP4Import::Opts::m4i_args (installed module)' => sub {
    plan skip_all => 'MOP4Import::Opts is not installed'
        unless eval { require MOP4Import::Opts; 1 };
    my $code = eval 'package M4iUser; MOP4Import::Opts->import(qw(m4i_args));'
        . ' sub { my ($pack, $opts, @r) = m4i_args(@_); $opts->{destpkg} }' or die $@;
    my $a = LibPerlRs::Inspect::analyze_with_helpers($code, 'MOP4Import::Opts::m4i_args')->{args};
    is_deeply [map { [$_->{name}, $_->{alts}] } @{$a->{params}}],
        [['$pack', [{front => 0}]], ['$opts', [{front => 1}]], ['@r', [{rest => 2}]]], 'params';
};

done_testing;

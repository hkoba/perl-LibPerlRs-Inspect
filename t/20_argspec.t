use strict;
use warnings;
use Test::More;
use LibPerlRs::Inspect;

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

done_testing;

package Fixtures;
use strict;
use warnings;

# Fixtures shared by all tests (name => source string to eval).
# Matching the primary use case (analyzing the anonymous sub returned by
# eval), they are always compiled via eval.
our @FIXTURES = (
    [empty        => 'sub {}'],
    [shift_style  => 'sub { my $self = shift; my $n = shift; $self + $n }'],
    [unpack_args  => 'sub { my ($x, $y) = @_; $x * $y }'],
    [signature    => 'use v5.36; sub ($x, $y = 5, @rest) { $x + $y }'],
    [branches     => 'sub { my ($v) = @_; return unless $v; if ($v > 10) { "big" } elsif ($v > 5) { "mid" } else { "small" } }'],
    [ternary      => 'sub { my ($x) = @_; $x ? "yes" : "no" }'],
    [foreach_loop => 'sub { my $sum = 0; $sum += $_ for @_; $sum }'],
    [eval_die     => 'sub { my ($x) = @_; eval { die "boom\n" if $x }; $@ }'],
    [method_call  => 'sub { my ($obj) = @_; $obj->frobnicate(1, 2) }'],
    [closure      => 'sub { my $c = 0; sub { $c++ } }'],
    [deref_chain  => 'sub { my ($x, $i) = @_; $x->[0]{k} + $x->{h}[$i] }'],
);

sub compile {
    my ($name, $src) = @_;
    my $code = eval $src;
    die "fixture $name failed to compile: $@" if $@ or ref $code ne 'CODE';
    $code;
}

1;

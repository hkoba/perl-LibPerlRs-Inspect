use strict;
use warnings;
use Test::More;
use LibPerlRs::Inspect;

sub returns_of {
    my ($src) = @_;
    my $code = eval $src;
    die "fixture failed: $@" if $@ or ref $code ne 'CODE';
    LibPerlRs::Inspect::analyze($code)->{returns};
}

subtest 'explicit and implicit returns' => sub {
    my $r = returns_of(<<'EOF');
sub {
    my ($v) = @_;
    return unless $v;
    return ($v, 1) if $v > 10;
    "small"
}
EOF
    my @kinds = map { $_->{kind} } @{$r->{returns}};
    is_deeply [sort @kinds], [sort 'empty', 'list', 'implicit'], 'three return sites'
        or diag explain $r;
    my ($list) = grep { $_->{kind} eq 'list' } @{$r->{returns}};
    is scalar @{$list->{exprs}}, 2, 'list return has 2 exprs';
    my ($imp) = grep { $_->{kind} eq 'implicit' } @{$r->{returns}};
    is_deeply $imp->{exprs}, ['"small"'], 'implicit return expr rendered';
};

subtest 'die with message' => sub {
    my $r = returns_of('sub { my ($x) = @_; die "boom\n" if $x; $x }');
    is scalar @{$r->{throws}}, 1, 'one throw site' or diag explain $r;
    is $r->{throws}[0]{via}, 'die', 'via die';
    like $r->{throws}[0]{message}, qr/boom/, 'message rendered';
};

subtest 'Carp::croak' => sub {
    my $r = returns_of('use Carp (); sub { my ($x) = @_; Carp::croak("bad arg") unless $x; $x * 2 }');
    my ($croak) = grep { $_->{via} eq 'croak' } @{$r->{throws}};
    ok $croak, 'croak detected' or diag explain $r;
    like $croak->{message} // '', qr/bad arg/, 'croak message';
};

subtest 'wantarray flag' => sub {
    my $r = returns_of('sub { wantarray ? (1, 2) : 1 }');
    ok $r->{uses_wantarray}, 'uses_wantarray true';
    my $r2 = returns_of('sub { 1 }');
    ok !$r2->{uses_wantarray}, 'uses_wantarray false';
};

done_testing;

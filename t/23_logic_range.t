use strict;
use warnings;
use Test::More;
use LibPerlRs::Inspect;
use JSON::PP ();

# Truth tables of a source line range, treated as one block
# (LibPerlRs::Inspect::analyze_logic_range). Line numbers below refer to
# the heredoc sources: line 1 is `sub {`.

sub compile {
    my ($src) = @_;
    my $code = eval $src;
    die "fixture failed: $@" if $@ or ref $code ne 'CODE';
    $code;
}

sub whens {
    my ($p) = @_;
    [map { [$_->{cond}, $_->{value} ? 1 : 0] } @{$p->{when}}];
}

sub outcomes {
    my ($l) = @_;
    [map { [$_->{line}, $_->{kind}, whens($_)] } @{$l->{paths}}];
}

my $guard = compile(<<'PERL');
sub {
 my ($x, $y) = @_;
 return 0 unless $x;
 log_it("go");
 if ($y) {
  note();
  return 1;
 }
 done();
 return 2;
}
PERL

subtest 'guard followed by a statement: fall-through outcome' => sub {
    my $r = LibPerlRs::Inspect::analyze_logic_range($guard, 3, 4);
    is_deeply $r->{lines}, [3, 4], 'lines as requested';
    ok !$r->{snapped}, 'not snapped';
    ok !$r->{tail}, 'not the tail of the sub';
    is_deeply $r->{logic}{conds}, ['$x'], 'one atom';
    is_deeply outcomes($r->{logic}),
        [[3, 'return', [[0, 0]]], [4, 'fallthrough', [[0, 1]]]],
        'return when !$x, fall through when $x';
    is_deeply [map { $_->{text} } @{$r->{logic}{paths}[1]{stmts}}],
        ['log_it("go")'], 'statement on the fall-through route';
    is scalar @{$r->{logic}{table}}, 2, '2 rows';
};

subtest 'header line only selects the whole if' => sub {
    my $r = LibPerlRs::Inspect::analyze_logic_range($guard, 5, 8);
    is_deeply $r->{lines}, [5, 7], 'snapped to the statement span';
    ok $r->{snapped}, 'snapped';
    is_deeply $r->{logic}{conds}, ['$y'], 'atom';
    is_deeply outcomes($r->{logic}),
        [[7, 'return', [[0, 1]]], [7, 'fallthrough', [[0, 0]]]],
        'return from the arm or fall through';
};

subtest 'lines inside an arm analyze the arm' => sub {
    my $r = LibPerlRs::Inspect::analyze_logic_range($guard, 6, 7);
    ok defined $r->{in_block_of}, 'in_block_of set';
    is_deeply $r->{logic}{conds}, [], 'no atoms inside the arm';
    is_deeply outcomes($r->{logic}), [[7, 'return', []]], 'unconditional return';
};

subtest 'tail of the sub keeps the implicit value' => sub {
    my $code = compile(<<'PERL');
sub {
 my ($a) = @_;
 if ($a) { note() } else { done() }
 finish();
}
PERL
    my $r = LibPerlRs::Inspect::analyze_logic_range($code, 4, 4);
    ok $r->{tail}, 'tail';
    is $r->{logic}{paths}[0]{kind}, 'implicit', 'implicit outcome';
    is_deeply $r->{logic}{paths}[0]{exprs}, ['finish()'], 'value';

    my $whole = LibPerlRs::Inspect::analyze_logic_range($code, 1, 100);
    # compare through JSON on both sides (analyze() returns native 1/0 for booleans)
    is_deeply $whole->{logic},
        JSON::PP->new->decode(LibPerlRs::Inspect::analyze_json($code))->{logic},
        'whole range equals the sub-level analysis';
    is_deeply [map { [map { $_->{text} } @{$_->{stmts}}] } @{$whole->{logic}{paths}}],
        [['note()'], ['done()']],
        'each arm route carries its statements past the if';
};

subtest 'loop control inside a loop body' => sub {
    my $code = compile(<<'PERL');
sub {
 my (@items) = @_;
 my $n = 0;
 for my $it (@items) {
  next unless $it;
  last if $it eq "stop";
  $n++;
 }
 return $n;
}
PERL
    my $r = LibPerlRs::Inspect::analyze_logic_range($code, 5, 7);
    is_deeply $r->{logic}{conds}, ['$it', '$it eq "stop"'], 'atoms';
    is_deeply outcomes($r->{logic}),
        [[5, 'next', [[0, 0]]], [6, 'last', [[0, 1], [1, 1]]], [7, 'fallthrough', [[0, 1], [1, 0]]]],
        'next / last / fall-through';
};

subtest 'no statement on the lines' => sub {
    my $r = eval { LibPerlRs::Inspect::analyze_logic_range($guard, 11, 12) };
    like $@, qr/no statement on lines 11-12/, 'dies with the range';
};

done_testing;

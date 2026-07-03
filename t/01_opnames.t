use strict;
use warnings;
use Test::More;

use OpTree::Analyzer;

# Primary use case: anonymous sub returned by eval.
my $sub = eval 'sub { my ($x) = @_; $x + 1 }';
die $@ if $@;

my $names = OpTree::Analyzer::op_names($sub);
is ref $names, 'ARRAY', 'op_names returns an arrayref';
cmp_ok scalar @$names, '>', 3, 'nontrivial op count';
is $names->[0], 'nextstate', 'execution order starts at nextstate';
is $names->[-1], 'leavesub', 'execution order ends at leavesub';
ok +(grep { $_ eq 'add' } @$names), 'contains add op'
    or diag explain $names;

# Named subs work the same way.
sub twice { $_[0] * 2 }
my $names2 = OpTree::Analyzer::op_names(\&twice);
ok +(grep { $_ eq 'multiply' } @$names2), 'named sub contains multiply'
    or diag explain $names2;

# Error cases must croak, not crash.
# (メッセージは #[xs_sub] の Cv 引数種別トランポリンが出す)
eval { OpTree::Analyzer::op_names_json(42) };
like $@, qr/must be a CODE reference/, 'croaks on non-ref';

eval { OpTree::Analyzer::op_names_json([]) };
like $@, qr/must be a CODE reference/, 'croaks on non-code ref';

eval { OpTree::Analyzer::op_names_json(\&utf8::is_utf8) };
like $@, qr/XSUB/, 'croaks on XSUB';

done_testing;

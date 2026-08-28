use strict;
use warnings;
use Test::More;
use FindBin;
use lib "$FindBin::Bin/lib";

use B qw(svref_2object class);
use LibPerlRs::Inspect;
use Fixtures;

# oracle テスト: capture_json の実行順チェーン (start_id → next) と
# 各ノードの class を、B モジュール (Perl 純正のリフレクション) の
# START → next チェーンおよび B::class と突き合わせる。

for my $fx (@Fixtures::FIXTURES) {
    my ($name, $src) = @$fx;
    my $code = Fixtures::compile($name, $src);

    my $ir = LibPerlRs::Inspect::capture($code);

    # IR 側: id → node の索引を作り、実行順チェーンを辿る
    my %by_id;
    my @stack = ($ir->{root});
    while (@stack) {
        my $n = pop @stack;
        $by_id{$n->{id}} = $n;
        push @stack, @{$n->{kids} || []};
    }
    my (@our_names, @our_classes);
    my %seen;
    my $id = $ir->{start_id};
    while (defined $id && !$seen{$id}++) {
        my $n = $by_id{$id} or last;
        push @our_names, $n->{name};
        push @our_classes, $n->{class};
        $id = $n->{next};
    }

    # B 側: CvSTART から op_next を辿る
    my $cv = svref_2object($code);
    my (@b_names, @b_classes);
    my %bseen;
    my $op = $cv->START;
    while (ref $op && $$op && !$bseen{$$op}++) {
        push @b_names, $op->name;
        push @b_classes, class($op);
        $op = $op->next;
    }

    is_deeply \@our_names, \@b_names, "$name: exec-order op names match B";
    is_deeply \@our_classes, \@b_classes, "$name: op classes match B::class";
}

done_testing;

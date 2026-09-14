use strict;
use warnings;
use Test::More;
use FindBin;
use lib "$FindBin::Bin/lib";

use B qw(svref_2object class);
use LibPerlRs::Inspect;
use Fixtures;

# Oracle test: cross-check capture_json's execution-order chain (start_id -> next)
# and each node's class against the START -> next chain and B::class of the
# B module (Perl's native reflection).

for my $fx (@Fixtures::FIXTURES) {
    my ($name, $src) = @$fx;
    my $code = Fixtures::compile($name, $src);

    my $ir = LibPerlRs::Inspect::capture($code);

    # IR side: build an id -> node index and walk the execution-order chain
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

    # B side: follow op_next starting from CvSTART
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

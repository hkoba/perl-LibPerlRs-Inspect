use strict;
use warnings;
use Test::More;
use FindBin;
use lib "$FindBin::Bin/lib";
use File::Spec;

use OpTree::Analyzer;
use JSON::PP ();
use Fixtures;

my $golden_dir = File::Spec->catdir($FindBin::Bin, 'golden');
mkdir $golden_dir unless -d $golden_dir;

my $json = JSON::PP->new->canonical->pretty;

for my $fx (@Fixtures::FIXTURES) {
    my ($name, $src) = @$fx;
    my $code = Fixtures::compile($name, $src);

    my $raw = OpTree::Analyzer::capture_json($code);
    # eval 連番はテストの実行順で変わるため正規化する
    $raw =~ s/\(eval \d+\)/(eval)/g;
    my $got = JSON::PP::decode_json($raw);

    my $path = File::Spec->catfile($golden_dir, "$name.json");
    if ($ENV{UPDATE_GOLDEN} or not -e $path) {
        open my $fh, '>', $path or die "$path: $!";
        print {$fh} $json->encode($got);
        close $fh;
        pass "$name: golden written";
        next;
    }

    open my $fh, '<', $path or die "$path: $!";
    my $want = JSON::PP::decode_json(do { local $/; <$fh> });
    close $fh;
    is_deeply $got, $want, "$name matches golden"
        or diag "regenerate with: UPDATE_GOLDEN=1 prove -b t/10_capture_golden.t";
}

done_testing;

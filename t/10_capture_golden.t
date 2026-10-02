use strict;
use warnings;
use Test::More;
use FindBin;
use lib "$FindBin::Bin/lib";
use File::Spec;
use Config;

use LibPerlRs::Inspect;
use JSON::PP ();
use Fixtures;

# The raw optree differs between perl versions and between threaded and
# non-threaded builds (e.g. GVs/consts are PADOPs/pad entries only under
# ithreads), so there is one golden set per "<major.minor>-<threading>".
my $config_key = sprintf '%s-%s', join('.', (split /\./, sprintf '%vd', $^V)[0, 1]),
    $Config{useithreads} ? 'threaded' : 'nonthreaded';
my $golden_dir = File::Spec->catdir($FindBin::Bin, 'golden', $config_key);
unless (-d $golden_dir) {
    plan skip_all => "no golden set for perl $config_key"
        . " (create with: UPDATE_GOLDEN=1 prove -b t/10_capture_golden.t)"
        unless $ENV{UPDATE_GOLDEN};
    mkdir $golden_dir or die "$golden_dir: $!";
}

my $json = JSON::PP->new->canonical->pretty;

for my $fx (@Fixtures::FIXTURES) {
    my ($name, $src) = @$fx;
    my $code = Fixtures::compile($name, $src);

    my $raw = LibPerlRs::Inspect::capture_json($code);
    # Normalize the eval sequence number, which varies with test execution order
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
    is_deeply $got, $want, "$name matches golden ($config_key)"
        or diag "regenerate with: UPDATE_GOLDEN=1 prove -b t/10_capture_golden.t";
}

done_testing;

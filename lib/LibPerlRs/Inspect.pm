package LibPerlRs::Inspect;
use strict;
use warnings;

our $VERSION = '0.01';

require XSLoader;
XSLoader::load('LibPerlRs::Inspect', $VERSION);

require JSON::PP;

# XS 側の String 返却は UTF8 フラグ付きの文字列 (バイト列ではない) なので
# decode_json ではなく文字列モードの decode を使う
my $_json = JSON::PP->new;

sub op_names {
    my ($code) = @_;
    $_json->decode(op_names_json($code));
}

sub capture {
    my ($code) = @_;
    $_json->decode(capture_json($code));
}

# analyze() は XS 側がネイティブの hashref を直接返す (JSON 経由なし)。
# JSON 文字列が欲しい場合は analyze_json() を使う。

1;
__END__

=head1 NAME

LibPerlRs::Inspect - analyze a subroutine reference at the OP tree level

=head1 SYNOPSIS

    use LibPerlRs::Inspect;

    my $sub = eval 'sub { my ($x) = @_; $x + 1 }';
    my $names = LibPerlRs::Inspect::op_names($sub);   # execution-order op names

=head1 DESCRIPTION

Rust-powered OP tree analysis for subroutine references, including
anonymous subs returned by C<eval>. See the project README.

=cut

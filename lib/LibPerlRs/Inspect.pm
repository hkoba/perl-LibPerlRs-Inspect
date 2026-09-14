package LibPerlRs::Inspect;
use strict;
use warnings;

our $VERSION = '0.01';

require XSLoader;
XSLoader::load('LibPerlRs::Inspect', $VERSION);

require JSON::PP;

# Strings returned from the XS side carry the UTF8 flag (they are not byte
# strings), so use string-mode decode rather than decode_json
my $_json = JSON::PP->new;

sub op_names {
    my ($code) = @_;
    $_json->decode(op_names_json($code));
}

sub capture {
    my ($code) = @_;
    $_json->decode(capture_json($code));
}

# analyze() returns a native hashref directly from the XS side (no JSON round trip).
# Use analyze_json() if you want the JSON string.

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

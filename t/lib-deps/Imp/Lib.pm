package Imp::Lib;
# Imp::User is loaded (and imports from this module) before this module
# has declared its exports.
use strict;
use Exporter 'import';
use Imp::User;
our @EXPORT_OK;
BEGIN { @EXPORT_OK = qw(helper) }
sub helper { 'helper' }
1;

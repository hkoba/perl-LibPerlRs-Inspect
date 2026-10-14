package Cyc::Hub;
use strict;
require Cyc::Router;

# a second package of this file inheriting from the first one: requires
# this file while it is still compiling, which is harmless
package Cyc::Hub::Part;
use base 'Cyc::Hub';
1;

use libperl_config::*;

fn main() {
    // The PL_savebegin write in begins.rs and UNOP_AUX SV items in
    // capture.rs take a different form depending on the threading mode,
    // so raise the perl_useithreads cfg in this crate too
    let cfg = PerlConfig::default();
    // ldopts (incl. rpath) are needed so that this crate's own test
    // binaries (tests/dbsrc.rs embeds perl) find libperl.so at run time
    cfg.emit_cargo_ldopts();
    cfg.emit_features(&["useithreads"]);
    // perlapi_verNN (e.g. perlapi_ver44 for OP_MULTIPARAM's aux struct)
    cfg.emit_all_perlapi_versions(42);
}

//! Generates notcurses bindings against the system headers.
//!
//! notcurses implements much of its API as `static inline` functions, which
//! have no symbol to link against. bindgen's `wrap_static_fns` emits a C shim
//! exposing each one as a real function, which we compile alongside.

use std::env;
use std::path::PathBuf;

/// Oldest notcurses we build against (Gentoo's current version).
const NC_MIN_VERSION: &str = "3.0.8";

fn main() {
    let lib = pkg_config::Config::new()
        .atleast_version(NC_MIN_VERSION)
        .probe("notcurses")
        .expect("pkg-config couldn't find notcurses");

    let out = PathBuf::from(env::var("OUT_DIR").unwrap());
    let shim = out.join("static_fns");

    println!("cargo:rerun-if-changed=wrapper.h");

    let bindings = bindgen::Builder::default()
        .header("wrapper.h")
        .clang_args(lib.include_paths.iter().map(|p| format!("-I{}", p.display())))
        .allowlist_function("(notcurses|ncplane|ncinput|nccell|ncchannels?|ncvisual|ncpile|ncpalette)_.*|ncstrwidth")
        .allowlist_type("(notcurses|ncplane|ncinput|nccell|ncvisual|ncblitter|ncscale|ncintype|ncalign).*")
        .allowlist_var("(NCOPTION|NCSTYLE|NCKEY|NCTYPE|NCALPHA|NCBLIT|NCSCALE|NCVISUAL_OPTION|NCMICE)_.*")
        .wrap_static_fns(true)
        .wrap_static_fns_path(&shim)
        .size_t_is_usize(true)
        .derive_default(true)
        .parse_callbacks(Box::new(bindgen::CargoCallbacks::new()))
        .generate()
        .expect("bindgen failed on notcurses headers");

    bindings.write_to_file(out.join("bindings.rs")).expect("couldn't write bindings");

    cc::Build::new()
        .file(shim.with_extension("c"))
        .include(env::current_dir().unwrap())
        .includes(&lib.include_paths)
        .warnings(false)
        .compile("notcurses_static_fns");
}

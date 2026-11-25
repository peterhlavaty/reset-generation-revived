//! Links the recompiled engine: a static library of the generated C, native/rt, the browser
//! platform layer (native/web/c) and its C library (native/web/libc), which `cargo x build-web`
//! compiles and names in RG_ENGINE_LIB.
fn main() {
    println!("cargo:rerun-if-env-changed=RG_ENGINE_LIB");
    let Ok(lib) = std::env::var("RG_ENGINE_LIB") else {
        panic!("the engine library is missing: build this crate with `cargo x build-web`");
    };
    let lib = std::path::PathBuf::from(lib);
    println!("cargo:rerun-if-changed={}", lib.display());
    println!("cargo:rustc-link-search=native={}", lib.parent().unwrap().display());
    println!("cargo:rustc-link-lib=static=rgengine");
}

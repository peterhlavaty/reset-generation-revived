//! Links the recompiled engine: a static library of the generated C, native/rt and the desktop
//! host shim, which `cargo x build-desktop` compiles and names in RG_ENGINE_LIB.
fn main() {
    println!("cargo:rerun-if-env-changed=RG_ENGINE_LIB");
    let Ok(lib) = std::env::var("RG_ENGINE_LIB") else {
        panic!("the engine library is missing: build this crate with `cargo x build-desktop`");
    };
    let lib = std::path::PathBuf::from(lib);
    println!("cargo:rerun-if-changed={}", lib.display());
    println!("cargo:rustc-link-search=native={}", lib.parent().unwrap().display());
    println!("cargo:rustc-link-lib=static=rgengine");
    let os = std::env::var("CARGO_CFG_TARGET_OS").unwrap();
    match os.as_str() {
        "windows" => {
            for l in ["ws2_32", "winmm"] {
                println!("cargo:rustc-link-lib={l}");
            }
            // one self-contained .exe (with crt-static, set by `cargo x build-desktop`): no
            // libwinpthread / libunwind DLLs next to it
            println!("cargo:rustc-link-lib=static=pthread");
        }
        _ => {
            println!("cargo:rustc-link-lib=pthread");
            println!("cargo:rustc-link-lib=m");
        }
    }
}

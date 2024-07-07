use std::env;
use std::path::PathBuf;

fn main() {
    let bindings = bindgen::Builder::default()
        .header("asterisk_wrapper.h")
        .clang_arg("-I/home/simon/src/asterisk/include")
        .clang_arg("-fblocks")
        .allowlist_item("ast_.*")
        .allowlist_item("__ast_.*")
        .allowlist_item("AST_.*")
        .allowlist_item("rust.*")
        .default_macro_constant_type(bindgen::MacroTypeVariation::Signed)
        .parse_callbacks(Box::new(bindgen::CargoCallbacks::new()))
        .generate()
        .expect("Unable to generate Asterisk bindings");

    let out_path = PathBuf::from(env::var("OUT_DIR").unwrap());
    bindings
        .write_to_file(out_path.join("bindings.rs"))
        .expect("Couldn't write bindings!");

    cc::Build::new()
        .file("asterisk_wrapper.c")
        .include("/home/simon/src/asterisk/include")
        .compile("asterisk_wrapper");
    println!("cargo::rerun-if-changed=asterisk_wrapper.c");
}

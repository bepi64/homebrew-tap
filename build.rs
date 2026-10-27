fn main() {
    cc::Build::new()
        .file("src/cache_init.c")
        .compile("cache_init");

    println!("cargo:rustc-link-arg=-Wl,-undefined,dynamic_lookup");

    if let Ok(lib_dir) = std::env::var("FAST_DOCS_LIB_DIR") {
        println!("cargo:rustc-link-arg=-Wl,-weak_library,{}/libgit2.dylib", lib_dir);
        println!("cargo:rustc-link-arg=-Wl,-rpath,{}", lib_dir);
    }
}

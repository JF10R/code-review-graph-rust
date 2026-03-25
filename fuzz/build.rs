fn main() {
    // On MSVC Windows, the libfuzzer static archive contains main() but the
    // MSVC linker won't pull it in automatically because no Rust code references
    // it directly. Force-include it via /INCLUDE and tell the linker the entry.
    if std::env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("msvc") {
        println!("cargo:rustc-link-arg=/INCLUDE:main");
        println!("cargo:rustc-link-arg=/ENTRY:mainCRTStartup");
    }
}

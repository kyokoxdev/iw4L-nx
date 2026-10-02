fn main() {
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("horizon") {
        return;
    }
    let dkp = std::env::var("DEVKITPRO").expect("DEVKITPRO must point at the devkitPro install");
    println!("cargo:rerun-if-env-changed=DEVKITPRO");
    println!("cargo:rustc-link-search=native={dkp}/portlibs/switch/lib");
    println!("cargo:rustc-link-search=native={dkp}/libnx/lib");
    // Early: libnx must precede newlib's -lc (both define fcntl).
    println!("cargo:rustc-link-lib=static=nx");
    // The late -lEGL/glapi/... args live in launcher/build.rs: link args only
    // reach binaries of the package that emits them.
}

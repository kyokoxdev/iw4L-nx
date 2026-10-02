fn main() {
    println!("cargo::rerun-if-changed=build.rs");
    if std::env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("msvc") {
        println!("cargo::rustc-link-arg-bins=/STACK:8388608");
    }
    // Switch: link args from a dependency's build script never reach the
    // binary, so the static GL stack is listed here, after every rlib.
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("horizon") {
        for lib in ["EGL", "glapi", "drm_nouveau", "stdc++", "nx"] {
            println!("cargo::rustc-link-arg-bins=-l{lib}");
        }
    }
}

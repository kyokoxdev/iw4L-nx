fn main() {
    println!("cargo::rerun-if-changed=build.rs");
    if std::env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("msvc") {
        println!("cargo::rustc-link-arg-bins=/STACK:8388608");
    }
    // Switch: link args from a dependency's build script never reach the
    // binary, so the static GL stack is listed here, after every rlib.
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("horizon") {
        for lib in ["SDL2", "EGL", "glapi", "drm_nouveau", "stdc++", "nx"] {
            println!("cargo::rustc-link-arg-bins=-l{lib}");
        }
        // platform_nx::tls: libnx has 12 pthread keys, serve more.
        for f in ["key_create", "key_delete", "setspecific", "getspecific"] {
            println!("cargo::rustc-link-arg-bins=-Wl,--wrap=pthread_{f}");
        }
        // platform_nx::__wrap_rename: replace an existing target.
        println!("cargo::rustc-link-arg-bins=-Wl,--wrap=rename");
        // platform_nx::__wrap_threadCreate: unpinned threads start on core 2.
        println!("cargo::rustc-link-arg-bins=-Wl,--wrap=threadCreate");
        // platform_nx::heap_samples: who holds the heap at an OOM.
        if std::env::var_os("CARGO_FEATURE_HEAP_SAMPLES").is_none() {
            return;
        }
        for f in ["malloc", "calloc", "realloc", "memalign", "aligned_alloc", "free"] {
            println!("cargo::rustc-link-arg-bins=-Wl,--wrap={f}");
        }
    }
}

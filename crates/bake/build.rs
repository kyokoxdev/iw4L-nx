use std::path::{Path, PathBuf};

fn sources(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            sources(&path, out);
        } else if matches!(path.extension().and_then(|e| e.to_str()), Some("c" | "cpp"))
            && !path.ends_with("source/main.cpp")
        {
            out.push(path);
        }
    }
}

fn main() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let uam = root.join("../../third_party/uam");
    println!("cargo:rerun-if-changed={}", uam.display());
    println!("cargo:rerun-if-changed=uam_shim.cpp");

    let mut files = vec![root.join("uam_shim.cpp")];
    for dir in ["source", "mesa-imported", "generated"] {
        sources(&uam.join(dir), &mut files);
    }
    files.sort();

    let windows = std::env::var("CARGO_CFG_TARGET_OS").unwrap() == "windows";
    let build = |cpp: bool| {
        let mut b = cc::Build::new();
        b.cpp(cpp)
            .std(if cpp { "c++11" } else { "c99" })
            .opt_level(3)
            .warnings(false)
            .define("NDEBUG", None)
            .define("PACKAGE_STRING", "\"uam 1.1.0\"")
            .define("DESKTOP", None)
            .define("_USE_MATH_DEFINES", None);
        for inc in [
            "source",
            "mesa-imported",
            "mesa-imported/glsl",
            "mesa-imported/glsl/glcpp",
            "generated",
            "generated/glsl",
            "generated/glsl/glcpp",
        ] {
            b.include(uam.join(inc));
        }
        // -mno-ms-bitfields keeps the DKSH struct layouts identical to Linux.
        if windows {
            b.flag("-mno-ms-bitfields")
                .define("__STDC_FORMAT_MACROS", None)
                .define("__USE_MINGW_ANSI_STDIO", "1");
        } else {
            b.define("_GNU_SOURCE", None).define("HAVE_POSIX_MEMALIGN", None);
        }
        if cpp {
            b.flag_if_supported("-Wno-class-memaccess")
                .flag_if_supported("-Wno-non-virtual-dtor");
        }
        b
    };

    let (cpp, c): (Vec<_>, Vec<_>) =
        files.iter().partition(|f| f.extension().unwrap() == "cpp");
    let mut cpp_build = build(true);
    if windows {
        // libstdc++.a as a cargo lib: it must come before rustc's -lgcc/-lmsvcrt.
        cpp_build.cpp_link_stdlib(None);
        let out = std::process::Command::new(cpp_build.get_compiler().path())
            .arg("-print-file-name=libstdc++.a")
            .output()
            .unwrap();
        let lib = PathBuf::from(String::from_utf8(out.stdout).unwrap().trim());
        println!("cargo:rustc-link-search=native={}", lib.parent().unwrap().display());
    }
    cpp_build.files(cpp).compile("uam_cpp");
    build(false).files(c).compile("uam_c");
    if windows {
        println!("cargo:rustc-link-lib=static=stdc++");
    }
}

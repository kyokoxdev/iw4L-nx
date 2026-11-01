# uam, vendored for iw4l-bake

- Upstream: https://github.com/devkitPro/uam, tag `v1.1.0` (commit `5a5afc2`), the version devkitPro installs. Licences: `LICENSE` (ISC for uam, MIT for `mesa-imported/`).
- `crates/bake/build.rs` compiles it with `cc`. `meson.build` and `source/main.cpp` are kept but not built.
- `generated/` holds the bison, flex and mako outputs from uam's meson build (bison, flex, Python 3.14 with mako 1.3), so the bake needs only a C/C++ compiler. To regenerate, run `meson setup build && ninja -C build` on upstream and copy the seven files from `build/mesa-imported/glsl{,/glcpp}`.
- One local change: `source/compiler_iface.h` declares `friend struct UamShim;`, so `crates/bake/uam_shim.cpp` can write DKSH to memory.

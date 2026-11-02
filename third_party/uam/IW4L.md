# uam, vendored for iw4l-bake

- Upstream: https://github.com/devkitPro/uam, tag `v1.1.0` (commit `5a5afc2`), the version devkitPro installs. Licences: `LICENSE` (ISC for uam, MIT for `mesa-imported/`).
- `crates/bake/build.rs` compiles it with `cc`. `meson.build` and `source/main.cpp` are kept but not built.
- `generated/` holds the bison, flex and mako outputs from uam's meson build (bison, flex, Python 3.14 with mako 1.3), so the bake needs only a C/C++ compiler. To regenerate, run `meson setup build && ninja -C build` on upstream and copy the seven files from `build/mesa-imported/glsl{,/glcpp}`.
- Two local changes:
  - `source/compiler_iface.h` declares `friend struct UamShim;`, so `crates/bake/uam_shim.cpp` can write DKSH to memory.
  - `mesa-imported/codegen/nv50_ir.cpp`: `Instruction::init` zeroes `sched`. `SchedDataCalculatorGM107::setDelay` reads the first instruction of the next block before that block is scheduled. A fresh process reads zeroed heap there, but the bake reuses heap, so without this one-byte stall/barrier fields vary from run to run. Zero matches the devkitPro CLI wherever that CLI reads zeroed memory (all but 1 of mp_rust's 1196 shaders; the CLI itself varies with `MALLOC_PERTURB_` on that one).

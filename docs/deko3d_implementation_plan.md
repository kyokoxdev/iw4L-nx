# deko3d backend for Switch: implementation plan

## Context

On Switch the render thread is the frame bottleneck. Run 1791470626 (non-profile, 6 bots) had a 28.6ms median frame, of which `graph_submit` was 7–21ms. That submit cost is GL command replay plus Mesa validation, about 22µs per draw. Draw-count work (merging, instancing, pretess) has reached its floor. deko3d is the native, NVN-like GPU library in devkitPro, and it removes the GL/Mesa layer entirely.

Vulkan is not an option on Horizon homebrew. Nintendo's Vulkan driver exists only inside the proprietary NintendoSDK, and devkitPro ships `switch-mesa` (GL) and deko3d only.

Goal: a deko3d backend for wgpu-hal in the wgpu-nx fork, so that all game and Bevy rendering code runs on it unchanged. Shaders are never compiled on the device. Players bake DKSH packs on their PC from their own game files and copy them to the SD card. The result has to be fast and complete enough to ship as an NSP.

## Locked decisions (from user)

| # | Decision |
|---|---|
| D1 | **Shape:** a new wgpu-hal backend `deko3d` in `~/projects/wgpu-nx`. No direct-API path, no custom renderer. |
| D2 | **Bake set:** exhaustive and headless. The bake loads every MP and SP zone on PC with the Switch ABI and enumerates every admitted pipeline. It does not record from play. |
| D3 | **Missing shader:** checked at load time. A map that has any missing DKSH refuses to load, with an on-screen error. It never stutters or shows holes mid-match. Dev builds with `IW4L_DKSH_STRICT=0` skip the draw and log key + label instead (amended 2026-10-28). |
| D4 | **Bake user:** end users, on Windows and Linux, with one command and no devkitPro install. |
| D5 | **Key:** a hash of the final GLSL text. The device runs the naga GLSL writer at pipeline creation; the bake runs the identical code. |
| D6 | **Packs:** `common.dkpk` plus one `<zone>.dkpk` per map. |
| D7 | **uam:** vendored (ISC licence) and linked into the bake tool as a library through a C shim. |
| D8 | **GL backend:** kept and selectable with `IW4L_BACKEND=gl\|deko3d` until parity, then deleted, along with Mesa, before the NSP. Default switched to `deko3d` on 2026-11-01 (user); `IW4L_BACKEND=gl` selects GL. |
| D9 | **SD root:** NSP reads `romfs:/shaders/dksh/` (LayeredFS from `sdmc:/atmosphere/contents/<NSP_TITLE_ID>/romfs/shaders/dksh/`). Dev NRO reads `sdmc:/switch/iw4l/shaders/dksh/`. `NSP_TITLE_ID` is a placeholder constant until M6. |
| D10 | **Testing:** hardware only. Dev builds link `libdeko3dd` (the debug library). |
| D11 | **Gate:** after the spike, continue only if submit+encode costs ≤8µs per draw on mp_rust and the frame matches the GL screenshot. |
| D12 | **Timing:** originally after the SP milestone; user started Phase 0 on 2026-10-28. |
| D13 | **Bake UX:** `iw4l-bake --games <MW2 dir> --out <dir>` writes the exact SD tree to copy as-is. |

## Corrections to the earlier draft (2026-10-28)

An earlier draft of this file conflicted with the decisions above. Kept here so it is not reintroduced:

- `Backend::Deko3d` is variant **6**; 5 is `BrowserWebGpu` (`wgpu-types/src/backend.rs`).
- FFI lives in `wgpu-nx/deko3d-sys/`, not `platform_nx` (wgpu-hal cannot depend on platform_nx). The `dk*MakerDefaults` helpers are `DK_CONSTEXPR static inline` in `deko3d.h`, so they are ported to Rust, not declared `extern`.
- The compiler is `uam` (`~/devkitpro/tools/bin/uam`). There is no `glsl_compiler` and no SPIR-V/spirv-cross step. `d3d9_sm3` emits WGSL text and has no naga dependency.
- No magenta fallback (see D3). No `PortId`-based key (not stable, misses Bevy pipelines). No single `shaders_maxwell.pack`, no `assets/shaders/` output (retail-derived, never in the repo).
- The bake is `crates/bake` (`iw4l-bake`), not `xtask` (xtask carries no engine crates and is not shipped).
- GL stays selectable (D8): `platform_nx` keeps EGL, the horizon GL throttle in `exact_pipeline.rs` is gated by backend, not deleted.
- No `TEXTURE_BINDING_ARRAY` (bound slots). Primitive topology comes from the pipeline. Pools are growable 32 MiB blocks, not 352 MB fixed. Texture uploads need staging (BlockLinear tiling).
- `[patch.crates-io]` keeps a git rev, never a local path. Fork branch: `switch-deko3d-v30` off `switch-gles-v30`.
- No perf promises beyond the D11 gate; wgpu-core record cost stays whatever the hal does.

## Derived rules (these follow from the decisions; no further choice needed)

- **No pipeline compile on device.** deko3d has no pipeline objects; render state is set dynamically. A hal `RenderPipeline` is therefore a CPU struct: DKSH handles plus prebuilt state blobs (rasterizer, colour, blend, depth-stencil, vertex attribute and vertex buffer state). Creating one costs about a millisecond (naga GLSL write, hash, lookup), so **every admitted pipeline is built during load**. This replaces the horizon-only rule "tech 5 + draw demand" (`render_gpu/src/drawsurf/colour_submit/pipeline.rs:245`) and the one-at-a-time GL compile rule (`exact_pipeline.rs:197`) under deko3d. `PipelineNotReady` stops occurring in play.
- **No game-files fingerprint.** The key is a content hash of the GLSL. If game files change, the result is clean misses, the load check refuses, and the user re-bakes. The pack header carries only the magic, the pack format version, the uam version and the entry count. The device refuses a pack whose format or uam version is wrong.
- **One translation module, two consumers.** The GLSL generation lives in one place, `wgpu-hal/src/deko3d_shader.rs`, and it builds for the host under the feature `deko3d-bake` without needing deko3d-sys. The device backend and `iw4l-bake` both call it, so they always produce byte-identical GLSL.
- **Coordinates:** the device is created with `DkDeviceFlags_DepthZeroToOne | DkDeviceFlags_OriginUpperLeft` (the defaults, which match WebGPU). The naga writer runs without `ADJUST_COORDINATE_SPACE`.
- **Bound slots stay.** The GLSL comes from the bound-slot WGSL ABI (`gpu_contract::bound_slots()`, already forced on horizon), so the same shaders work on GL and deko3d during A/B testing. Bindless rendering through deko3d texture handles is out of scope.
- **Shader packs live outside `cache/`.** `artifact_cache::sweep` (`asset_transport/src/artifact_cache.rs:37`) evicts by LRU under `cache/`, so packs go in `shaders/`.
- **Hash:** reuse `assets::fnv1a64` for the 64-bit key. Each entry also stores the GLSL byte length as a collision guard.

---

## Phase 0: Setup

**S1. deko3d-sys bindings crate.**
- Location: `~/projects/wgpu-nx/deko3d-sys/`, as a workspace member.
- Bindings are pregenerated once with bindgen from `~/devkitpro/libnx/include/deko3d.h` (v0.5.0) and checked in, so building needs no libclang.
- `build.rs` links `deko3d` by default, or `deko3dd` when the `debug` feature is on, from `~/devkitpro/libnx/lib`.
- It compiles only for `target_os = "horizon"`. On other targets it is an empty crate.
- *Done when:* `switch/build.sh build` links a test call to `dkDeviceCreate`/`dkDeviceDestroy`.

**S2. Register the backend in wgpu (no behaviour yet).** These are the files the survey found:
- `wgpu-types/src/backend.rs`: the `Backend::Deko3d` variant, `ALL`, `to_str`, the `Backends::DEKO3D` flag, and the `from_comma_list` alias `"deko3d"`.
- `wgpu-hal/src/lib.rs`: `pub mod deko3d` under `cfg(deko3d)`, plus the `api::Deko3d` re-export. Also `wgpu-hal/build.rs` (`deko3d` alias: feature plus horizon) and the `[features]` section of `wgpu-hal/Cargo.toml`.
- `wgpu-core/src/instance.rs:113-128`: the `try_add_hal(hal::api::Deko3d, …)` call.
- `wgpu-core/src/device/resource.rs:2414`: an arm in the `create_shader_module_passthrough` match that returns unsupported.
- `wgpu-core/build.rs`, `wgpu-core/Cargo.toml`, and `platform-deps/windows-linux-android/Cargo.toml` for feature forwarding.
- `wgpu/Cargo.toml` and `wgpu/build.rs`.
- `player/tests/player/main.rs:70`, if its match is exhaustive.
- `wgpu-core/src/device/queue.rs:1133`: guard the GLES downcast so it can't panic on another backend.
- The backend starts as a copy of the Noop skeleton (`wgpu-hal/src/noop/`, 931 lines), with `init` failing until S4.
- *Done when:* a desktop build with `--features deko3d` compiles, and `Backends::from_comma_list("deko3d")` round-trips.

**S3. Backend selection in the game.**
- `IW4L_BACKEND=gl|deko3d` is read from `sdmc:/switch/iw4l/.env`, defaults to `deko3d` (was `gl` until 2026-11-01), and is mapped to `WgpuSettings.backends`. It sits beside the existing wgpu feature setup in `crates/bootstrap/src/plugins.rs:114-127`.
- The `wgpu` git rev in the root `Cargo.toml` `[patch.crates-io]` is bumped to the fork commit that carries S1 and S2.
- *Done when:* a Switch build boots on GL as before, and `IW4L_BACKEND=deko3d` logs `backend not available` to `boot.txt` and exits cleanly.

**S4. Device, queue and swapchain clear.**
- Instance, adapter and device: `dkDeviceCreate`, with the debug callback writing to boot.txt through the existing `breadcrumb`.
- One `dkQueueCreate` (Graphics) queue.
- The surface is the NWindow already passed as an `AndroidNdk` raw handle (`platform_nx/src/lib.rs:782-830`). It uses `dkSwapchainCreate` with `nwindowGetDefault()`, three images, and an sRGB format.
- Configure, acquire and present map to `dkQueueAcquireImage` and `dkQueuePresentImage`. Vsync uses `dkSwapchainSetSwapInterval`.
- First check that the swapchain accepts `RGBA8_Unorm_sRGB`. If it does, the fork's sRGB present shader (`srgb_present.frag`) is not needed on deko3d. If not, keep a linear format plus shader sRGB writes, as GL does now.
- *Done when:* `IW4L_BACKEND=deko3d` shows the launcher's display smoke test (cycling clear colour) on hardware, with no game files.

**S5. GPU memory suballocator** (`wgpu-hal/src/deko3d/memory.rs`).
- There are three pools of `DkMemBlock` (32 MiB each, `DK_MEMBLOCK_ALIGNMENT` 0x1000):
  - buffers and descriptors: `CpuUncached|GpuCached`
  - images: `GpuCached|Image`
  - shader code: `CpuUncached|GpuCached|Code`, with 0x100 alignment
- Each block is suballocated with the existing `offset-allocator` 0.2 dependency, already in Cargo.lock through Bevy. A small self-check covers alloc, free and alignment.
- Memory is freed only after the fence of the last submit that used it. Each resource records `last_submit_index`, and the hal's `Fence` wraps `DkFence` plus a counter.
- *Done when:* the self-check passes on the host build, and the smoke test runs for 10 minutes on hardware with a stable heap line in boot.txt.

## Phase 1: Shader bake pipeline (on PC)

**B1. Shared GLSL translation** (`wgpu-hal/src/deko3d_shader.rs`, a sibling of `deko3d/` so it builds on host under `deko3d-bake` without deko3d-sys). Done 2026-11-01; `uam` accepts the sample. Instance index uses `gl_BaseInstanceARB` (`DRAW_PARAMETERS`); confirm on hardware in Phase 2 that `gl_InstanceID` excludes the base instance.
- Input: a naga `Module` and `ModuleInfo`, the entry point and stage, the pipeline constants, and the layout's `ResourceBinding → slot` map.
- Output: GLSL 460 text and its key, `fnv1a64(glsl)`.
- The logic mirrors `gles/device.rs:220-299`: `process_overrides`, then `BoundsCheckPolicies` all `Unchecked`, `zero_initialize_workgroup_memory`, then `glsl::Writer`.
- Binding slots follow deko3d's per-stage tables: 16 UBOs, 16 SSBOs, 32 textures (`DK_NUM_*`). Immediates map to a reserved UBO slot.
- Texture and sampler pairs from naga reflection become combined sampler slots, which later turn into `dkMakeTextureHandle` handles.
- Version is `Desktop(460)` with no `ADJUST_COORDINATE_SPACE`.
- *Done when:* a host unit test gives stable GLSL text for one sample bound-slot SM3 module, and `uam` accepts that GLSL.

**B2. Vendored uam plus C shim.** Done 2026-11-01. The bison, flex and mako outputs are pre-generated in `third_party/uam/generated/` (see `IW4L.md` there). The Windows exe links libstdc++ statically. Its selftest ran on Windows through WSL interop, not Wine.
- `third_party/uam/` holds the sources pinned at v1.1.0, the version installed in devkitPro, with the ISC licence file kept.
- `crates/bake/build.rs` builds it with the `cc` crate (C++), together with a shim `uam_compile(glsl, stage, out_buf) -> rc`.
- Windows is cross-built from Linux as `x86_64-pc-windows-gnu`.
- *Done when:* `iw4l-bake selftest` compiles B1's sample GLSL to DKSH on Linux and on Windows (Wine or a real PC), and the output is byte-identical to the devkitPro `uam` CLI.

**B3. Headless zone enumeration.** Partly done 2026-11-01: mp_rust gives 5041 modules (ports plus probe twins) and 6633 pipelines in 27 s. The bake loads zones with `load_prepared_match`, and its keys come from the runtime's `for_each_scheduled_key`. Still to do: SP maps; the Switch view signature (the lens default is used for now); and a check that every device-learned key (`exact-demand-v2`) is in the bake set.
- A new crate, `crates/bake`, with the bin `iw4l-bake`.
- For each zone from `asset_transport::list_mp_maps` (`discover.rs:785`) and `list_sp_maps` (`:851`), plus the common zones, it runs the real catalog and `MaterialProgramCompile` path. The entry is `material_compile.rs:529`, across every technique × vertex type (`technique_compile_vertex_types`, `:16`).
- The Switch ABI is forced: `bound_slots=true`, `shader_srgb_write=true`, and the instancing prologue on. Add an explicit `gpu_contract` override so these don't depend on `cfg!(horizon)`.
- The Dxbc arm (`material_runtime.rs:~70-80`, IW5/T5/T6 maps) is included.
- It then enumerates `ExactColourPipelineKey`s the way `schedule_admitted_pipelines` does (`pipeline.rs:227-320`), with the horizon tech-5 filter turned off, across all module variants (plain, `cached_lighting`, `probe_array`), the view formats and the sample counts.
- *Done when:* for mp_rust the bake reports N modules and M pipelines, and M is at least the learned count (371) and the ~1500 noted at `pipeline.rs:237`.

**B4. Engine and Bevy pipeline enumeration (common pack).**
- Every non-SM3 pipeline is listed with its full specialization key space:
  - floatz (MSAA and non-MSAA depth)
  - `iw_tess`
  - `script_grade` (five entry points)
  - `shellshock_screen`, `hud_blood`, postfx
  - `sun_effects` (each `SunPass` × the `SUN_FLARE` defs)
  - `geometry_diagnostic`, if it ships on Switch (otherwise excluded)
  - the Bevy UI, text and sprite pipelines
  - `Mesh2d`+`ColorMaterial` for the presenter (`Rgba16Float`)
  - `ViewUpscalingPipeline`
- Each specialized pipeline gains an `all_keys()` enumerator used only by the bake.
- The bake drives these through a host `deko3d-bake` "record" hal device. It is the Noop backend plus B1, and its `create_render_pipeline` emits GLSL to the bake instead of the GPU. Each Bevy pipeline is then specialized for every key.
- *Done when:* `common.dkpk` holds every engine pipeline. The device check in P2 later confirms that no runtime miss happens in a full menu → match → menu cycle.

**B5. Pack writer and CLI.** Mostly done 2026-11-02. mp_rust bakes twice to identical bytes, after a uam fix: `Instruction::init` now zeroes `sched` (see `third_party/uam/IW4L.md`). Compiles run in one persistent `iw4l-bake uam` worker process per core, since Mesa's context is global; mp_rust takes 55–62 s, down from about 100 s. `--dump` writes the GLSL and DKSH; for TGSI, run devkitPro `uam -t` on the dumped GLSL. Still to do: the all-zone double bake.
- Format: header `DKPK | format u32 | uam_version | count`, then entries sorted by key `{key u64, glsl_len u32, stage u8, dksh_len u32, dksh bytes, 0x100-aligned}`.
- Packs are written atomically (`.tmp` then rename), the same pattern as `artifact_cache.rs:34-76`.
- Compiles run in parallel across cores. Identical GLSL is compiled once.
- Command: `iw4l-bake --games <MW2 dir> --out <dir> [--zones a,b] [--dump <key>]`. It writes `<out>/switch/iw4l/shaders/dksh/{common,<zone>}.dkpk`; the NSP layout is added in R2.
- At the end it prints counts, timing and total size.
- `--dump` writes the GLSL plus uam's raw/TGSI output for one key, for debugging.
- *Done when:* baking every zone twice gives byte-identical packs (this is the runnable check), and one host test round-trips a pack.

## Phase 2: Spike (world colour pass on mp_rust)

**K1. Resources.**
- Buffers come from the S5 pool. `map` returns the CPU address of an uncached block, and `write_buffer` staging is a direct `memcpy` into it.
- Textures use `dkImageLayoutInitialize` + `dkImageInitialize`, covering BC1/2/3 (unorm and sRGB), BC5, RGBA8/16F, R32F, Depth24Stencil8, Depth32F and cube arrays.
- Uploads go through a CPU staging block and `dkCmdBufCopyBufferToImage`.
- Samplers use `dkSamplerDescriptorInitialize`.
- Bind groups are CPU records of (slot → buffer range or image+sampler). Image and sampler descriptors live in per-device descriptor sets bound once with `dkCmdBufBindImageDescriptorSet`/`dkCmdBufBindSamplerDescriptorSet`, and handles come from `dkMakeTextureHandle`.

**K2. Pipelines.**
- `create_shader_module` stores the naga module, as GLES does.
- `create_render_pipeline` runs B1 per stage, looks the key up in the installed packs, calls `dkShaderInitialize` from code memory, and prebuilds the state blobs.
- On a miss it returns `PipelineError::Linkage("missing dksh <key> <label>")`.
- Pack install API: `wgpu_hal::deko3d::packs::install(bytes)`, which builds a key → offset index and uploads the code into the Code pool lazily, on first use.

**K3. Command encoding.**
- The hal `CommandEncoder` records straight into a `DkCmdBuf` backed by a ring of command memory blocks.
- Mapping:
  - `begin_render_pass` → `dkCmdBufBindRenderTargets`, plus clear and discard
  - `set_render_pipeline` → `dkCmdBufBindShaders` + `Bind*State`
  - `set_bind_group` → `dkCmdBufBindUniformBuffers`/`StorageBuffers`/`Textures`
  - vertex and index buffers → `dkCmdBufBindVtxBuffers`/`BindIdxBuffer`
  - `draw_indexed` → `dkCmdBufDrawIndexed`
  - `multi_draw_indexed_indirect` → `dkCmdBufDrawIndexedIndirect`
  - copies → `dkCmdBufCopyBuffer`/`CopyImage`/`BlitImage`
  - transitions → `dkCmdBufBarrier`
- Redundant-bind filtering is per encoder, the same idea as fork commit `2b384a390`.
- `submit` maps to `dkQueueSubmitCommands` + `dkQueueSignalFence` + `dkQueueFlush`.

**K4. Spike wiring in the game.**
- The device loads `shaders/dksh/common.dkpk` at boot and `<zone>.dkpk` at map load, through the existing transport read path.
- `IW4L_DEKO3D_SPIKE=1` parks the UI, postfx and sun effects (the cameras and passes that aren't world colour), so the spike only needs K1–K3 for the world colour pass and the presenter.
- Timing spans: `deko3d encode` (inside `CommandEncoder::finish`) and `deko3d submit`. The existing `colour draws:` line gives the draw count.

**K5. Gate measurement (D11).**
- Same scene, same play build, mp_rust solo. Five fixed spawn viewpoints, including the open view of about 1100 draws.
- Metric: µs per draw = (`graph_render` + `graph_submit` render-thread ms) ÷ colour draws, for GL and then for deko3d.
- Visual: the existing screenshot capture (`render/src/diag/capture.rs`, which needs hal `copy_texture_to_buffer`) taken on both backends and diffed on PC.
- **Go** if deko3d is ≤8µs/draw (GL is about 28 by this metric) and the diff is visually equal.
- **No-go:** stop and report where the time goes (wgpu-core record versus hal encode), using the span profile. Then decide with the user whether to thin wgpu-core inside the fork.

## Phase 3: Full backend (after Go)

**F1. Everything else.** Compute passes (the hal must support them even though the game uses none), queries (timestamps through `dkCmdBufReportCounter`), `clear_texture`, `copy_texture_to_buffer` and readback, MSAA resolve (`dkCmdBufResolveImage`), and the remaining Device and CommandEncoder trait methods. Any method left unimplemented returns an error, never `todo!()`.

**F2. Limits and features.** Report honest limits from the `DK_*` constants: 16 UBOs, 16 SSBOs, 32 textures, 8 render targets, 32 vertex attributes, 16 vertex buffers. Binding arrays are off. Features: BC compression, `DEPTH32FLOAT_STENCIL8`, `INDIRECT_FIRST_INSTANCE`, `IMMEDIATES`, `TIMESTAMP_QUERY`, `CLEAR_TEXTURE`. The features requested in `bootstrap/src/plugins.rs:114-127` must all resolve.

**F3. Pipeline scheduling under deko3d.**
- In `pipeline.rs`/`exact_pipeline.rs`, the horizon tech-5 and one-at-a-time rules apply only when the backend is GL.
- With deko3d, every admitted pipeline is built in parallel on `assets::load_pool()` during the world-spawn "gpu" phase.
- The `learned.rs` list is skipped under deko3d, because it isn't needed.

**F4. Load-time check (D3).**
- The world-spawn gpu phase (`prepare/scene/spawn.rs`) waits for every scheduled pipeline.
- Any `missing dksh` error refuses the load and shows: `Shaders missing for <zone> (N). Re-run iw4l-bake.` It then returns to the menu.
- At boot, the same check applies to the common pack and the engine pipelines.
- A runtime miss after load (which should be impossible) is logged with the key and label, and the draw is refused. `IW4L_DKSH_STRICT=1` makes it panic instead, for sweeps.

## Phase 4: Parity and debugging

**P1. Debug tooling (built first; used by everything below).**
- `--features deko3d-debug` links `libdeko3dd`, routes its callback to boot.txt, and checks `dkQueueIsInErrorState` after every submit.
- deko3d has no GPU debug markers, so a CPU ring of the last 64 encoded passes and pipeline labels is dumped on error.
- A GPU hang watchdog: if a fence wait goes over 2s, it logs the ring and the in-flight submit, then aborts with a breadcrumb.
- On a miss, `iw4l-bake --dump <key>` gives the matching GLSL and DKSH from the label the device logged.

**P2. Coverage sweep.**
- With `IW4L_DKSH_STRICT=1` on hardware, load every MP map and every SP level, and do menu → match → menu twice.
- Any miss is a bake-enumeration bug, fixed in B3 or B4. It is not worked around on the device.

**P3. Visual parity.**
- The K5 screenshot diff on four maps (mp_rust, mp_terminal, mp_subbase, one SP level) and on the UI, console, HUD, postfx, killcam and cinematics (bink `ImageNode`).
- Check sRGB, depth range, cull winding and alpha test against GL.

**P4. Stability.**
- Ten-minute matches with 6 bots, docked and handheld.
- The heap and GPU pool lines must not grow (the boot.txt sampler gets a GPU pool usage field).
- Teardown and reload must not leak memory blocks.
- HOME suspend and resume must survive, which means the swapchain is recreated if it is invalidated.

## Phase 5: Performance

**R1. Measure and trim.** Run the span profile with deko3d. Targets: render thread well under the main thread, and a 6-bot median ≤16.7ms. Likely levers: command-memory ring sizing, fewer `dkQueueFlush` calls, descriptor-set updates batched per frame, and uncached CPU-write patterns (sequential writes only, no read-back from uncached memory).

## Phase 6: Cut-over and M6 hooks

**R2.** Make deko3d the default. Remove the GL backend from Switch builds: the `gles` feature for horizon, the Mesa/EGL link, `srgb_present.frag` if S4 showed it is unneeded, `learned.rs` if it has no remaining user, and the GL-only scheduling branches. Add `--layout nsp` to `iw4l-bake`, which writes `<out>/atmosphere/contents/<NSP_TITLE_ID>/romfs/shaders/dksh/`. The device reads from `romfs:/shaders/dksh/` when it runs as an NSP. Note: the NSP must contain a romfs, even a stub one, for LayeredFS to apply. Docs to update: `docs/SWITCH.md` (bake step, SD layout, `IW4L_BACKEND`) and a new `docs/DEKO3D.md` covering the backend and pack format.

---

## Critical files

- **wgpu-nx:**
  - `deko3d-sys/` (new)
  - `wgpu-hal/src/deko3d/{mod,adapter,device,queue,command,memory,shader,surface,conv,packs}.rs` (new)
  - `wgpu-hal/src/lib.rs`, `build.rs`, `Cargo.toml`
  - `wgpu-types/src/backend.rs`
  - `wgpu-core/src/instance.rs`, `device/resource.rs`, `device/queue.rs`, `build.rs`, `Cargo.toml`
  - `wgpu/Cargo.toml`, `build.rs`
- **iw4L-nx:**
  - `crates/bake/` (new; bin `iw4l-bake`)
  - `third_party/uam/` (new)
  - `crates/bootstrap/src/plugins.rs`
  - `crates/render_frame/src/gpu_contract.rs` (ABI override)
  - `crates/render_gpu/src/drawsurf/colour_submit/pipeline.rs`, `drawsurf/exact_pipeline.rs`
  - `crates/render_frontend/src/prepare/scene/spawn.rs`
  - the pipeline files that gain `all_keys()`: `sun_effects.rs`, `floatz.rs`, `postfx.rs`, `script_grade.rs`, `hud_blood.rs`, `shellshock_screen.rs`, `iw_tess.rs`
  - root `Cargo.toml` (patch rev)
  - `docs/SWITCH.md`

## Reuse

- `assets::fnv1a64`: the key hash.
- `offset-allocator`: GPU suballocation.
- `artifact_cache` atomic write pattern: pack writes.
- `asset_transport::list_mp_maps`/`list_sp_maps`: the zone list.
- `MaterialProgramCompile` and `schedule_admitted_pipelines`: enumeration.
- GLES `create_shader`/`create_pipeline_layout`: the template for B1.
- Noop backend: the skeleton.
- `breadcrumb`/boot.txt: device logging.
- Span profile and the `colour draws:` line: gate metrics.
- `diag/capture.rs`: screenshots.

## Verification

1. **Host:**
   - the B1 GLSL-stability test
   - the B2 `iw4l-bake selftest`, Linux and Windows, matching the devkitPro `uam` CLI byte for byte
   - the B5 pack round-trip test, and two full bakes giving identical bytes
   - the S5 allocator self-check
2. **Hardware, in order:**
   - S4 clear colour
   - the K5 gate (µs/draw and screenshot diff) → the go/no-go decision is reported to the user
   - P2 strict sweep across all maps with zero misses
   - P3 visual diffs
   - P4 ten-minute docked and handheld matches
   - R1 frame targets
3. **Load-check behaviour:** delete one zone pack, load that map, and confirm the "Re-run iw4l-bake" message appears and the game returns to the menu without a crash.
4. **Before any public push:** `make publish-check` and the `CONTEXT.md` push pass. No retail-derived shaders or DKSH are ever committed or shipped; only the bake tool is.

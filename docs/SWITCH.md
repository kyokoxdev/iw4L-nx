# Nintendo Switch (homebrew)

`target_os = "horizon"`, a `.nro` started from the Homebrew Menu (hbmenu). The
platform layer is `crates/platform_nx`: libnx link, Bevy runner, SDL2 gamepads
in place of gilrs, focus from `appletGetFocusState`, system keyboard (`swkbd`)
for menu edit fields and the console. Audio is cpal's `horizon` host over SDL2.

GL has no binding arrays, so material passes bind **bound slots**: group 1
holds one texture and one sampler per sampler register
(`d3d9_sm3::bound_slot_bindings`), built per draw from the same texture table
the bindless path indexes. On for Horizon; `IW4L_BOUND_SLOTS=1` forces it on a
desktop build to compare against bindless on the same backend.

Needs devkitPro (`devkitA64`, `libnx`, `switch-sdl2`, `switch-mesa`) in
`~/devkitpro`, and nightly Rust with `rust-src` (build-std).

```bash
switch/build.sh build --profile play -p launcher
nacptool --create "IW4L" "<author>" "0.1.0" iw4l.nacp      # once
mkdir -p switch/out
elf2nro target/nx/aarch64-nintendo-switch-newlib/play/iw4l.elf switch/out/iw4l.nro --nacp=iw4l.nacp
cargo clean   # target/ grows to tens of GB; keep only switch/out/
```

`switch/build.sh` copies the toolchain's `library/` into
`target/nx-std-src/` once per nightly, so std links the libc fork too. Forked
crates come through `[patch.crates-io]` in `Cargo.toml`: `libc`, the `wgpu`
family (static EGL, `NWindow` surface, shader sRGB present: switch-mesa's
EGL has no sRGB surfaces), and
[`iw4l-nx-crate-patches`](https://github.com/kyokoxdev/iw4l-nx-crate-patches)
for `bevy_asset`, `memmap2` and `cpal`. Each fork change is cfg-gated, so
desktop builds compile crates.io code.

SD card layout, all under `sdmc:/switch/iw4l/` (process working directory and
`BEVY_ASSET_ROOT`):

```
iw4l.nro
.env                 optional, same keys as desktop
games/MW2/zone/...   MW2 Multiplayer files; `IW4L_GAMES` overrides
games/MW2/main/video/*.bik  cinematics (`cinematic <name>`); the launch intro is
                     `IW4L_INTRO` (comma list, default IW_logo,legal; `0` = off)
iw4l-artifacts/      writable state, cache, logs
boot.txt             breadcrumbs from main to the first frames, Bevy/wgpu warnings,
                     native aborts and exits; rewritten per launch
```

Console: click both sticks to toggle it; while open, A types a line on the
system keyboard and runs it, B closes.

With no game files the launcher runs a display smoke test: a cycling clear
color. A boot that dies before logging is set up leaves its last step in
`boot.txt`.

Run from hbmenu in **title takeover** (hold R while starting a game), not
album: applet mode has too little memory.

# Nintendo Switch (homebrew)

`target_os = "horizon"`, a `.nro` started from the Homebrew Menu (hbmenu). The
platform layer is `crates/platform_nx`: libnx link, Bevy runner, SDL2 gamepads
in place of gilrs, focus from `appletGetFocusState`, system keyboard (`swkbd`)
for menu edit fields and the console. Audio is cpal's `horizon` host over SDL2.

Needs devkitPro (`devkitA64`, `libnx`, `switch-sdl2`, `switch-mesa`) in
`~/devkitpro`, and nightly Rust with `rust-src` (build-std).

```bash
switch/build.sh build --profile play -p launcher
nacptool --create "IW4L" "<author>" "0.1.0" iw4l.nacp      # once
elf2nro target/nx/aarch64-nintendo-switch-newlib/play/iw4l.elf iw4l.nro --nacp=iw4l.nacp
```

`switch/build.sh` copies the toolchain's `library/` into
`target/nx-std-src/` once per nightly, so std links the libc fork too. Forked
crates come through `[patch.crates-io]` in `Cargo.toml`: `libc`, the `wgpu`
family (static EGL, `NWindow` surface), and
[`iw4l-nx-crate-patches`](https://github.com/kyokoxdev/iw4l-nx-crate-patches)
for `bevy_asset`, `memmap2` and `cpal`. Each fork change is cfg-gated, so
desktop builds compile crates.io code.

SD card layout, all under `sdmc:/switch/iw4l/` (process working directory and
`BEVY_ASSET_ROOT`):

```
iw4l.nro
.env                 optional, same keys as desktop
games/MW2/zone/...   MW2 Multiplayer files; `IW4L_GAMES` overrides
iw4l-artifacts/      writable state, cache, logs
boot.txt             breadcrumbs from main to the first frames, rewritten per launch
```

Console: click both sticks to toggle it; while open, A types a line on the
system keyboard and runs it, B closes.

With no game files the launcher runs a display smoke test: a cycling clear
color. A boot that dies before logging is set up leaves its last step in
`boot.txt`.

Run from hbmenu in **title takeover** (hold R while starting a game), not
album: applet mode has too little memory.

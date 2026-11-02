# iw4L-nx

Fork of vladtrc/iw4L (`upstream`) with a Nintendo Switch port. `origin` = kyokoxdev/iw4L-nx.
Read first: `AGENT.md` → `CONTEXT.md` → `docs/INDEX.md`.

## Branches
- `switch-port` = working branch. `master` only mirrors upstream; never commit Switch work to it.
- Sync: fast-forward master to upstream, merge into `switch-port`.
- After a merge, the first load rebuilds the WGSL and nav caches (~9 min). Judge perf on later loads.

## Commands
- Desktop: `make map mp_boneyard`, `make menu`, `make bench <demo>` (`make help` lists the rest). Needs `.env` (copy `.env.example`, set `IW4L_GAMES`).
- Switch: `switch/build.sh build --profile play -p launcher`, then `elf2nro` (see `docs/SWITCH.md`). Needs devkitPro in `~/devkitpro` and nightly Rust.
- Build outputs (`.nro`, `iw4l.sym.elf`, `iw4l-bake`, test packs) go in `switch/out/` (gitignored), never `~/iw4l-nx`. Run `cargo clean` after each build or bake once outputs are copied: `target/` reaches 30 GB+.
- Before a public push: `make publish-check` and the "Making a public push" pass in `CONTEXT.md`.

## Gotchas
- Switch GL has no binding arrays; material passes use bound slots (`IW4L_BOUND_SLOTS=1` forces this on desktop).
- Forked crates come through `[patch.crates-io]` in `Cargo.toml` (`libc`, `wgpu`, `iw4l-nx-crate-patches`). Fork changes are cfg-gated.
- `IW4L_MULTI_DRAW=1` crashes the Switch at load end. Keep it off.
- Don't touch other agents' `context/mrs/*`. `context/` is gitignored.
- No retail offsets or `FUN_`/`DAT_` names in code.

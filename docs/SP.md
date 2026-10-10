# Singleplayer (campaign): status and plan

Fork-only. Checked against retail files, except the load path.

## What already holds

- `fastfile_iw4` walks every retail SP and Special Ops zone, `GameWorldSp`
  (`PathData`, `VehicleTrack`, glass) included; nothing consumes path data yet.
- `asset_transport::list_sp_maps`: non-MP IW4 zones with a
  `video/<stem>_load.bik` (SP has no `_load` zones and no `localized_common`).
  Resolve stems with `find_zone_file_version`, not `find_zone_file` (it aliases
  `boneyard` to `mp_boneyard`). Special Ops levels layer over a base level zone.
- `Catalog::iw4_sp()`: the generated MP list plus the hand-kept
  `profile/iw4_sp_catalog.rs` (only the video builtins so far).
- `iw4l check-scripts <zone>...`: links each GSC module in the zones (load
  order, later wins) as its own root against `Catalog::iw4_sp()`, prints parse
  faults and unresolved names, exits 1 if any. Retail ships unloaded scripts
  that do not parse (`animscripts/traverse/stairs_*`).
- Compiler: `childthread` (ends with its spawning thread), vector constants.
- Video: `CinematicInGame[Sync|Loop]`, `StopCinematicInGame` and
  `IsCinematicPlaying` set `EngineState::cinematic`, timed by `.bik` header
  lengths read at install (`LevelData::videos`, only when scripts use them);
  `console::cinematic` plays it on the local client, not yet on remote ones.
  `video/<map>_load.bik`, if any, plays over a map load and holds advancement
  and admission (`frame::LoadCinematicPlaying`); skippable once the map is
  ready. In-world screens (`CinematicInGameLoopResident`) are refused.

## Where MP is hardwired

| what | where |
|---|---|
| common zone is `common_mp` | `assets::match_load::approve_map_load`, `session_load/common_cache.rs`, `common_walks.rs` |
| MP zone alias on bare stems | `asset_transport::discover::find_zone_stem` |
| spawns are `mp_dm_spawn*` | `gamemode_iw4::ffa`, `asset_world::map_entities` |
| session links against `Catalog::iw4()` | `session::match_apply` (SP needs `iw4_sp()`) |
| frontend tiles say NOT AVAILABLE | `crates/ui/menus/frontend.json` |
An unknown builtin refuses the install, so SP GSC waits on a full `IW4_SP`.

## Slices

1. Discovery, zone walks, compiler gaps — done. `code_post_gfx common <level>`
   over all 45 levels: 341 unknown builtins, 269 shared by every level.
2. Static SP load: an SP request swaps `common_mp` for the SP common set
   (`code_pre_gfx`, `code_post_gfx`, `common`, `patch`, `ui`), loads the
   map, skips GSC, spawns at `info_player_start`. Done when you can walk it.
3. Frontend: Campaign tile lists `list_sp_maps`.
4. Fill `IW4_SP`; run `maps/_load` with AI natives stubbed; in-world video
   (`CINEMATIC_*` textures) once a mission needs it.
5. Script anim trees, AI actors (`PathData`, animscripts, combat), saves.
6. Switch: SP zones' memory budget.

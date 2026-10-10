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
  `profile/iw4_sp_catalog.rs` (every name `maps/trainer` reaches).
- `iw4l check-scripts <zone>... [<root module>...]`: links the named roots
  (`maps/trainer`), or else each GSC module in the zones (load order, later
  wins) as its own root, against `Catalog::iw4_sp()`, prints parse
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
| Special Ops tile says NOT AVAILABLE | `crates/ui/menus/frontend.json` |

## Slices

1. Discovery, zone walks, compiler gaps — done. `code_post_gfx common <level>`
   over all 45 levels: 341 unknown builtins, 269 shared by every level.
2. Static SP load: an SP request swaps `common_mp` for the SP common set
   (`code_pre_gfx`, `code_post_gfx`, `common`, `patch`, `ui`), loads the
   map, skips GSC, spawns at `info_player_start`. Done when you can walk it.
   Holds on desktop for trainer: the engine spawns the joining player
   (`MatchBootstrap::campaign`), kits come from `body_*` + the level's one
   `*_viewhands`. The level zone's WeaponDefs join the registry where the
   common zone lacks the name. Every campaign level loads (load-only sweep).
   Left: campaign still runs the MP match flow as `dm` (FFA label, dm spawn
   lookup); level GSC needs a gametype-less campaign mode.
3. Frontend — done. The Campaign tile opens `iw4l_campaign`: `list_sp_maps`
   without `so_*`, in story order (`console::campaign_menu`), 20 rows; a row
   requests the level like `map <level>`. Names are stems (`AF CAVES`).
4. Level GSC — in progress. Campaign runs `maps/<level>::main` plus
   `codescripts/{delete,struct}` against `iw4_sp()`, started once the local
   player begins (SP scripts expect the player before `main`). Builtins not
   built yet are stubs (`natives/sp_pending.rs`, typed defaults, first call
   logged as `campaign builtin X not built yet`). Campaign weapon script names
   are bare (`m4_grunt`); level-zone world gun models join the catalog.
   SP `playsound` notifies after a fixed 2 s; SP profile fields last one
   session. Trainer runs; its faults left need actors, anim trees, vehicles.
   In-world video (`CINEMATIC_*` textures) once a mission needs it.
5. Script anim trees — in progress. `host/script_anim.rs`: `useanimtree`,
   the `setanim`/`setflaggedanim` family, `clearanim`, anim time/weight,
   `animscripted` with root motion (zone `delta_quat` keys), notetrack
   notifies, `getanimlength` and the clip queries. Trees are the level's
   `animtrees/<tree>.atr`, kept to the `%anim` names scripts use
   (`session::match_apply::script_anim_library`); clips from the xanim catalog.
   Trainer's prop and vehicle trees bind every clip; scenes on actors wait on 6.
6. AI actors (`PathData`, animscripts, combat) on the bot system, saves.
7. Switch: SP zones' memory budget.

# Singleplayer (campaign): status and plan

Fork-only. Checked against retail files, except the load path.

Upstream merges: campaign code in a file upstream also edits lives in a child
module `<file>/campaign.rs` (it keeps private access), declared by one line in
the parent, so a merge meets one-line hooks rather than SP blocks. Upstream's
`isspawner` and `playsound` stay as they are; `actors::register` runs last and
replaces them. Campaign `RoundScript` fields are `RoundScript::campaign`.

## What already holds

- `fastfile_iw4` walks every retail SP and Special Ops zone, `GameWorldSp`
  (`PathData`, `VehicleTrack`, glass) included. Path nodes are decoded
  (`asset_world::path_nodes`) for SP AI; vehicle tracks are not consumed yet.
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
   Campaign mode: no gametype (`gametype=campaign`; no `g_gametype`,
   `onlinegame`, `sv_maxclients`), no MP rank tables or player data (the
   install's rank steps are skipped; weapon content gets one rank at 0 XP).
   Level flow (`host/mission.rs`): `missionsuccess`/`changelevel` load the
   named level (empty = menu) through `CampaignLevelCalled` → session
   `request_zone`; `missionfailed` and the player's death reload the level
   after 4 s. No death screen, retry prompt or weapon carry-over yet.
   The bootstrap kind is still `FreeForAll` internally (campaign is
   `MatchBootstrap::campaign`).
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
   `spawndrone` is a script model run through its `aitype/<type>::main`
   (team, weapon, character); campaign links an aitype for each `actor_*`
   spawner. Trainer's props, vehicles and drones animate and notetracks fire;
   scenes on actors wait on 6.
6. AI actors — in progress. Actors are script entities (`EntityKind::Actor`,
   `host/actors.rs`), not bot clients. `dospawn`/`stalingradspawn` spend spawner
   `count`, run the aitype `main` and `animscripts/init::main`; aitype `precache`
   then `spawner` run before level main. Retail animscripts drive them: the engine
   picks `stop`/`move`/`cover_*`/`combat` from goal route, claimed node and
   `enemy`, holds `animcustom`, `animscripted` (via `scripted::init`), `pain` and
   `death`, and switches with `killanimscript` + `end_script`. The motor walks the
   route at the anim tree's blended root-motion speed (`animmode` none) or applies
   root motion, turning per `orientmode`. Path nodes are script objects; goals
   route with the bots' A\* (`bots::nav::campaign`); node objects take the keys of
   the map's `node_*` entity at their origin (`script_linkto`, ...). A route crossing a
   `Begin` node (`target` = an `End` node's `targetname`) runs `animscripts/traverse/<animscript>::main`
   (`getnegotiationstartnode`/`endnode`), then the actor stands at the end node; no such script = plain walk. `usecovernode` claims a `Cover *` node (`claimedby`, lapses when the claimer's `node` moves) and goes to it; `findbestcovernode`/`findshufflecovernode` pick free covers in the goal area, passing over covers facing the enemy. `setgoalvolume` clamps the goal into an `info_volume` box; `isingoal` tests the goal area. Sight picks the nearest
   visible hostile (`enemy`) by distance minus threat bias; `createthreatbiasgroup`/`setthreatbiasgroup`/`setignoremegroup`/`setthreatbias` feed it (`ignoreme`/`ignoreall`/`favoriteenemy` too); `shoot` fires the actor's weapon through the turret
   bullet path, which raises `EV_FIRE_WEAPON` (fire sound, muzzle flash on the
   attached gun's `tag_flash`) for actors, turrets and vehicles alike. Tags of
   attached models answer on the sim side (`gettagorigin`, `muzzle`). Damage
   fills the `damage*` fields. Map `script_vehicle`s are live, spawners
   `vehicle_dospawn()`; ground vehicles roll `info_vehicle_node` paths. Level
   `weapon_*` and actor drops are weapon items, level `misc_turret`s turrets of
   their `weaponinfo`; ownerless level rockets count as the player's. Trainer and favela run with no
   script faults. Most levels reach no combat without input; roadkill's opening
   firefight does. `setviewmodel` stores the name; `getviewmodel` returns it.
   Arms bind at load: the level's hands (`setViewmodel` literal from its own
   script, else its `_loadout` branch for `level.script`) are prepared for every
   weapon (`match_walk/campaign.rs`). Runtime switches are ignored; per-hands FPV prep is the gap. `setthreatbiasagainstall`
   and `iscovervalidagainstenemy` are in. `weaponisauto`/`weaponburstcount`/
   `weaponisboltaction` read the weapon's fire type; `lastknownpos`/`lastknowntime`
   answer for the last hostile seen (others: where they are now);
   `canattackenemynode` traces muzzle → the enemy's node (else eye);
   `maymovetopoint` sweeps a body-sized box at step height. Hostile bullets that
   hit something within 64 units of an actor's eye fill `suppressionmeter`
   (drains over ~3 s); `issuppressed` reads it, `ignoresuppression` blocks it.
   Not yet: `ismovesuppressed`, `getvalidcoverpeekouts` (corner peek geometry), `setfixednodesafevolume`/
   `clearfixednodesafevolume` (no consumer in the retail dump), `nearclaimnode`/`nearclaimnodeandangle`, `startcoverarrival`
   (arrival motion). Then saves.
7. Switch: SP zones' memory budget.

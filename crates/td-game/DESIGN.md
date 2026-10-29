# td-game design

A single-player defense game over a streamed Azeroth, on `benilla-world` with no server, login or
FrameXML UI. M0–M2 are built: the engine boot, the RTS camera, creature spawning with matched gait
animation, and a data-driven level whose waves walk a fixed polyline along the Goldshire road.

This document replaces the tower-defense plan with a units-and-structures design, records what the
engine can and cannot give it, and sets the milestones from M3 on.

## 1. The game

The player defends an exit against waves that come down the roads. They do it with two things that
cost two different currencies:

- **Units** (Gold, Food): footmen, archers and the like, placed on the field with a behaviour the player
  sets. Units fight, can be knocked out, and recover.
- **Structures** (Lumber, Stone): walls, barricades and towers. They do not fight. They reshape where
  enemies can walk, and a tower lifts a garrisoned unit out of melee reach.

Enemies path over a grid flow field towards the exit. The roads are the cheapest ground, so a level
plays "road based" until the player builds; walls and barricades then push enemies onto longer
routes or through chokepoints the player's units hold. A placement that would leave no route to the
exit is refused.

### 1.1 Units

A unit is a creature body with a role, stats and a behaviour. Defined in data (`units.ron`):

| Field | Meaning |
|---|---|
| `display_id` | `CreatureDisplayInfo` id. A single-skin creature stand-in until humanoid rendering lands (§3). |
| `role` | `Soldier` or `Archer` (more later: caster, healer). |
| `health`, `damage`, `attack_interval`, `attack_range` | Combat stats. `attack_range` is ~1.5 yd for melee. |
| `move_speed` | Ground speed; the gait clip is matched to it (`anim.rs`). |
| `cost_gold` | Recruit cost. |
| `garrisonable` | Whether it may stand on a tower. |
| `recovery_secs` | How long it stays knocked out (§1.4). |

Behaviours, set per placed unit and changeable at any time:

- **Archer: hold position.** Never moves. Picks a target in range by a priority:
  - *First*: the enemy closest to the exit, by the flow field's remaining distance, not straight
    line. This is the classic tower-defense "first".
  - *Nearest*: the enemy nearest the archer.
  - *Strongest*: the enemy with the most current health.
  The target is kept until it dies or leaves range (no flicker between equal candidates).
- **Soldier: guard area.** Has a post and a guard radius (player-set, clamped to e.g. 4–15 yd).
  It engages the nearest enemy that enters the radius, chases it up to a leash (radius + 5 yd),
  and walks back to its post when it has no target. An enemy a soldier is fighting stops to fight
  back (melee blocking), which is what makes soldiers worth their gold.

Units move on the same grid as enemies, with A* to a goal cell (a post, a chase target), not on the
enemy flow field, which only leads to the exit.

### 1.2 Structures

Defined in data (`structures.ron`):

| Field | Meaning |
|---|---|
| `model` | An M2 path (static, see §5). |
| `scale` | Placement scale. |
| `footprint` | One or more rectangles in model space (length, width, offset), in yards. Authored, never derived from the hull, because some models have none (§5.3). |
| `cost_lumber`, `refund` | Build cost and the fraction returned on sell (e.g. 0.75). |
| `kind` | `Wall`, `Barricade` or `Tower { slots, range_bonus }`. |

Placement: the cursor ray hits the ground, the player rotates in 15° steps (Q/E or the wheel with
Shift), and a ghost model plus a footprint overlay (green valid, red refused) shows the result.
A placement is refused when:

- any footprint cell is statically impassable (slope, water, a building, a tree);
- it overlaps another structure or a unit;
- an enemy stands in the footprint;
- it would cut every spawn off from the exit (§4.5);
- the player cannot afford it.

Walls are cheap and long. Barricades are short and cheap, for plugging gaps. Towers are expensive,
block a square, and garrison units.

### 1.3 Garrison

A unit with `garrisonable` can be ordered onto a tower with a free slot. On the tower it:

- stands at a per-model anchor height (authored in the structure def), still animated;
- gets `range_bonus` added to its attack range;
- cannot be attacked by melee enemies; only ranged enemies can hit it;
- cannot move; its behaviour is forced to hold position (target priority still applies).

Ungarrisoning puts the unit on the nearest free passable cell beside the tower.

### 1.4 Enemies fight back

Enemies get combat stats in the wave data (`damage`, `attack_interval`, `attack_range`,
`aggro_radius`). An enemy that has a unit inside its aggro radius, and can reach it (not a
garrisoned unit for a melee enemy), leaves the flow field to attack it, then returns to the flow
field when the unit is down or out of reach.

A unit at 0 health is **knocked out**, not killed: it plays Death, lies still, cannot act or be
targeted, and after `recovery_secs` stands up (Stand from the Death pose's end) at its post with
half health. Losing units is a tempo loss, not a gold loss, which keeps the economy legible.

Enemies never attack structures in the first version; a route to the exit always exists, so they
have no reason to. Sappers that do can be a later enemy type.

### 1.5 Economy

Two pairs of currencies, kept apart so units and structures do not compete for one pool: units
cost Gold and Food, structures Lumber and Stone.

| | Gold, Food (units) | Lumber, Stone (structures) |
|---|---|---|
| Start | Per level | Per level |
| Income | Gold: a bounty per kill, by enemy type. Food: a grant per wave cleared | A grant per wave cleared, plus a Lumber trickle per second during waves |
| Spent on | Recruiting (Gold), each unit's upkeep slot (Food), later upgrades | Building: wooden structures cost mostly Lumber, stone ones mostly Stone |
| Refund | None (units are not sold; they can be dismissed) | `refund` × cost on sell |

All numbers live in the level and def files so balancing never needs a rebuild. Calling the next
wave early pays a Gold bonus scaled by the time saved.

### 1.6 Level data

`levels/*.ron` grows from M2's file:

- `bounds`: the grid rectangle in WoW coords (§4.1).
- `spawns` and `exit`: cells, or small rectangles, in WoW coords. Several spawns are allowed.
- `roads`: polylines with a width. They give road cells a lower walk cost (§4.4). The M2 path
  becomes the first road.
- `start_gold`, `start_lumber`, `lumber_per_wave`, `lumber_per_sec`.
- `waves`: as now, plus the enemy combat stats and `bounty`.
- Optional `forbidden`: rectangles the player may not build on (the inn door, the spawn).

## 2. What exists and what is reused

| Need | Source |
|---|---|
| Boot, map, camera | `td-game` M0 (`boot.rs`, `camera.rs`) |
| Creature spawn by display id | `td-game` M1/M2 (`creature.rs`); generic over non-humanoid displays |
| Gait, matched clip rate | `anim.rs`; extend with attack, death and combat-ready states |
| Terrain height | `benilla_world::world_point::TerrainHeights::under` |
| Liquid | `benilla_world::world_point::WorldPoint::liquid_at` |
| Ray/shape casts against world geometry | `benilla_world::collision::WorldCollision` (`ray_body`, `cast_body`) |
| Static M2 forms and materials | `ModelForms::require_static`, `M2BatchMaterials::steady`/`entity_variants` |
| Character tables, geosets, compositor | `benilla_formats::characters` (`CharacterGeosets`, `CharSections::composite_body`), `benilla_formats::load_item_display_catalog` |
| Screenshots | `shot.rs` (`WOW_WORLDVIEW_SHOT`, `TD_SHOT_MOMENT`, `TD_CAM`) |

No engine changes are needed for anything in this plan. One new direct dependency is:
`avian3d = "0.6"`, already in the graph at that version through `benilla-world`, for structure
colliders (§5.3).

## 3. Humanoid rendering

### 3.1 How `benilla-app` dresses a character body

A humanoid NPC's `CreatureDisplayInfo` row has an `ExtendedDisplayInfoID` into
`CreatureDisplayInfoExtra` (race, sex, skin, face, hair style/colour, facial hair, ten worn
`ItemDisplayInfo` ids, and a bake name). `benilla_formats::CreatureCatalog::model` resolves it into
`CreatureModel::npc_appearance` (`benilla-formats/src/creatures.rs`). The model itself is the
shared race/sex character M2 (`Character\Human\Male\HumanMale.m2`), so a body is dressed from five
inputs:

1. **Body skin atlas** (`CharSkinSlot::Body`). `benilla-app/src/entities/attach/char_skin.rs`
   `body_atlas`: a baked NPC loads `Textures\BakedNpcTextures\<bake>.blp` as is
   (`BodySkin::Baked`); anything else is composited live from `CharSections` skin, face and
   underwear sections plus each worn item's region textures (`skin_composite.rs`, over
   `benilla_formats::characters::sections::composite_body`/`composite_plan`).
2. **Hair and extra skin** (`CharSkinSlot::Hair`, `SkinExtra`): textures from `CharSections` by hair
   style and colour; `SkinExtra` is the tauren fur only.
3. **Geosets**: a character M2 carries every hair, beard, glove, boot, sleeve and robe variant;
   `CharacterGeosets::visible_geosets(race, sex, hair, facial, &EquipGeosets)`
   (`benilla-formats/src/characters/geosets.rs`) picks the ones to draw from the customization and
   each worn item's `geoset_groups` and helm hide rows. `attach/mod.rs` filters the parts by it.
4. **Attached item models**: helm, shoulders, weapons and shield are separate M2s
   (`Item\ObjectComponents\...`) hung from the body's attachment points
   (`benilla-app/src/entities/equipment/{resolve,spawn}.rs`, ~3,000 lines, much of it for the
   player's inventory, sheathing and the wire). An NPC's weapons are not in
   `CreatureDisplayInfoExtra` at all; the server sends them as virtual items.
5. **Merge and fades** (`attach/merge.rs`, `dress.rs`): a performance and appear-fade layer td-game
   can skip.

### 3.2 Baked NPC textures in the 1.12 data

Checked against this install:

- `CreatureDisplayInfoExtra.dbc` has 6,939 rows; **6,931 name a bake**.
- The MPQ chain holds **7,538** files under `Textures\BakedNpcTextures\`; **all 6,836 distinct
  bake names resolve** to a shipped file.
- By race: human 1,880 rows, orc 1,068, dwarf 964, night elf 679, undead 569, tauren 530,
  gnome 313, troll 627, goblin 309.

So a guard or a grunt display already has its whole body atlas, armour painted in, on disk. What
the bake does not cover: hair texture, geosets, and the attached models (helm, shoulders, weapons).

### 3.3 Routes and effort

| Route | Work | Estimate |
|---|---|---|
| **A. Baked NPC displays** | Character M2 per race/sex; Body slot = baked atlas; Hair slot from `CharSections`; geosets from `visible_geosets` with `EquipGeosets` built from the Extra row's items; helm and shoulder M2s on their attachment anchors (`RigPose::anchor_for`, attachment ids from `M2Model::attachments`), helm/shoulder model and texture names from `ItemDisplayInfo` with the race/sex suffix rule; weapons from `ItemDisplayInfo` ids the unit def names (a footman's sword and shield, an archer's bow). | **3–4 days** to a recognisable Stormwind guard or Orgrimmar grunt with weapons; **+1–2 days** for the edge cases (helm hair hiding, mirrored shoulders, two-handers, bows held in the left hand). |
| **B. Live composite** | Route A plus the `CharSections` compositor for bodies without a bake or with gear the player changes: port `skin_composite.rs`'s caching around `composite_body`, and region textures from `ItemDisplayInfo`. | **+2–3 days** on top of A. Only worth it if unit upgrades should visibly change armour per unit. |
| **C. Stand-ins** | Single-skin creatures (as M2). | Done. |

Recommendation: build every mechanic on stand-ins (C), then do A as its own milestone, behind a
`display_id` swap in `units.ron`. B only if armour upgrades become a feature.

Picking displays: `TD_FIND_DISPLAY` already lists displays by model path with a humanoid flag; it
should also print race, sex and the Extra row's items so a guard or grunt is picked by eye.
Animation ids beyond Stand 0, Death 1, Walk 4 and Run 5 (attacks, combat-ready, bow) are taken from
`AnimationData.dbc` (`benilla-extract dbc`) when M5 needs them.

## 4. Grid and flow field

### 4.1 The grid

- **Cell size: 1 yard.** Elwynn's fences are 0.3–0.5 yd thick and its roads 6–10 yd wide; 1 yd
  cells give a road 6+ cells across (room for walls to narrow it in steps) and a fence a one-cell
  line. Enemies are 0.5–1.5 yd across.
- **Bounds: authored per level**, a rectangle around the playable area, not the 200 yd camera
  radius. Goldshire at ~250 × 200 yd is 50,000 cells: one `u8` passability layer, one `u8` cost
  layer, one `u16` integration layer and one `i8×2` (or 8-way `u8`) direction layer, ~250 KB.
- Cells are indexed in WoW x/y, the same frame the level file uses; a cell's centre converts with
  `wow_to_bevy` for drawing and steering.

### 4.2 The static layer: baked once, after the world loads

The bake waits for `WorldLoadProgress::presentable()` with `colliders_pending == 0`, so every
building and doodad hull is in the spatial tree. For each cell:

1. **Terrain and holes.** `TerrainHeights::under` at the four corners. Any `None` (an MCNK hole or
   a tile not resident) blocks the cell.
2. **Slope.** The steepest corner-to-corner rise over run. A cell is blocked above a threshold
   (start at 45°, i.e. rise/run > 1.0, tuned in the level file). A moderate slope can instead add
   cost, so enemies prefer flat ground.
3. **Liquid.** `WorldPoint::liquid_at(Subject::Eye, wow)` at the cell centre, outdoors. A cell is
   blocked when the surface is more than ~1 yd above the ground (deep water, any magma or slime), so
   shallow fords stay walkable. `Subject::Eye` reads the open-world MCLQ while the camera is
   outdoors, which in an RTS view it always is.
4. **World geometry.** `WorldCollision::ray_body` straight down from ground + 15 yd at the centre
   and four inset points (±0.35 yd). A hit more than 0.75 yd above the terrain is a building, a
   tree trunk, a fence or a rock: blocked. This uses the body filter, so it sees terrain, doodad
   hulls and WMO walking colliders, the same geometry the client's player collides with. A hit
   that is not the terrain but within 0.75 yd (a bridge deck, a low step) is walkable at that
   height.

The static layer is baked in a few frames (50,000 cells × 5 rays is cheap, but it can be split
across frames behind the level's loading cover) and kept for the level's lifetime.

### 4.3 Structures in the grid

A structure's footprint rectangles are transformed by its placement (position, yaw, scale) and
rasterized: every cell whose centre falls inside a rectangle inflated by 0.5 yd (an enemy's
half-width) is marked blocked in a **structure layer** kept apart from the static one, so selling a
structure clears exactly its cells. Towers mark a square; walls a long thin rectangle; a multi-part
barricade several.

Units do not block enemy pathing; enemies stop for them through combat (§1.4).

### 4.4 Costs and the road

Each passable cell has a step cost: 1.0 on a road cell, 2.0 off road, plus a slope term. Road cells
come from the level's `roads` polylines and widths, rasterized like footprints. That is what keeps
the game road based: with no structures, the cheapest route is the road, and enemies leave it only
when the player makes the road more expensive than the detour.

(The ADT's texture layers could mark roads automatically. The authored corridors are simpler,
exact, and let a level designer say which road matters.)

### 4.5 The flow field

- **Integration field**: Dijkstra from all exit cells over the passable cells, 8-connected (a
  diagonal step only when both orthogonal neighbours are passable, so nothing cuts a wall corner),
  step cost × 1 or × √2. 50,000 cells with a binary heap is a few milliseconds; it runs only when
  the structure layer changes.
- **Direction field**: each cell points to its lowest-integration neighbour. An enemy steers
  towards the bilinear blend of the four nearest cells' directions, so it does not zig-zag on cell
  edges, and keeps a small separation from its neighbours.
- **Distance to exit** for an enemy is its cell's integration value, which also drives the
  archers' *First* priority.
- **Placement check**: before committing a placement, run a breadth-first reachability pass over
  `static ∧ ¬structures ∧ ¬candidate` from the exit; refuse if any spawn cell is unreached. Enemies
  already on the field need no check of their own: they are in passable cells (a placement on an
  enemy is refused), and every passable cell connected to a spawn is connected to the exit.
- An enemy whose cell becomes unreachable through some edge case (none expected) walks straight to
  the nearest reachable cell, a safety net that logs a warning.

### 4.6 Debug views

F1 stays the path overlay; it becomes the integration field's main route from each spawn. F2 draws
the grid: blocked cells red, road cells tan, structure cells blue, and direction arrows on a
thinned subset. Both are needed for the screenshot checks.

## 5. Placing static M2 structures

### 5.1 Drawing

A structure is the creature recipe without a rig: load the `M2Model` (`m2_url`), wait for
`ModelForms::require_static`, and spawn one child per batch with `Mesh3d` (the static form),
`MeshMaterial3d` (`M2BatchMaterials::steady`, the batch's own texture), its build-time `Aabb`, and
`ModelPart { kind: ModelKind::GameObject, blend }` so the engine's visibility authority (far clip,
distance fade) manages it. A model that animates (a banner, a torch) gets
`benilla_world::doodad_anim::spawn_anim_host`, the engine's own placed-doodad host.

The placement ghost uses `M2BatchMaterials::entity_variants`'s `fade_blend` twin with a
`MeshTag` alpha of ~0.5.

WMOs (`WmoModel`) can be drawn the same way (`require_static`, `steady` per batch), as the app does
for WMO GameObjects, but a runtime-placed WMO gets none of the engine's portal culling or interior
lighting. The structures this design needs exist as M2s, so WMOs are avoided.

### 5.2 Candidate models (all in the 1.12 data)

| Use | Model | Hull |
|---|---|---|
| Wall | `World\Azeroth\Elwynn\PassiveDoodads\ElwynnFences\ElwynnStoneFence.m2` (5.8 × 0.5 × 1.3 yd) | yes |
| Wall | `World\Azeroth\Elwynn\PassiveDoodads\ElwynnFences\ElwynnWoodFence01.m2` (4.3 × 0.3 × 2.4 yd) | yes |
| Barricade | `World\Azeroth\RedRidge\PassiveDoodads\spikewall\BattleGladeWoodSpikesShortRow.m2` (11 × 2.7 yd) | yes |
| Barricade | `World\Azeroth\Elwynn\PassiveDoodads\BattleGladeWoodSpikes\BattleGladeWoodSpikes.m2` (1.3 × 1.5 yd) | yes |
| Tower | `World\Azeroth\Elwynn\Buildings\HumanGuardTower\HumanGuardTower.m2` | **none** |
| Tower | `World\Azeroth\Elwynn\Buildings\HumanWatchTower\HumanWatchTower.m2` | **none** |

Sizes are `benilla-extract m2coll` model-space boxes, unscaled.

### 5.3 Collision

The grid, not physics, is what blocks enemies, so a structure's collider serves the world queries
only: the cursor ray stops on a wall instead of the ground behind it, and the grid bake (§4.2) can
be re-run over a structure. Two cases:

- **A model with a hull**: `Collider::trimesh` from `M2Model::collision` through `wow_to_bevy`, a
  port of `model_local_collider` in `benilla-app/src/entities/display.rs`, with
  `RigidBody::Static`.
- **A model without one** (both towers): `Collider::cuboid` sized from the authored footprint and
  the model's height.

Either way, inserting a collider outside the terrain streamer must bump
`benilla_world::collision::ColliderEpoch` (`.bump()`), as the app's GameObject lane does, so cached
collision answers are re-dated. Colliders go on avian's default layer, the one the body filter
(`WorldCollision::body_filter`) and so `ray_body` see.

This needs `avian3d = "0.6"` as a direct dependency of `td-game`; it is already in the graph at
that version through `benilla-world`.

## 6. Milestones

Every milestone ends with `cargo build`, a run, and screenshots checked by eye, as M0–M2 did.
Units use single-skin creature stand-ins until M9.

| # | Milestone | Done when |
|---|---|---|
| **M3** | **Grid and flow field.** Grid bounds, spawns, exit and roads in the level file; static bake (holes, slope, liquid, world geometry); road costs; Dijkstra integration and direction fields; enemies steer by the field instead of the polyline; F1/F2 overlays. | Top-down shot of the grid overlay over Goldshire with buildings and trees blocked and the road cheapest; mid-wave shot of enemies on the road following the field. |
| **M4** | **Structures.** `structures.ron`; static M2 spawn with collider; cursor placement with ghost, rotation and validity overlay; rasterize into the structure layer; recompute; refuse a blocking placement; sell with refund; Lumber and Stone with start amounts, wave grants and a Lumber trickle. | Shot of a wall forcing enemies off the road and back on; a refused placement logged with its reason; the grid overlay showing the structure cells. |
| **M5** | **Units and combat.** `units.ron` with stand-ins (picked with `TD_FIND_DISPLAY`); recruit and place; health and damage on both sides; enemy death (Death clip, then despawn); archers with hold position and First/Nearest/Strongest; soldiers with guard, engage, leash and return; A* for unit moves; Gold with bounties; projectiles as gizmo lines, then an arrow M2. Attack and combat-ready clips added to `anim.rs`. | Mid-wave shot of archers firing and a soldier engaging inside its radius; log lines for each priority choosing different targets in one scripted test. |
| **M6** | **Enemies fight back.** Enemy combat stats and aggro; melee blocking; units knocked out and recovering at their post; garrison rules prepared (melee cannot target a flagged unit). | Shot of a knocked-out unit lying down while enemies pass; log of its recovery. |
| **M6.5** | **Commander mode.** One commander per battle (stand-in creature; stats in `units.ron`), placed by the exit. V toggles RTS and commander mode with a 0.7 s eased camera blend (position, rotation, FOV; input locked). WoW-style controls and a follow camera that rides the terrain and pulls in on world collision; Tab targeting and auto-attack; building and recruiting off in commander mode; death forces RTS and respawns at the exit after a timer; the HUD shows the current mode's controls. | Shots of the RTS view, mid-transition, and commander mode fighting a kobold. |
| **M7** | **Towers and garrison.** Tower slots and anchor heights; order a unit up and down; range bonus; melee immunity. | Shot of an archer on a guard tower outranging ground archers. |
| **M8** | **Game loop and HUD.** Bevy UI (not FrameXML): Gold, Lumber, lives, wave, build bar, unit panel with behaviour controls, call-wave-early, victory and defeat; a second wave set tuned for the new mechanics. | Shots of the HUD mid-wave and of the end screens. |
| **M9** | **Humanoid units, route A.** Baked NPC bodies with hair, geosets, helm and shoulders, and def-chosen weapons; footman and archer swapped from stand-ins by `display_id` alone. | Close shot of a guard and a grunt beside their stand-ins. |
| M10 | *Optional:* route B composites for visible armour upgrades; polish (arrow and spell M2s, ground decals for unit radii). | — |

M3 and M4 are ordered first because units, garrisons and the economy all sit on the grid, and the
grid has the most unknowns (bake cost, ray behaviour against Goldshire's buildings). M5–M7 are pure
game code over the M1 creature recipe. M9 is the only milestone that ports from `benilla-app`, and
it is isolated behind the `display_id` swap.

The campaign layer (§8) comes after this list; nothing in M4–M10 waits on it. What M4–M8 do owe
it is the battle interface (§7): new state should arrive through, and leave by, the shapes that
section defines, so the later refactor is a change of where the data comes from, not of how the
battle uses it.

## 7. Battle interface

A battle is one defense of one location. Today the binary is the battle and `goldshire.ron` is all
of its input. With a campaign above it, the campaign builds the input, runs the battle (or
auto-resolves it, §8.4), and applies the result. This section fixes that boundary.

### 7.1 What a battle takes in

```text
BattleSetup {
    location: Location,          // where: static, authored once per settlement
    attackers: AttackerForce,    // who comes, how and when
    defenders: DefenderForce,    // what the player brings
    structures: Vec<PlacedStructure>,  // what already stands there
    rules: BattleRules,
}

Location {                       // today's level file, minus the waves and lives
    name, map_id, arena_center,
    bounds, slope_limit,
    spawns: Vec<Spawn>,          // each named by approach ("north road"), so a raid can pick one
    exit,                        // the settlement core the attackers try to reach
    roads,
    forbidden: Vec<Rect>,        // no-build areas
}

AttackerForce {
    waves: Vec<WaveDef>,         // today's waves, generated from the army's composition
    spawn_for_wave: Vec<SpawnId>,
}

DefenderForce {
    units: Vec<RosterUnit>,      // unit def, health, knocked-out state, veterancy later
    resources: Resources,        // gold, food, lumber, stone available to this battle
    income: BattleIncome,        // per-wave grants and trickles, if the campaign allows any
}

PlacedStructure {                // also the shape the battle returns (§7.2)
    def: StructureId, position: (x, y), yaw, health,
}

BattleRules {
    integrity: u32,              // today's lives: leaks before the settlement falls
    time_limit: Option<f32>,
    allow_building: bool,        // a surprise raid may forbid it
}
```

`Location` is static and reusable: the grid bake (§4.2) depends only on it, so its static layer
can be cached per location. Everything else changes per battle.

### 7.2 What a battle returns

```text
BattleResult {
    outcome: Held { integrity_left } | Fell { leaks },
    defenders: Vec<RosterUnit>,      // survivors' health; knocked-out units count as alive
    attackers_left: Vec<(EnemyId, u32)>,  // for a raid that retreats to try again
    structures: Vec<PlacedStructure>,     // what still stands, with its damage
    resources: Resources,            // what is left of the battle's budget, refunds included
    stats: { kills, leaks, duration, resources_spent },
}
```

Structures persist between battles at the same location: a settlement that was walled stays
walled, at whatever health the last battle left it.

### 7.3 What would change in the current code

Nothing here is refactored yet. When the campaign starts, these are the seams:

| Code | Reads today | Would read |
|---|---|---|
| `boot.rs` `run` | `level::load()` once, before the app builds; map and focus fixed for the process | A `BattleSetup` resource, possibly at runtime: the battle becomes a Bevy `State`, and entering it sets `ViewFocus::entry` and `CurrentMap` (the engine already handles a map change through `MapChange`) |
| `level.rs` | `LevelFile` with everything | Splits into `Location` (bounds, spawns, exit, roads) and the per-battle parts; `waves` and `lives` leave it |
| `waves.rs` | `level.waves`, `level.lives`; logs leaks and defeat | `AttackerForce` and `BattleRules`; writes a `BattleResult` and leaves the state instead of logging |
| `creature.rs` `load_library` | Displays named by `level.waves` | Displays named by attackers and defenders alike |
| `grid.rs` | Bakes from `Level` every run | Bakes per `Location`, cached; the structure layer seeded from `BattleSetup::structures` |
| `camera.rs` | `level.center_bevy()` | `Location::arena_center` |
| Structures (M4) | `structures.ron` defs; placements made in the battle | The same defs; placements seeded from the setup and returned in the result |
| Economy (M4/M5) | Start amounts from the level file | `DefenderForce::resources`; leftover returned |
| Units (M5) | Recruited in the battle | Also brought in from `DefenderForce::units`; survivors returned |

Two rules for M4–M8 keep this cheap: battle state lives in resources and components that a setup
could fill (never in `const`s or the level loader), and anything the result would report
(placements, spend, survivors) is kept as it happens, not reconstructed at the end.

## 8. Campaign layer (future)

A grand-strategy layer over both continents, in the spirit of Crusader Kings III and Victoria 3 but
deliberately small at first: provinces, income, recruitment and raids. Every attack on a
settlement the player owns can become a battle on that place's real terrain, which is the point:
the strategic map and the battlefield are the same world.

### 8.1 The map

- **Both continents**: Eastern Kingdoms (map 0) and Kalimdor (map 1). A battle already streams
  whichever map its location names.
- **Provinces** are zones and their major subzones, from `AreaTable.dbc` (zone and parent area),
  with outlines and placement from `WorldMapArea.dbc` and `WorldMapOverlay.dbc`. All three ship in
  the 1.12 data, as does the world-map art (`Interface\WorldMap\`, 1,540 files: continent and
  zone tiles), so the strategic map can be drawn from the game's own maps in Bevy UI.
- **Adjacency** (which provinces border) is authored, since the client data has no graph; zone
  boundaries in `WorldMapArea` help draft it.
- **Settlements**: each province has zero or more, each with a battle `Location` (§7.1). Only
  settlements with an authored location can host a battle; the rest always auto-resolve.

### 8.2 Factions

Playable: human, dwarf, gnome, night elf, orc, troll, tauren and undead, each starting at its
capital:

| Faction | Capital (start province) |
|---|---|
| Human | Stormwind City (Elwynn Forest) |
| Dwarf | Ironforge (Dun Morogh) |
| Gnome | Gnomeregan's exile, at Ironforge's Tinker Town (Dun Morogh) |
| Night elf | Darnassus (Teldrassil) |
| Orc | Orgrimmar (Durotar) |
| Troll | Sen'jin Village (Durotar) |
| Tauren | Thunder Bluff (Mulgore) |
| Undead | Undercity (Tirisfal Glades) |

Gnomes and dwarves share a capital province and trolls and orcs share Durotar, as in 1.12; the
campaign can either give each a distinct start or make them a two-faction start. The rest of the
map starts neutral or held by non-playable powers (Defias, Scourge, Burning Legion remnants), which
supply the raids.

### 8.3 The minimal loop

A turn (a month, say) does four things:

1. **Income**: each owned province yields Gold, Food, Lumber and Stone by its type (farmland,
   forest, mountain, town).
2. **Recruitment**: the player spends Gold and Food to raise units into a province's roster, the
   pool a battle's `DefenderForce` draws from.
3. **Building**: Lumber and Stone fortify a settlement between battles, placing structures into its
   persistent `PlacedStructure` list (or the player builds during the battle itself).
4. **Raids**: hostile powers send forces at adjacent owned provinces; the player may send their own
   to take or raid neutral and enemy provinces.

Deeper systems (characters and dynasties, diplomacy, pops and markets) are out of scope until the
loop is fun.

### 8.4 Attacks and battles

- An attack on an owned settlement produces a `BattleSetup`: its location, the raiding force as
  waves, the province's roster and stores, and its standing structures.
- **Minor attacks auto-resolve**: when the defenders' strength exceeds the attackers' by a margin
  (or the player chooses), a formula over unit and structure strength decides the result, with
  losses, and returns the same `BattleResult` a played battle would.
- **Major attacks are played** as tower-defense battles on the location's real terrain.
- The result feeds back: survivors return to the roster, structures keep their damage, a fallen
  settlement changes hands (or is sacked), and resources left are returned to the province.

### 8.5 Prerequisites

The campaign needs, beyond M4–M10: the battle interface refactor (§7.3); battle locations
authored per settlement (a level file each, so road tracing must get cheaper, e.g. derived from the
ADT's road texture layers); humanoid units for every playable race (M9, route A, which covers all
eight races' baked NPCs); Kalimdor locations verified to stream and bake like Goldshire; and a
strategic-map UI. None of it has a milestone number yet.

## 9. Open questions

- **Starting balances and Food's role.** Whether Food is a per-unit cost or an upkeep cap.
- **Moving placed units.** Free, with a cooldown, or a Gold cost?
- **Unit cap.** A hard cap, or Gold alone as the limit?
- **Enemies and structures.** Never attack them (this design), or a sapper type later?
- **Map scope.** One arena per level around a crossroads, or larger maps with several roads and
  exits?

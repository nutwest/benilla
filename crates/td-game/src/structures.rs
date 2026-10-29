//! Structures: static M2s the player builds. Each has an authored footprint that is rasterized
//! into the grid's structure layer (the flow field reroutes around it) and a collider for the
//! world's queries (the cursor ray stops on a wall). Defs come from `data/structures.ron`.
//!
//! The draw is the creature recipe without a rig: the static mesh forms and the batch's steady
//! material, with [`ModelPart`] so the engine's visibility authority (far clip, distance fade)
//! manages it. The collider is the model's hull as a trimesh, or for a hull-less model (both
//! towers) a box per footprint rectangle; either way the engine's [`ColliderEpoch`] is bumped,
//! as a collider added outside the terrain streamer must.

use std::collections::HashSet;

use avian3d::prelude::{Collider, RigidBody};
use bevy::mesh::MeshTag;
use bevy::prelude::*;
use serde::Deserialize;

use benilla_assets::coords::wow_to_bevy;
use benilla_assets::{AssetSet, M2Model};
use benilla_world::collision::ColliderEpoch;
use benilla_world::model_forms::ModelForms;
use benilla_world::model_render::{BatchVariants, M2BatchMaterials, ModelKind, ModelPart};
use benilla_world::world_point::TerrainHeights;

use crate::economy::{Amount, Stores};
use crate::enemy::Enemy;
use crate::grid::{Grid, GridChanged};
use crate::level::Level;

/// Footprints grow by an enemy's half-width, so a body does not clip a wall's face.
pub const INFLATE: f32 = 0.5;

#[derive(Deserialize)]
struct DefsFile {
    structures: Vec<StructureDef>,
}

#[derive(Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
pub enum StructureKind {
    Wall,
    Barricade,
    Tower,
}

/// A footprint rectangle in model space (WoW axes: x forward, y left), yards before scale.
#[derive(Deserialize, Clone, Copy, Debug)]
pub struct Rect {
    pub center: (f32, f32),
    pub size: (f32, f32),
}

#[derive(Deserialize, Clone, Debug)]
pub struct StructureDef {
    pub id: String,
    pub name: String,
    pub model: String,
    pub scale: f32,
    pub footprint: Vec<Rect>,
    pub cost: Amount,
    pub refund: f32,
    pub kind: StructureKind,
}

/// Every def, with its model loading.
#[derive(Resource)]
pub struct StructureDefs {
    pub defs: Vec<StructureDef>,
    handles: Vec<Handle<M2Model>>,
}

impl StructureDefs {
    pub fn find(&self, id: &str) -> Option<usize> {
        self.defs.iter().position(|d| d.id == id)
    }
}

/// A placement: which def, where (WoW `(x, y)`) and its yaw in radians about WoW `z`.
#[derive(Clone, Copy, Debug)]
pub struct Placement {
    pub def: usize,
    pub at: Vec2,
    pub yaw: f32,
}

/// Marks a placed structure's root entity; its placement and footprint are in [`Placed`].
#[derive(Component)]
pub struct Structure;

/// Every placed structure's footprint, updated the moment it is built or sold rather than when
/// commands apply, so several placements in one frame (a script, a fast click) see each other.
#[derive(Resource, Default)]
pub struct Placed(Vec<PlacedEntry>);

struct PlacedEntry {
    entity: Entity,
    /// Kept as built, for the battle result (DESIGN.md §7.2).
    placement: Placement,
    /// The footprint itself, which no other structure's may share: walls chain end to end.
    core: Vec<usize>,
    /// The footprint grown by [`INFLATE`]: what the grid's structure layer marks.
    cells: Vec<usize>,
}

/// The two rasterizations of a valid placement.
pub struct Footprint {
    pub core: Vec<usize>,
    pub cells: Vec<usize>,
}

/// Why a placement was refused.
#[derive(Debug)]
pub enum Refusal {
    NotReady,
    OffGrid,
    Blocked { at: Vec2, what: &'static str },
    Overlaps,
    EnemyInside,
    CutsOffExit,
    CannotAfford { need: Amount },
}

impl std::fmt::Display for Refusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Refusal::NotReady => write!(f, "the grid or the model is not ready"),
            Refusal::OffGrid => write!(f, "part of it is outside the arena"),
            Refusal::Blocked { at, what } => {
                write!(f, "part of it is on {what} at ({:.1}, {:.1})", at.x, at.y)
            }
            Refusal::Overlaps => write!(f, "it overlaps another structure"),
            Refusal::EnemyInside => write!(f, "an enemy stands in its footprint"),
            Refusal::CutsOffExit => write!(f, "it would cut a spawn off from the exit"),
            Refusal::CannotAfford { need } => write!(f, "it costs {need}"),
        }
    }
}

pub fn plugin(app: &mut App) {
    app.init_resource::<Placed>()
        .add_systems(Startup, load_defs.after(AssetSet::Open))
        .add_systems(Update, log_bounds);
}

pub(crate) fn load_defs(mut commands: Commands, asset_server: Res<AssetServer>) {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/data/structures.ron");
    let file: DefsFile = match std::fs::read_to_string(path)
        .map_err(|e| e.to_string())
        .and_then(|t| ron::from_str(&t).map_err(|e| e.to_string()))
    {
        Ok(f) => f,
        Err(e) => {
            error!("structures: {path}: {e}");
            DefsFile {
                structures: Vec::new(),
            }
        }
    };
    let handles = file
        .structures
        .iter()
        .map(|d| asset_server.load(benilla_assets::m2_url(&d.model)))
        .collect();
    info!("structures: {} defs from {path}", file.structures.len());
    commands.insert_resource(StructureDefs {
        defs: file.structures,
        handles,
    });
}

/// The cells a placement covers: each footprint rectangle scaled, turned and moved into place,
/// grown by `inflate`, and every cell whose centre falls inside. `None` if any of it is off the
/// grid.
pub fn footprint_cells(
    grid: &Grid,
    def: &StructureDef,
    p: &Placement,
    inflate: f32,
) -> Option<Vec<usize>> {
    let (sin, cos) = p.yaw.sin_cos();
    let turn = |v: Vec2| Vec2::new(v.x * cos - v.y * sin, v.x * sin + v.y * cos);
    let mut cells = Vec::new();
    for rect in &def.footprint {
        let center = p.at + turn(Vec2::new(rect.center.0, rect.center.1) * def.scale);
        let half = Vec2::new(rect.size.0, rect.size.1) * def.scale * 0.5 + inflate;
        let (ax, ay) = (turn(Vec2::X), turn(Vec2::Y));
        // The rectangle's world bounding box, then the exact test per cell centre.
        let reach = ax.abs() * half.x + ay.abs() * half.y;
        let lo = grid.cell_at(center - reach)?;
        let hi = grid.cell_at(center + reach)?;
        let ((x0, y0), (x1, y1)) = (grid.coords(lo), grid.coords(hi));
        for iy in y0..=y1 {
            for ix in x0..=x1 {
                let i = grid.index(ix, iy);
                let d = grid.center(i) - center;
                if d.dot(ax).abs() <= half.x && d.dot(ay).abs() <= half.y {
                    cells.push(i);
                }
            }
        }
    }
    cells.sort_unstable();
    cells.dedup();
    Some(cells)
}

/// Whether `p` may be built, and its footprint. Checks in order of cheapness; the reachability
/// pass last. `taken` is every placed structure's core cells.
pub fn validate(
    grid: &Grid,
    level: &Level,
    defs: &StructureDefs,
    stores: &Stores,
    enemies: &[Vec2],
    taken: &HashSet<usize>,
    p: &Placement,
) -> Result<Footprint, Refusal> {
    if !grid.ready {
        return Err(Refusal::NotReady);
    }
    let def = &defs.defs[p.def];
    let cells = footprint_cells(grid, def, p, INFLATE).ok_or(Refusal::OffGrid)?;
    let core = footprint_cells(grid, def, p, 0.0).ok_or(Refusal::OffGrid)?;
    // The core only: a wall may stand against a building, its margin over the building's cells.
    if let Some(&i) = core.iter().find(|&&i| grid.static_blocked(i)) {
        return Err(Refusal::Blocked {
            at: grid.center(i),
            what: grid.blocked_reason(i),
        });
    }
    if core.iter().any(|i| taken.contains(i)) {
        return Err(Refusal::Overlaps);
    }
    if enemies
        .iter()
        .any(|&e| grid.cell_at(e).is_some_and(|c| cells.contains(&c)))
    {
        return Err(Refusal::EnemyInside);
    }
    if !stores.can_afford(&def.cost) {
        return Err(Refusal::CannotAfford { need: def.cost });
    }
    if !grid.spawns_reach_exit(level, &cells) {
        return Err(Refusal::CutsOffExit);
    }
    Ok(Footprint { core, cells })
}

/// The Bevy transform of a placement standing on the ground.
pub fn placement_transform(def: &StructureDef, p: &Placement, ground: f32) -> Transform {
    Transform::from_translation(wow_to_bevy([p.at.x, p.at.y, ground]))
        .with_rotation(Quat::from_rotation_y(p.yaw))
        .with_scale(Vec3::splat(def.scale))
}

/// Everything a structure spawn reads and writes, as one system parameter.
#[derive(bevy::ecs::system::SystemParam)]
pub struct StructureBuilder<'w, 's> {
    pub commands: Commands<'w, 's>,
    pub defs: Option<Res<'w, StructureDefs>>,
    pub grid: ResMut<'w, Grid>,
    pub level: Res<'w, Level>,
    pub stores: ResMut<'w, Stores>,
    pub heights: TerrainHeights<'w>,
    m2s: Res<'w, Assets<M2Model>>,
    forms: ResMut<'w, ModelForms>,
    mats: M2BatchMaterials<'w>,
    uv_reg: ResMut<'w, benilla_world::doodad_anim::UvAnimMaterials>,
    uv_table: ResMut<'w, benilla_world::mat_anim_table::MatAnimTable>,
    epoch: ResMut<'w, ColliderEpoch>,
    changed: MessageWriter<'w, GridChanged>,
    enemies: Query<'w, 's, &'static Transform, With<Enemy>>,
    placed: ResMut<'w, Placed>,
}

impl StructureBuilder<'_, '_> {
    /// Whether every def's model is loaded and its static forms built.
    pub fn ready(&mut self) -> bool {
        let Some(defs) = self.defs.as_ref() else {
            return false;
        };
        if !self.mats.ready() {
            return false;
        }
        let mut all = true;
        for h in &defs.handles {
            all &= self.m2s.contains(h) && self.forms.require_static(h, 0);
        }
        all
    }

    /// Enemy positions, WoW `(x, y)`.
    pub fn enemy_positions(&self) -> Vec<Vec2> {
        self.enemies
            .iter()
            .map(|xf| {
                let w = benilla_assets::coords::bevy_to_wow(xf.translation);
                Vec2::new(w[0], w[1])
            })
            .collect()
    }

    /// Every placed structure's core cells.
    fn taken(&self) -> HashSet<usize> {
        self.placed
            .0
            .iter()
            .flat_map(|s| s.core.iter().copied())
            .collect()
    }

    pub fn validate(&self, p: &Placement) -> Result<Footprint, Refusal> {
        let defs = self.defs.as_ref().ok_or(Refusal::NotReady)?;
        validate(
            &self.grid,
            &self.level,
            defs,
            &self.stores,
            &self.enemy_positions(),
            &self.taken(),
            p,
        )
    }

    /// Validates, pays, marks the grid and spawns. The refusal names its reason.
    pub fn place(&mut self, p: Placement) -> Result<Entity, Refusal> {
        if !self.ready() {
            return Err(Refusal::NotReady);
        }
        let footprint = self.validate(&p)?;
        let defs = self.defs.as_ref().ok_or(Refusal::NotReady)?;
        let def = defs.defs[p.def].clone();
        if !self.stores.spend(def.cost, &def.name) {
            return Err(Refusal::CannotAfford { need: def.cost });
        }
        self.grid.mark_structure(&footprint.cells, true);
        self.changed.write(GridChanged);
        let ground = self.ground(p.at);
        let blocked = footprint.cells.len();
        let root = self
            .commands
            .spawn((
                placement_transform(&def, &p, ground),
                Visibility::default(),
                Structure,
            ))
            .id();
        self.placed.0.push(PlacedEntry {
            entity: root,
            placement: p,
            core: footprint.core,
            cells: footprint.cells,
        });
        self.spawn_parts(root, p.def, None);
        self.attach_collider(root, p.def);
        info!(
            "structures: built {} ({:?}) at ({:.1}, {:.1}) yaw {:.0}° — blocks {blocked} cells",
            def.name,
            def.kind,
            p.at.x,
            p.at.y,
            p.yaw.to_degrees(),
        );
        Ok(root)
    }

    /// The structure whose footprint covers a WoW `(x, y)`, if any.
    pub fn structure_at(&self, at: Vec2) -> Option<Entity> {
        let cell = self.grid.cell_at(at)?;
        self.placed
            .0
            .iter()
            .find(|s| s.cells.contains(&cell))
            .map(|s| s.entity)
    }

    /// Sells a structure: refund, clear its cells, despawn.
    pub fn sell(&mut self, entity: Entity) {
        let Some(defs) = self.defs.as_ref() else {
            return;
        };
        let Some(at) = self.placed.0.iter().position(|s| s.entity == entity) else {
            return;
        };
        let entry = self.placed.0.swap_remove(at);
        let def = &defs.defs[entry.placement.def];
        let refund = def.cost.scaled(def.refund);
        let name = def.name.clone();
        self.grid.mark_structure(&entry.cells, false);
        self.changed.write(GridChanged);
        self.commands.entity(entity).despawn();
        self.stores.grant(refund, &format!("refund for a {name}"));
    }

    /// The terrain height at a WoW `(x, y)`, or the arena centre's guess off the streamed set.
    pub fn ground(&self, at: Vec2) -> f32 {
        self.heights
            .under(wow_to_bevy([at.x, at.y, 0.0]))
            .unwrap_or(self.level.center[2])
    }

    /// One child per batch under `root`: the static form with the steady material, or for a
    /// `ghost` alpha the blend twin at that alpha and no [`ModelPart`], so the visibility
    /// authority leaves its look alone.
    pub fn spawn_parts(&mut self, root: Entity, def: usize, ghost: Option<f32>) {
        let Some(defs) = self.defs.as_ref() else {
            return;
        };
        let handle = defs.handles[def].clone();
        let Some(model) = self.m2s.get(&handle) else {
            return;
        };
        let built = self.forms.slices(&handle);
        for (pi, sub) in model.submeshes.iter().enumerate() {
            if sub.billboard.is_some() {
                continue;
            }
            let order = u16::try_from(pi + 1).unwrap_or(u16::MAX);
            let material = match ghost {
                None => self.mats.steady(sub, sub.texture.clone(), order),
                Some(_) => {
                    let mut lane = benilla_world::model_render::EntityUvLane {
                        reg: &mut self.uv_reg,
                        table: &mut self.uv_table,
                        instance: None,
                    };
                    self.mats
                        .entity_variants(sub, sub.texture.clone(), order, &mut lane)
                        .map(|v: BatchVariants| v.fade_blend)
                }
            };
            let Some(material) = material else { continue };
            let (mesh, aabb) = built.stat.get(pi).cloned().unwrap_or_default();
            let alpha = ghost.unwrap_or(1.0);
            let mut part = self.commands.spawn((
                Mesh3d(mesh),
                MeshMaterial3d(material),
                Transform::default(),
                ChildOf(root),
                MeshTag(benilla_world::mesh_tag::spawn_tag(0, alpha)),
            ));
            if ghost.is_none() {
                part.insert(ModelPart {
                    kind: ModelKind::GameObject,
                    blend: sub.blend,
                });
            }
            if let Some(aabb) = aabb {
                part.insert(aabb);
            }
        }
    }

    /// The hull as a trimesh on the root, or one box per footprint rectangle for a hull-less
    /// model, as children sized to the model's height.
    fn attach_collider(&mut self, root: Entity, def: usize) {
        let Some(defs) = self.defs.as_ref() else {
            return;
        };
        let d = &defs.defs[def];
        let Some(model) = self.m2s.get(&defs.handles[def]) else {
            return;
        };
        let hull = model.collision.as_ref().filter(|h| h.indices.len() >= 3);
        match hull {
            Some(hull) => {
                let verts: Vec<Vec3> = hull.positions.iter().map(|p| wow_to_bevy(*p)).collect();
                let tris: Vec<[u32; 3]> = hull
                    .indices
                    .as_chunks::<3>()
                    .0
                    .to_vec();
                self.commands
                    .entity(root)
                    .insert((RigidBody::Static, Collider::trimesh(verts, tris)));
            }
            None => {
                let height = model
                    .bounds
                    .map_or(5.0, |b| (b.bbox_max[2] - b.bbox_min[2]).max(1.0));
                self.commands.entity(root).insert(RigidBody::Static);
                for rect in &d.footprint {
                    // Model-space box: WoW x (forward) is Bevy -z, WoW y (left) is Bevy -x.
                    let center = wow_to_bevy([rect.center.0, rect.center.1, height * 0.5]);
                    self.commands.spawn((
                        Transform::from_translation(center),
                        Collider::cuboid(rect.size.1, height, rect.size.0),
                        ChildOf(root),
                    ));
                }
            }
        }
        self.epoch.bump();
    }
}

/// Logs every def's model bounds once loaded, the source for a hull-less model's footprint.
pub fn log_bounds(defs: Option<Res<StructureDefs>>, m2s: Res<Assets<M2Model>>, mut done: Local<bool>) {
    let Some(defs) = defs else { return };
    if *done || !defs.handles.iter().all(|h| m2s.contains(h)) {
        return;
    }
    *done = true;
    for (d, h) in defs.defs.iter().zip(&defs.handles) {
        let Some(m) = m2s.get(h) else { continue };
        let hull = m.collision.as_ref().map_or(0, |c| c.indices.len() / 3);
        match m.bounds {
            Some(b) => info!(
                "structures: {} bbox x {:.2}..{:.2} y {:.2}..{:.2} z {:.2}..{:.2}, hull {hull} tris",
                d.id, b.bbox_min[0], b.bbox_max[0], b.bbox_min[1], b.bbox_max[1], b.bbox_min[2], b.bbox_max[2]
            ),
            None => info!("structures: {} has no bounds, hull {hull} tris", d.id),
        }
    }
}

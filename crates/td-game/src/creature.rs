//! Creatures: a `CreatureDisplayInfo` id to a skinned, animated M2 standing on the terrain. The
//! recipe is `benilla-app`'s (`entities::display::build_parts`, `entities::attach`) cut to a
//! plain creature: no geoset filter (only character bodies have one), no fades, no emitters, no
//! billboard cards. M1 spawns one kobold that stands, walks 20 yards up the road and stops.

use bevy::animation::transition::AnimationTransitions;
use bevy::camera::primitives::Aabb;
use bevy::prelude::*;
use bevy::mesh::MeshTag;

use benilla_assets::coords::wow_to_bevy;
use benilla_assets::{AssetSet, LockRecover, M2Model, WorldAssets};
use benilla_formats::{load_creature_catalog, CreatureModel};
use benilla_world::model_forms::ModelForms;
use benilla_world::model_render::{M2BatchMaterials, ModelKind, ModelPart};
use benilla_world::rig_anim::{GlobalSeqDrive, RigPose};
use benilla_world::rig_palette::{RigPalettes, RigSkin};
use benilla_world::world_point::TerrainHeights;
use benilla_world::world_unit::WorldUnit;

use crate::anim::{CreatureAnim, Gait};
use crate::boot::ARENA_CENTER;
use crate::shot::ShotMoments;

/// The kobold's model; the display is the lowest `CreatureDisplayInfo` id on it with a skin.
const KOBOLD_MODEL: &str = "creature\\kobold\\kobold.mdx";

/// The walk: from 10 yards south of the arena centre to 10 yards north, along the road (WoW +x).
const WALK_FROM: [f32; 2] = [ARENA_CENTER[0] - 10.0, ARENA_CENTER[1]];
const WALK_TO: [f32; 2] = [ARENA_CENTER[0] + 10.0, ARENA_CENTER[1]];

/// A creature's walk speed in yards per second, the client's default for a walking NPC.
const WALK_SPEED: f32 = 2.5;

/// Seconds the kobold stands before it sets off.
const STAND_SECS: f32 = 4.0;

pub fn plugin(app: &mut App) {
    app.add_systems(Startup, load_catalog.after(AssetSet::Open))
        .add_systems(Update, (spawn_kobold, walk).chain());
}

/// The kobold waiting to spawn: its display, the resolved model and the M2 loading.
#[derive(Resource)]
struct PendingKobold {
    display_id: u32,
    model: CreatureModel,
    handle: Handle<M2Model>,
}

fn load_catalog(
    mut commands: Commands,
    world_assets: Option<Res<WorldAssets>>,
    asset_server: Res<AssetServer>,
) {
    let Some(world_assets) = world_assets else {
        return;
    };
    let catalog = {
        let mut chain = world_assets.chain.lock_recover();
        match load_creature_catalog(&mut chain) {
            Ok(c) => c,
            Err(e) => {
                error!("creature catalog unavailable: {e:#}");
                return;
            }
        }
    };
    let kobold = catalog
        .display_models()
        .map(|(display, _)| display)
        .filter_map(|d| catalog.model(d).map(|m| (d, m)))
        .filter(|(_, m)| {
            m.model_path.eq_ignore_ascii_case(KOBOLD_MODEL) && m.textures[0].is_some()
        })
        .min_by_key(|(d, _)| *d);
    match kobold {
        Some((display_id, model)) => {
            info!(
                "creature: kobold is display {display_id} ({}, skins {:?}, scale {})",
                model.model_path, model.textures, model.scale
            );
            let handle = asset_server.load(benilla_assets::m2_url(&model.model_path));
            commands.insert_resource(PendingKobold {
                display_id,
                model,
                handle,
            });
        }
        None => error!("creature: no CreatureDisplayInfo row uses {KOBOLD_MODEL}"),
    }
}

/// Walks a creature from `from` to `to` (Bevy space, `y` ignored) once it has stood a while.
#[derive(Component)]
struct Walker {
    from: Vec3,
    to: Vec3,
    /// Seconds since the spawn.
    clock: f32,
    /// Yards covered so far.
    covered: f32,
}

/// Spawns the kobold once its model, forms, materials and the ground under it are all there.
fn spawn_kobold(
    mut commands: Commands,
    pending: Option<Res<PendingKobold>>,
    m2s: Res<Assets<M2Model>>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut forms: ResMut<ModelForms>,
    mut mats: M2BatchMaterials,
    mut palettes: ResMut<RigPalettes>,
    heights: TerrainHeights,
    asset_server: Res<AssetServer>,
) {
    let Some(pending) = pending else {
        return;
    };
    let from = wow_to_bevy([WALK_FROM[0], WALK_FROM[1], 0.0]);
    let to = wow_to_bevy([WALK_TO[0], WALK_TO[1], 0.0]);
    // Terrain first: off the streamed set the column has no height, and a guess floats or sinks.
    let Some(ground) = heights.under(from) else {
        return;
    };
    let Some(model) = m2s.get(&pending.handle) else {
        return;
    };
    if !mats.ready() {
        return;
    }
    // Both forms, since the kobold rigs; one forced build rather than the paced furnish.
    if !forms.require_rigged(&pending.handle, 0) {
        forms.ensure_now_rigged(&pending.handle, &model.submeshes, &mut meshes);
        if !forms.require_rigged(&pending.handle, 0) {
            return;
        }
    }
    let Some(anims) = model.animations.clone() else {
        error!("creature: {} has no animations", pending.model.model_path);
        commands.remove_resource::<PendingKobold>();
        return;
    };

    let scale = pending.model.scale;
    let at = Vec3::new(from.x, ground, from.z);
    let root = commands
        .spawn((
            Transform::from_translation(at)
                .looking_to(to - from, Vec3::Y)
                .with_scale(Vec3::splat(scale)),
            Visibility::default(),
        ))
        .id();

    // The rig on the collapsed lane: the pose buffer and a palette slot, no joint entities.
    let nbones = model.skeleton.joints.len() as u32;
    let slot = match RigSkin::allocate_bones(&mut palettes, nbones, model.inverse_bindposes.clone())
    {
        Some(skin) => {
            let slot = skin.slot;
            commands.entity(root).insert(skin);
            slot
        }
        None => {
            warn!("creature: rig palette full, the kobold draws its bind pose");
            0
        }
    };
    commands
        .entity(root)
        .insert(RigPose::new(root, &model.skeleton));
    if let Some(drive) = GlobalSeqDrive::new_rig(&anims.global_bones, nbones as usize) {
        commands.entity(root).insert(drive);
    }

    let stand_box = anims
        .find(crate::anim::STAND)
        .map(|c| Aabb::from_min_max(c.bounds_min, c.bounds_max));
    commands.entity(root).insert((
        WorldUnit {
            wades: true,
            scale,
            height: pending.model.collision_height * scale,
            bound: stand_box,
        },
        AnimationPlayer::default(),
        AnimationTransitions::new(),
        AnimationGraphHandle(anims.graph.clone()),
        anims,
        CreatureAnim::new(Gait::Stand),
        Walker {
            from: at,
            to: Vec3::new(to.x, ground, to.z),
            clock: 0.0,
            covered: 0.0,
        },
        benilla_world::interact::WorldObject {
            kind: ModelKind::Creature,
            label: "Kobold".into(),
            id: pending.display_id,
            detail: String::new(),
        },
    ));

    // One child per batch: the skinned twin when the rig has a slot, else the static mesh.
    let dir = model_dir(&pending.model.model_path);
    let built = forms.slices(&pending.handle);
    let mut skipped_cards = 0;
    for (pi, sub) in model.submeshes.iter().enumerate() {
        if sub.billboard.is_some() {
            skipped_cards += 1;
            continue;
        }
        let texture = match (&sub.texture, sub.skin_slot) {
            (Some(t), _) => Some(t.clone()),
            (None, Some(s)) => pending
                .model
                .textures
                .get(s as usize)
                .and_then(Option::as_ref)
                .map(|name| asset_server.load(benilla_assets::skin_url(dir, name))),
            (None, None) => None,
        };
        let order = u16::try_from(pi + 1).unwrap_or(u16::MAX);
        let Some(material) = mats.steady(sub, texture, order) else {
            continue;
        };
        let skinned = built.skin.and_then(|s| s.get(pi)).filter(|_| slot != 0);
        let (stat, aabb) = built.stat.get(pi).cloned().unwrap_or_default();
        let mut part = commands.spawn((
            Mesh3d(skinned.cloned().unwrap_or(stat)),
            MeshMaterial3d(material),
            Transform::default(),
            ChildOf(root),
            ModelPart {
                kind: ModelKind::Creature,
                blend: sub.blend,
            },
            MeshTag(benilla_world::mesh_tag::spawn_tag(slot, 1.0)),
        ));
        if let Some(aabb) = aabb {
            part.insert(aabb);
        }
    }
    info!(
        "creature: kobold spawned at wow {:?} (slot {slot}, {} batches, {skipped_cards} cards skipped)",
        benilla_assets::coords::bevy_to_wow(at),
        model.submeshes.len()
    );
    commands.remove_resource::<PendingKobold>();
}

/// The walk script: stand, walk the line on the ground, stand again. It announces the shot
/// moments `standing` and `walking` (at the halfway mark).
fn walk(
    time: Res<Time>,
    heights: TerrainHeights,
    mut moments: ResMut<ShotMoments>,
    mut walkers: Query<(&mut Walker, &mut Transform, &mut CreatureAnim)>,
) {
    for (mut w, mut xf, mut anim) in &mut walkers {
        w.clock += time.delta_secs();
        let length = w.from.distance(w.to);
        if w.clock < STAND_SECS {
            if w.clock >= STAND_SECS - 1.0 {
                moments.announce("standing");
            }
            continue;
        }
        if w.covered >= length {
            anim.set(Gait::Stand, 0.0);
            continue;
        }
        w.covered = (w.covered + WALK_SPEED * time.delta_secs()).min(length);
        let flat = w.from.lerp(w.to, w.covered / length);
        let y = heights.under(flat).unwrap_or(xf.translation.y);
        xf.translation = Vec3::new(flat.x, y, flat.z);
        anim.set(Gait::Walk, WALK_SPEED);
        if w.covered >= length * 0.5 {
            moments.announce("walking");
        }
    }
}

/// `Creature\Kobold\Kobold.mdx` → `Creature\Kobold`, where the skin variations live.
fn model_dir(model_path: &str) -> &str {
    model_path
        .rsplit_once(['\\', '/'])
        .map(|(dir, _)| dir)
        .unwrap_or("")
}

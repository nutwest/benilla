//! Creatures: a `CreatureDisplayInfo` id to a skinned, animated M2 standing on the terrain. The
//! recipe is `benilla-app`'s (`entities::display::build_parts`, `entities::attach`) cut to a
//! plain creature: no geoset filter (only character bodies have one), no emitters, no billboard
//! cards; each part carries its fade twins so a death can feather it out. A humanoid NPC display
//! (one with `CreatureDisplayInfoExtra`) needs the character-body composite and is refused.

use std::collections::HashMap;

use bevy::animation::transition::AnimationTransitions;
use bevy::camera::primitives::Aabb;
use bevy::ecs::system::SystemParam;
use bevy::mesh::MeshTag;
use bevy::prelude::*;

use benilla_assets::{AssetSet, LockRecover, M2Model, WorldAssets};
use benilla_formats::{load_creature_catalog, CreatureModel};
use benilla_world::model_fade::FadeMaterials;
use benilla_world::model_forms::ModelForms;
use benilla_world::model_render::{M2BatchMaterials, ModelKind, ModelPart};
use benilla_world::rig_anim::{GlobalSeqDrive, RigPose};
use benilla_world::rig_palette::{RigPalettes, RigSkin};
use benilla_world::world_unit::WorldUnit;

use crate::anim::{CreatureAnim, Gait};
use crate::level::Level;
use crate::units::UnitDefs;

pub fn plugin(app: &mut App) {
    app.add_systems(Startup, load_library.after(AssetSet::Open));
}

/// The displays this level uses, each with its resolved model and its M2 loading.
#[derive(Resource)]
pub struct CreatureLibrary {
    displays: HashMap<u32, Display>,
}

struct Display {
    model: CreatureModel,
    handle: Handle<M2Model>,
}

/// Loads the catalog and starts every display the level's waves name, so a wave never waits on
/// a file. `TD_FIND_DISPLAY=<text>` lists the displays whose model path contains `text`.
fn load_library(
    mut commands: Commands,
    world_assets: Option<Res<WorldAssets>>,
    asset_server: Res<AssetServer>,
    level: Res<Level>,
    units: Res<UnitDefs>,
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
    if let Ok(needle) = std::env::var("TD_FIND_DISPLAY") {
        let needle = needle.to_ascii_lowercase();
        let mut found: Vec<(u32, CreatureModel)> = catalog
            .display_models()
            .filter_map(|(d, _)| catalog.model(d).map(|m| (d, m)))
            .filter(|(_, m)| m.model_path.to_ascii_lowercase().contains(&needle))
            .collect();
        found.sort_by_key(|(d, _)| *d);
        for (d, m) in found.iter().take(40) {
            info!(
                "find: display {d} {} skins {:?} scale {} humanoid {}",
                m.model_path,
                m.textures,
                m.scale,
                m.npc_appearance.is_some()
            );
        }
    }

    // The enemies' displays and the player's units'.
    let wanted = level
        .waves
        .iter()
        .map(|w| w.display_id)
        .chain(units.defs.iter().map(|u| u.display_id));
    let mut displays = HashMap::new();
    for display_id in wanted {
        if displays.contains_key(&display_id) {
            continue;
        }
        match catalog.model(display_id) {
            Some(m) if m.npc_appearance.is_some() => error!(
                "creature: display {display_id} is a humanoid NPC ({}), which td-game cannot dress yet",
                m.model_path
            ),
            Some(model) => {
                info!(
                    "creature: display {display_id} is {} (skins {:?}, scale {})",
                    model.model_path, model.textures, model.scale
                );
                let handle = asset_server.load(benilla_assets::m2_url(&model.model_path));
                displays.insert(display_id, Display { model, handle });
            }
            None => error!("creature: no CreatureDisplayInfo row {display_id}"),
        }
    }
    commands.insert_resource(CreatureLibrary { displays });
}

/// Everything a creature spawn reads and writes, as one system parameter.
#[derive(SystemParam)]
pub struct CreatureSpawner<'w, 's> {
    commands: Commands<'w, 's>,
    library: Option<Res<'w, CreatureLibrary>>,
    m2s: Res<'w, Assets<M2Model>>,
    meshes: ResMut<'w, Assets<Mesh>>,
    forms: ResMut<'w, ModelForms>,
    mats: M2BatchMaterials<'w>,
    uv_reg: ResMut<'w, benilla_world::doodad_anim::UvAnimMaterials>,
    uv_table: ResMut<'w, benilla_world::mat_anim_table::MatAnimTable>,
    palettes: ResMut<'w, RigPalettes>,
    asset_server: Res<'w, AssetServer>,
}

impl<'w, 's> CreatureSpawner<'w, 's> {
    /// The command queue the spawns go through, for the caller's own components.
    pub fn commands(&mut self) -> &mut Commands<'w, 's> {
        &mut self.commands
    }

    /// Whether `display_id` can spawn this frame: its model is loaded, its meshes built (forced,
    /// not paced) and the shared light buffer is up.
    pub fn ready(&mut self, display_id: u32) -> bool {
        let Some(display) = self.library.as_ref().and_then(|l| l.displays.get(&display_id)) else {
            return false;
        };
        let Some(model) = self.m2s.get(&display.handle) else {
            return false;
        };
        if !self.mats.ready() {
            return false;
        }
        if !self.forms.require_rigged(&display.handle, 0) {
            self.forms
                .ensure_now_rigged(&display.handle, &model.submeshes, &mut self.meshes);
        }
        self.forms.require_rigged(&display.handle, 0)
    }

    /// Spawns `display_id` standing at `at` (Bevy space, on the ground) facing `facing`, or
    /// `None` when it is not [`ready`](Self::ready).
    pub fn spawn(&mut self, display_id: u32, at: Vec3, facing: Vec3) -> Option<Entity> {
        if !self.ready(display_id) {
            return None;
        }
        let library = self.library.as_ref()?;
        let display = library.displays.get(&display_id)?;
        let model = self.m2s.get(&display.handle)?;
        let anims = model.animations.clone()?;

        let scale = display.model.scale;
        let root = self
            .commands
            .spawn((
                Transform::from_translation(at)
                    .looking_to(facing, Vec3::Y)
                    .with_scale(Vec3::splat(scale)),
                Visibility::default(),
            ))
            .id();

        // The rig on the collapsed lane: the pose buffer and a palette slot, no joint entities.
        let nbones = model.skeleton.joints.len() as u32;
        let slot = match RigSkin::allocate_bones(
            &mut self.palettes,
            nbones,
            model.inverse_bindposes.clone(),
        ) {
            Some(skin) => {
                let slot = skin.slot;
                self.commands.entity(root).insert(skin);
                slot
            }
            None => {
                warn!("creature: rig palette full, display {display_id} draws its bind pose");
                0
            }
        };
        self.commands
            .entity(root)
            .insert(RigPose::new(root, &model.skeleton));
        if let Some(drive) = GlobalSeqDrive::new_rig(&anims.global_bones, nbones as usize) {
            self.commands.entity(root).insert(drive);
        }

        let stand_box = anims
            .find(crate::anim::STAND)
            .map(|c| Aabb::from_min_max(c.bounds_min, c.bounds_max));
        self.commands.entity(root).insert((
            WorldUnit {
                wades: true,
                scale,
                height: display.model.collision_height * scale,
                bound: stand_box,
            },
            AnimationPlayer::default(),
            AnimationTransitions::new(),
            AnimationGraphHandle(anims.graph.clone()),
            anims,
            CreatureAnim::new(Gait::Stand),
            benilla_world::interact::WorldObject {
                kind: ModelKind::Creature,
                label: display.model.model_path.clone(),
                id: display_id,
                detail: String::new(),
            },
        ));

        // One child per batch: the skinned twin when the rig has a slot, else the static mesh.
        let dir = model_dir(&display.model.model_path);
        let built = self.forms.slices(&display.handle);
        for (pi, sub) in model.submeshes.iter().enumerate() {
            if sub.billboard.is_some() {
                continue;
            }
            let texture = match (&sub.texture, sub.skin_slot) {
                (Some(t), _) => Some(t.clone()),
                (None, Some(s)) => display
                    .model
                    .textures
                    .get(s as usize)
                    .and_then(Option::as_ref)
                    .map(|name| self.asset_server.load(benilla_assets::skin_url(dir, name))),
                (None, None) => None,
            };
            let order = u16::try_from(pi + 1).unwrap_or(u16::MAX);
            // The whole variant set, so a part can feather out on death through the engine's
            // own fade (`RenderFade` over `FadeMaterials`).
            let mut lane = benilla_world::model_render::EntityUvLane {
                reg: &mut self.uv_reg,
                table: &mut self.uv_table,
                instance: None,
            };
            let Some(v) = self.mats.entity_variants(sub, texture, order, &mut lane) else {
                continue;
            };
            let skinned = built.skin.and_then(|s| s.get(pi)).filter(|_| slot != 0);
            let (stat, aabb) = built.stat.get(pi).cloned().unwrap_or_default();
            let mut part = self.commands.spawn((
                Mesh3d(skinned.cloned().unwrap_or(stat)),
                MeshMaterial3d(v.steady.clone()),
                FadeMaterials {
                    cutout: v.steady,
                    blend: v.fade_blend,
                    bake_blend: Some(v.interior_bake_blend),
                    zfill: v.zfill,
                },
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
        Some(root)
    }
}

/// `Creature\Kobold\Kobold.mdx` → `Creature\Kobold`, where the skin variations live.
fn model_dir(model_path: &str) -> &str {
    model_path
        .rsplit_once(['\\', '/'])
        .map(|(dir, _)| dir)
        .unwrap_or("")
}

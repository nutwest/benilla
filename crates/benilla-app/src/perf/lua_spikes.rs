//! The Lua spike breakdown (`WOW_LUA_SPIKES=<ms>`): on a frame whose UI Lua ran longer than `<ms>`,
//! one line of the chunks that spent it, from the VM's chunk profile (`benilla_lua::vm::Profile`),
//! the addon profiler's clock:
//!
//! ```text
//! [lua] frame 812 lua=34.1ms  FrameXML\PlayerFrame.lua=9.2 FrameXML\ActionButton.lua=7.0 …
//! ```
//!
//! Profiling stays on in every VM while the switch is set, so it costs what crossing chunks costs.

use benilla_ui::script::UiScript;
use bevy::prelude::*;

/// The chunks named on a spike line.
const TOP: usize = 8;

#[derive(Resource)]
struct LuaSpikes {
    threshold_ms: f32,
    session: u64,
    last_ns: Vec<u64>,
    frame: u64,
}

pub(super) fn plugin(app: &mut App) {
    let Some(threshold_ms) = std::env::var("WOW_LUA_SPIKES")
        .ok()
        .and_then(|v| v.parse::<f32>().ok())
    else {
        return;
    };
    app.insert_resource(LuaSpikes {
        threshold_ms,
        session: 0,
        last_ns: Vec::new(),
        frame: 0,
    })
    .add_systems(Last, sample);
}

fn sample(script: Option<NonSend<UiScript>>, mut spikes: ResMut<LuaSpikes>) {
    spikes.frame += 1;
    let Some(script) = script else { return };
    if spikes.session != script.session() {
        // A new VM: profile it from here.
        script.lua().with_vm(|vm| vm.set_profiling(true));
        spikes.session = script.session();
        spikes.last_ns.clear();
        return;
    }
    let spikes = &mut *spikes;
    let line = script.lua().with_vm(|vm| {
        let ns = vm
            .profile
            .as_ref()
            .map(|p| p.ns.clone())
            .unwrap_or_default();
        let mut per: Vec<(u32, u64)> = ns
            .iter()
            .enumerate()
            .map(|(id, &n)| {
                let d = n.saturating_sub(spikes.last_ns.get(id).copied().unwrap_or(0));
                (id as u32, d)
            })
            .filter(|(_, d)| *d > 0)
            .collect();
        spikes.last_ns = ns;
        let total_ms = per.iter().map(|(_, d)| *d).sum::<u64>() as f32 / 1e6;
        if total_ms < spikes.threshold_ms {
            return None;
        }
        per.sort_by_key(|p| std::cmp::Reverse(p.1));
        let top: Vec<String> = per
            .iter()
            .take(TOP)
            .map(|(id, d)| {
                let name = vm
                    .heap
                    .source_name(*id)
                    .map(|n| String::from_utf8_lossy(n).into_owned())
                    .unwrap_or_else(|| format!("#{id}"));
                format!("{name}={:.1}", *d as f32 / 1e6)
            })
            .collect();
        Some(format!("lua={total_ms:.1}ms  {}", top.join(" ")))
    });
    if let Some(line) = line {
        info!("[lua] frame {} {line}", spikes.frame);
    }
}

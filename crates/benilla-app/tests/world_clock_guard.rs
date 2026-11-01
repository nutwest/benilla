//! Deviation guard: nothing that keeps time with the world may read the wall clock, or it runs
//! through a pause and comes apart from the stores on `benilla_world::world_clock`. A new
//! `Instant::now()` or `Time<Real>` in `benilla-app` fails here unless its file is listed below as
//! real time, a claim made in review. `Instant::elapsed` is a hidden `Instant::now()`, which only
//! review catches: a store on the world clock reads `saturating_duration_since(world_clock::now())`.

use std::path::Path;

/// Files and folders on the wall clock, by what they are.
const WALL_CLOCK_INSTANT: &[&str] = &[
    // Sound and video play on through a pause.
    "sound/",
    "video.rs",
    // Instruments.
    "capture/",
    "crash.rs",
    "perf/",
    "target/arm.rs",
    "ui_pass.rs",
    // The connection, whose ping and wall-clock sample don't stop.
    "net.rs",
    "net/io.rs",
    // The interface, which runs through a pause; `UiClock` maps its stores onto `GetTime`.
    "tutorial.rs",
    "ui_chat/",
    "ui_layout.rs",
    "ui_script/",
    "ui_world_enter.rs",
];

const WALL_CLOCK_SECONDS: &[&str] = &[
    // The camera's glide and the appear and teardown fades, which the reference runs on the wall
    // clock (`FadeTo`), so a unit created while paused still fades in.
    "aura_visual.rs",
    "entities.rs",
    "entities/",
    "player/swap_blend.rs",
    // Instruments.
    "asset_churn.rs",
    "console.rs",
    "debug_panel/",
    "hover_log.rs",
    "perf/",
    "pipe_warm/",
    "capture/",
    // Sessions and screens, none of which the server's world clock holds.
    "glue_session/",
    "loading_screen.rs",
    "net/party_swap.rs",
    "portrait/",
    "ui_billing.rs",
    "ui_chat/",
    "ui_guild/",
    "ui_memory.rs",
    "ui_script/",
];

fn listed(rel: &str, list: &[&str]) -> bool {
    list.iter()
        .any(|p| rel == *p || (p.ends_with('/') && rel.starts_with(p)))
}

/// `src` up to its test module, with comment lines dropped.
fn code(src: &str) -> String {
    let body = src
        .find("#[cfg(test)]\nmod ")
        .or_else(|| src.find("#[cfg(test)]\r\nmod "))
        .map_or(src, |at| &src[..at]);
    body.lines()
        .filter(|l| !l.trim_start().starts_with("//"))
        .collect::<Vec<_>>()
        .join("\n")
}

fn walk(dir: &Path, root: &Path, out: &mut Vec<String>) {
    for entry in std::fs::read_dir(dir).unwrap().flatten() {
        let path = entry.path();
        if path.is_dir() {
            if path.file_name().is_some_and(|n| n != "tests") {
                walk(&path, root, out);
            }
            continue;
        }
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        if !name.ends_with(".rs") || name == "tests.rs" || name.ends_with("_tests.rs") {
            continue;
        }
        let rel = path
            .strip_prefix(root)
            .unwrap()
            .to_string_lossy()
            .replace('\\', "/");
        let body = code(&std::fs::read_to_string(&path).unwrap());
        if body.contains("Instant::now") && !listed(&rel, WALL_CLOCK_INSTANT) {
            out.push(format!("{rel}: Instant::now, use world_clock::now"));
        }
        if body.contains("Time<Real>") && !listed(&rel, WALL_CLOCK_SECONDS) {
            out.push(format!("{rel}: Time<Real>, use Time<WorldClock>"));
        }
    }
}

#[test]
fn the_wall_clock_is_read_only_where_it_is_listed() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut found = Vec::new();
    walk(&root, &root, &mut found);
    assert!(
        found.is_empty(),
        "world code on the wall clock:\n{}",
        found.join("\n")
    );
}

#[test]
fn the_guard_sees_past_comments_and_stops_at_tests() {
    let src = "// Instant::now\nfn a() {}\n#[cfg(test)]\nmod tests { Instant::now(); }\n";
    assert!(!code(src).contains("Instant::now"));
    assert!(code("fn a() { Instant::now(); }").contains("Instant::now"));
}

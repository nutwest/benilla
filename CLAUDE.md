\# Project: tower defense built on benilla-world

\- Read AGENTS.md and docs/MAP.md before making changes.

\- All game code lives in a new crate, crates/td-game. Do not modify existing benilla crates

&#x20; unless asked; if engine changes seem necessary, propose them first.

\- No networking, no benilla-app, no FrameXML UI. Use benilla-world + benilla-assets only.

\- Game data comes from WOW\_DATA at runtime. Never copy or commit game assets.

\- After each change: cargo build, run it, and take a screenshot to verify (see WOW\_WORLDVIEW\_SHOT).


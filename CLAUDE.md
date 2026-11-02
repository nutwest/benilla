# Project: singleplayer party WoW (branch "party")
- Read AGENTS.md, docs/MAP.md and docs/PARTY_DESIGN.md before changes.
- Local embedded server only (vmangos fork at F:\Game Prototype\vmangos, branch
  "party"). Server changes go there, committed with clear messages.
- Changes to benilla-app are allowed: keep them minimal and log each one (file + why) in
  the "Engine changes" list in PARTY_DESIGN.md. New code goes in crates/party where
  possible.
- Never copy or commit game assets.
- Make code edits with the Edit tool, one change at a time, not with sed/regex or
  multi-edit scripts. Never re-run a partially applied edit script.
- Before any live run, confirm the build succeeded in that same step; if it failed,
  fix it and do not run.
- After changing benilla-app, run its tests for the touched modules before reporting a
  milestone done.
- Respect the verification budget given in each prompt. If it isn't enough, stop and
  ask instead of exceeding it.
- Stop at the end of each milestone and report: what was built, what was checked, what
  wasn't.

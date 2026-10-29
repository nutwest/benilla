# Party — design

Single-player classic WoW against a local vmangos server, played through benilla, with three
additions:

1. You control a party of your own characters and switch which one you are playing.
2. You can pause the game and keep a free RTS camera while it is paused.
3. Otherwise it plays like normal WoW: the active character has the full stock UI, bags, spells,
   talents and quests.

This document reports what the code says about each requirement, recommends an approach and plans
milestones. Nothing is built yet.

**Recommendation in one line:** keep benilla single-session. The characters you are not playing
stay in the world as **server-side party bots**. **Switching swaps which character the one client
session is logged in as.** **Pause is a server command** paired with a **pausable client world
clock**. The RTS camera and the mode blend are ported from td-game.

---

## 1. Findings

### 1.1 Multi-session: how deeply benilla-app assumes one player

benilla-app is about 970 files and 609k lines. A single session is built into every layer of it:

| Layer | The single-session assumption | Reach |
|---|---|---|
| Net | `spawn_net` starts **one** read thread and **one** write thread. The write thread holds the only `WorldWriter` (`net/io.rs`). `NetEvents`, `NetCommands`, `CharPick` and `LoginSubmit` are global resources (`net.rs`). There are 30 process-wide statics and 13 `thread_local!`s across the app, for example `INBOUND_PACKETS`. | `NetCommands` is used in 305 files |
| Object mirror | `GuidIndex` is one guid→entity map, "like the reference's one `ClntObjMgr`". `Objects::player()` is `With<SelfPlayer>` and expects exactly one. | `GuidIndex` is used in 241 files |
| Player | `SelfGuid(Option<u64>)`. `SelfPlayer`, `Embodied` and `ActiveMover` each sit on at most one entity. `Player` is one resource holding the controller state. `MoverClaim` is one global that mirrors the reference's active-mover global. | `SelfGuid` is used in 170 files |
| Camera | `player/camera*` drives the single camera off `Embodied`. It has 20+ modules: zoom, clip, water, saved views. | |
| UI | One Lua VM (`ui_script`) runs the stock FrameXML. Its unit token `"player"` always resolves to `SelfPlayer`. Field-watch notices, action bars, cooldown lanes, bag and quest caches are all per client. | `ui_script::` is used in 244 files |
| ECS overall | 624 `Resource` types and 151 `NonSend` uses, nearly all meaning "this client's". | |

Running N world sessions in one client would mean one of two things:

- Every one of those resources becomes per-session: keyed by a session id, or held in a Bevy
  sub-app per session.
- Each session's object mirror is merged into one shared rendered world. Every party member is
  streamed by every session, so the same guid shows up N times and must be deduplicated.

The UI cannot be swapped by flipping a flag. FrameXML holds Lua state for one character, so each
session would need its own VM, plus a way to show only the active one. The work amounts to
rewriting the app's spine, against a codebase whose rule is to mirror the 1.12 client, and that
client is single-session. **Rejected.**

There is a cheaper partial form: extra sessions as headless protocol-only clients.
`benilla-protocol::WorldSession` already runs standalone, and the `benilla-world` CLI does this.
These sessions would keep the other characters online. But they would stand idle unless we wrote
a bot AI on the client side. The server can already supply that AI (§2), so this form is
rejected too.

The server constraint matters more than any of this. vmangos allows one world session per
account, and a character can be online only once. Nothing on the client changes that.

### 1.2 Possession: server-granted control of another unit

benilla already implements 1.12's possession model faithfully:

- **`SelfPlayer`** is the character. **`Embodied`** is the body the camera is attached to.
  **`ActiveMover`** is the body we may move. Together they mirror the reference's active player,
  camera anchor and active-mover global (`net.rs`, `player/embody.rs`).
- **`MoverLock.foreign_mover`** (`player/state.rs`) holds a possessed unit's guid.
  `maintain_embodiment` moves `Embodied` and `ActiveMover` onto that unit and reseats the
  controller. `MoverClaim::claim` sends `CMSG_SET_ACTIVE_MOVER`. Outbound `MSG_MOVE_*` then drive
  the creature. Losing control (fear) removes only `ActiveMover`.
- **Possessed casts** go out as `CMSG_PET_CAST_SPELL` from the pet bar (`ui_pet/`, fed by
  `SMSG_PET_SPELLS`).
- `CHARMEDBY` is watched (`field_watch.rs`) and drives minimap dots, the combat log, and the
  `UNIT_PET` and `PET_BAR_UPDATE` events.

Missing for *full* character control through possession:

- **Private fields.** The server sends a player's private update fields only to that player's own
  session. Possessing a bot character therefore never gives us its bags, equipment slots, money,
  XP, skills, quest log, spellbook or talents.
- **Action bars, spellbook and talents.** `SMSG_ACTION_BUTTONS`, `SMSG_INITIAL_SPELLS` and
  talents go only to the owning session. A charm gets a ten-slot pet bar.
- **The UI's identity.** FrameXML's `"player"` stays the possessor. Unit frames, the casting bar,
  buffs, the character sheet and so on keep showing the original character.
- **Server behaviour.** vmangos treats a charmed player as charmed: charm auras, threat, a faction
  change, and AI hooks.

Possession lets you *steer* another body with a pet bar. It does not let you *play* that
character as normal WoW, which fails requirement 3. Closing the gap needs server changes as large
as a session swap, plus UI work benilla's stock-UI rule forbids. **Rejected as the switching
mechanism.** It could still be an optional "direct steer" in a later milestone.

### 1.3 Pause: which client clocks must freeze, and how pause reaches the client

**There is no pause in 1.12 or in vmangos.** No opcode exists for it, so pause must be added on
both sides.

**Server.** World and map updates run on `getMSTime` diffs. Some state is stored as absolute
timestamps: spell and item cooldowns, creature respawn times, and the game clock. A pause has to
do three things:

- Run map and world updates with a diff of 0 while paused, but keep handling session packets so
  the client stays connected and can send the unpause.
- On resume, shift the absolute deadlines by the paused span.
- Refuse movement and cast packets while paused.

This is a small vmangos patch exposed as a GM command (`.pause on|off`). The reply comes back as a
system chat message the client crate recognises. The client initiates the pause, so it already
knows the state; the reply is its acknowledgement.

**Client.** There is no single game clock. Time is read in four ways:

| Clock | Count | Examples |
|---|---|---|
| `Res<Time>` (virtual) | 131 sites | creature and rig animation, particles, doodad animation, lighting, player controller |
| `Time<Real>` | 80 sites | aura timers (`ui_aura`), billing and `OsGetAsyncTimeMs` (`ui_billing::now_ms`), art scope, deliberately for material animation |
| `Instant::now()` | 381 sites | **spline sampling** (`net/motion/spline.rs`), remote-unit extrapolation, parts of creature and rig animation |
| `GetTime` | — | the process clock. FrameXML's casting bar, cooldown sweeps (`GetActionCooldown` start), buff timers and auction deadlines all read it. |

`Time<Virtual>::pause()` alone would freeze most animation. It would leave these running:

- Splines and extrapolation, so mobs slide to their destination.
- Cast bars and cooldown sweeps, which run on `GetTime`.
- Aura timers.
- The sky clock.

`benilla-world::frame_pace` also rewrites `Time<Virtual>::elapsed`, so a pause has to go through
the pacer.

What must freeze:

- Movement splines and extrapolation.
- Creature and player animation.
- Particles and ribbons.
- Cast and channel bars.
- Cooldowns, aura durations and the swing timer.
- The game clock and sky.
- Combat text and chat bubbles. This is optional; freezing them looks better.

What must keep running:

- Rendering.
- The RTS camera, on `Time<Real>`.
- UI input and menus. `OnUpdate`'s elapsed keeps flowing, but `GetTime` holds still.
- The net threads and keepalive.

The clean solution is one **pausable world clock** resource that these consumers read in place of
`Instant::now()` or real time. That is an engine change to benilla-app and benilla-world (E2 in
§3). It touches many files but is mechanical, and the gates catch regressions.

**Input** must also freeze. While paused the controller must stop emitting `MSG_MOVE_*`, and
action-bar presses must be held or refused. Otherwise the client predicts movement the server is
not running.

### 1.4 What can be ported from td-game (branch `td-game`)

| Piece | File | Verdict |
|---|---|---|
| RTS camera | `camera.rs` (360 lines): boom over a ground focus, WASD and edge pan, Q/E and drag turn, wheel zoom, terrain riding via `TerrainHeights` | **Port.** Replace the arena clamp with a radius around the party and run it on `Time<Real>`. It needs benilla-app's camera to yield the camera entity while active (E3). The existing dev free-fly (`Player::detached`, Ctrl+Shift+F in `player/controller.rs`) shows the app already supports "camera off the body, body frozen". |
| Mode transition | `ControlMode` blend in `camera.rs`/`commander.rs`: position, rotation and FOV eased over `BLEND_SECS`, with camera input ignored mid-blend | **Port** as the play↔pause transition. Blend from the WoW third-person pose to the RTS boom and back. On unpause, return to the *active character's* saved camera. |
| Unit behaviours | `units.rs`/`combat.rs`: Hold, Guard (anchor and radius), Escort (follows a charge), a selection panel, anchor and escort picking | **Port the vocabulary and UI, not the executor.** Party members are server-controlled, so each behaviour becomes a bot order on the server (§2, M6). The client keeps the panel, the pick flow and the circle gizmos. td-game's `think` loop, grid, flow field and A* stay behind. |
| HUD patterns | `hud.rs`: `tagged_button`, bevy_ui panels | **Port the patterns** for a party bar and an order panel, drawn as a bevy_ui overlay above FrameXML. This keeps the stock UI untouched, as METHOD requires. |
| Instruments | `TD_CAM`, `TD_SHOT_MOMENT`, scripted input | **Port the idea.** `PARTY_*` env scripts for repeatable switch and pause shots and timing. |

Not portable: the grid, the flow field, waves, the economy, structures, enemy AI and
`fight_back`.

---

## 2. Recommended approach

**One client session. The party lives on the server.**

- **Party members are vmangos player bots.** vmangos ships a player-bot system (`PlayerBots/`,
  with `PartyBotAI` and `.partybot …` commands). Those bots follow, fight and heal in a group with
  server AI, as real `Player` objects. The client streams them as ordinary players, so the shared
  world renders with no client change.
  - *To verify in M1:* whether a bot can be loaded from an *existing named character* of yours.
    The stock commands create or clone bots. If they can't, the patch is a small server command,
    `.party load <name>`.
- **Switching is a session swap.**
  1. The client logs out of A and the server hands A to a bot.
  2. The server removes B's bot and the client logs into B.
  3. The client then receives B's own full login stream: private fields, spells, action bars,
     talents and quests.

  Requirement 3 holds by construction, because the active character is a genuine login with the
  stock UI.
  - The PoC (M1) uses benilla's existing logout→enter path, skipping the glue screens.
  - A later milestone (M3) makes it seamless. The server rebinds the session in place and the
    client re-enters without a glue round trip or a loading screen when the map is unchanged.
- **Pause is server plus client.** A vmangos `.pause` patch (§1.3) is paired with a pausable world
  clock and an input freeze in benilla (E2). The RTS camera runs on real time during the pause.
  Orders issued while paused are queued and run on resume.
- **Client code lives in a new crate, `crates/party`,** launched through `benilla_app::run_with`
  ("Building on top", CONTRIBUTING). Settings live under `benilla_app::config_dir()`.

### Engine changes this needs (to be approved before each is made)

The crate reaches benilla-app through `run_with`, `Objects` and `Interact` only, and every other
item in benilla-app is `pub(crate)`. Each item below is a proposal. None is made without your
go-ahead. Upstream policy keeps non-1.12 features out of benilla itself, so on this branch they
are fork changes.

| # | Change | For |
|---|---|---|
| E0 | **Done (M0).** `benilla_app::AutoLogin { user, pass, character }`: a silent login and the character to enter, sent when the crate chooses (`glue_session/login.rs`, `roster.rs`) | M0 |
| E1 | A public "enter character" request: log out, then enter character guid X without showing glue, keeping the window and world assets | M1 |
| E2 | A pausable world clock: route spline sampling, extrapolation, animation, particles, `GetTime`, aura and cooldown clocks and the sky through it, freeze controller output, and make `frame_pace` aware of it | M4 |
| E3 | A camera-yield hook: an external mode takes the camera transform and input. The app's camera saves its view and stops reading input. | M5 |
| E4 | Public read access to the party roster and member guids (`ui_party`) and to `SelfGuid` | M2, M6 |
| E5 | A hook for chat replies: let the crate see system-message lines so it can read server command replies | M2, M4 |

Server-side work goes in a vmangos fork kept beside this repo:

- Loading a named character as a bot.
- Swapping a bot and a player.
- `.pause`.
- Bot orders.

---

## 3. Milestones

Each milestone ends as the td-game ones did: `cargo build`, a live run against the local server
under METHOD's rules (`.probe-identity`, `WOW_UNATTENDED=1`, `WOW_NOSOUND=1`), screenshots, and a
stop for review.

**M0 — Embedded single-player server**

The game starts and stops its own server. The player never runs or configures anything.

- **Server choice: vmangos.** The altbot check, read in both codebases:
  - vmangos's `.partybot load <name>` (`PlayerBots/PlayerBotMgr.cpp`) loads an existing named
    character as a grouped `PartyBotAI`. That character keeps its own level, spells and gear,
    since the "temporary character" setup is skipped for a loaded character. Its progress is
    saved when `PlayerBot.AllowSaving = 1`. The one limit is that the character's account must be
    offline (`sWorld.FindSession(accountId)`), so each party character gets its own local account.
    The launcher owns all of them.
  - cMaNGOS playerbots allows same-account altbots (`.bot add`) and has a richer order set
    (`rtsc`, formations). But its Windows build ships no map extractors. benilla is also built and
    verified against vmangos, whose `file:line` citations run through the code. So the altbot
    check does not favour cMaNGOS. Revisit at M6 if PartyBotAI's orders prove too thin.
- **What is bundled.** Nothing is committed. First-run setup downloads everything into
  `benilla-config/party-server/` (gitignored):
  - MariaDB 11.4.5 (winx64 zip).
  - vmangos's latest Windows build (`dev-windows-amd64-<sha>.zip`, which includes the extractors).
  - Its database snapshot (`db_latest`).
  - Its `mangosd`/`realmd` `.conf.dist` at the build's commit.

  Archives are cached, so a re-run does not download again. Rolling releases are resolved through
  the GitHub API, and what was installed is recorded.
- **Processes.** MariaDB, realmd and mangosd are started hidden (`CREATE_NO_WINDOW`) inside a
  Windows job object that kills them if the game dies. All three bind 127.0.0.1 only, on private
  ports: 13306 (DB), 13724 (realmd) and 18085 (world). mangosd's console is a stdin pipe, used to
  create accounts and to shut down cleanly.
- **First-run setup**, resumable step by step:
  1. Download and unpack.
  2. Extract maps, DBCs and vmaps from `WOW_DATA`. The install is only read, never written.
     mmaps take about 27 minutes on 14 threads, so they do not block the first run. They build
     in the background at below-normal priority, one map at a time, and resume across launches.
     The server enables them (`mmap.enabled`) at the first launch after all are built.
  3. Initialise the database directory.
  4. Create the databases and import the dumps.
  5. Apply any migrations the build needs that the snapshot lacks.
  6. Add the realm row.
  7. Write the configs.
  8. Create the local account (GM level 6, which `.partybot` needs).

  Progress shows in-game on a full-screen overlay.
- **Auto-login.** Once the world port answers, the crate sends benilla-app a login request: the
  one engine change in M0, `benilla_app::AutoLogin { user, pass, character }`.
  - `drive_policy` treats it as the env fast path's silent attempt.
  - The roster policy takes `character` as it takes `WOW_CHAR`. `WOW_CHAR` alone cannot do this,
    because it is read on the first frame, long before the server is up.
  - `WOW_HOST` and `WOW_REALM` pin the private realm, so no login screen or realm list is shown.
  - With a character on the account, the most recently played one enters the world. With none,
    the stock create screen comes up.
- **Shutdown and recovery.**
  - On exit: mangosd gets `saveall` then `server exit` on its console, realmd is stopped, and
    MariaDB gets `mariadb-admin shutdown`. Each step has a timeout, after which the process is
    killed.
  - A run file records the PIDs. On the next launch, leftovers from a crash (a live PID running
    our binary) are stopped before a fresh start. MariaDB recovers its own tables on start.
- **Accept when:**
  - A cold first run reaches the world unattended.
  - A warm run reaches the world.
  - Quitting leaves no server process behind.
  - Killing the game and relaunching recovers.
  - Startup time is measured for both runs.
- **Measured** (2026-10-29, Ryzen with 16 threads, debug build, `logs/startup.txt`):

  | Run | Time | Breakdown |
  |---|---|---|
  | Cold, first launch with nothing downloaded | about 102 s to the character screen | fetch 6.2, maps 10.7, vmaps 69.8, database init 1.4, database start 1.2, import 7.9, migrations 1.3, servers 2.5, account 0.2, then login to the create screen 0.6 s (all in seconds) |
  | Warm | 6.5, 6.7 s to in the world | server ready 4.5 s (database 0.7, servers 2.7), then login, select and Enter World about 2 s |
  | Warm after a crash | 7.0 s | adds the stale-process stop and a 0.2 s table check |

  - Quitting stops all three processes in 1.2–2.3 s, and nothing is left running.
  - A hard kill of the game takes the server down with it through the job object.

**M1 — Proof of concept: switch between two characters**

- **Setup.** Two level-appropriate characters, A and B, standing near each other, on two local
  accounts. `.partybot load` needs the bot's account offline (M0).
- **Client.**
  - E1, the enter-character request: `AutoLogin` already carries a character, so E1 is a logout
    followed by it.
  - A switch key: F12, or a bevy_ui button with the two names.
  - Request the swap, then restore the camera's distance and pitch for the new character.
- **Server.** None needed for the switch itself. Also verify vmangos's bot facilities, recording
  the commands that exist and whether a named character can be loaded as a bot. If one can, keep
  B in the world as a bot while A is played.
- **Accept when:**
  - A→B→A round-trips three times without a crash.
  - Each switch shows the right name, unit frame, action bars and bags.
  - Switch time is measured (trace timestamps, not eyeballed) and reported.
  - Screenshots are taken before and after a switch.

**M2 — Party bots.** The inactive characters stay in the world as grouped server bots and follow
the active one. Switching swaps a bot and the player through a server command. This needs E4, E5
and a server patch. A party bar shows the members with health, and clicking a member switches to
it.

**M3 — Seamless switch.** The server rebinds the session in place. On the same map the client
re-enters without glue or a loading screen, and the world and terrain stay resident. The target is
a switch under 1 s, measured. This needs a client world-entry path that resets per-character
state (the `SelfGuid`, `MoverClaim`, `Player` and UI caches) without a teardown.

**M4 — Pause.**

- Server `.pause on|off`, with the deadline shift on resume.
- Client E2: a P key, a "Paused" overlay, and frozen input.
- **Accept when** probes show, across a 10 s pause:
  - spline positions unchanged;
  - animation times frozen;
  - a cast bar and a cooldown sweep resume from where they stopped;
  - aura remaining times unchanged;
  - no `MSG_MOVE_*` sent.

**M5 — RTS camera in pause.** Port `camera.rs` and the mode blend, and add E3. Pausing blends into
the RTS view, and unpausing blends back to the active character. While paused you can click party
members to select them and double-click to switch.

**M6 — Party orders.**

- td-game's behaviours become server bot orders: Hold here, Guard an area, Escort a member,
  Attack a target, Move to a point.
- The order panel and the anchor and escort picks come from td-game's `hud.rs` and `units.rs`.
- Orders given while paused are queued and run on resume.

**M7 — Polish.**

- Per-character saved camera and keybinds.
- Party member hotkeys.
- What happens to a member that dies while you play someone else.
- Optional "direct steer" by possession (§1.2) for a quick hands-on moment without a full switch.

## 4. Risks and open questions

- **What vmangos bots support** is unverified until M1. The vmangos source is not on this machine
  yet. If named characters can't be loaded as bots, M2 needs a server patch first.
- **Pausing the server** is new. Any system that uses absolute time and that we miss will tick
  through a pause: mail, auctions, battleground queues, instance resets. Scope the pause to map
  updates and list what is excluded.
- **E2 touches hundreds of clock sites.** Do it as a series of small changes, one subsystem at a
  time, each with a numeric probe.
- **Saved variables per character.** A relog-based switch writes them as a real logout would,
  which is correct. A seamless switch (M3) must still flush them.
- **Upstream divergence.** Every E-change makes this branch harder to rebase on `main`. Keep them
  small and isolated.

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
  - Verified in M0: `.partybot load <name>` loads an existing character, on its own account.
- **Switching is an in-world session swap (§2.1).** Every party member is always in the world:
  you play one, and the others are party bots. A server command hands your client connection
  from your character to a bot's character, and gives your old character the bot's AI. Both stay
  in the world. The server then sends the owner-only data of the new character, with no logout,
  no teleport and no loading screen. benilla moves its player identity to the unit it already
  mirrors and rebinds the UI.
  - The relog switch was built and measured, then rejected. It took about 3 s to switch, plus a
    3.1–3.3 s loading screen, so about 6.3 s visible.
  - Breakdown: removing the target's bot 0.8–1.0 s, logout 0.2–0.35 s, login and Enter World
    1.2–1.3 s, the old leader's bots dropping 0.16 s, re-loading the bots 0.4–0.6 s.
  - The loading screen dominated: the world is torn down at logout and streamed again, on the same
    map. Next came the login handshake and the bot round trips.
  - The session swap removes all of these.
- **Pause is server plus client.** A vmangos `.pause` patch (§1.3) is paired with a pausable world
  clock and an input freeze in benilla (E2). The RTS camera runs on real time during the pause.
  Orders issued while paused are queued and run on resume.
- **Client code lives in a new crate, `crates/party`,** launched through `benilla_app::run_with`
  ("Building on top", CONTRIBUTING). Settings live under `benilla_app::config_dir()`.

### 2.1 The session swap

Read from vmangos at the pinned commit (`9b7fb4d`) and benilla-app.

**What a bot is in vmangos.**
- A party bot is a real `Player` with its own `WorldSession`, created by `PlayerBotMgr::AddBot` as
  `new WorldSession(accountId, nullptr, …)`. That session has no socket and a `m_bot` entry, a
  `PlayerBotEntry` holding the `PartyBotAI`.
- It is logged in through the ordinary `WorldSession::LoginPlayer`. Packets sent to it go nowhere.
  `PlayerBotAI::UpdateAI` answers its teleport acks itself.
- The AI is attached by `PlayerBotMgr::OnPlayerInWorld`, which calls `player->SetAI` and
  `ai->SetPlayer`.
- `PartyBotAI` follows `m_leaderGuid`. It removes itself (`requestRemoval`) when it finds no
  leader, or when it has become the group leader itself.

**What belongs to the session, and what to the Player.**

| Session (`WorldSession`) | Player |
|---|---|
| The account: id, security, username, `m_accountData`, tutorials | Descriptor fields, inventory and items |
| The socket, session key and receive queues | Spells and auras |
| Warden | Quests, skills, reputation, honor |
| `m_cheatData` (`MovementAnticheat`) | Group membership |
| `m_clientMoverGuid` | Map and visibility set (`m_visibleGUIDs`) |
| `m_bot` | Packet broadcaster (`m_broadcaster`) |
| `MasterPlayer`: action buttons, friends and ignore lists, mail | |

`Player::SaveToDB` writes `account = GetSession()->GetAccountId()`.

**So the swap moves the connection, not the sessions.**
- Swapping `Player::m_session` between A and B would let the next save move B onto A's account.
  It would also give B A's action bars and friends list.
- Instead, each Player keeps its own session, and the swap moves two things between sessions:
  - the client's **socket**, from A's session to B's;
  - the **bot role** (`m_bot` and the AI), from B's session to A's.
- B's session becomes a client session, with its own account, action bars and social data. A's
  session becomes a socketless bot session.
- vmangos already does the in-world half of this. In `HandlePlayerLogin`, when a character is still
  in the world (a reconnect), it does `SetSession` and `m_broadcaster->ChangeSocket`, then calls
  `SendPacketsAtRelogin` and `Map::ExistingPlayerLogin`. The last one re-sends the self object,
  resets the visibility set and re-sends aura durations. The swap reuses that path minus its
  account check and its `SMSG_LOGIN_VERIFY_WORLD`.

**What breaks on a swap, and the fix for each.**
- **Threading.** `WorldSocket::m_Session` is a raw pointer that the network thread reads in
  `ProcessIncoming`, while sessions update on the world and map threads (`World::UpdateSessions`,
  `PACKET_PROCESS_MAP`).
  - The swap must run at the world thread's single-threaded point, as a queued world task, never
    inside the chat handler.
  - The socket's session pointer must change under a lock.
  - Packets already queued in A's session are dropped. They were the client's input for A.
- **Mover.**
  - `m_clientMoverGuid` decides `IsControlledByOwnClient` and the spline acks
    (`MoveSplineInit` awaits `CMSG_MOVE_SPLINE_DONE` for a client-moved player). It must be
    cleared on A's session, or the server keeps waiting for acks A's client will never send.
  - A's movement flags are cleared, since a held forward key would keep it running.
  - B's mover is set when the client sends `CMSG_SET_ACTIVE_MOVER(B)`.
- **Anticheat.** `MovementAnticheat::InitNewPlayer` is run on both sessions, so B's first client
  movement is not judged against A's history.
- **Bot bookkeeping.**
  - `PlayerBotMgr::m_bots` is keyed by player guid, so B's entry is re-keyed to A: new
    `playerGUID` and `accountId`, `ai->SetPlayer(A)`, `A->SetAI(ai)`, `B->SetAI(nullptr)`.
  - The AI's `m_leaderGuid` becomes B, as does every other party bot's.
  - `PlayerBotMgr::OnBotLogin` bookkeeping must not run twice.
- **Group.** Leadership moves to B (`Group::ChangeLeader`). A party bot that leads the group
  removes itself.
- **Broadcaster.** A's broadcaster loses the socket, and B's is created on it. Otherwise the
  client would keep receiving A's view.
- **Saving.** `PlayerBot.AllowSaving=1` keeps both saving. `m_saveDisabled` is set only for bots
  created without saving.

**The login burst the client needs for B.** Taken from `HandlePlayerLogin` and
`SendInitialPackets*`. Each item is sent to the client's socket, now on B's session.
1. Our own notice, `SMSG_PARTY_SWAP {old, new}`, first, so benilla moves its identity before the
   data arrives. 1.12 has no such opcode, so this is a custom opcode in an unused number, added to
   both the vmangos fork and benilla-protocol.
2. The self object: `SMSG_UPDATE_OBJECT` creates B with `UPDATEFLAG_SELF` and every owner-only
   field, plus B's item objects. This is what `Map::SendInitSelf` builds. Destroys go out for A's
   items, which the client may no longer hold.
3. `SendInitialPacketsBeforeAddToMap`: rest start, bind point, tutorials, initial spells
   (`SMSG_INITIAL_SPELLS`, with cooldowns), action buttons (from B's own `MasterPlayer`), initial
   reputations, honor, time speed.
4. `SendInitialPacketsAfterAddToMap`: zone and world states, aura re-sends, enchantment and item
   durations.
5. From the rest of `HandlePlayerLogin`:
   - `SendPacketsAtRelogin`: proficiencies and spell mods;
   - account data times, and the friends and ignore lists;
   - guild event and corpse reclaim delay;
   - pet spells and mirror timers;
   - loot rolls under way.
6. A's object is re-sent to the client as an ordinary player, and the visibility set is rebuilt
   for B (`ExistingPlayerLogin`). Objects already in view arrive as re-creates of guids the client
   holds, which benilla applies in place (`net/objects.rs` `Landing::Live`).
7. Left out:
   - `SMSG_LOGIN_VERIFY_WORLD`, which is the client's world-entry and loading-screen edge;
   - the MOTD and the cinematic.

**In benilla-app: what has to change to switch the player identity at runtime.**
- **Identity.**
  - `SelfGuid` (used in 170 files) changes from A to B.
  - The `SelfPlayer` tag moves from A's entity to B's. It is only ever attached at spawn
    (`net/objects.rs`, where `self_guid` matches), so a re-create of B would not get it.
  - The name cache entry for "us" changes.
- **Body and movement.**
  - `Embodied` and `ActiveMover` move to B through `maintain_embodiment`, which already keys on
    guid ("the reins change hands by guid").
  - `MoverClaim::claim(B)` sends `CMSG_SET_ACTIVE_MOVER`.
  - The `Player` controller resource is reseated from B's streamed pose, which the possession path
    (`MoverLock.reseat`) already does.
  - Follow, click-to-move and casts are cancelled on A.
  - A goes back to remote replay. Without `ActiveMover`, the server-replay lanes take it.
- **Per-character caches**, rebuilt from B's burst:
  - item, bag and inventory caches (`item_lane`, `item_watch::BagCache`);
  - spellbook and cooldowns, action-bar state, reputations, proficiencies, quest log;
  - talents, skills, honor and pet bar.

  Each is fed by the packets above, but each was written for one character per session. Every one
  needs a reset hook on the swap, the same list a logout teardown clears (`net/session.rs`)
  without despawning the world.
- **The UI.**
  - The stock FrameXML reads `"player"` live, so unit frames, the casting bar and buffs redraw from
    events.
  - Spellbook, action bars, bags, quests, skills, reputation, talents and the character sheet
    redraw on their events (`SPELLS_CHANGED`, `ACTIONBAR_SLOT_CHANGED`, `BAG_UPDATE`,
    `QUEST_LOG_UPDATE`, `SKILL_LINES_CHANGED`, `UPDATE_FACTION`, `CHARACTER_POINTS_CHANGED`,
    `UNIT_INVENTORY_CHANGED`, `PLAYER_ENTERING_WORLD`), which the burst raises through benilla's
    field watch.
  - What does not rebind by events is per-character Lua state: `SavedVariablesPerCharacter`,
    per-character macros, key bindings and chat-frame layout. That state is loaded once per VM.
  - **So the proof of concept rebinds by an in-world UI reload.** benilla's `ReloadUI()` already
    runs the teardown and load edges "without leaving the world" (`ui_script/lifecycle.rs`). The
    in-game UI comes up in 0.5–0.7 s in a debug build. The swap points its per-character identity
    (`record_from_roster`, the realm/character the state files are keyed by) at B first.
  - An event-only rebind with no reload is a later optimisation, once the reload's cost is
    measured.

**Estimated switch time.**
- Server: one world tick, 50–100 ms, for the queued swap and the burst over loopback.
- Client: applying the burst takes tens of ms. With the UI reload, 0.5–0.7 s in debug (less in
  release).
- **Total about 0.6–0.9 s, with no loading screen and nothing re-streamed.** Without the reload,
  about 0.15 s.

**The riskiest unknown** is server-side consistency at the moment of the handover:
- moving a live `WorldSocket` between two `WorldSession`s while the network thread may be inside
  `ProcessIncoming`;
- in-flight state on A that assumes a client: pending spline acks, a cast in progress, the
  anticheat history, an open loot window or trade;
- `PlayerBotEntry` bookkeeping that assumes the AI created its own player.

If any of these crashes the server or desyncs it, the fallback keeps the swap but goes through
vmangos's existing reconnect path. That path is tested for sockets, and the account check would
be relaxed for party accounts. It costs a little more time.

### Engine changes this needs (to be approved before each is made)

The crate reaches benilla-app through `run_with`, `Objects` and `Interact` only, and every other
item in benilla-app is `pub(crate)`. Each item below is a proposal. None is made without your
go-ahead. Upstream policy keeps non-1.12 features out of benilla itself, so on this branch they
are fork changes.

| # | Change | For |
|---|---|---|
| E0 | **Done (M0).** `benilla_app::AutoLogin { user, pass, character }`: a silent login and the character to enter, sent when the crate chooses (`glue_session/login.rs`, `roster.rs`) | M0 |
| E1 | **The identity swap.** `SMSG_PARTY_SWAP {old, new}` in benilla-protocol, and its handling in benilla-app: move `SelfGuid`, `SelfPlayer`, `Embodied`/`ActiveMover` and `MoverClaim` to the new guid; reset the per-character caches without touching the world; reload the UI in place under the new character | M1 |
| E1b | **Done (relog M1, kept).** `benilla_app::SessionRequest::{Command, Target, Relog, CreateCharacter}` (`glue_session/requests.rs`): server commands for the bots, and character creation for party setup | M1 |
| E2 | A pausable world clock: route spline sampling, extrapolation, animation, particles, `GetTime`, aura and cooldown clocks and the sky through it, freeze controller output, and make `frame_pace` aware of it | M4 |
| E3 | A camera-yield hook: an external mode takes the camera transform and input. The app's camera saves its view and stops reading input. | M5 |
| E4 | Public read access to the party roster and member guids (`ui_party`) and to `SelfGuid` | M2, M6 |
| E5 | A hook for chat replies: let the crate see system-message lines so it can read server command replies | M2, M4 |

Server-side work goes in a vmangos fork kept beside this repo:

- The session swap (§2.1): a `.party swap <name>` command queued to the world thread, and
  `SMSG_PARTY_SWAP`.
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

**M1 — Session swap: F6 proof of concept** (§2.1). Done already, from the relog attempt:
- the party setup: three accounts, Partyone the warrior, Partytwo the priest, Partythree the
  mage, all GM 6;
- the party bots loading under whoever is played;
- pinned and checksummed server archives;
- the trial navmeshes.

Steps:

1. **Build vmangos from source at the pinned commit.**
   - Clone `vmangos/core` at `9b7fb4d` into `party-server/src` and build with VS 2022. The
     CMake bundled with VS, or a portable CMake, plus the dependencies vmangos's Windows docs
     name.
   - Check that it reproduces the release binary's behaviour, then switch the launcher's pinned
     build to our own (`crates/party` setup; the archive pin becomes a local build).
   - Risk: the dependency setup. Do this first.
2. **Server: `.party swap <name>`**, admin-only, the target a party bot of ours in our group.
   - The chat handler only validates and queues a world task.
   - The task runs at `World::Update`'s single-threaded point and performs §2.1's handover:
     socket, bot role, mover, anticheat, leader and group lead, broadcaster.
   - Then it sends `SMSG_PARTY_SWAP` and the burst.
   - Log every step with timestamps.
3. **benilla-protocol:** parse `SMSG_PARTY_SWAP`, emitted as `SessionEvent::PartySwap { old, new }`.
4. **benilla-app (E1).** On `PartySwap`:
   - move `SelfGuid` and `SelfPlayer`;
   - reseat `Embodied`/`ActiveMover`/`MoverClaim` (claim the new mover);
   - stop A's local movement and casts;
   - reset the per-character caches without touching the world;
   - then `ReloadUI` in place, with the per-character identity pointed at B.

   The world, terrain and objects stay. The burst's re-creates land in place.
5. **The crate:**
   - all members are loaded as bots at startup (as now);
   - F6 sends `.party swap <next>`;
   - it measures from the key to the server's swap log, to `PartySwap` in the client, to the UI
     being up.

   The relog switch and its checks are removed. `SessionRequest::Relog` stays for party setup.
6. **Checks**, kept from the relog M1:
   - follow and fight by the swapped-away character;
   - its XP, items and position across swaps;
   - for both characters, after a swap: unit frame name, action bars, spellbook, bags, quest log
     and the character sheet;
   - no `MSG_MOVE_*` sent for the old character after the swap (`WOW_MOVE_TRACE`, tags
     `out,snd`);
   - no anticheat or `[CRASH]` lines in `mangosd.out`.

**Accept when:**
- A→B→C→A swaps three full rounds with no loading screen, no world despawn (an entity count
  before and after) and no server error.
- Each swap is under 1 s, measured from F6 to the UI being up.
- Screenshots are taken before and after a swap.

**M2 — Party bar.**
- Party frames with health. Clicking a member swaps to it.
- The swap when a target is out of range or on another map is defined here. The PoC requires the
  same map.

**M3 — Swap without the UI reload.** Rebind the stock UI by events alone, if M1's reload cost is
worth removing. Measure first.

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

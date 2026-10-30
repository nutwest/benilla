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
1. Our own notice, `SMSG_PARTY_SWAP`, first, so benilla moves its identity before the data
   arrives. 1.12 has no such opcode, so it is a custom one, outside the client's table:

   | Field | Type | Meaning |
   |---|---|---|
   | opcode | `uint16` `0x0400` (1024) | The 1.12 table ends at 827 (`NUM_MSG_TYPES` 828). Defined in the fork's `Opcodes.h`. |
   | old | `uint64` guid, unpacked | The character the client played until now. |
   | new | `uint64` guid, unpacked | The character it plays from now on. |

   - The body is 16 bytes, little-endian like every 1.12 body, under the ordinary server header
     (`uint16` size, big-endian, then the opcode).
   - It is sent only to the client that asked for the swap. A stock 1.12 client would not know it.
   - benilla-protocol never fails on an unknown opcode, so a client without M1c support just
     drops it.
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
   - the MOTD and the cinematic;
   - `SMSG_CLIENT_CONTROL_UPDATE`, which a reconnect sends (`UpdateControl`). A login sends none:
     the client claims its own mover. A benilla without M1c took it as a possession grant, claimed
     B and drove a body it had no terrain for. B fell through the world in the first M1b run. On
     `SMSG_PARTY_SWAP` the client claims B itself (`CMSG_SET_ACTIVE_MOVER`).

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
- **What is bundled.** Nothing is committed. Everything lands in `benilla-config/party-server/`
  (gitignored):
  - Downloaded, pinned by SHA-256: MariaDB 11.4.5 (winx64 zip) and vmangos's database snapshot
    (`db-4641790.zip`). Archives are cached, so a re-run does not download again.
  - Our own vmangos build (since M1a), copied from the checkout beside benilla: mangosd, realmd,
    the extractors, the two DLLs the checkout vendors, the `.conf.dist` templates, the navmesh
    inputs, and the SQL migrations. It is re-copied whenever the build changes.
  - What was installed is recorded in `state/installed.txt`.
  - Until M1a, the server was vmangos's prebuilt `dev-windows-amd64-9b7fb4d.zip`.
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

1. **M1a: build vmangos from source.** Done; see the next section.
2. **M1b: server `.party swap <name>`.** Done; see the M1b section below.
3. to 6. **M1c: the client swap, F6 and the checks.** Done; see the M1c section below.

**Accept when:**
- A→B→C→A swaps three full rounds with no loading screen, no world despawn (an entity count
  before and after) and no server error.
- Each swap is under 1 s, measured from F6 to the UI being up.
- Screenshots are taken before and after a swap.

**M1c — The client swap.** Done.

- **benilla-protocol:** `SMSG_PARTY_SWAP` (0x400) parses to `ServerPacket::PartySwap` and
  `SessionEvent::PartySwap { old, new }` (`messages/opcode.rs`, a `Deviation:` row).
- **benilla-app** (`net/party_swap.rs`, a `Deviation:` module). On `PartySwap`:
  - `SelfGuid` becomes B, and the `SelfPlayer` tag moves from A's entity to B's (already
    streamed; a swap to a unit not streamed is refused and logged).
  - The mover is claimed (`MoverClaim::claim`, `CMSG_SET_ACTIVE_MOVER`) and control set as ours.
    The existing embodiment (`player/embody.rs`) then re-seats `Embodied`, `ActiveMover`, the
    camera and the controller by guid, as possession does. A, without `ActiveMover`, goes back to
    the server-replay lanes.
  - B's self create in the burst lands in place and runs our arrival (`net/objects/arrival.rs`):
    the bag cache, reactive spells and the world-enter cascade.
  - Per-character stores reset on the same event, by their own modules, as at a world entry: the
    cast state (`spell/net.rs`), the cooldowns as a world load frees them (the burst's
    `SMSG_INITIAL_SPELLS` appends), and the action-bar slots (`ui_action/net.rs`). The world,
    terrain and objects stay.
  - **Per-character identity:** the roster's pick names B. A row is made for B from its
    descriptor and name when it is on another account, which a party member is. The pick keys
    the saved variables, macros, add-on enable state and the player record.
  - `ReloadUI` runs in place (`ReloadUiPending`); it now logs `ReloadUI done in N ms`. M3 replaced
    this reload with an event rebind.
- **Server, found by M1c:** the swap sends `Group::SendUpdate()` after the burst.
  `SMSG_GROUP_LIST` lists the others, not oneself, and the client still held the old character's
  list. Without it, the party frames showed the new character and not the old one.
- **The crate** (`crates/party`):
  - F6 sends `.party swap <next>` and waits for the client to play it.
  - Each swap's time from the key goes to `logs/swap.txt`.
  - The relog switch and its checks are gone. `.party lead` still runs before `.partybot load`.
- **Checks** (`PARTY_M1C_TEST=1`, `crates/party/src/server/party/m1c_test.rs`):
  - **Swaps:** six out of combat (two rounds of the party) and two mid-fight, each through the
    key's own code path.
  - **After each swap:**
    - the client walks the new character on the movement key (W, as a keyboard message);
    - the server's `.party status` checks who holds the client, the claimed mover, who moved and
      who follows;
    - the streamed unit count is compared before and after.
  - **UI probe** (`WOW_PROBE_LUA_EACH_VM=1`, a dev probe extension that re-runs `WOW_PROBE_LUA` in
    every new VM): every rebuilt UI logs `UnitName`/`UnitClass`, `PlayerName:GetText()`, the
    spellbook, the first action slots and bag 0, takes a screenshot, and casts a class spell
    (mage Frost Armor, priest Lesser Heal).
  - **Run it with:**

    ```bash
    PARTY_M1C_TEST=1 WOW_PROBE_LUA="$(cat m1c_probe.lua)" WOW_PROBE_LUA_AT=8 WOW_PROBE_LUA_AGAIN=1 WOW_PROBE_LUA_EACH_VM=1 WOW_UNATTENDED=1 WOW_NOSOUND=1 BENILLA_HOME="$PWD/benilla-config" WOW_PROBE_EXIT_AT=200 ./target/debug/party.exe
    ```

    `m1c_probe.lua`, the chunk the runs used, trimmed of extra log fields:

    ```lua
    local n = UnitName("player") or "?"
    local _, c = UnitClass("player")
    c = c or "?"
    local nspells = 0
    while GetSpellName(nspells + 1, "spell") do nspells = nspells + 1 end
    local items = 0
    for s = 1, GetContainerNumSlots(0) or 0 do if GetContainerItemLink(0, s) then items = items + 1 end end
    ProbeLog("ui player=" .. n .. " class=" .. c .. " frame=" .. (PlayerName and PlayerName:GetText() or "?")
      .. " spells=" .. nspells .. " spell2=" .. (GetSpellName(2, "spell") or "-")
      .. " action1=" .. (GetActionTexture(1) or "-") .. " action2=" .. (GetActionTexture(2) or "-")
      .. " bag0items=" .. items .. " mana=" .. UnitMana("player"))
    Screenshot()
    local spells = { MAGE = "Frost Armor", PRIEST = "Lesser Heal" }
    local f, t, stage, mana0 = CreateFrame("Frame"), 0, 0, UnitMana("player")
    f:SetScript("OnUpdate", function()
      t = t + arg1
      if stage == 0 and t > 2.5 then
        if spells[c] then
          CastSpellByName(spells[c])
          if SpellIsTargeting() then SpellTargetUnit("player") end
        end
        stage = 1
      elseif stage == 1 and t > 5 then
        local buffs = ""
        for i = 1, 16 do local b = UnitBuff("player", i) if b then buffs = buffs .. b .. " " end end
        ProbeLog("after " .. n .. " mana " .. mana0 .. " -> " .. UnitMana("player") .. " buffs " .. buffs)
        f:SetScript("OnUpdate", nil)
      end
    end)
    ```
- **Measured** (2026-10-29, debug build):
  - **Visible switch time:** key to UI up 699–834 ms over eight swaps, in a run with nothing else
    on the CPU:
    - key to `SMSG_PARTY_SWAP` handled: 143–214 ms. The crate's 10 ms poll, the chat command, one
      server tick and the reply.
    - the in-place `ReloadUI`: 507–568 ms, most of the rest.
    - client playing B by the bridge's next look: 704–854 ms.
  - The final run, with clippy compiling alongside, took 855–956 ms.
- **Checked** (the final run: 42 findings ok, 0 FAIL; earlier runs agree):
  - **No loading screen:** one `loading screen: up` per run, the world entry's, none for 8 swaps.
    No world despawn: the streamed count held, 22–29 units across every swap.
  - **The camera and body:** on the new character in every after-shot.
  - **The client claims the new mover:** `.party status` reads `mover=<B's guid>` on B's session
    after every swap.
  - **Moving:** the new character moved 2.7–14.5 yd under the client's W key.
  - **Casting:**
    - the mage's Frost Armor landed on all three of its swaps (mana 165 → 105, the buff present);
    - the priest's Lesser Heal spent mana each time.
  - **The old character walks off as a bot:**
    - `session=bot ai=PartyBotAI leader=<B>`, no client mover, no pending movement acks;
    - it followed B 4.8–16.4 yd.
  - **UI per character** (probe, after every swap):
    - the player frame's name, class, spellbook, action bar (warrior empty; priest Smite and
      Lesser Heal; mage Fireball and Frost Armor) and bag 0 are the new character's;
    - both other members are in the party frames (after the `SendUpdate` fix).
  - **Mid-combat:** in both fights the swapped-away character fought on as a bot and the creature
    died, at 38 s and 15 s.
  - **Tests:** benilla-app's tests for the touched modules pass (net, spell, ui_action, probes,
    ui_script lifecycle, player: 1,291), including three new swap-handler tests and a parse test
    in benilla-protocol.
  - **Lint:** `cargo fmt --check` and clippy `-D warnings` on benilla-app, benilla-protocol and
    party are clean.
- **Known, not fixed:**
  - **Per-character files and the account:** saved variables and macros sit under the account
    the launch logged in with. The same character's files therefore differ by which member the
    game was launched as. A fix logs in with one fixed account and swaps to the last played
    member.
  - **The reload is the cost:** about 0.5 s of the 0.7–0.8 s. M3 (an event-only rebind) is where
    that goes.
  - **Lua movement verbs:** in these unattended runs `MoveForwardStart()` from the probe's Lua
    sent no movement, before any swap too. The checks use the key. Not investigated.
  - **Unrelated test failure:** benilla-protocol's `auth::tests::the_challenge_carries_the_time_zone_bias`
    fails on this machine without any M1c change. Windows does not re-read a changed `TZ` within
    one process.

**M1b — The server swap.** Done, in the fork's branch `party` (`src/game/PlayerBots/PartySwap.cpp`
and the pieces listed below).

- **Commands** (`SEC_ADMINISTRATOR`, in-game only):
  - `.party swap <name>`. The client's character must be played by its client and not be logging
    in or out. The target must be:
    - one of its party bots, online, initialized and following it;
    - in its group and on its map;
    - not teleporting or flying.

    The handler only checks and queues the swap on `World::GetMessager()`. That runs in
    `World::Update` before the sessions update, while no map and no async packet is processed.
    The world task checks again and swaps.
  - `.party status`: one line per group member, in chat and in `Server.log`, as
    `[PartyStatus] <name> guid= account= session=client|bot mover= ai= leader= dist=
    group_leader= hp= combat= victim= victim_hp= pending_moves= spline_ack= pos=`.
  - `.party lead`: makes the client's character the group leader.
    - Why it's needed: a swap moves the lead, and the group keeps it across a restart. A party bot
      loaded into a group it leads removes itself, which is how Partyone left the group in the
      third M1b run.
    - The launcher sends it after entering the world, before `.partybot load`.
- **The swap, in order** (`PlayerBotMgr::SwapPartyControl`):
  1. Releases A's loot and cancels its trade, while A's client still hears it.
  2. `WorldSession::TakeOverClient`, under the socket's new session lock:
     - **To B's session:** the socket, address, latency, build, OS, platform, locale, session key
       and sniff file. B's session takes B's account name, global account data and tutorials from
       the database, since a bot session has none.
     - **To A's session:** `m_bot`.
     - **Cleared:** both receive queues and both client movers.
     - **Warden:** moved when it is on (it is off here).
  3. Both sessions' anticheat state is reset (`InitCheatData`). A's broadcaster loses the socket and
     B's gets it.
  4. A's pending movement changes are applied (`ResolvePendingMovementChanges`), its spline-done
     flag cleared, and it is stopped.
  5. The bot entry is re-keyed to A (guid, account, name). A gets a new `PartyBotAI`:
     - leader B, the old AI's CC and focus marks;
     - `m_initialized` set, so it does not teleport or refill A;
     - role, then spell data for A's class;
     - GM mode off, as a bot loaded from the database gets.

     The old AI is destroyed, which detaches it from B.
  6. B's MotionMaster (the AI's follow or chase) is cleared and B stopped. Its spline-done flag is
     cleared, so the client's first moves are not held for an ack.
  7. Every party bot following A follows B, and B takes the group's lead (`Group::ChangeLeader`).
  8. `SMSG_PARTY_SWAP`, destroys for A's item objects, then the burst (§2.1).
  9. One log line: who has the client, who runs the AI, the wait in the queue and the swap's time.
- **Socket safety** (`WorldSocket`): `m_Session` is guarded by a mutex wherever the network thread
  routes a packet to it or sets its latency, and wherever the world thread finalizes or changes
  it.
  - `ChangeSession` moves the sessions' state under that lock, so no packet reaches A's queue
    after the swap clears it.
  - Until the client claims B, its movement is dropped quietly: `GetConfirmedMover()` is null
    with no client mover, and 1.12 movement carries no guid.
- **Checked** (2026-10-29, unattended, `PARTY_SWAP_TEST=1`, `crates/party/src/server/party/swap_test.rs`):
  - **Method:**
    - benilla does not understand the swap yet, so every check reads the server:
      `[PartySwap]`/`[PartyStatus]` lines and the characters database.
    - 12 swaps per run: a follow, 5 idle swaps (two rounds), and two fights with two swaps each.
      Per fight, the first swap takes the client off a character not in combat. The second takes
      it off a character fighting at that moment.
  - **Result:** 43 findings ok, 0 FAIL, no server error (`Server.log`, `mangosd.out`,
    `Anticheat.log`, `gm_critical.log`). The 12 swaps are repeated across the last five runs.
  - **After every swap:**
    - the client is on the new character's session (`session=client`), which leads the group;
    - every other member runs PartyBotAI under it;
    - the old character has no client mover and no pending movement change, so nothing waits
      on its acks.
  - **Follow:** the played character jumped 25 yd, then was swapped away. As a bot it came back
    to the new leader: 15.6 → 5.2 yd in 6 s, and 23.3 → 2.4 yd in another run.
  - **Fights:**
    - Kobold Vermin with 200 HP, `.partybot attackstart`.
    - The swapped-away characters fought on as bots, polled about every 0.6 s:
      - the damage dealers took it as their victim 0.2–2.0 s after their swap;
      - the priest (a healer, no victim) was in combat, healing, about 4 s after.
    - The creature died at 25.7 s and 29.3 s.
    - In one earlier run, the client's own warrior stood unplayed and never turned to the
      creature, and the fight outlasted the check. So a fight passes on the creature's health
      falling after the second swap, as well as on its death.
  - **Kept:** XP (174 → 213/214 from the two kills), items, level and each character's account
    (`characters.account` unchanged).
  - **Time:**
    - The server's swap takes 0–1 ms.
    - The command waits about 50 ms in the queue (one world tick).
    - From the client sending the command to the `done` line: 152–176 ms. That includes the
      frame that sends it and the test's 20 ms log polling.
  - **Not checked here (M1c):**
    - what the client shows;
    - the client claiming B;
    - the client's movement after a swap;
    - the UI.

**M1a — Our own vmangos build.** Done.

- **Source.** The fork [nutwest/core](https://github.com/nutwest/core), branch `party`, made from
  `9b7fb4dfdb1d863ef93c5cf24d57aff92124f41f` (2026-10-28, "Replace new with make_shared in auth
  socket").
  - That is exactly the commit of the prebuilt `dev-windows-amd64-9b7fb4d.zip` used through M1;
    mangosd reports `Core revision: 9b7fb4dfdb1d863ef93c`.
  - The swap work (M1 steps 2–6) lands on that branch.
- **Prerequisites** (Windows 10 or 11, x64):
  - **Visual Studio 2022** (Community is enough) with the "Desktop development with C++"
    workload: MSVC v143 and a Windows 10/11 SDK. Built with MSVC 14.44.35207 and SDK
    10.0.26100.0.
  - **CMake 3.31.6, portable:**
    - Download `cmake-3.31.6-windows-x86_64.zip` from
      `https://github.com/Kitware/CMake/releases/tag/v3.31.6`, SHA-256
      `d163cd3ab4959b0a53fa8988f2ddbd2e6c501658201e6a154386bad9dbe4f836`.
    - Unzip it anywhere; it needs no install or admin rights.
    - VS's own CMake component works too.
    - Avoid CMake 4.x: it drops compatibility with the old `cmake_minimum_required` lines in the
      vendored libraries.
  - **Git.**
  - **Nothing else.** The MySQL client, OpenSSL and zlib ship in the repo under `dep/windows`,
    cpptrace and the other libraries build from the repo, and the build fetches nothing.
- **Steps**, from the folder that holds the benilla checkout (here `F:\Game Prototype`), in Git
  Bash:

  ```bash
  git clone -b party https://github.com/nutwest/core vmangos
  cd vmangos && mkdir build && cd build
  <cmake>/bin/cmake.exe .. -DBUILD_EXTRACTORS=1 -DBUILD_WARNINGS_AS_ERROR=1 -G "Visual Studio 17 2022" -A x64
  "/c/Program Files/Microsoft Visual Studio/2022/Community/MSBuild/Current/Bin/MSBuild.exe" MaNGOS.sln //p:Platform=x64 //p:Configuration=Release //m
  ```

  - These are the flags of vmangos's own release workflow
    (`.github/workflows/development-release.yaml`, `build-windows`).
  - The output is `vmangos/bin/Release/`: `mangosd.exe`, `realmd.exe`, `MapExtractor.exe`,
    `VMapExtractor.exe`, `VMapAssembler.exe` and `MoveMapGenerator.exe`.
  - A full build takes about 75 s on 16 threads. The only warnings are three D9025 flag overrides
    in the vendored libmpq and g3dlite.
  - Rebuilding after a change is the same MSBuild line.
- **How the game uses it** (`crates/party`, `setup::server_build`):
  - **Where the build is found:** `PARTY_VMANGOS`, else the `vmangos` folder beside the benilla
    checkout.
  - **What is copied, on every launch whose build changed** (size and mtime of the files), into
    `party-server/vmangos/`:
    - everything in `bin/Release`;
    - `dep/windows/lib/x64_release/libmySQL.dll` and `libeay32.dll` (the release archive adds the
      same two; CMake copies them only on `install`);
    - the `.conf.dist.in` templates;
    - `contrib/mmap/offmesh.txt` and `config.json`.

    A rebuilt mangosd is therefore picked up by the next launch.
  - **Migrations** come from the checkout's `sql/migrations`. The setup no longer reads GitHub's
    API or raw files.
  - **Unchanged:** the database snapshot and MariaDB downloads, and the config overrides.
  - **No build found:** the launcher stops with an error naming this section.
- **Checked** (2026-10-29, debug client, unattended runs over the M1 install):
  - The first launch copied the build. The client logged straight into the world as Partythree,
    and both party bots loaded.
  - The server logs match the prebuilt's:
    - `Server.log`'s one ERROR (`Gameobject 148500`) comes from the database snapshot and was
      there before.
    - `Anticheat.log` holds only the startup lines.
    - `gm_critical.log` and `Scripts.log` are empty.
  - Shutdown is clean (mangosd 0.7 s, total 1.0 s).
  - Warm start, launch to in-world:

    | Build | Time | Breakdown |
    |---|---|---|
    | Prebuilt | 6.0–6.9 s | servers 2.4–2.7 s |
    | Ours, warm | 6.1 s, 6.1 s | database 1.0, servers 2.4 |
    | Ours, first launch after the copy | 15.4 s | servers 5.0 s, then a 7 s loading screen |

    The first launch after the copy was slow, as the prebuilt's first launch was (15.9 s, servers
    5.0 s). A freshly written exe being scanned is the likely cause; not investigated.
  - **Not run:** a cold first-run setup with this build's extractors on a fresh state folder.

**M2 — Party bar.**
- Party frames with health. Clicking a member swaps to it.
- The swap when a target is out of range or on another map is defined here. The PoC requires the
  same map.

**M3 — The instant swap.** Done. Goals: no UI reload, no flicker, no frame spikes, and under
150 ms from F6 to a controllable character with the right UI.

- **Measured first** (debug build, six swaps; `WOW_FRAME_PHASES=25`, `WOW_STALL=0`,
  `WOW_FPS_JOURNAL`, and the new `WOW_LUA_SPIKES`).
  - **Normal play:** 16.7 ms per frame (vsync), about 5 ms of main-thread work.
  - **Every swap, two spikes:**
    - The swap-packet frame, 46–57 ms. Of it, `Update/Input` was 34–41 ms, about 20 ms of it Lua
      (stock `ActionButton.lua`, `ItemButtonTemplate.lua` and `PaperDollFrame.lua` reacting to the
      burst).
    - The next frame, 500–650 ms. The in-place `ReloadUI` was 445–545 ms of it, the new UI's first
      update 35–78 ms more.
  - **Key to UI up:** 699–834 ms.
- **What changed:**
  1. **Event rebind instead of `ReloadUI`.**
     - The reload is gone. By the reference's rule (`0x465d79`), a create for an object already
       shown runs its all-fields watch, not its create hook. So the swap marks the new
       character's in-place self create as our arrival
       (`PartyArrivalOwed`, `net/objects/arrival.rs`). That gives the arrival's full path: reactive
       spells, the bag cache, the mover claim, and the world-enter cascade with
       `PLAYER_ENTERING_WORLD`.
     - A rebind leg follows (`ui_world_enter::party_rebind`). It seats the player record that
       `UnitName`/`UnitClass("player")` answer, from a roster row the swap makes for the new
       character from its descriptor (`ui_world_enter::PartyRow`). The roster itself stays as the
       login set it.
     - It then fires only what `PLAYER_ENTERING_WORLD` leaves, taken from the stock FrameXML's
       registrations:
       - `"player"` unit events: `UNIT_NAME_UPDATE`, `UNIT_PORTRAIT_UPDATE`, `UNIT_FACTION`,
         `UNIT_AURA`, `UNIT_INVENTORY_CHANGED`, `UNIT_PET`;
       - player events: `PLAYER_AURAS_CHANGED`, resting, money, guild;
       - the spell and bag cooldowns, the pet bar, `SPELLCAST_STOP`, and the combat state.
     - On `PLAYER_ENTERING_WORLD` the stock frames already redraw the player frame
       (`UnitFrame_Update`), every action button, the paper doll's model, experience, honor and
       the shapeshift bar.
     - The swap's cascade keeps the target. A login clears it (`SelectTarget(0)`); mid-fight that
       re-signalled every target reader and cost one swap's frame 237 ms.
  2. **One UI configuration for the party.**
     - `benilla_app::share_character_state("Party")` (`local_state`) files every character's state
       under one key: saved variables, macros, bindings, camera, chat, layout and add-on state.
       Action bars stay per character, server-side.
     - The fork's swap no longer sends `SMSG_ACCOUNT_DATA_MD5`, so a swap fetches no character's
       stored chat or macros. Applying the stored chat setup had cost 8 ms per swap.
  3. **Preload by a warm-up.**
     - The client learns a member's spells, bar, bags and model only from that member's own burst.
       So at startup, behind the loading cover, the launcher swaps once through the party and back.
     - That loads every member's icons, item records, textures and portrait, and swaps after it
       load nothing new. Before, a member's first swap cost more (a 9.8 ms rebind against
       2.6 ms after).
     - It adds 0.5–4 s to startup: retries wait while freshly loaded party bots initialize.
  4. **The work spread over two frames.**
     - A one-drain hold (`lanes::LaneHold`, set when the party arrival is taken) stops the lane
       drain at the arrival.
     - The burst and its item records fire in frame N; the cascade and the rebind fire in N+1, in
       order.
  5. **Camera glide.** For 0.25 s after a swap, the camera the controller places behind the new
     body is offset back toward the old one by the bodies' gap, eased with smoothstep
     (`player/swap_blend.rs`, after `control`).
  6. **Latency:**
     - **F6 path:** the swap goes out on F6's own frame, from the bridge, in party chat
       (`SessionRequest::PartyCommand`). vmangos takes party chat on its async packet thread every
       20 ms, where a say waits for the 100 ms map update.
     - **World tick:** the fork's world loop tick is a config key, `World.TickMs` (`WorldRunnable`,
       default 50 ms). The embedded server runs it at 10 ms, so the queued swap runs within 10 ms.
- **After** (debug build, same trace, after the warm-up):

  | | Before | After |
  |---|---|---|
  | Key to controllable (client plays the new character) | 703–856 ms | 34–53 ms |
  | Key to UI rebound | 699–834 ms | 87–110 ms |
  | Slow frames per swap | 46–57 ms, then 500–650 ms | two frames of 31–42 ms |
  | Mid-fight swap | as above | two frames of 33–37 ms |
  | Key to UI rebound, `play` build | — | 62–114 ms |
  | Slow frames per swap, `play` build | — | two or three frames of 21–42 ms |

  - The two remaining frames are the stock UI's own work: about 12 ms of `PLAYER_ENTERING_WORLD`,
    11–20 ms of item set-up, and 12–15 ms of Lua.
  - The debug build compiles our crates, the Lua VM among them, at opt-level 1. The `play` rows are
    the release profile (`cargo build --profile play`), the one a player runs.
  - Every fired event carries the reference's arguments (`ui_script::event_shape_gate`). An early
    rebind fired `CHARACTER_POINTS_CHANGED` with none, and `ChatFrame.lua` raised an error dialog on
    it; it is left out.
- **Checked:**
  - **The UI per character, by probe after every swap:** name, class, the player frame's text, the
    spellbook, the action bar and bag 0. Casts work: the mage's Frost Armor lands, the priest's
    Lesser Heal spends mana.
  - **The M1c checks:** the mover claimed, the walk and the old character following, 30 of 30
    in the final `play` run, with no UI runtime error and one loading screen (the login)
    across it.
  - **Screenshots 0.15 s after each swap:** the camera mid-glide, every bar and frame drawn, no
    error dialog.
  - **Tests:** benilla-app's touched modules pass, 3,228, among them the swap handler's, the
    glide's, the event-shape gate and the lane guards. fmt and clippy are clean.
  - **Not fixed:** the M1c fight check is flaky. The client's `.modify hp` on the summoned
    creature does not hold, so a fight can end before the check looks. That fails the check
    without showing a swap fault.
- **New instruments:**
  - `WOW_LUA_SPIKES=<ms>` (`perf/lua_spikes.rs`): the chunks a slow frame's Lua ran.
  - `WOW_PROBE_LUA_EACH_VM=1`: re-runs the Lua probe on each change of character too.
  - `ui: party rebind fired` and the `pty` trace tag time the rebind.

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

# RFC 022: Peer Handle API

**Status:** In progress (Phases A–C + PeerLike TS surface landed on mainline; release still unreleased as 0.6.0)  
**Author:** James Yong + Claude  
**Date:** 2026-07-09 · **Revision:** 2 (same day, post design review — see §19)  
**Depends on:** RFC 012 (layered architecture), RFC 015 (bindings rewrite), RFC 017 (identity & namespacing), RFC 021 (raw transport JS API)  
**Supersedes (partial):** RFC 017 §7.3–§7.4 consumer guidance and §15 as-built deviation (identity flip / dual “best practices”)  
**Target release:** 0.6.0 (breaking)

### Implementation status (2026-07-09)

| Phase | Status |
|-------|--------|
| A — Registry + honest projection + §7.7 lifecycle + attribution | **Done** |
| B — `Node::peer`, PeerLike send/ping, createMeshNode Peer registry (`===`) | **Done** |
| C — Eager identity (default on, concurrency cap, one-shot) | **Done** |
| PeerLike on net/quic/dgram/ws | **Done** |
| D — Hostinfo advertisement | Not started |
| E — Full docs site rewrite | **Done** — `docs/index.html` + `docs/api.html` Peer-first rewrite; all 8 examples migrated |
| Native NAPI `Reference<Peer>` table | Deferred; `===` via packages/core `PeerRegistry` |

---

## 1. Problem Statement

Truffle’s product goal is a **nice interface for networking on top of Tailscale**: discover peers, send messages, open sockets, share state — without the caller learning tunnels, mTLS, DERP, or Tailscale control-plane details.

RFC 017 correctly introduced two real identity layers:

| Concept | Role | Stability |
|---------|------|-----------|
| **Device id (ULID)** | Durable app-level identity | Across restarts, including `ephemeral: true` |
| **Tailscale stable id** | L3 routing / session map key | Per Tailscale registration |

The **as-built public API** (v0.5.x) collapses those into a single string field and forces consumers to understand the collapse:

```rust
// crates/truffle-core/src/node.rs — impl From<PeerState> for Peer
None => (s.id.clone(), s.name.clone(), …)  // s.id == Tailscale stable id
// …and the legacy `Peer.id` field applies the same ULID-else-Tailscale fallback
```

So `NapiPeer.deviceId` means:

```text
deviceId := ULID if hello seen else tailscaleId
```

### 1.1 Concrete failures

1. **Identity flip.** `deviceId` changes once per session when the envelope-bus WebSocket connects (documented in RFC 017 §15). Maps keyed by `deviceId` grow **two entries for one peer**.
2. **Two best practices.** Envelope-bus apps are told to key on `deviceId` (once `wsConnected`); raw-transport apps (RFC 021) are told to key on `tailscaleId`. That is the opposite of “one mesh library.”
3. **Disconnect events lie.** `ws_disconnected` / `left` often carry the Tailscale id after hello state is gone; app maps keyed by ULID never clean up.
4. **API surface teaches ids, not networking.** Every example is `send(peers[0].deviceId, …)` or `openTcp(host, port)` where `host` is “maybe a name, maybe a ULID, maybe a Tailscale id.” Callers must learn *which* string is legal *when*.

### 1.2 Root cause (product, not transport)

The internal model is already sound:

```text
PeerState.id          = Tailscale stable id   (routing key)
PeerState.identity    = Option<PeerIdentity>  (ULID + display name after hello)
```

The failure is **projection + parameter design**:

- Projection **lies** by stuffing the routing key into `deviceId`.
- Parameters are **strings** that force the caller to re-derive what the registry already knows.

Teaching honest dual fields (`deviceId | null` + `peerKey`) would fix the lie but still force users to **choose which id to pass**. That is still library-shaped, not product-shaped.

### 1.3 Desired product contract

> **Truffle gives you Peer objects. Networking takes Peers. Identity resolution is not your problem.**

Ids remain available as **read-only fields** for display, persistence, and debugging. They are not the currency of the networking API.

---

## 2. Goals

1. **Single user-facing peer handle.** Discovery, events, and inbound messages yield a `Peer`. All networking APIs accept a `Peer` in the happy path.
2. **Zero id-picking for networking.** Callers do not decide between ULID, Tailscale id, hostname, or IP when sending or dialing.
3. **Honest fields.** `deviceId` is either a real ULID or `null` — never a Tailscale id in disguise. It changes only on an *announced* identity rotation (§7.7), never by silent fallback.
4. **Same-instance handles.** For a given live peer, `getPeers()`, peer-change events, and `message.from` return the **same** `Peer` object so `Map<Peer, T>` works.
5. **Complexity stays inside truffle.** Dual-index, routing keys, eager identity, hello correlation — all internal to `PeerRegistry`.
6. **String overloads remain as *queries*, not as “the id API”.** Names and saved ULIDs are for lookup (`mesh.peer('Bob')`) and persistence, not for teaching a multi-id protocol.
7. **One story for bus and raw transports.** Envelope `send` and RFC 021 TCP/QUIC/UDP share the same `Peer` type.
8. **Clear restart story.** Process-local handles die with the process; durable re-binding uses `deviceId` only when non-null, via `mesh.peer(deviceId)`.

---

## 3. Non-Goals

- **Federated / cryptographic identity.** Unchanged from RFC 017: ULID is opaque; apps may supply `deviceId` at construction.
- **Removing Tailscale ids from the system.** They remain the L3/session routing key; they simply stop being the primary *user* parameter.
- **Guaranteeing `deviceId` is non-null at first sight of every peer** in v1 of this RFC. Eager identity (Phase C) makes that true in the common case; hostinfo advertisement may follow. Until then, `null` is honest.
- **Persisting `Peer` handles across process restarts.** Handles are process-local. Cross-restart identity is `deviceId` + `mesh.peer(…)`.
- **Changing the hello wire format** (RFC 017 §8) except where needed to attach identity earlier (eager dial reuses the existing hello).
- **Breaking node-stdlib mimicry for raw transports.** `net.connect(port, 'alice')` string hosts stay; they resolve internally to the same registry entry a `Peer` would use.
- **Multi-app identity federation** or global directory services.

---

## 4. Design Principles

1. **Handle-first.** The unit of work is `Peer`, not `string`.
2. **Never lie in a field.** A field’s meaning does not change over the peer’s lifetime.
3. **Two ids, two jobs (internal).** Routing key ≠ durable app identity. Callers need neither job for day-to-day networking.
4. **Same object, live fields.** Handles update in place as discovery, hello, and connectivity change.
5. **Strings are queries.** `mesh.peer(query)` and string overloads mean “resolve this human/debug input,” not “pass the correct id type.”
6. **Advanced fields stay advanced.** `deviceId` / `tailscaleId` live under display, persistence, and diagnostics — not Getting Started.

---

## 5. User-Facing Mental Model

```text
  mesh.getPeers()   ──►  Peer[]
  mesh.peer(query)  ──►  Peer | null
  event.peer        ──►  Peer
  message.from      ──►  Peer

  mesh.send(peer, namespace, data)
  mesh.ping(peer)
  mesh.net.connect({ peer, port })
  mesh.quic.connect(peer, port)
  peer.send(namespace, data)          // optional sugar
```

**First tutorial (no ids):**

```ts
const mesh = await createMeshNode({ appId: 'chat', deviceName: 'laptop' });

mesh.onMessage('chat', async (msg) => {
  console.log(msg.from.displayName, msg.payload);
  await msg.from.send('chat', Buffer.from('ack'));
});

const peers = await mesh.getPeers();
if (peers[0]) {
  await peers[0].send('chat', Buffer.from('hello'));
}
```

**Persistence (only place `deviceId` appears in app code):**

```ts
// Save across restarts (only when known)
if (peer.deviceId) {
  localStorage.setItem('lastPeer', peer.deviceId);
}

// Next launch. Identity arrives via hello, so right after start the saved
// ULID may not resolve yet even though the peer is online — `waitMs` blocks
// until identity is learned or the timeout expires (then resolves null).
const id = localStorage.getItem('lastPeer');
const peer = id ? await mesh.peer(id, { waitMs: 5000 }) : null;
if (peer) await mesh.send(peer, 'chat', Buffer.from('welcome back'));
```

**Collections:**

```ts
// Preferred: Map by handle (same instance from events + getPeers —
// true `===` in the Node bindings, guaranteed; §9.1)
const sessions = new Map<Peer, AppSession>();
sessions.set(ev.peer, session);

// If a string key is required in-process (logging, IPC): peer.ref
// If durable across restarts: peer.deviceId when non-null
```

### 5.1 What users do *not* need to learn

- Which string `send` wants before vs after WebSocket hello  
- That raw-transport apps “should key on tailscaleId”  
- Dual-key maps to survive disconnect  
- That `deviceId === tailscaleId` is a temporary fallback  

---

## 6. Public API

### 6.1 `Peer` handle

`Peer` is a **live handle** owned by the node’s peer registry — not a one-shot plain object snapshot (though bindings may project a snapshot *view* that still carries a stable `ref` and identity equality).

```ts
/**
 * Opaque process-local peer token: a branded string encoding the registry
 * entry's (tailscaleId, generation) — see §7.7. JSON-serializable so it can
 * cross IPC (Tauri commands, workers). A peer that leaves and rejoins gets a
 * NEW ref. Never persist across restarts; that job belongs to `deviceId`.
 */
type PeerRef = string & { readonly __truffleRef: unique symbol };

/**
 * User-facing peer handle. Opaque for networking.
 * Properties are for UI / persistence / debug — not required for send/dial.
 */
interface Peer {
  // ── Display (always safe) ──────────────────────────────────────────
  /** Best label for UI: identity name → hostname slug (app prefix stripped) → short ref. */
  readonly displayName: string;
  readonly online: boolean;
  /**
   * Envelope-bus WebSocket up. Advanced: most apps can ignore this — never
   * gate `send` on it. Deliberately named `wsConnected`, not `connected`,
   * so it cannot be misread as peer liveness (that is `online`); see §16.3.
   */
  readonly wsConnected: boolean;
  readonly ip: string | null;
  readonly os: string | null;
  /** L3 hostname (`truffle-{appId}-{slug}`). Debug / DNS-ish. */
  readonly hostname: string;

  // ── Durable identity (honest Option) ───────────────────────────────
  /**
   * Stable ULID once known; null until identity is learned.
   * Never equals tailscaleId; never silently falls back to one.
   * After first set it changes only on explicit identity rotation — the peer
   * wiped its state and re-helloed with a new ULID — which re-emits the
   * `identity` event (§6.4, §7.7). Never silently.
   *
   * Use ONLY for:
   *   - persisting app data across process restarts
   *   - settings / audit UI
   * Not required for mesh.send / connect.
   */
  readonly deviceId: string | null;

  // ── Advanced / diagnostics ─────────────────────────────────────────
  /**
   * Tailscale stable node id — internal routing key.
   * Documented as advanced; not used in happy-path tutorials.
   */
  readonly tailscaleId: string;

  /**
   * Process-local token for Map/Set keys when object identity is
   * inconvenient (IPC, workers) and for log lines. Stable for this peer
   * entry's generation; do not persist across restarts.
   */
  readonly ref: PeerRef;

  /**
   * True iff both handles refer to the same registry entry *generation*
   * (§7.7). A peer that left and rejoined — even with the same tailscaleId —
   * compares unequal. Compare `deviceId` to ask "same durable device?".
   */
  equals(other: Peer): boolean;

  // ── Instance sugar (bindings-level; the Rust core keeps methods on Node, §9.2)
  send(namespace: string, data: Buffer | Uint8Array): Promise<void>;
  ping(): Promise<PingResult>;
}
```

**Invariants:**

| Invariant | Rule |
|-----------|------|
| I1 | `deviceId` is `null` or a valid ULID — never a Tailscale stable id |
| I2 | `tailscaleId` is always non-empty for a live peer entry |
| I3 | `ref` is stable for the lifetime of the registry entry (one generation) |
| I4 | `equals` is true iff same registry entry **generation** — a left-and-rejoined peer is a new entry and compares unequal, even with the same `tailscaleId` (§7.7) |
| I5 | After `left`, the handle may remain but `online === false`; networking rejects with `PeerGone` |
| I6 | After `mesh.stop()`, property reads stay safe (last-known values); methods reject with `Stopped` |
| I7 | `by_device` resolves each ULID to at most one **live** entry; duplicate claims follow §7.7 (first-wins while both online, repoint once the holder is gone) |

### 6.2 Discovery

```ts
interface MeshNode {
  /** Snapshot of live peers (same handle instances as events). */
  getPeers(): Promise<Peer[]>;

  /**
   * Resolve a query to a Peer handle.
   * Accepts: display/device name, ULID, ULID prefix (≥4 unique chars),
   * hostname slug, Tailscale id, tailnet IP.
   *
   * Not found → null. Ambiguous (device names are not unique; short
   * prefixes) → throws AmbiguousPeerError listing the candidates.
   * `waitMs` blocks until the query becomes resolvable — e.g. a saved ULID
   * whose owner has not helloed yet (§8) — and resolves null on timeout.
   */
  peer(query: string, opts?: { waitMs?: number }): Promise<Peer | null>;
}
```

`resolvePeerId(query): Promise<string>` is **deprecated**. New code uses `peer(query)` or handle parameters. If kept for one release, it is documented as “legacy string handle for dial” — not as the primary API.

### 6.3 Networking (handle-first)

```ts
type PeerLike = Peer | string; // string = query, resolved internally

interface MeshNode {
  send(to: PeerLike, namespace: string, data: Buffer | Uint8Array): Promise<void>;
  ping(to: PeerLike): Promise<PingResult>;

  openTcp(to: PeerLike, port: number): Promise<TcpSocket>;
  connectQuic(to: PeerLike, port: number): Promise<QuicConnection>;
}

// RFC 021 namespaces
mesh.net.connect({ peer: bob, port: 8080 });
mesh.net.connect({ host: 'Bob', port: 8080 }); // query form; node-like
mesh.quic.connect(bob, 9000);
sock.send(datagram, 9999, bob);   // mesh.dgram socket (RFC 021) — target is PeerLike too
```

`PeerLike` covers **every** peer-addressed surface, not only the four above:
`fileTransfer.sendFile(to, path)` / `offerChannel(to, …)` (RFC 014), UDP
datagram addressing, and the HTTP proxy target. Phase B sweeps every `&str`
peer parameter in core — `send`, `send_typed`, `ping`, `open_tcp`,
`connect_quic`, `resolve_peer_ip`, file transfer, proxy — so no surface is
left teaching the old string contract.

Resolution:

```ts
async function asPeer(node: MeshNode, x: PeerLike): Promise<Peer> {
  if (isPeerHandle(x)) {
    if (!x.online && /* not yet join-complete */) { /* allow send to try reconnect paths as today */ }
    return x;
  }
  const p = await node.peer(x);
  if (!p) throw new PeerNotFoundError(x);
  return p;
}
```

### 6.4 Events

```ts
type PeerEvent =
  | { type: 'joined'; peer: Peer }
  | { type: 'updated'; peer: Peer }
  | { type: 'identity'; peer: Peer }         // deviceId set (or rotated, §7.7)
  | { type: 'ws_connected'; peer: Peer }     // envelope bus up (advanced)
  | { type: 'ws_disconnected'; peer: Peer }  // envelope bus down (advanced)
  | { type: 'left'; peer: Peer }
  | { type: 'auth'; url: string };

mesh.onPeerChange((ev: PeerEvent) => void): void;
```

**Rules:**

- Peer-related events **always** include `peer: Peer` (the same instance as in `getPeers()`).
- There is **no** overloaded `peerId` string that sometimes means ULID and sometimes Tailscale id.
- Cleanup uses `ev.peer` / `ev.peer.ref`, not a guessed string form.
- `identity` fires when `deviceId` transitions `null → Some(ulid)` — and again on the rare explicit rotation `ULID → new ULID` (§7.7). Handlers must tolerate more than one firing per entry.
- **Events are edges; fields are levels.** Handles update in place, so during bursts a handler may read `ev.peer` fields *newer* than the event that woke it. Use `ev.type` for what happened and the fields for what is.

Naming (decided): keep the bus-explicit `ws_connected` / `ws_disconnected` type strings. An earlier draft preferred `connected` / `disconnected` for brevity, but next to `joined` / `left` those read as peer-level liveness and invite `if (!peer.connected)` gating — exactly the bus-awareness this RFC removes (§16.3). Keeping today’s strings also spares 0.5.x listeners a rename.

### 6.5 Inbound messages

```ts
interface NamespacedMessage {
  from: Peer;              // was string device id
  namespace: string;
  msgType: string;
  payload: unknown;
  timestamp?: number;
}
```

`from` is the live handle for the sender, attributed by the connection's
WhoIs-verified Tailscale binding — never by the self-declared ULID (§7.5).
On this path `from.deviceId` is always non-null (hello precedes any bus
traffic), so subsystems like SyncedStore may keep keying durable rows by it.
Replies: `await msg.from.send(ns, data)`.

### 6.6 Local identity (unchanged shape, clarified)

```ts
interface NodeIdentity {
  appId: string;
  deviceId: string;       // always real ULID for *this* node
  deviceName: string;
  tailscaleHostname: string;
  tailscaleId: string;    // advanced
  dnsName?: string;
  ip?: string;
}
```

Local `getLocalInfo().deviceId` remains always the real ULID (RFC 017). The peer-side lie is what this RFC fixes.

### 6.7 Field guide (advanced chapter only)

| Goal | Use |
|------|-----|
| Network right now | `Peer` handle |
| In-process Map/Set | `Peer` (preferred) or `peer.ref` |
| Persist across restarts | `peer.deviceId` when non-null, re-bind via `mesh.peer(id)` |
| Debug Tailscale / DERP | `peer.tailscaleId` |

Getting Started documents only the first row (and `displayName`).

---

## 7. Internal Architecture

### 7.1 Layers (unchanged responsibilities)

```text
L3 NetworkProvider     NetworkPeer { id: tailscaleId, hostname, ip, online, … }
                         optional: advertised identity hint (Phase D)

L5 PeerRegistry        dual-index + PeerHandle table + hello / eager identity
L6 Envelope / stores   message.from → PeerHandle lookup
Bindings               NAPI/TS Peer class; MeshNode methods
```

### 7.2 Registry

```text
PeerRegistry
  by_tailscale: HashMap<TailscaleId, PeerInner>
  by_device:    HashMap<DeviceId, TailscaleId>    // ≤1 live entry per ULID (§7.7)
  handles:      HashMap<TailscaleId, PeerHandle>  // stable user-facing objects

PeerInner {
  tailscale_id,
  generation,                      // bumped per re-join; part of PeerRef (§7.7)
  hostname, ip, online, ws_connected, connection_type, os, last_seen,
  identity: Option<PeerIdentity>,  // ULID, device_name, app_id, os, …
}

PeerHandle → Arc<PeerInner> + PeerRef; core handles carry NO session pointer
             (methods live on Node — §9.2); bindings attach send/ping sugar
```

**Routing** always uses `tailscale_id` internally.  
**Attribution** of inbound traffic uses the connection's WhoIs-verified `tailscale_id` — never `by_device` (§7.5).  
**Indexing** by ULID exists only for queries (`mesh.peer(ulid)`) and persistence re-binding.  
**Projection** never copies `tailscale_id` into `device_id`.

### 7.3 Projection (honesty)

```rust
// BEFORE (lie)
None => device_id = s.id.clone()  // Tailscale id

// AFTER (honest)
Some(i) => device_id = Some(i.device_id)
None    => device_id = None
// display_name = identity.device_name
//              ?? hostname with the `truffle-{appId}-` prefix stripped
//              ?? short(tailscale_id)
```

### 7.4 Event emission

| Core condition | User event | Handle |
|----------------|------------|--------|
| L3 joined | `joined` | create handle (new generation) |
| L3 updated | `updated` | same handle, fields refresh |
| Identity first learned | `identity` | same handle; `deviceId` set; insert `by_device` |
| Identity rotated (same entry, new ULID) | `identity` (again) | same handle; `deviceId` updated; `by_device` row moved (§7.7) |
| ULID claimed by a new entry while the old holder is offline | `left` for the ghost, then `identity` | ghost retired; `by_device` repointed (§7.7) |
| WS up | `ws_connected` | same handle |
| WS down | `ws_disconnected` | same handle; **do not clear `deviceId`** once known |
| L3 left | `left` | same handle; mark offline; remove from maps after delivery |

Critical: **`deviceId` does not regress to null or to a Tailscale id on disconnect.** Once learned for this live entry, it stays until `left` tears the entry down.

### 7.5 Message path

On inbound envelope:

1. The session layer knows which WS connection the frame arrived on, and every
   accepted connection was bound at hello time to a **WhoIs-verified**
   `tailscale_id` (close code 4003 on mismatch — `transport/websocket.rs`).
2. Look up `by_tailscale[conn.tailscale_id]` → handle.
3. Deliver `NamespacedMessage { from: PeerHandle, … }`.

**`by_device` is never consulted on the message path.** The ULID claimed in a
hello is self-declared — RFC 017 §8 carries no proof of possession — so using
it for attribution would let a same-tailnet, same-app node claim another
device's ULID and receive its `msg.from` attribution. The connection's
authenticated `tailscale_id` is the only attribution key; `by_device` exists
solely for queries (`mesh.peer(ulid)`) and persistence re-binding, where the
§7.7 collision policy bounds the blast radius.

### 7.6 Same-instance guarantee

For a given `Node` / `MeshNode` and live peer entry:

```ts
const a = (await mesh.getPeers()).find(p => p.displayName === 'Bob');
// later, on event or message:
assert(ev.peer === a);       // guaranteed in the Node/NAPI bindings (§9.1)
assert(ev.peer.equals(a));   // portable form (Tauri views, serialized contexts)
```

True `===` is a **requirement** for the Node bindings, not best-effort — the
`Map<Peer, T>` idiom in §5 and §10 breaks *silently* without it (misses become
duplicate entries; deletes no-op). `equals` / `ref` are the portable contract
for boundaries that object identity cannot cross (Tauri IPC §9.3, workers,
serialization).

### 7.7 Handle lifecycle: generations, ghosts, ULID collisions

Every registry entry carries a **generation**, bumped each time the same
`tailscale_id` re-joins after `left`. `PeerRef` encodes
`(tailscale_id, generation)`; `equals` compares refs. Three cases fall out:

- **Restart, same Tailscale node (non-ephemeral).** `left` retires the entry;
  the re-join creates generation *n+1*. A stale held handle compares
  `equals === false` against the new one, so `Map<Peer, T>` naturally treats
  the re-join as a new session. "Same durable device?" is answered by
  comparing `deviceId`, not `equals`.

- **Restart, ephemeral node (the ghost problem).** Ephemeral re-registration
  gets a **new** `tailscale_id` while Tailscale reaps the old node lazily — so
  an offline ghost entry (`deviceId` still set, per §7.4) can coexist with the
  new live one, both carrying the same ULID. This is RFC 017 §15's "two
  entries for one peer" and must not leak to users:
  - When identity *U* is learned on entry *B* while `by_device[U]` points to
    entry *A* and **A is offline**: repoint `by_device[U] → B`, retire *A* by
    synthesizing `left` for it, then emit `identity` for *B*. Users see the
    ghost leave and the real peer gain identity — never two live peers with
    one ULID.
  - If **A is still online** — two simultaneously-online nodes claiming one
    ULID (a copied state dir, or an impersonation attempt; the hello's
    `tailscale_id` is WhoIs-verified but its ULID is self-declared):
    **first-wins**. `by_device[U]` keeps pointing at *A*; *B* projects
    `deviceId = null`, and the registry emits a loud `duplicate-device-id`
    diagnostic naming both tailscale ids. No existing binding is silently
    hijacked. When the holder goes offline, the survivor claims the ULID via
    the rule above.

- **Identity rotation (same entry, new ULID).** A node that wiped its state
  dir re-hellos with a fresh ULID on the same entry: update
  `PeerInner.identity`, move the `by_device` row, emit `identity` again.
  `deviceId` thus changes only on an announced re-hello — never silently, and
  never to a Tailscale id.

Message attribution is immune to all three cases because it never reads
`by_device` (§7.5).

---

## 8. Learning Identity (when `deviceId` becomes non-null)

Identity learning is an **internal** concern. Multiple sources may fill `PeerInner.identity`; whichever fills it first emits the user-visible `identity` event (later confirmations from other sources are silent; an actual rotation re-emits — §7.7).

### 8.1 Priority

1. **Eager envelope hello (default on)** — when a peer is online, the session layer dials the bus WS **once per registry entry**, solely to exchange the RFC 017 hello, without waiting for app `send`.
2. **Lazy hello** — existing path on first `send` / bus traffic.
3. **Hostinfo / netmap advertisement (optional follow-up)** — stamp ULID at L3 so identity can appear before any dial.
4. **Raw-transport first-frame (optional)** — for `eagerIdentity: false` pure TCP/QUIC/UDP apps that never open the bus.

**One-shot, not keep-alive.** `ensure_identity` runs until the entry's
identity is learned, then stops. The WS it opened is allowed to close — if the
sidecar's idle reaper (RFC 021 §10) closes it later, apps see
`ws_disconnected` without losing `deviceId` (§7.4), and the eager dialer does
**not** redial just to keep the bus warm. Identity, once learned, needs no
live connection.

**Startup is rate-limited.** The eager dialer runs behind a small concurrency
cap (default: 4 in-flight) with per-peer jitter, reusing the existing
reconnect backoff for failures. A 50-peer tailnet must not produce a
thundering herd of WS handshakes on every app launch.

**Observable side effect.** With eager identity on, `ws_connected` (and later,
after idle reap, `ws_disconnected`) fires for peers the app never messaged.
Consumers that allocate resources on `ws_connected` (§10) must be idempotent
about that; apps that want the 0.5.x lazy profile set `eagerIdentity: false`.

### 8.2 Config

```ts
createMeshNode({
  appId: 'chat',
  /**
   * When true (default), truffle proactively exchanges hello with online peers
   * so Peer.deviceId is usually non-null without app traffic.
   */
  eagerIdentity?: boolean; // default true
});
```

Raw-only, high-peer-count deployments may set `eagerIdentity: false` and accept `deviceId === null` until first contact; networking still works via the `Peer` handle.

### 8.3 Security

- Hello continues to enforce `appId` match and WhoIs / `tailscale_id` correlation (`CLOSE_APP_MISMATCH`, `CLOSE_IDENTITY_MISMATCH`).
- The **ULID inside the hello remains self-declared** — WhoIs verifies the
  Tailscale identity, not the claimed device id, and there is no proof of
  possession (cryptographic device identity stays a non-goal, §3). This RFC
  therefore never uses ULIDs for inbound attribution (§7.5) and bounds
  duplicate claims with the §7.7 first-wins policy plus a loud
  `duplicate-device-id` diagnostic.
- **Privacy note:** eager identity means every same-app peer on the tailnet
  automatically learns this device's ULID, device name, and OS at discovery
  time, with no app-level traffic. That is the feature — but it is a behavior
  change from 0.5.x's lazy hello, flagged in §11 and disable-able via
  `eagerIdentity: false`.
- Netmap hints (if added) are **untrusted until confirmed** by hello or an equivalent authenticated path.

---

## 9. Binding Notes

### 9.1 TypeScript / NAPI

| Concern | Approach |
|---------|----------|
| Handle type | NAPI **class** `Peer` (not only `#[napi(object)]` bags) |
| Instance table | Addon holds one `Reference<Peer>` per live entry, keyed by `ref` (tailscaleId + generation); dropped when `left` is delivered |
| Object identity | **`===` is a hard requirement** (§7.6): `getPeers()`, `ev.peer`, and `msg.from` all return the table instance |
| Threading | Instances are created/fetched on the JS thread only — inside the ThreadsafeFunction callback for events, at promise resolution for `getPeers` |
| Methods | `send` / `ping` on class; mesh methods accept `Peer \| string` |
| `getPeers` | Returns existing instances; does not allocate duplicates |
| Deprecations | `resolvePeerId`; string-only docs examples |

The instance table (napi-rs `Reference` lifetimes + TSFN access) is the
riskiest item in Phase B — **prototype it first**, before migrating any API
signatures (§12).

### 9.2 Rust core

| Concern | Approach |
|---------|----------|
| User type | `Peer` handle wrapping `Arc<PeerInner>` + `PeerRef` — plain data, **no methods** |
| Why no core sugar | `peer.send()` in core needs a session pointer per handle: either a viral `Peer<N: NetworkProvider>` generic in every consumer signature or a type-erased `Arc<dyn …>`, plus `Weak` care against registry ↔ handle ↔ session cycles. §13.6's methods-on-node subset is the core design; bindings (where the provider is concrete) add the sugar. |
| `Node::send` | `impl Into<PeerSelector>` — handle, ref, or `&str` query |
| Events | Carry the handle (`Arc` clone) — safe across `broadcast` lag; the handle keeps the entry readable even after `left` |
| Tests | I1–I7 invariants; same-handle; no flip; disconnect keeps ULID; ghost retire; duplicate-ULID first-wins; rotation re-emits `identity` |

### 9.3 Tauri / other FFI

Live handles may not cross IPC. Pattern:

- Main process holds `Peer` / registry.
- UI gets serializable views `{ ref, displayName, deviceId, online, … }`.
- Commands accept `ref` or `deviceId` query; main resolves to handle.

This matches RFC 021’s “id-keyed registry” note without teaching dual ids to UI authors: the UI uses **one** `ref` token issued by the main process.

### 9.4 CLI

```text
truffle peers                 # table: displayName, online, deviceId?, …
truffle send <query> …        # query resolves via mesh.peer
```

Human-visible columns may show short ULID when known; the CLI never asks the user “pass tailscale id or device id?”

---

## 10. Impact on Dependent Libraries (e.g. avocado)

Consumers today (avocado `transport-truffle`) dual-key maps and special-case disconnect ids. After this RFC:

```ts
// PTYMeshBridge sketch
const transports = new Map<Peer, MeshPTYTransport>();

mesh.onPeerChange((ev) => {
  switch (ev.type) {
    case 'ws_connected': {
      if (transports.has(ev.peer)) break;   // idempotent — eager identity (§8)
      const t = createTransport({ peer: ev.peer, node: mesh });
      transports.set(ev.peer, t);           // Map works: `===` guaranteed (§9.1)
      break;
    }
    case 'ws_disconnected':
    case 'left': {
      transports.get(ev.peer)?.dispose();
      transports.delete(ev.peer);
      break;
    }
  }
});
```

Note the eager-identity interaction: with the default `eagerIdentity: true`,
`ws_connected` fires for every online peer shortly after startup, so this
bridge eagerly creates one transport per peer — which is what avocado wants.
Apps that only want transports for peers they actually talk to should key off
their own traffic instead of `ws_connected`.

SyncedStore rows may still key durable slices by ULID **inside** store writers using `peer.deviceId` when publishing local state; live PTY routing uses the handle only.

---

## 11. Migration (0.5.x → 0.6.0)

### 11.1 Breaking changes

| Area | Before | After |
|------|--------|-------|
| `NapiPeer.deviceId` | `string` (sometimes Tailscale id) | `string \| null` (ULID only) |
| Core `Peer.id` (legacy ULID-else-Tailscale) | Present | Removed |
| `name` / `deviceName` overloads | Hostname-else-hello-name | Honest split: `displayName` + `hostname` |
| `wsConnected` | Field on plain object | Same name, now on the handle (still advanced) |
| `send(peerId: string, …)` | Primary | Still works as **query**; primary is `send(peer, …)` |
| `NamespacedMessage.from` | `string` | `Peer` |
| Peer-change `peerId` | Overloaded string | Removed / replaced by `peer: Peer` (type strings `ws_connected` / `ws_disconnected` kept) |
| `resolvePeerId` | Primary resolver | Deprecated |
| Docs “key on tailscaleId if raw” | Official guidance | Deleted |
| Hello timing | Lazy (first `send`) | Eager by default (§8) — behavioral, not wire, change |

**Wire compatibility:** this is an API break only. Envelope, hello (RFC 017
§8), and close codes are unchanged, so 0.5.x and 0.6.0 nodes interoperate on
the same tailnet — eager identity merely sends the same hello earlier.
Mixed-version meshes need no flag day.

### 11.2 Compatibility shims (optional, one minor max)

- Accept deprecated event field `peerId` as alias of `peer.ref` or `peer.tailscaleId` with console warning.
- `from` as string in a compatibility mode — **not recommended**; prefer hard break in 0.6.0 given pre-1.0 status.

### 11.3 Consumer checklist

1. Replace `send(peer.deviceId, …)` with `send(peer, …)` or `peer.send(…)`.
2. Replace maps keyed by `deviceId` string with `Map<Peer, T>` or `peer.ref`.
3. On disconnect/left, delete by `ev.peer`, not by a stored ULID alone.
4. Treat `deviceId === null` as “identity pending,” not as an error for networking.
5. Persist only non-null `deviceId`; rehydrate with `mesh.peer(id, { waitMs })`.
6. `identity` handlers must tolerate a second firing per entry (rotation / ghost retire — §7.7).

---

## 12. Implementation Plan

Phases are sequential for release; work within a phase can parallelize.

### Phase A — Registry handles + honest projection — **Done** (e67fbdd + f1784a0 + e23d756)

- [x] Dual-index (`peers` by tailscale id, `by_device`) in `PeerRegistry` — as-built: `PeerState` carries `generation`, no separate `PeerInner` struct
- [x] Stable handle per live entry; `generation` + `peer_ref` — as-built: interned `Peer` instances live in the packages/core `PeerRegistry`; Rust core projects `peer_ref`/`generation` fields
- [x] §7.7 lifecycle: ghost retire, duplicate-ULID first-wins, rotation re-emit (unit-tested)
- [x] Projection: `device_id: Option`; never fallback to Tailscale id; legacy `Peer.id` dropped
- [x] `display_name` derivation (strip `truffle-{appId}-` prefix)
- [x] Message path: attribute `from` by connection `tailscale_id` (§7.5) — `by_device` never consulted for attribution; I1 also enforced in `validate_hello` (e23d756)
- [x] Unit tests for invariants I1–I7 (incl. first-wins, ghost retire, generation bump, PeerGone)
- [x] Update RFC 017 §15 note: “superseded by RFC 022” (done alongside this revision)

### Phase B — Handle-first Node + bindings API — **Done with one deferral** (e67fbdd + f1784a0 + 548b939 + e23d756)

- [x] `===` same-instance guarantee — as-built via the packages/core `PeerRegistry` (one internal peer-change subscription maintains it, incl. receiver-only `msg.from`); the native NAPI `Reference` instance table is **deferred** (revisit if non-TS NAPI consumers need object identity)
- [x] `Node::peer(query, wait_ms)`, `peers() -> Vec<Peer>` (projected views); ambiguity → `AmbiguousPeer` listing candidates
- [x] Peer-selector sweep: core strings accept ULID / name / hostname / IP / tailscale id / generation-checked `{tsId}:{gen}` refs across `send` / `send_typed` / `ping` / `open_tcp` / `connect_quic` / `resolve_peer_ip` / file transfer; JS `PeerLike` on send/ping/openTcp/connectQuic/fileTransfer + net/quic/dgram/ws namespaces
- [x] HTTP **proxy** surface: N/A — `NapiProxyConfig` carries no peer-addressed parameter (a proxy forwards a mesh TLS listen port to a *local* target), so there is nothing to sweep
- [x] Events carry peer state/handles incl. final-state `left`; `identity` event added; `ws_connected` / `ws_disconnected` type strings kept (§6.4)
- [x] Inbound messages: `from` is an interned `Peer` at the JS layer; core carries the WhoIs-verified tailscale id (§7.5)
- [x] TS types for `MeshNode`; `resolvePeerId` marked `@deprecated`
- [x] `PeerLike` on `net.connect({ peer })` / `quic.connect(peer, port)` wrappers
- [x] Tauri plugin: serializable peer views (`peerRef`/`generation`, honest nullability) + commands accept `ref` via the shared resolver; guest-js types mirror `PeerEventJs` incl. `identity`
- [x] CLI + TUI migrated (resolve.rs query resolution; TUI peer tables keyed by tailscale id)

### Phase C — Eager identity (default on) — **Done**

- [x] On L3 joined + online: background `ensure_identity` (WS hello), one-shot per entry (§8.1)
- [x] `eagerIdentity` option (default `true`; `eager_identity_concurrency` knob)
- [x] Concurrency cap (semaphore, default 4) + reconnect backoff reuse
- [x] Per-peer jitter on eager dials — a hardcoded hash-based stagger already existed; now a configurable `PeerRegistryOptions.eager_identity_jitter_ms` knob (default 250, `0` disables), applied before semaphore acquire so idle waits never hold a dial slot, with a determinism unit test
- [x] No redial after idle reap: identity survives WS close; re-hello emits no duplicate `identity` (unit-tested, e23d756)
- [x] Integration test: `tests/integration_eager_identity.rs` — no-warm-up pair (`PairOpts.warm_up = false`), zero app sends, each side learns the counterpart's real ULID (asserted against the peer's actual local `device_id`, never the tailscale id) within 45s; `identity` fires ≤ 1 per side

### Phase D — Optional hostinfo advertisement — **Not started** (deliberately deferred; not required for 0.6, §16.5)

- [ ] Sidecar stamps versioned app identity into hostinfo/service metadata
- [ ] L3 parse → identity **hint** on `NetworkPeer`
- [ ] Registry adopts hint; hello confirms
- [ ] Only if Phase C metrics still show long `deviceId === null` windows

### Phase E — Docs & examples — **Done**

- [x] Rewrite Getting Started around `Peer` only (`docs/index.html`)
- [x] Move id fields to “Persistence & debugging” (`docs/index.html` Peers section + `docs/api.html`)
- [x] Update examples — all 8 audited: chat / discovery / netcat / quic-streams / ws-chat-over-mesh / express-over-mesh / playground migrated to Peer-first (also fixed two latent runtime bugs: `Peer` class handles JSON-serializing to `{}` in express-over-mesh's `/api/peers`, and playground's `deviceId`-keyed peer cache never matching the Tailscale-keyed `msg.from`); shared-state already clean
- [x] CHANGELOG unreleased breaking notes (0.6.0 release notes at ship time)

---

## 13. Alternatives Considered

### 13.1 Honest dual strings, still pass strings

`deviceId: string | null` + `peerKey: string`; document which to pass when.

**Rejected as the end state.** Fixes the lie but keeps “which id?” in every app. Acceptable only as an internal step toward handles, not as the public contract.

### 13.2 Keep the flip; document harder (RFC 017 §15)

**Rejected.** Documented footguns are still footguns. Downstream libraries (avocado, canvas, chat) all re-implement dual-key maps.

### 13.3 Make `deviceId` always Tailscale id

**Rejected.** Destroys RFC 017 durable identity across `ephemeral` rotations and state re-auth.

### 13.4 Encode ULID in Tailscale hostname

**Rejected** in RFC 017 §12.4 (admin UI unreadable; no room for human names). Unchanged.

### 13.5 Plain snapshot objects without same-instance guarantee

Return new object literals every `getPeers()` / event.

**Rejected as primary design.** Breaks natural `Map<Peer, T>` and forces `ref` everywhere. Snapshots may exist for serialization; live networking uses handles.

### 13.6 Methods only on `mesh`, never on `Peer`

`mesh.send(peer, …)` without `peer.send(…)`.

**Adopted for the Rust core** (§9.2): core handles are plain data, avoiding a
generic or type-erased session pointer in every handle. The JS/TS bindings DO
ship `peer.send(…)` sugar — the provider is concrete there and the DX win is
the point. Handle-as-parameter remains the mandatory part everywhere.

---

## 14. Test Plan

### 14.1 Invariants

- No peer ever has `device_id == Some(tailscale_id)`.
- Pre-identity: `device_id.is_none()`, `display_name` non-empty.
- Post-identity: `device_id` is valid ULID; disconnect does not clear it until `left`.
- Same handle: `get_peers` / `joined` / `message.from` identity equality; NAPI: literal `===` across `getPeers` / events / `msg.from`.
- Rejoin: same `tailscaleId` after `left` → new generation; old handle `equals(new) === false`.
- Ghost retire: ephemeral rejoin (new `tailscaleId`, same ULID, old entry offline) → synthesized `left` for the ghost; `mesh.peer(ulid)` resolves only the live entry.
- Duplicate ULID while both online → first-wins; `msg.from` attribution unaffected (connection-keyed, §7.5); `duplicate-device-id` diagnostic emitted.
- Rotation: re-hello with a new ULID on the same entry → second `identity` event; `by_device` repointed.

### 14.2 API

- `send(peer, …)` and `send('Bob', …)` both deliver.
- `peer(ulid)`, `peer(name)`, `peer(tailscale_id)`, `peer(ip)` resolve to the same handle when unambiguous.
- Not found → `null`; ambiguous (duplicate names, short prefix) → `AmbiguousPeerError` listing candidates (require ≥4 unique chars for prefixes).
- `peer(savedUlid, { waitMs })` resolves once the owner hellos; times out to `null`.

### 14.3 Eager identity

- With default config, two online peers learn ULIDs without app `send`.
- Post-hello idle-reap of the eager WS: `deviceId` retained; no redial storm; no duplicate `identity`.
- Startup against many online peers respects the concurrency cap (no thundering herd).
- With `eagerIdentity: false`, networking still works; `deviceId` may stay null until contact.

### 14.4 Regression

- Existing hello app-mismatch and identity-mismatch tests still pass.
- File transfer / SyncedStore / raw TCP+QUIC integration tests updated to handles.

---

## 15. Documentation Plan

| Surface | Content |
|---------|---------|
| Getting Started | Peer only; send/reply; list `displayName` |
| Guide: Peers | Handles, events, `Map<Peer,T>`, stale peers |
| Guide: Persistence | `deviceId` + `mesh.peer` |
| Guide: Diagnostics | `tailscaleId`, hostname, DERP |
| API reference | `Peer`, `PeerLike`, events, deprecations |
| Migration 0.5 → 0.6 | Consumer checklist §11.3 |
| RFC 017 §15 | Point to this RFC as the fix |

---

## 16. Open & Decided Questions

### 16.1 Hard break on `message.from` type?

**Proposal:** Hard break in 0.6.0 (`string` → `Peer`). Pre-1.0 and few external consumers. Confirm against monorepo examples + avocado before release.

### 16.2 `===` vs `equals` across NAPI? — **Decided**

`===` is a hard requirement for the Node bindings (§7.6, §9.1); `equals` /
`ref` are the portable contract only where object identity cannot cross a
boundary (Tauri IPC, workers, serialization). Best-effort was rejected: the
§5 / §10 `Map<Peer, T>` idiom breaks silently without real object identity.

### 16.3 Should bus connectivity stay on the public Peer? — **Decided**

Yes, but under the bus-explicit name `wsConnected` (kept from 0.5.x), never
`connected`: next to `online`, a field named `connected` reads as peer
liveness and invites `if (!peer.connected)` send-gating — the exact
bus-awareness this RFC removes. Useful for indicators; docs never gate
`deviceId` or the send API on it after Phase C.

### 16.4 `left` handle lifetime — **Decided**

Deliver `left` with a still-equatable, still-readable handle; subsequent
`getPeers()` omits it; networking on it rejects with `PeerGone`. Generations
(§7.7) make a later rejoin a distinct handle, so lingering references cannot
alias the new session.

### 16.5 Hostinfo phase required for 0.6?

**Proposal:** No. Phase A–C are sufficient for 0.6.0. Phase D only if eager hello is too expensive in measured deployments.

---

## 17. Success Criteria

1. A new user can list peers and send messages **without reading** what a ULID or Tailscale id is.
2. Getting Started examples contain **no** `deviceId` / `tailscaleId` parameters.
3. No official doc says “if you use raw transports, key on X.”
4. Invariant I1 holds in CI.
5. Downstream avocado (or equivalent) can use `Map<Peer, Transport>` without dual-key heuristics.
6. With default `eagerIdentity`, online peers typically expose non-null `deviceId` without application `send`.
7. A duplicate-ULID hello (accidental clone or impersonation attempt) cannot redirect `msg.from` attribution — enforced by a §14.1 test.

---

## 18. Summary

RFC 017 fixed **what identity is** (app namespace + durable ULID + Tailscale routing).  
RFC 022 fixes **how users hold and pass peers**:

| | RFC 017 as-built (0.5.x) | RFC 022 (0.6.0 target) |
|--|-------------------------|-------------------------|
| Primary value | String ids | **Peer handle** |
| `deviceId` | String, sometimes Tailscale id | **ULID or null** |
| Networking args | “Pick the right string” | **`Peer` (or query string)** |
| Bus vs raw | Different keying rules | **Same handle** |
| Dual-index | Every app | **PeerRegistry only** |
| Inbound attribution | Self-declared ULID from hello | **Connection’s WhoIs-verified tailscaleId** |

**One-sentence contract:**  
*Truffle gives you Peer objects; networking takes Peers; identity resolution is not your problem.*

---

## 19. Acknowledgements

This RFC consolidates:

- RFC 017 §15 probe results (deviceId flip on WS hello; raw-transport permanent Tailscale fallback).
- Production pain in dependent mesh UIs (duplicate map keys, orphaned transports on disconnect).
- Product feedback that even an “honest” dual-id API still forces users to learn too much — the right abstraction is a single **Peer** handle, with ids demoted to fields.
- A same-day design review against the v0.5.x codebase (2026-07-09), which produced revision 2: connection-authenticated attribution (§7.5), generations / ghost retire / ULID-collision policy (§7.7), the NAPI `===` commitment (§9.1), eager-identity one-shot semantics (§8), rehydration `waitMs` (§6.2), and the full Phase B parameter sweep (§12).

# RFC 006: Rust Implementation Audit Findings

**Status**: Resolved -- All findings addressed by RFC 007 implementation
**Created**: 2026-03-17
**Resolved**: 2026-03-18
**Audited against**: TypeScript reference at `p008-claude-on-the-go/packages/desktop/src/main/services/foundation/mesh/` and `packages/tsnet-sidecar/`

> **Resolution Summary**: All 9 bugs, 14 architecture issues, and 7 missing features identified in this audit have been addressed by the RFC 007 comprehensive refactor. See RFC 007 for implementation details of each changeset (CS-1 through CS-10).

---

## Critical Bugs (Must Fix)

### BUG-1: Outgoing dials completely broken — dual pending_dials maps
**Severity**: CRITICAL
**Files**: `bridge/shim.rs:96`, `bridge/manager.rs:77`, `napi/mesh_node.rs`

GoShim has `pending_dials: HashMap<String, oneshot::Sender<TcpStream>>` and BridgeManager has its own `pending_dials: HashMap<String, oneshot::Sender<BridgeConnection>>`. These are separate maps with different types. When Go dials a peer and bridges back, the BridgeManager intercepts outgoing connections with request_ids (manager.rs:189-208) and looks in **its own** pending_dials — which is empty. The connection is logged as "unknown outgoing request_id" and silently dropped.

**Impact**: No outgoing WebSocket connections can be established. Only incoming connections work.

**Fix**: Before calling `shim.dial()`, insert into `BridgeManager::pending_dials`. When the bridge connection arrives, it will be delivered via the oneshot. Then pass the `BridgeConnection` to `ConnectionManager::handle_outgoing()`.

### BUG-2: serde case mismatches — `tailscaleIP`, `tailscaleDNSName`, `tailscaleIPs`
**Severity**: CRITICAL (silent data loss)
**Files**: `types/mod.rs:52,55,87`, `bridge/protocol.rs:82`

Rust's `#[serde(rename_all = "camelCase")]` produces:
- `tailscale_ip` → `tailscaleIp` (but TS/Go send `tailscaleIP`)
- `tailscale_dns_name` → `tailscaleDnsName` (but TS sends `tailscaleDNSName`)
- `tailscale_ips` → `tailscaleIps` (but TS/Go send `tailscaleIPs`)

**Impact**: Device IP addresses and DNS names are silently lost in deserialization. Already fixed for `StatusEventData.tailscale_ip` (with `alias`), but the same bug exists in `BaseDevice` and `TailnetPeer`.

**Fix**: Add explicit `#[serde(alias = "tailscaleIP")]`, `#[serde(rename = "tailscaleDNSName")]`, `#[serde(rename = "tailscaleIPs")]` to the affected fields.

### BUG-3: `election:timeout` sentinel broadcast to all peers
**Severity**: CRITICAL
**Files**: `mesh/election.rs:248-259`, `mesh/node.rs:868-884,1028-1032`

The Rust election sends `election:timeout` as an `ElectionEvent::Broadcast`, which the MeshNode forwards to ALL connected peers. But this sentinel is meant to trigger local-only `decide_election()`. Remote peers also call `decide_election()` when they receive it, causing double/conflicting election decisions.

In TS, `setTimeout` calls `this.decideElection()` locally — no wire message.

**Fix**: Use a new `ElectionEvent::DecideNow` variant instead of `Broadcast` for the timeout. Handle it in the MeshNode event loop without sending to peers.

### BUG-4: Grace period handler doesn't update election state
**Severity**: CRITICAL
**Files**: `mesh/election.rs:133-188`

`handle_primary_lost()` spawns an async task that broadcasts `election:start` and `election:candidate` after the grace period. But the async task cannot access `&mut self`, so `self.phase` stays `Waiting` (not `Collecting`) and `self.candidates` is never populated. When `decide_election()` runs, it has empty/stale candidates.

In TS, the timeout calls `this.startElection()` which properly transitions state.

**Fix**: The grace period task should send `ElectionEvent::GracePeriodExpired`. The MeshNode handles it by calling `election.start_election()` on the actual `&mut self`.

---

## Medium Bugs

### BUG-5: `broadcast_device_list()` not called after winning election
**Files**: `mesh/node.rs:860-863`
The code has a TODO comment but never actually broadcasts the device list. Secondaries keep stale role info.

### BUG-6: Connections not closed on shutdown
**Files**: `mesh/node.rs:223-273`
`stop()` never calls `connection_manager.close_all()`. Connections leak until heartbeat timeout.

### BUG-7: Goodbye may not be delivered
**Files**: `mesh/node.rs:235-243`
`broadcast_goodbye()` sends messages, then immediately aborts the event loop. Write pumps may not flush before abort.

### BUG-8: Bridge connection failure not reported
**Files**: `sidecar-slim/main.go:407-419`
If Go can't connect to Rust's bridge port during a dial, no `bridge:dialResult` failure event is sent. The Rust side hangs for 30s.

### BUG-9: GoShim pending_dials leaks entries
**Files**: `bridge/shim.rs:514-517`
Successful dials insert into GoShim's `pending_dials` but the oneshot sender is never consumed (the NAPI layer discards the receiver). Entries accumulate.

---

## Missing Features

| # | Feature | TS Reference | Priority |
|---|---------|-------------|----------|
| M-1 | Immediate announce on local device change | mesh-service.ts:567-571 | Medium |
| M-2 | Connection timeout for outgoing dials | ws-service.ts:222-227 (10s) | Medium |
| M-3 | `close_all()` on stop | ws-service.ts:159-181 | Medium |
| M-4 | `primary:redirect` for non-primary connections | mesh-service.ts:647-665 | Low (PWA) |
| M-5 | `primary:changed` notification | mesh-service.ts:1097-1125 | Low (PWA) |
| M-6 | PWA detection timer (3s) | mesh-service.ts:669-679 | Low (PWA) |
| M-7 | `MeshMessageBus.unsubscribe()` | mesh-message-bus.ts:79-87 | Low |

---

## Divergences (By Design, Not Bugs)

| # | Difference | Assessment |
|---|-----------|------------|
| D-1 | Binary bridge protocol vs JSON IPC | Rust is better (less overhead) |
| D-2 | Go sidecar is thin TCP proxy vs full WS manager | Rust is better (less Go code) |
| D-3 | Binary codec (msgpack+flags) vs plain JSON | Not activated yet, OK |
| D-4 | No `sessions` field in BaseDevice (uses generic `metadata`) | Intentional generalization |
| D-5 | No device type filter for connection (connects to all) | Minor, can add later |
| D-6 | Peer hostname filtering removed | Intentional (truffle is generic) |
| D-7 | stdin close = shutdown | Improvement over original |
| D-8 | Auth storm prevention | Improvement over original |
| D-9 | Auto-restart with exponential backoff | Improvement over original |

---

## Recommended Fix Order

**Phase 1 — Unblock connections (BUG-1)**
Without this, nothing works. Fix the dual pending_dials maps so outgoing dials succeed.

**Phase 2 — Fix serde mismatches (BUG-2)**
Add aliases/renames for all camelCase acronym fields. Quick fix, high impact.

**Phase 3 — Fix election (BUG-3, BUG-4)**
Redesign election timeout as local-only event. Fix grace period to properly call start_election().

**Phase 4 — Shutdown cleanup (BUG-5, BUG-6, BUG-7)**
Call close_all() on stop, await goodbye delivery, broadcast device list after election win.

**Phase 5 — Sidecar robustness (BUG-8, BUG-9)**
Report bridge failures, clean up pending_dials.

---

## Part 2: Architecture & Code Quality Findings

### ARCH-1: Transport event loop is ~240 lines with no test coverage
**Severity**: CRITICAL (Testing) / HIGH (Code Quality)
**File**: `mesh/node.rs:900-1136`

The most complex and critical code path in the entire codebase — handling transport connect, disconnect, message parsing, mesh protocol dispatch, STAR routing, and MessageBus dispatch — has zero test coverage. It's also a monolithic inline closure with 5-6 levels of nesting.

**Fix**: Extract into named handler methods (`handle_transport_connected()`, `handle_transport_message()`, etc.). Write integration tests that inject synthetic transport events.

### ARCH-2: Dead code duplication in node.rs
**Severity**: HIGH
**File**: `mesh/node.rs:534,581,640-684`

`handle_mesh_message()`, `handle_incoming_data()`, and `handle_route_envelope()` are all `#[allow(dead_code)]` — the same logic is duplicated inline in the event loop. Drift risk.

**Fix**: Delete the dead methods. Extract the inline logic into proper methods.

### ARCH-3: Potential deadlock — lock ordering between device_manager and election
**Severity**: HIGH
**File**: `mesh/node.rs:496-520`

`handle_tailnet_peers()` acquires `device_manager.read()` then `election.read()`. If another task holds `election.write()` and tries `device_manager.write()`, classic ABBA deadlock.

**Fix**: Document strict lock ordering, or combine into a single `RwLock<MeshState>`.

### ARCH-4: NAPI layer owns ~200 lines of core orchestration logic
**Severity**: MEDIUM
**File**: `napi/mesh_node.rs:138-352`

Sidecar bootstrap, lifecycle event handling, peer dialing — all pure Rust domain logic living in the NAPI binding layer. Tauri plugin would need to duplicate all of it.

**Fix**: Create `truffle-core/src/runtime.rs` with `bootstrap_sidecar()`, `spawn_lifecycle_handler()`, and `dial_truffle_peers()`. NAPI/Tauri become thin wrappers.

### ARCH-5: `current_timestamp_ms()` duplicated in 3 files
**Severity**: LOW
**Files**: `mesh/node.rs:1196`, `election.rs:477`, `message_types.rs:106`

**Fix**: Move to a single shared utility.

### ARCH-6: DeviceEvent/ElectionEvent silently dropped via `try_send()`
**Severity**: MEDIUM
**Files**: `mesh/device.rs:331`, `mesh/election.rs:472`

Channel capacity 256, `try_send()` drops silently if full. Critical events like `PrimaryChanged` or `DeviceOffline` could be lost.

**Fix**: Log warnings on drop. Consider `send().await` for critical events.

### ARCH-7: `SubscriptionId` is dead API — no `unsubscribe()` exists
**Severity**: MEDIUM
**File**: `mesh/message_bus.rs:114-119`

**Fix**: Implement unsubscribe with UUID keys, or remove the return type.

### ARCH-8: Peer auto-dial holds shim lock across all network I/O
**Severity**: MEDIUM
**File**: `napi/mesh_node.rs:296-322`

Lock held for entire peer iteration + dial loop. Blocks other shim operations.

**Fix**: Clone shim ref, collect dials, release lock, then dial.

### ARCH-9: `BridgeManager::run()` consumes self — no graceful shutdown
**Severity**: MEDIUM
**File**: `bridge/manager.rs:126`

**Fix**: Add `CancellationToken` or `Notify` for shutdown. Take `&self` or `Arc<Self>`.

### ARCH-10: `resume_auto_restart()` overloads `shutdown` Notify
**Severity**: MEDIUM
**File**: `bridge/shim.rs:583-585`

Using the same `Notify` for shutdown and resume signals. Could trigger wrong code path.

**Fix**: Separate `Notify` instances or use `mpsc` with signal enum.

### ARCH-11: `MeshNode` has 14 fields, 9 wrapped in `Arc<RwLock<>>`
**Severity**: LOW
**File**: `mesh/node.rs:90-118`

God object pattern. Related mutable state could be grouped.

**Fix**: Group `running`, `started_at`, `auth_status`, `auth_url`, handles into `Arc<RwLock<MeshNodeState>>`.

### ARCH-12: `StoreSyncAdapter` uses `tokio::sync::RwLock` for boolean flags
**Severity**: LOW
**File**: `store_sync/adapter.rs:66-67`

**Fix**: Use `AtomicBool`.

### ARCH-13: Duplicate `TailnetPeer` type in `types/mod.rs` and `bridge/protocol.rs`
**Severity**: LOW
**Files**: `types/mod.rs:81-91`, `bridge/protocol.rs:95-106`

Different `os` field types (`Option<String>` vs `String`).

**Fix**: Use single canonical type.

### ARCH-14: Envelope creation pattern duplicated ~6 times in node.rs
**Severity**: MEDIUM
**File**: `mesh/node.rs` (multiple locations)

**Fix**: Consolidate into a helper method. `send_mesh_message_raw()` exists but is unused in the event loop.

---

## Recommended Fix Priority (Combined)

| Priority | Items | Impact |
|----------|-------|--------|
| **P0** | BUG-1 (outgoing dials broken) | Nothing works without this |
| **P1** | BUG-2 (serde case), BUG-3+4 (election), ARCH-1 (event loop tests) | Core protocol correctness |
| **P2** | BUG-5,6,7 (shutdown), ARCH-4 (runtime.rs) | Stability + architecture |
| **P3** | BUG-8,9 (sidecar), ARCH-3 (deadlock risk), ARCH-6 (event drops) | Robustness |
| **P4** | ARCH-2,5,7,8,9,10,11,12,13,14 | Code quality |

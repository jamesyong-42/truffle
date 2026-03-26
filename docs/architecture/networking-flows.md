# Truffle Networking Architecture: Complete Flow Analysis (v1 — Archived)

> **This document describes the OLD v1 architecture (pre-RFC 012).** The entire codebase was rebuilt with a clean 7-layer design in RFC 012. For the current architecture, see the [Architecture guide](/guide/architecture).

> Generated from exhaustive source code analysis of the v1 codebase. All file paths are absolute.
> Line numbers refer to the state of the v1 codebase at the time of analysis.

---

## 1. System Overview

```
+------------------------------------------------------------------+
|  USER                                                             |
|  $ truffle <command>                                              |
+--------+---------------------------------------------------------+
         |
         v
+--------+---------------------------------------------------------+
|  CLI LAYER (truffle-cli crate)                                   |
|                                                                   |
|  main.rs          Parse CLI args (clap), dispatch to command mod  |
|  commands/*.rs    Format output, call DaemonClient                |
|  resolve.rs       Name resolution (alias > name > hostname > IP) |
|  target.rs        Parse node:port/path target strings             |
+--------+---------------------------------------------------------+
         |  Unix Socket / Named Pipe (newline-delimited JSON-RPC 2.0)
         v
+--------+---------------------------------------------------------+
|  DAEMON LAYER (truffle-cli/src/daemon/)                          |
|                                                                   |
|  server.rs        Accept loop on IPC socket, owns TruffleRuntime |
|  handler.rs       JSON-RPC dispatch -> runtime methods            |
|  client.rs        DaemonClient: connect, request, notifications  |
|  ipc.rs           Cross-platform IPC (Unix socket / named pipe)  |
|  protocol.rs      DaemonRequest, DaemonResponse, methods/errors  |
+--------+---------------------------------------------------------+
         |  Direct Rust function calls (Arc<TruffleRuntime>)
         v
+--------+---------------------------------------------------------+
|  CORE RUNTIME (truffle-core crate)                               |
|                                                                   |
|  runtime.rs       TruffleRuntime: lifecycle, sidecar, events     |
|  mesh/node.rs     MeshNode: device tracking, envelope routing    |
|  mesh/device.rs   DeviceManager: peer state, announce/goodbye    |
|  mesh/handler.rs  TransportHandler: WS message dispatch          |
|  transport/       ConnectionManager: WS upgrade, send/receive    |
|    connection.rs  WSConnection lifecycle, v3 handshake            |
+--------+---------------------------------------------------------+
         |  Two channels:
         |    1. JSON lines on stdin/stdout (commands/events)
         |    2. TCP bridge on 127.0.0.1:<ephemeral> (binary header + data)
         v
+--------+---------------------------------------------------------+
|  BRIDGE LAYER (truffle-core/src/bridge/)                         |
|                                                                   |
|  manager.rs       BridgeManager: TCP listener, header parse,     |
|                   route by (port, direction), pending_dials       |
|  header.rs        Binary header: TRFF magic, token, port, etc.   |
|  shim.rs          GoShim: spawn child, send commands, read events|
|  protocol.rs      ShimCommand/ShimEvent JSON types               |
+--------+---------------------------------------------------------+
         |  stdin/stdout JSON lines      TCP 127.0.0.1:<bridge_port>
         v                               v
+--------+----------+  +----------------+----+
|  GO SIDECAR        |  |  BRIDGE TCP       |
|  (sidecar-slim)    |  |  CONNECTION       |
|                    |  |                    |
|  main.go           |  |  Go connects to   |
|  - tsnet.Server    |  |  Rust's bridge    |
|  - tsnet.Dial()    |  |  port, writes     |
|  - ListenTLS :443  |  |  binary header,   |
|  - Listen :9417    |  |  then bidi copy   |
|  - Listen :DYNAMIC |  |                    |
+--------+-----------+  +---+----------------+
         |                   |
         |  Tailscale WireGuard tunnels (tsnet)
         v
+--------+---------------------------------------------------------+
|  TAILSCALE NETWORK (WireGuard mesh)                              |
|                                                                   |
|  - MagicDNS resolution (truffle-cli-<id>.tailnet.ts.net)         |
|  - Direct connections (NAT traversal via STUN)                   |
|  - DERP relay fallback                                           |
|  - Tailscale TLS certificates (port 443)                         |
+------------------------------------------------------------------+
```

### Key: Communication channels between components

```
CLI <-> Daemon:     Unix socket (~/.config/truffle/truffle.sock)
                    Protocol: newline-delimited JSON-RPC 2.0

Daemon <-> Runtime: Direct Rust function calls (Arc<TruffleRuntime>)

Runtime <-> GoShim: stdin/stdout JSON lines (commands and events)

GoShim <-> Bridge:  TCP 127.0.0.1:<ephemeral_bridge_port>
                    Protocol: Binary header (TRFF) + raw TCP data

Bridge <-> Mesh:    Routed by (port, direction) to handlers:
                    - port 443/incoming  -> HttpRouter -> /ws WS upgrade
                    - port 9417/incoming -> HttpRouter -> /ws WS upgrade
                    - port 443/outgoing  -> ChannelHandler -> WS client
                    - port 9417/outgoing -> ChannelHandler -> WS client
                    - port 9418/incoming -> TcpProxyHandler -> axum file xfer
                    - port 9418/outgoing -> pending_dials (DialFn)

Peer <-> Peer:      WebSocket over Tailscale TLS/TCP (mesh envelopes)
                    File transfer: HTTP PUT/HEAD over Tailscale TCP :9418
```

---

## 2. Port Map

| Port | Transport | Direction | Protocol | Purpose |
|------|-----------|-----------|----------|---------|
| **443** | TLS (ListenTLS) | Incoming | HTTPS -> WebSocket `/ws` | Primary mesh transport (WS connections between peers) |
| **443** | TLS (Dial + TLS) | Outgoing | WebSocket client | Outgoing mesh connections to peers |
| **9417** | TCP (Listen) | Incoming | HTTP -> WebSocket `/ws` | Fallback mesh transport (plain TCP, no TLS) |
| **9417** | TCP (Dial) | Outgoing | WebSocket client | Outgoing mesh connections (no TLS) |
| **9418** | TCP (Listen, dynamic) | Incoming | HTTP (axum) | File transfer receiver (PUT/HEAD `/transfer/{id}`) |
| **9418** | TCP (Dial) | Outgoing | HTTP client (hyper) | File transfer sender (PUT to receiver) |
| **IPC** | Unix socket | Local | JSON-RPC 2.0 | CLI <-> Daemon communication |
| **bridge** | TCP 127.0.0.1:0 | Local | Binary header + data | Go sidecar <-> Rust bridge |

**Source references:**
- Port 443 TLS listener: `main.go:418` (`s.server.ListenTLS("tcp", ":443")`)
- Port 9417 TCP listener: `main.go:443` (`s.server.Listen("tcp", ":9417")`)
- Port 9418 dynamic listener: `server.rs:141` (`s.listen(9418, false).await`)
- Bridge port: `manager.rs:121` (`TcpListener::bind("127.0.0.1:0")`)
- IPC socket: `ipc.rs:22` (`TruffleConfig::socket_path()` -> `~/.config/truffle/truffle.sock`)

---

## 3. The Bridge Protocol

### 3.1 Binary Header Format (RFC 003)

Every bridge TCP connection begins with a binary header. After the header, the TCP stream carries raw application data (WebSocket frames, HTTP requests, etc.).

```
+--------+--------+----------------------------------+--------+--------+
| Offset | Size   | Field                            | Type   | Notes  |
+--------+--------+----------------------------------+--------+--------+
| 0      | 4      | Magic                            | u32 BE | 0x54524646 ("TRFF") |
| 4      | 1      | Version                          | u8     | 0x01   |
| 5      | 32     | SessionToken                     | [u8;32]| Random, shared secret |
| 37     | 1      | Direction                        | u8     | 0x01=Incoming, 0x02=Outgoing |
| 38     | 2      | ServicePort                      | u16 BE | 443, 9417, 9418 |
| 40     | 2      | RequestIdLen (N)                 | u16 BE | 0 for incoming |
| 42     | N      | RequestId                        | UTF-8  | UUID for outgoing dials |
| 42+N   | 2      | RemoteAddrLen (M)                | u16 BE | |
| 44+N   | M      | RemoteAddr                       | UTF-8  | e.g. "100.64.0.2:12345" |
| 44+N+M | 2      | RemoteDNSNameLen (K)             | u16 BE | |
| 46+N+M | K      | RemoteDNSName                    | UTF-8  | JSON PeerIdentity or DNS name |
+--------+--------+----------------------------------+--------+--------+
```

**Minimum header size:** 46 bytes (all variable-length fields empty)
**Maximum field lengths:** RequestId=128, RemoteAddr=256, RemoteDNSName=512

**Source:** `/Users/jamesyong/Projects/project100/p008/truffle/crates/truffle-core/src/bridge/header.rs:1-250`

### 3.2 Session Token

- Generated at bootstrap: 32 random bytes via `getrandom` (runtime.rs:618)
- Converted to 64-char hex string for Go sidecar (runtime.rs:621)
- Go decodes hex back to 32 bytes (main.go:315-316)
- Every bridge connection is validated via constant-time comparison (manager.rs:224-229)
- Rejection of invalid tokens is silent (connection dropped, logged as warning)

### 3.3 Go <-> Rust Command/Event Channel

Commands (Rust -> Go) and Events (Go -> Rust) flow as newline-delimited JSON on stdin/stdout of the Go child process.

**Commands (Rust -> Go via stdin):**

| Command | Purpose | Data fields |
|---------|---------|-------------|
| `tsnet:start` | Start tsnet server | hostname, stateDir, authKey, bridgePort, sessionToken, ephemeral, tags |
| `tsnet:stop` | Stop tsnet server | (none) |
| `tsnet:getPeers` | Request peer list | (none) |
| `bridge:dial` | Dial a peer via tsnet | requestId, target, port |
| `tsnet:listen` | Start dynamic listener | port, tls |
| `tsnet:unlisten` | Stop dynamic listener | port |
| `tsnet:ping` | Ping a peer | target (IP), pingType |
| `tsnet:pushFile` | Push file via Taildrop | targetNodeId, fileName, filePath |
| `tsnet:waitingFiles` | List waiting files | (none) |
| `tsnet:getWaitingFile` | Download waiting file | fileName, savePath |
| `tsnet:deleteWaitingFile` | Delete waiting file | fileName |

**Events (Go -> Rust via stdout):**

| Event | Purpose |
|-------|---------|
| `tsnet:started` | Server is running |
| `tsnet:stopped` | Server stopped |
| `tsnet:status` | Status update (state, IP, DNS) |
| `tsnet:authRequired` | Auth URL needed |
| `tsnet:needsApproval` | Admin approval needed |
| `tsnet:stateChange` | Backend state changed |
| `tsnet:keyExpiring` | Key expiry warning |
| `tsnet:healthWarning` | Health warnings |
| `tsnet:peers` | Peer list update |
| `tsnet:error` | Error occurred |
| `tsnet:listening` | Dynamic listener started |
| `tsnet:unlistened` | Dynamic listener stopped |
| `tsnet:pingResult` | Ping result |
| `tsnet:pushFileResult` | Taildrop push result |
| `tsnet:waitingFilesResult` | Waiting files list |
| `tsnet:getWaitingFileResult` | File download result |
| `tsnet:deleteWaitingFileResult` | File deletion result |
| `bridge:dialResult` | Dial success/failure |

**Source:** `/Users/jamesyong/Projects/project100/p008/truffle/crates/truffle-core/src/bridge/protocol.rs`

### 3.4 Bridge Connection Routing

After header parsing and token validation, the BridgeManager routes connections:

```
BridgeManager.run() accept loop (manager.rs:165-276)
    |
    +-- Read header with 2s timeout (manager.rs:206-221)
    +-- Verify session token (constant-time) (manager.rs:224-231)
    |
    +-- IF direction=Outgoing AND request_id non-empty:
    |       Look up pending_dials[request_id]
    |       Send BridgeConnection via oneshot::Sender
    |       (Completes the DialFn future on the Rust side)
    |
    +-- ELSE: Route by (port, direction) key:
            handlers[(port, direction)].handle(conn)
```

**Registered handlers (at bootstrap_sidecar, runtime.rs:630-663):**

| Port | Direction | Handler | Purpose |
|------|-----------|---------|---------|
| 443 | Incoming | HttpRouterBridgeHandler | Routes HTTP/WS to /ws handler |
| 9417 | Incoming | HttpRouterBridgeHandler | Same router, plain TCP |
| 443 | Outgoing | ChannelHandler | Delivers to WS client upgrade |
| 9417 | Outgoing | ChannelHandler | Delivers to WS client upgrade |
| 9418 | Incoming | TcpProxyHandler | Proxies to local axum file transfer server |
| 9418 | Outgoing | (via pending_dials) | Completed by DialFn for file transfer sender |

---

## 4. Per-Command Flow Diagrams

### 4.1 `truffle up`

```
User                CLI (up.rs)           DaemonServer         TruffleRuntime         GoShim            Tailscale
 |                      |                      |                     |                    |                  |
 |--truffle up--------->|                      |                     |                    |                  |
 |                      |                      |                     |                    |                  |
 |  [foreground mode]   |                      |                     |                    |                  |
 |                      |--DaemonServer::start()-->|                 |                    |                  |
 |                      |                      |                     |                    |                  |
 |                      |                      |--find sidecar----->check paths           |                  |
 |                      |                      |                     |                    |                  |
 |                      |                      |--start ft listener->|                    |                  |
 |                      |                      |  (axum :0 ephemeral)|                    |                  |
 |                      |                      |                     |                    |                  |
 |                      |                      |--TruffleRuntime::builder().build()       |                  |
 |                      |                      |  .bridge_handler(9418,In,TcpProxy)       |                  |
 |                      |                      |                     |                    |                  |
 |                      |                      |--runtime.start()-->|                     |                  |
 |                      |                      |                     |                    |                  |
 |                      |                      |                     |--bootstrap_sidecar->|                  |
 |                      |                      |                     |   1. gen token      |                  |
 |                      |                      |                     |   2. BridgeManager  |                  |
 |                      |                      |                     |      .bind(:0)      |                  |
 |                      |                      |                     |   3. Register        |                  |
 |                      |                      |                     |      handlers        |                  |
 |                      |                      |                     |   4. Spawn bridge    |                  |
 |                      |                      |                     |      accept loop     |                  |
 |                      |                      |                     |                    |                  |
 |                      |                      |                     |--GoShim::spawn()-->|                  |
 |                      |                      |                     |  stdin: tsnet:start |  tsnet.Start()   |
 |                      |                      |                     |  {hostname,stateDir,|-------->         |
 |                      |                      |                     |   bridgePort,token} |                  |
 |                      |                      |                     |                    |                  |
 |                      |                      |                     |                    |<--auth URL-----  |
 |                      |                      |                     |                    |  tsnet:authRequired
 |                      |                      |                     |<-lifecycle event---|                  |
 |                      |                      |                     |                    |                  |
 |                      |                      |                     |                    |<--Running------  |
 |                      |                      |                     |                    |  tsnet:status     |
 |                      |                      |                     |<-Status event------|                  |
 |                      |                      |                     |  set_local_online() |                  |
 |                      |                      |                     |  TruffleEvent::Online                 |
 |                      |                      |                     |                    |                  |
 |                      |                      |                     |  Go auto-listens:  |                  |
 |                      |                      |                     |    ListenTLS :443   |--accept loop--> |
 |                      |                      |                     |    Listen :9417     |--accept loop--> |
 |                      |                      |                     |                    |                  |
 |                      |                      |  shim.listen(9418)  |--tsnet:listen----->|                  |
 |                      |                      |  (for file xfer)    |                    |--Listen :9418--> |
 |                      |                      |                     |                    |                  |
 |                      |                      |  create_dial_fn()   |                    |                  |
 |                      |                      |  FileTransferAdapter|                    |                  |
 |                      |                      |  wire_file_transfer |                    |                  |
 |                      |                      |                     |                    |                  |
 |                      |                      |--write PID file     |                    |                  |
 |                      |                      |--bind IPC socket    |                    |                  |
 |                      |                      |--server.run()       |                    |                  |
 |                      |                      |  (accept loop)      |                    |                  |
 |                      |                      |                     |                    |                  |
 |  [background mode]   |                      |                     |                    |                  |
 |                      |--fork child process                        |                    |                  |
 |                      |  (truffle up --foreground)                  |                    |                  |
 |                      |--poll IPC socket until ready               |                    |                  |
 |                      |--request(STATUS)                           |                    |                  |
 |                      |--display status                            |                    |                  |
 |<--status display-----|                      |                     |                    |                  |
```

**Key files:**
- `up.rs:19-59` - Entry point, fork vs foreground decision
- `up.rs:65-219` - Foreground: start server, subscribe events, display dashboard
- `up.rs:222-361` - Background: fork child, poll for readiness
- `server.rs:58-234` - DaemonServer::start(): sidecar discovery, runtime build, file transfer wiring
- `runtime.rs:412-421` - start(): bootstrap_sidecar + mesh_node.start()
- `runtime.rs:612-721` - bootstrap_sidecar(): token gen, bridge bind, handler registration, shim spawn
- `main.go:308-345` - handleStart(): create tsnet.Server, start
- `main.go:347-408` - waitForRunning(): poll status, emit events, start listeners

### 4.2 `truffle status`

```
User           CLI (status.rs)     DaemonClient      DaemonServer     handler.rs        TruffleRuntime
 |                  |                   |                  |                |                  |
 |--truffle status->|                   |                  |                |                  |
 |                  |                   |                  |                |                  |
 |                  |--is_daemon_running?|                  |                |                  |
 |                  |  (check PID file) |                  |                |                  |
 |                  |                   |                  |                |                  |
 |                  |  IF not running:  |                  |                |                  |
 |<--"offline"------|  show offline msg |                  |                |                  |
 |                  |                   |                  |                |                  |
 |                  |  IF running:      |                  |                |                  |
 |                  |--request("status")|                  |                |                  |
 |                  |                   |--connect(sock)-->|                |                  |
 |                  |                   |--JSON-RPC------->|                |                  |
 |                  |                   |                  |--dispatch----->|                  |
 |                  |                   |                  |  handle_status |                  |
 |                  |                   |                  |                |--local_device()  |
 |                  |                   |                  |                |--auth_status()   |
 |                  |                   |                  |                |--devices().len() |
 |                  |                   |                  |<--response-----|                  |
 |                  |                   |<--JSON-RPC-------|                |                  |
 |                  |                   |                  |                |                  |
 |                  |--request("peers")-|  (same flow)     |                |                  |
 |                  |                   |                  |                |                  |
 |<--dashboard------|                   |                  |                |                  |
```

**Key files:**
- `status.rs:28-95` - run_once: check PID, request status + peers, print dashboard
- `handler.rs:82-121` - handle_status: reads MeshNode state, returns JSON
- `handler.rs:124-151` - handle_peers: reads DeviceManager, filters self, returns JSON

**Data transformations:**
1. `DaemonRequest{method:"status"}` -> JSON-RPC line over Unix socket
2. Handler reads `MeshNode.local_device()` -> `BaseDevice` struct
3. `DaemonResponse{result:{device_id, name, status, ip, dns, uptime_secs}}` -> JSON-RPC line
4. CLI formats as dashboard table

### 4.3 `truffle ls`

```
User         CLI (ls.rs)       DaemonClient    handler.rs      MeshNode        DeviceManager
 |               |                  |               |               |                |
 |--truffle ls-->|                  |               |               |                |
 |               |--ensure_running->|               |               |                |
 |               |  (auto_up if    |               |               |                |
 |               |   configured)   |               |               |                |
 |               |                  |               |               |                |
 |               |--request("peers")|               |               |                |
 |               |                  |--JSON-RPC---->|               |               |
 |               |                  |               |--handle_peers->|               |
 |               |                  |               |               |--devices()---->|
 |               |                  |               |               |               |--collect all
 |               |                  |               |               |<--Vec<BaseDevice>
 |               |                  |               |               |                |
 |               |                  |               |--filter self   |               |
 |               |                  |               |--map to JSON   |               |
 |               |                  |<--response----|               |               |
 |               |                  |               |               |               |
 |               |--filter by --all |               |               |               |
 |               |--format table    |               |               |               |
 |<--table-------|                  |               |               |               |
```

**Key files:**
- `ls.rs:18-143` - Main flow: ensure_running, request peers, filter, format
- `handler.rs:124-151` - handle_peers: `runtime.devices()`, filter `d.id != local_id`, serialize

**Peer data source:** `DeviceManager.devices` HashMap, populated by:
1. `device-announce` mesh messages from WebSocket peers (via TransportHandler)
2. `tsnet:peers` events from Go sidecar (via lifecycle handler in runtime.rs:795-801)

### 4.4 `truffle ping`

```
User           CLI (ping.rs)     NameResolver    DaemonClient    handler.rs     DeviceManager
 |                  |                 |               |               |               |
 |--truffle ping -->|                 |               |               |               |
 |   laptop         |                 |               |               |               |
 |                  |--request("peers")|              |               |               |
 |                  |  (for name resolution)          |               |               |
 |                  |                 |               |               |               |
 |                  |--NameResolver::resolve("laptop")|               |               |
 |                  |                 |--match name-->|               |               |
 |                  |                 |  returns IP,  |               |               |
 |                  |                 |  device_id    |               |               |
 |                  |                 |               |               |               |
 |    [for each ping in count]:      |               |               |               |
 |                  |--request("ping",{node,device_id})              |               |
 |                  |                 |               |--handle_ping->|               |
 |                  |                 |               |               |--devices()    |
 |                  |                 |               |               |--find by id   |
 |                  |                 |               |               |--check online |
 |                  |                 |               |<--reachable,  |               |
 |                  |                 |               |   latency_ms  |               |
 |                  |<--response------|               |               |               |
 |<--"reply from laptop: time=2.1ms"|               |               |               |
 |                  |                 |               |               |               |
 |    [print stats: min/avg/max, loss%]              |               |               |
```

**Key files:**
- `ping.rs:18-175` - Resolve name, loop pings, show stats
- `resolve.rs:140-237` - NameResolver::resolve: alias > name > hostname > IP > DNS
- `handler.rs:157-226` - handle_ping: looks up device in DeviceManager, returns online status

**IMPORTANT NOTE:** The current ping implementation does NOT actually ping through the Tailscale network. It only checks the device's online status in the DeviceManager (handler.rs:192). The `connection` field is hardcoded to `"direct"` for online peers (handler.rs:194-198). A future version should use the Go sidecar's `tsnet:ping` command.

### 4.5 `truffle send`

```
User               CLI (send.rs)     DaemonClient      handler.rs       MeshNode         ConnectionMgr
 |                      |                 |                 |                |                  |
 |--truffle send------->|                 |                 |                |                  |
 |  laptop "hello"      |                 |                 |                |                  |
 |                      |--request("peers")|               |                |                  |
 |                      |--resolve("laptop")|              |                |                  |
 |                      |                 |                 |                |                  |
 |                      |--request("send_message",         |                |                  |
 |                      |  {device_id, namespace:"chat",   |                |                  |
 |                      |   type:"text",                   |                |                  |
 |                      |   payload:{text:"hello"}})       |                |                  |
 |                      |                 |                 |                |                  |
 |                      |                 |--handle_send_message----------->|                  |
 |                      |                 |                 |                |                  |
 |                      |                 |                 |  MeshEnvelope::new("chat","text") |
 |                      |                 |                 |                |                  |
 |                      |                 |                 |--send_envelope(device_id, env)--->|
 |                      |                 |                 |                |                  |
 |                      |                 |                 |                |  find conn_id for|
 |                      |                 |                 |                |  device_id       |
 |                      |                 |                 |                |                  |
 |                      |                 |                 |                |--send(conn_id,   |
 |                      |                 |                 |                |  serialized      |
 |                      |                 |                 |                |  envelope)------>|
 |                      |                 |                 |                |                  |
 |                      |                 |                 |                |  WS binary frame |
 |                      |                 |                 |                |  -> bridge TCP   |
 |                      |                 |                 |                |  -> Go sidecar   |
 |                      |                 |                 |                |  -> tsnet tunnel  |
 |                      |                 |                 |                |  -> remote peer   |
 |                      |                 |                 |                |                  |
 |                      |                 |<--{sent:true}---|                |                  |
 |<--"Sent to laptop"---|                 |                 |                |                  |
```

**Key files:**
- `send.rs:25-163` - Parse args, resolve name, request send_message
- `handler.rs:229-307` - handle_send_message: build MeshEnvelope, send or broadcast
- `node.rs` MeshNode::send_envelope -> ConnectionManager::send -> WS frame

**Data transformations:**
1. CLI: `{device_id, namespace:"chat", type:"text", payload:{text:"hello"}}` (JSON-RPC params)
2. Handler: `MeshEnvelope{namespace:"chat", msg_type:"text", payload:{text:"hello"}, timestamp}`
3. MeshNode: serialized to JSON, sent as WS binary frame via ConnectionManager
4. Transport: v3 wire frame (2-byte header + msgpack/JSON payload)

### 4.6 `truffle cp` (Upload)

```
User           CLI (cp.rs)      DaemonClient    handler.rs       Adapter         Manager        GoShim
 |                 |                 |               |               |               |               |
 |--truffle cp --->|                 |               |               |               |               |
 | file.txt        |                 |               |               |               |               |
 | server:/tmp/    |                 |               |               |               |               |
 |                 |                 |               |               |               |               |
 |  parse_location("file.txt")=Local |               |               |               |               |
 |  parse_location("server:/tmp/")=Remote            |               |               |               |
 |  direction = Upload               |               |               |               |               |
 |                 |                 |               |               |               |               |
 |                 |--verify file exists              |               |               |               |
 |                 |                 |               |               |               |               |
 |                 |--request_with_notifications(     |               |               |               |
 |                 |  "push_file",{node,local_path,   |               |               |               |
 |                 |   remote_path})                  |               |               |               |
 |                 |                 |               |               |               |               |
 |                 |                 |----------handle_push_file----->|               |               |
 |                 |                 |               |               |               |               |
 |                 |                 |               |  1. resolve node -> device_id  |               |
 |                 |                 |               |  2. generate transfer_id, token|               |
 |                 |                 |               |  3. subscribe to manager events|               |
 |                 |                 |               |                               |               |
 |                 |                 |               |  4. manager.prepare_file()---->|               |
 |                 |                 |               |     (stat + SHA-256 hash)      |               |
 |                 |                 |               |                               |               |
 |                 |                 |               |  5. wait for Prepared event <--|               |
 |                 |                 |               |     (file_name, size, sha256)  |               |
 |                 |                 |               |                               |               |
 |                 |                 |               |  6. adapter.register_pending_send()            |
 |                 |                 |               |                               |               |
 |                 |                 |               |  7. adapter.send_offer(device_id, offer)       |
 |                 |                 |               |     offer = {transfer_id,      |               |
 |                 |                 |               |      sender_device_id,          |               |
 |                 |                 |               |      sender_addr (dns:9418),    |               |
 |                 |                 |               |      file, token, cli_mode:true,|               |
 |                 |                 |               |      save_path}                 |               |
 |                 |                 |               |                               |               |
 |                 |                 |               |     bus_tx.send() -->           |               |
 |                 |                 |               |     [outgoing pump task]         |               |
 |                 |                 |               |     MeshEnvelope("file-transfer","OFFER",...)  |
 |                 |                 |               |     MeshNode.send_envelope()    |               |
 |                 |                 |               |     -> WS frame to peer         |               |
 |                 |                 |               |                               |               |
 |   REMOTE PEER receives OFFER     |               |                               |               |
 |   (auto-accepts because cli_mode=true)            |                               |               |
 |   sends ACCEPT back via mesh     |               |                               |               |
 |                 |                 |               |                               |               |
 |                 |                 |               |  [incoming pump task receives ACCEPT]           |
 |                 |                 |               |  adapter.handle_bus_message("ACCEPT",...)       |
 |                 |                 |               |                               |               |
 |                 |                 |               |  8. manager.register_send()    |               |
 |                 |                 |               |  9. spawn send_file task:      |               |
 |                 |                 |               |                               |               |
 |                 |                 |               |     a. open local file          |               |
 |                 |                 |               |     b. HEAD to query resume offset              |
 |                 |                 |               |     c. dial_fn(receiver_addr)--->|              |
 |                 |                 |               |        (creates pending_dial)   |               |
 |                 |                 |               |        shim.dial_raw(dns,9418)-->|--bridge:dial->|
 |                 |                 |               |                               |               |
 |                 |                 |               |        Go dials tsnet:9418     |               |
 |                 |                 |               |        Go bridges back to Rust  |               |
 |                 |                 |               |        pending_dial completes   |               |
 |                 |                 |               |                               |               |
 |                 |                 |               |     d. HTTP PUT /transfer/{id}  |               |
 |                 |                 |               |        with file body           |               |
 |                 |                 |               |        (via hyper over TcpStream)|              |
 |                 |                 |               |                               |               |
 |<--progress-----|  notification_tx  <-- Progress events from manager               |               |
 |  (cp.progress)  |                 |               |                               |               |
 |                 |                 |               |                               |               |
 |                 |                 |<--Complete event (sha256, size, duration_ms)   |               |
 |<--"SHA-256:..." |                 |               |                               |               |
```

**Key files:**
- `cp.rs:81-164` - run: parse locations, determine direction, call do_upload/do_download
- `cp.rs:171-288` - do_upload: validate file, request_with_notifications("push_file")
- `handler.rs:331-600` - handle_push_file: prepare file, send OFFER, wait for events
- `adapter.rs:306-319` - send_offer: serialize to JSON, send via bus_tx
- `adapter.rs:414-449` - ACCEPT handler: register_send, spawn send_file
- `sender.rs:21-145` - send_file: open file, HEAD resume, dial, HTTP PUT
- `server.rs:474-554` - create_dial_fn: dial through GoShim bridge

### 4.7 `truffle cp` (Download)

```
User           CLI (cp.rs)       DaemonClient     handler.rs        Adapter           Remote Node
 |                 |                  |                |                 |                   |
 |--truffle cp --->|                  |                |                 |                   |
 | server:/f.txt . |                  |                |                 |                   |
 |                 |                  |                |                 |                   |
 |                 |--request_with_notifications(      |                 |                   |
 |                 |  "get_file",{node,remote_path,    |                 |                   |
 |                 |   local_path})                    |                 |                   |
 |                 |                  |                |                 |                   |
 |                 |                  |----handle_get_file------------->|                   |
 |                 |                  |                |                 |                   |
 |                 |                  |                | 1. resolve node -> device_id       |
 |                 |                  |                | 2. generate request_id              |
 |                 |                  |                | 3. subscribe to manager events      |
 |                 |                  |                |                 |                   |
 |                 |                  |                | 4. adapter.send_pull_request()      |
 |                 |                  |                |    PULL_REQUEST{request_id,         |
 |                 |                  |                |     remote_path,                    |
 |                 |                  |                |     requester_device_id,            |
 |                 |                  |                |     save_path}                      |
 |                 |                  |                |    -> bus_tx -> mesh envelope        |
 |                 |                  |                |    -> WS frame to remote peer------->|
 |                 |                  |                |                 |                   |
 |                 |                  |                |  REMOTE PEER:                       |
 |                 |                  |                |  adapter.handle_bus_message("PULL_REQUEST")
 |                 |                  |                |    - validate path exists            |
 |                 |                  |                |    - prepare_file (stat + SHA-256)   |
 |                 |                  |                |    - register_pending_send           |
 |                 |                  |                |    - send OFFER back (cli_mode=true, |
 |                 |                  |                |      save_path = local dest path)    |
 |                 |                  |                |                 |                   |
 |                 |                  |                |  LOCAL: receives OFFER               |
 |                 |                  |                |  (cli_mode=true, auto-accept)        |
 |                 |                  |                |    - register_receive in manager     |
 |                 |                  |                |    - send ACCEPT back to remote      |
 |                 |                  |                |                 |                   |
 |                 |                  |                |  REMOTE: receives ACCEPT              |
 |                 |                  |                |    - register_send + spawn send_file  |
 |                 |                  |                |    - dial our_dns:9418               |
 |                 |                  |                |    - HTTP PUT /transfer/{id}         |
 |                 |                  |                |                 |                   |
 |                 |                  |                |  LOCAL: axum receives PUT             |
 |                 |                  |                |    - validate transfer + token        |
 |                 |                  |                |    - write to disk                    |
 |                 |                  |                |    - emit Progress, Complete events   |
 |                 |                  |                |                 |                   |
 |<--progress------|  notification_tx  <-- Progress events              |                   |
 |<--complete------|                  <-- Complete event (sha256, etc.)  |                   |
```

**Key files:**
- `cp.rs:295-402` - do_download: request_with_notifications("get_file")
- `handler.rs` - handle_get_file (similar structure to handle_push_file, sends PULL_REQUEST)
- `adapter.rs:471-599` - PULL_REQUEST handler: validate path, prepare, send OFFER back
- `adapter.rs:393-411` - OFFER handler (cli_mode=true): auto-accept
- `receiver.rs:14-18` - file_transfer_router: axum `PUT /transfer/{id}`, `HEAD /transfer/{id}`
- `receiver.rs:75-150` - handle_put: validate token, write to disk, emit events

### 4.8 `truffle tcp`

```
User            CLI (tcp.rs)      DaemonClient     handler.rs
 |                  |                  |                |
 |--truffle tcp --->|                  |                |
 | server:5432      |                  |                |
 |                  |                  |                |
 |  1. Parse target |                  |                |
 |  2. request("tcp_connect",         |                |
 |     {target,node,port,check})      |                |
 |                  |                  |                |
 |  [check mode]:   |                  |                |
 |  Return {connected:true/false}      |                |
 |                  |                  |                |
 |  [stream mode]:  |                  |                |
 |  3. Get fresh IPC connection        |                |
 |  4. Send JSON-RPC {stream:true}     |                |
 |  5. Read upgrade response           |                |
 |  6. Extract raw reader/writer       |                |
 |  7. pipe_stdio(reader, writer)      |                |
 |     stdin -> IPC socket -> daemon   |                |
 |     daemon -> IPC socket -> stdout  |                |
 |                  |                  |                |
 |  [interactive]   |                  |                |
 |  Type data ----->|  stdin bytes --> daemon           |
 |  Receive data <--|  daemon bytes -> stdout           |
```

**Key files:**
- `tcp.rs:26-134` - Parse target, check mode vs stream mode
- `stream.rs` - pipe_stdio: bidirectional copy between stdin/stdout and IPC socket

**Status:** The TCP streaming path requires the daemon handler (`handle_tcp_connect`) to establish the actual tsnet TCP connection and bridge it to the IPC socket. The handler implementation at `handler.rs:55` dispatches to `handle_tcp_connect`, which needs to use the GoShim bridge:dial to connect.

### 4.9 `truffle ws`

```
User           CLI (ws.rs)       DaemonClient      handler.rs
 |                 |                  |                 |
 |--truffle ws --->|                  |                 |
 | server:8080/ws  |                  |                 |
 |                 |                  |                 |
 |  1. Parse target (node, port, path)|                 |
 |  2. Get fresh IPC connection       |                 |
 |  3. Send JSON-RPC ws_connect       |                 |
 |     {url, node, port, path}        |                 |
 |  4. Read upgrade response          |                 |
 |  5. Enter ws_repl loop:            |                 |
 |     stdin line -> write_line(IPC)   |                 |
 |     read_line(IPC) -> stdout        |                 |
 |                 |                  |                 |
 |  [interactive REPL]                |                 |
 |  > {"subscribe":"events"}          |                 |
 |  < {"type":"ack"}                  |                 |
```

**Key files:**
- `ws.rs:28-107` - Parse target, connect, send ws_connect, enter REPL
- `ws.rs:116-169` - ws_repl: bidirectional line-based loop (stdin <-> IPC)

**Note:** The daemon handler (`handle_ws_connect` at handler.rs:56) needs to establish the WebSocket connection through the tsnet tunnel and relay frames between the IPC socket and the remote WebSocket.

### 4.10 `truffle chat`

```
User           CLI (chat.rs)     DaemonClient      handler.rs       MeshNode
 |                 |                  |                 |                |
 |--truffle chat ->|                  |                 |                |
 |  laptop         |                  |                 |                |
 |                 |                  |                 |                |
 |  1. Get fresh IPC connection       |                 |                |
 |  2. Send JSON-RPC "chat_start"     |                 |                |
 |     {node: "laptop"}               |                 |                |
 |  3. Read handshake response        |                 |                |
 |     {result: {local_name: "you"}}  |                 |                |
 |                 |                  |                 |                |
 |  4. Enter chat_loop:               |                 |                |
 |     stdin line -> JSON event       |                 |                |
 |       {type:"message",text:"hi"}   |                 |                |
 |       -> write_line(IPC)           |                 |                |
 |                 |                  |                 |                |
 |     read_line(IPC) -> parse event  |                 |                |
 |       {type:"message",from:"laptop",text:"hey"}     |                |
 |       -> println "[14:23] laptop: hey"               |                |
 |                 |                  |                 |                |
 |     {type:"presence",node:"laptop",status:"typing"}  |                |
 |       -> println "* laptop is typing"                |                |
```

**Key files:**
- `chat.rs:28-106` - Connect, chat_start handshake, enter chat_loop
- `chat.rs:113-200` - chat_loop: bidirectional JSON event loop
- `handler.rs:63` - "chat_start" dispatches to handle_chat_start

**Data format:** Chat uses a streaming JSON-line protocol over the IPC socket:
- Outgoing: `{type:"message", text:"..."}` lines from stdin
- Incoming: `{type:"message", from:"laptop", text:"...", ts:"14:23"}` lines from daemon
- Presence: `{type:"presence", node:"laptop", status:"typing"}` lines

### 4.11 `truffle down`

```
User          CLI (down.rs)     DaemonClient     handler.rs      DaemonServer     TruffleRuntime
 |                |                  |                |                |                |
 |--truffle down->|                  |                |                |                |
 |                |                  |                |                |                |
 |                |--is_daemon_running?               |                |                |
 |                |  (PID file check)|                |                |                |
 |                |                  |                |                |                |
 |                |  IF not running: |                |                |                |
 |<--"not running"|                  |                |                |                |
 |                |                  |                |                |                |
 |                |--request("peers")| (get count)    |                |                |
 |                |                  |                |                |                |
 |                |--request("shutdown")              |                |                |
 |                |                  |--handle_shutdown()              |                |
 |                |                  |                |                |                |
 |                |                  |  shutdown_signal.notify_one()   |                |
 |                |                  |                |                |                |
 |                |                  |<--{shutting_down:true}          |                |
 |                |                  |                |                |                |
 |                |                  |    server.run() accept loop exits               |
 |                |                  |                |--cleanup()---->|                |
 |                |                  |                |                |--ft_manager.stop()
 |                |                  |                |                |--runtime.stop()-->|
 |                |                  |                |                |                |--shim.stop()
 |                |                  |                |                |                |  stdin: tsnet:stop
 |                |                  |                |                |                |--mesh_node.stop()
 |                |                  |                |                |                |
 |                |                  |                |  remove socket |                |
 |                |                  |                |  remove PID    |                |
 |                |                  |                |                |                |
 |                |  poll until daemon exits          |                |                |
 |                |  (PID file gone) |                |                |                |
 |<--"Node stopped"|                 |                |                |                |
```

**Key files:**
- `down.rs:14-99` - Check running, get peer count, send shutdown, poll, force-kill fallback
- `handler.rs:309-314` - handle_shutdown: notify shutdown signal
- `server.rs:284-308` - Accept loop exits on shutdown notification
- `server.rs:422-439` - cleanup: stop ft_manager, stop runtime, remove socket+PID

---

## 5. Pending Dials Map Analysis

### 5.1 The Two HashMaps

There are **two separate** `HashMap<String, oneshot::Sender<BridgeConnection>>` instances in the system:

#### Map A: `TruffleRuntime.bridge_pending_dials`
- **Location:** `runtime.rs:346`
- **Type:** `Arc<Mutex<HashMap<String, oneshot::Sender<BridgeConnection>>>>`
- **Created:** In `TruffleRuntime::builder().build()` at `runtime.rs:316`
- **Used by:** `TruffleRuntime::dial_peer()` at `runtime.rs:572-610`
- **Purpose:** For the runtime's own `dial_peer()` method (used for mesh WS connections)

#### Map B: `BridgeManager.pending_dials`
- **Location:** `manager.rs:113`
- **Type:** `Arc<Mutex<HashMap<String, oneshot::Sender<BridgeConnection>>>>`
- **Created:** In `BridgeManager::bind()` at `manager.rs:127`
- **The authoritative map:** This is the one the BridgeManager's accept loop actually reads from (manager.rs:240-254)

### 5.2 How They Are Connected

During `bootstrap_sidecar()` (runtime.rs:666-670):

```rust
// runtime.rs:669
let bridge_pending = bridge_manager.pending_dials().clone();
*self.bridge_manager_pending.lock().await = Some(bridge_pending.clone());
```

The `bridge_manager_pending` field stores a reference to Map B. The `pending_dials()` accessor (runtime.rs:481-488) checks this field:

```rust
pub async fn pending_dials(&self) -> Arc<Mutex<HashMap<...>>> {
    let guard = self.bridge_manager_pending.lock().await;
    if let Some(ref arc) = *guard {
        arc.clone()       // Returns Map B (BridgeManager's map) -- CORRECT
    } else {
        self.bridge_pending_dials.clone()  // Returns Map A -- WRONG (before start())
    }
}
```

### 5.3 The File Transfer DialFn and Its HashMap

The `create_dial_fn()` in `server.rs:474-554` calls:

```rust
// server.rs:476
let pending = runtime.pending_dials().await;
```

This is called **during `DaemonServer::start()`**, AFTER `runtime.start()` has been called (server.rs:128-132). By this time, `bootstrap_sidecar()` has completed and `bridge_manager_pending` is set. Therefore, `pending_dials()` returns **Map B** (the BridgeManager's map).

The DialFn closure captures this `pending` Arc and uses it at server.rs:502-504:

```rust
let mut dials = pending.lock().await;
dials.insert(request_id.clone(), tx);
```

Then when the Go sidecar completes the dial and bridges back, the BridgeManager's accept loop reads from the **same Map B** (manager.rs:240-241):

```rust
let mut dials = pending_dials.lock().await;
if let Some(tx) = dials.remove(&conn.header.request_id) {
    tx.send(conn) ...
}
```

### 5.4 Potential Issue: Race Between Maps

The `dial_peer()` method at runtime.rs:578 inserts into Map A (`self.bridge_pending_dials`), NOT Map B. This means:

- **`dial_peer()`** (used for mesh WS connections): Inserts into Map A, but BridgeManager reads from Map B. **This is a bug if dial_peer() is called after bootstrap.** However, looking at the code, `dial_peer()` is currently used for manual peer dialing and the lifecycle handler's peer dial loop, which may not be active in the CLI daemon flow.

- **`create_dial_fn()`** (used for file transfer): Correctly uses Map B. No bug here.

### 5.5 Summary

| Component | Map Used | Correct? |
|-----------|----------|----------|
| `BridgeManager.run()` accept loop | Map B (own `pending_dials`) | Baseline |
| `create_dial_fn()` in server.rs | Map B (via `runtime.pending_dials()`) | YES |
| `TruffleRuntime::dial_peer()` | Map A (`bridge_pending_dials`) | POTENTIAL BUG |
| Lifecycle handler peer dial | Uses `dial_peer()` -> Map A | POTENTIAL BUG |

---

## 6. File Transfer Flow: Detailed OFFER -> ACCEPT -> Send Path

### 6.1 Upload Flow (Step by Step)

#### Step 1: CLI sends `push_file` JSON-RPC

```
cp.rs:248-260
  client.request_with_notifications("push_file", {
    node: "server",
    local_path: "/abs/path/file.txt",
    remote_path: "~/Downloads/truffle/file.txt"
  }, |notif| { render_progress(notif) })
```

#### Step 2: Handler prepares file

```
handler.rs:331-487
  1. Parse params (node, local_path, remote_path)
  2. Validate file exists and is a regular file
  3. Resolve "server" -> device_id via resolve_node_device_id()
  4. Generate transfer_id = "ft-<uuid>" and token = <64 hex chars>
  5. Subscribe to manager events BEFORE preparing
  6. manager.prepare_file(&transfer_id, local_path)
     -> Spawns task that stats file, computes SHA-256, emits Prepared event
  7. Wait for Prepared event with 60s timeout
     -> Gets (file_name, file_size, file_sha256)
```

#### Step 3: Handler registers and sends OFFER

```
handler.rs:496-529
  8. adapter.register_pending_send(transfer_id, local_path, file_info, device_id)
     -> Stores file_path in adapter.file_paths HashMap
     -> Stores AdapterTransferInfo in adapter.transfers HashMap

  9. Construct offer:
     sender_addr = local_device.tailscale_dns_name + ":9418"
     FileTransferOffer {
       transfer_id, sender_device_id, sender_addr,
       file: {name, size, sha256}, token, cli_mode: true,
       save_path: Some(remote_path)
     }

  10. adapter.send_offer(device_id, &offer)
      -> Serializes to JSON
      -> bus_tx.send((device_id, "OFFER", json))

      [Outgoing pump task (integration.rs:85-120)]:
      -> MeshEnvelope::new("file-transfer", "OFFER", payload)
      -> MeshNode.send_envelope(device_id, envelope)
      -> ConnectionManager.send(conn_id, serialized_envelope)
      -> WS binary frame -> bridge TCP -> Go -> tsnet tunnel -> REMOTE PEER
```

#### Step 4: Remote receives OFFER, auto-accepts

```
Remote's integration.rs incoming pump:
  MeshNodeEvent::Message { namespace: "file-transfer", msg_type: "OFFER", ... }
  -> adapter.handle_bus_message("OFFER", payload)

adapter.rs:393-411:
  - Parse FileTransferOffer
  - cli_mode=true && save_path is Some && non-empty
  - Call self.accept_transfer(&offer, Some(save_path))

adapter.rs:190-246:
  accept_transfer():
    1. manager.register_receive(transfer_id, file, token, save_path)
       -> Stores Transfer in manager.transfers HashMap
       -> Transfer has state=Registered, save_path set
    2. Construct FileTransferAccept {
         transfer_id, receiver_device_id, receiver_addr (dns:9418), token
       }
    3. bus_tx.send((sender_device_id, "ACCEPT", json))
       -> MeshEnvelope -> WS frame -> bridge -> Go -> tsnet -> ORIGINAL SENDER
```

#### Step 5: Sender receives ACCEPT, starts sending

```
Sender's integration.rs incoming pump:
  MeshNodeEvent::Message { namespace: "file-transfer", msg_type: "ACCEPT", ... }
  -> adapter.handle_bus_message("ACCEPT", payload)

adapter.rs:414-449:
  - Parse FileTransferAccept
  - Look up file_paths[transfer_id] -> original local file path
  - Look up transfers[transfer_id] -> AdapterTransferInfo
  - manager.register_send(transfer_id, file, token, path)
    -> Stores Transfer in manager.transfers as sender
  - tokio::spawn:
      manager.send_file(transfer, receiver_addr, dial_fn)
```

#### Step 6: send_file dials and sends HTTP PUT

```
sender.rs:21-145:
  send_file():
    1. Open local file
    2. HEAD /transfer/{id} to query resume offset (via dial_fn)
    3. Seek if resuming
    4. Build ProgressReader wrapping the file
    5. Build StreamBody from ProgressReader
    6. Build HTTP PUT request:
       URI: http://{receiver_addr}/transfer/{transfer_id}
       Headers: x-transfer-token, x-file-name, x-file-sha256, content-length
    7. dial_fn(receiver_addr) -> TcpStream

       [Inside dial_fn (server.rs:478-553)]:
         a. Parse host:port from receiver_addr (e.g., "dns.name:9418")
         b. Generate request_id UUID
         c. Insert (request_id, oneshot::Sender) into pending_dials (Map B)
         d. shim.dial_raw(host, 9418, request_id)
            -> stdin: {"command":"bridge:dial","data":{"requestId":"...","target":"dns.name","port":9418}}

         [Go sidecar main.go:548-598]:
           e. s.server.Dial(ctx, "tcp", "dns.name:9418") -- tsnet dial through WireGuard
           f. (Port 9418 is NOT 443, so no TLS wrapping)
           g. s.bridgeToRust(conn, 9418, dirOutgoing, requestId, addr, target)
              -> Connect to 127.0.0.1:<bridge_port>
              -> Write binary header (TRFF, token, Outgoing, 9418, requestId, ...)
              -> Bidirectional copy

         [Rust BridgeManager accept loop (manager.rs:237-254)]:
           h. Read header, verify token
           i. direction=Outgoing, request_id non-empty
           j. Remove from pending_dials[request_id], send BridgeConnection via oneshot

         [Back in dial_fn]:
           k. Receive BridgeConnection from oneshot
           l. Return bridge_conn.stream (TcpStream)

    8. hyper HTTP/1.1 over the TcpStream
       -> PUT /transfer/{id} with streaming body
```

#### Step 7: Receiver's axum handles PUT

```
receiver.rs:75-200+:
  handle_put():
    1. Extract transfer_id from URL path
    2. Validate x-transfer-token header
    3. manager.get_transfer(id, token) -- constant-time token compare
    4. Check state = Registered -> transition to Transferring
    5. Create output directory, open file for writing
    6. Read body bytes in chunks, write to disk
    7. Compute SHA-256 on-the-fly
    8. Emit Progress events periodically
    9. On completion: verify SHA-256, emit Complete event
```

#### Step 8: Handler receives Complete, responds to CLI

```
handler.rs:570-592:
  - Receives FileTransferEvent::Complete for matching transfer_id
  - Returns DaemonResponse::success with {transfer_id, bytes_transferred, sha256, duration_ms}

  Progress events (handler.rs:547-568):
  - Receives FileTransferEvent::Progress
  - Sends DaemonNotification("cp.progress", {transfer_id, bytes_transferred, total_bytes, percent, bytes_per_second, eta})
  - client.rs:185-218 reads notifications and calls on_notification callback
  - cp.rs:412-417 render_progress: calls output::print_progress
```

### 6.2 Download Flow

The download flow uses PULL_REQUEST as an additional step:

1. **Local daemon** sends `PULL_REQUEST` to remote via mesh (handler.rs `handle_get_file`)
2. **Remote adapter** receives PULL_REQUEST (adapter.rs:471-599):
   - Validates path exists
   - Prepares file (stat + SHA-256)
   - Sends OFFER back with `cli_mode:true` and `save_path` = local destination
3. **Local adapter** receives OFFER, auto-accepts (adapter.rs:393-411)
4. **Remote adapter** receives ACCEPT, starts send_file (adapter.rs:414-449)
5. Same HTTP PUT flow as upload (just the direction is reversed)

### 6.3 Where File Transfer Could Break

1. **Name resolution failure:** `resolve_node_device_id()` can't find the target node in DeviceManager
2. **File preparation timeout:** SHA-256 hashing of large files takes >60s
3. **OFFER delivery failure:** No WS connection to the remote peer (peer offline, no mesh connection established)
4. **Auto-accept failure:** Remote adapter doesn't have `cli_mode` support or malformed OFFER
5. **ACCEPT not received:** 60s timeout in handler.rs waiting for Complete/Error event
6. **Dial failure:** GoShim can't dial target's port 9418 (target not listening on 9418, network issue)
7. **Dial timeout:** pending_dials entry expires (10s timeout in create_dial_fn at server.rs:531)
8. **HTTP PUT failure:** Connection drops during transfer (resumable via HEAD offset query)
9. **SHA-256 mismatch:** Integrity check fails after transfer
10. **Bridge port exhaustion:** BridgeManager semaphore (256 max concurrent, manager.rs:14)

### 6.4 File Transfer Port 9418 Listener Setup

The port 9418 listener is set up in two places:

1. **Bridge handler registration** (before runtime.start):
   ```
   server.rs:115
   let ft_proxy = Arc::new(TcpProxyHandler::new(format!("127.0.0.1:{ft_listener_port}")));

   server.rs:122
   .bridge_handler(9418, Direction::Incoming, ft_proxy)
   ```
   This registers a TcpProxyHandler that forwards incoming bridge connections on port 9418
   to the local axum HTTP server.

2. **Dynamic tsnet listener** (after runtime.start):
   ```
   server.rs:137-142
   let shim_guard = runtime.shim().lock().await;
   if let Some(ref s) = *shim_guard {
       let _ = s.listen(9418, false).await;
   }
   ```
   This tells the Go sidecar to start listening on port 9418 via tsnet, so remote peers
   can connect. The sidecar's accept loop calls `bridgeToRust(conn, 9418, dirIncoming, ...)`.

---

## Appendix: Key File Paths

### CLI Layer
- `/Users/jamesyong/Projects/project100/p008/truffle/crates/truffle-cli/src/main.rs`
- `/Users/jamesyong/Projects/project100/p008/truffle/crates/truffle-cli/src/commands/up.rs`
- `/Users/jamesyong/Projects/project100/p008/truffle/crates/truffle-cli/src/commands/down.rs`
- `/Users/jamesyong/Projects/project100/p008/truffle/crates/truffle-cli/src/commands/status.rs`
- `/Users/jamesyong/Projects/project100/p008/truffle/crates/truffle-cli/src/commands/ls.rs`
- `/Users/jamesyong/Projects/project100/p008/truffle/crates/truffle-cli/src/commands/ping.rs`
- `/Users/jamesyong/Projects/project100/p008/truffle/crates/truffle-cli/src/commands/send.rs`
- `/Users/jamesyong/Projects/project100/p008/truffle/crates/truffle-cli/src/commands/cp.rs`
- `/Users/jamesyong/Projects/project100/p008/truffle/crates/truffle-cli/src/commands/tcp.rs`
- `/Users/jamesyong/Projects/project100/p008/truffle/crates/truffle-cli/src/commands/ws.rs`
- `/Users/jamesyong/Projects/project100/p008/truffle/crates/truffle-cli/src/commands/chat.rs`

### Daemon Layer
- `/Users/jamesyong/Projects/project100/p008/truffle/crates/truffle-cli/src/daemon/server.rs`
- `/Users/jamesyong/Projects/project100/p008/truffle/crates/truffle-cli/src/daemon/handler.rs`
- `/Users/jamesyong/Projects/project100/p008/truffle/crates/truffle-cli/src/daemon/client.rs`
- `/Users/jamesyong/Projects/project100/p008/truffle/crates/truffle-cli/src/daemon/ipc.rs`
- `/Users/jamesyong/Projects/project100/p008/truffle/crates/truffle-cli/src/daemon/protocol.rs`
- `/Users/jamesyong/Projects/project100/p008/truffle/crates/truffle-cli/src/resolve.rs`

### Core Runtime
- `/Users/jamesyong/Projects/project100/p008/truffle/crates/truffle-core/src/runtime.rs`

### Bridge Layer
- `/Users/jamesyong/Projects/project100/p008/truffle/crates/truffle-core/src/bridge/manager.rs`
- `/Users/jamesyong/Projects/project100/p008/truffle/crates/truffle-core/src/bridge/header.rs`
- `/Users/jamesyong/Projects/project100/p008/truffle/crates/truffle-core/src/bridge/shim.rs`
- `/Users/jamesyong/Projects/project100/p008/truffle/crates/truffle-core/src/bridge/protocol.rs`

### Transport Layer
- `/Users/jamesyong/Projects/project100/p008/truffle/crates/truffle-core/src/transport/connection.rs`

### Mesh Layer
- `/Users/jamesyong/Projects/project100/p008/truffle/crates/truffle-core/src/mesh/node.rs`
- `/Users/jamesyong/Projects/project100/p008/truffle/crates/truffle-core/src/mesh/device.rs`
- `/Users/jamesyong/Projects/project100/p008/truffle/crates/truffle-core/src/mesh/handler.rs`

### File Transfer
- `/Users/jamesyong/Projects/project100/p008/truffle/crates/truffle-core/src/services/file_transfer/manager.rs`
- `/Users/jamesyong/Projects/project100/p008/truffle/crates/truffle-core/src/services/file_transfer/adapter.rs`
- `/Users/jamesyong/Projects/project100/p008/truffle/crates/truffle-core/src/services/file_transfer/sender.rs`
- `/Users/jamesyong/Projects/project100/p008/truffle/crates/truffle-core/src/services/file_transfer/receiver.rs`

### Integration
- `/Users/jamesyong/Projects/project100/p008/truffle/crates/truffle-core/src/integration.rs`

### Go Sidecar
- `/Users/jamesyong/Projects/project100/p008/truffle/packages/sidecar-slim/main.go`

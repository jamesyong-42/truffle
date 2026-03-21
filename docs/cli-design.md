# Truffle CLI Design Document

**Status**: Draft
**Created**: 2026-03-19
**Companion to**: RFC 010 (P2P + CLI Redesign)

---



Now I have a thorough understanding of truffle's architecture -- its 6-layer stack, STAR topology mesh, Go sidecar for Tailscale tsnet, file transfer, store sync, HTTP services, and the existing (skeletal) CLI crate. Let me synthesize everything into the complete CLI design.

---

# Truffle CLI Design Document

## Part 1: Research -- What Makes Great Networking CLIs

### Connectivity Tools

**curl** -- The universal verb
- Core pattern: `curl [options] URL`. URL is always the last argument. Everything reads left to right.
- What feels good: Predictable. You always know where the URL goes. `-v` for verbose is burned into muscle memory.
- What feels bad: Flag soup. `-X POST -H "Content-Type: application/json" -d '{"key":"val"}'` is hostile to humans.
- Discovery: None -- you must know the URL. That is both its strength (universal) and its weakness.
- Status: Silent by default, progress bar with `-#`, verbose with `-v`. The principle of "quiet unless asked" is correct.
- Help: Man page is enormous but well-structured. `curl --help` categories (`curl --help category`) added in recent versions are excellent.

**httpie** (`http`) -- The human-first curl
- Core pattern: `http METHOD URL key=value`. Positional method, URL, then key-value pairs that become JSON.
- What feels good: Colored output by default. JSON formatted automatically. `http POST api.com name=truffle` just works -- no `-H`, no `-d`, no quoting.
- Key insight: **Output is designed for humans first, machines second.** Headers are colored differently from body. JSON is syntax-highlighted. This is the standard truffle should meet.
- Status: Streaming output with `--download` shows a progress bar. Otherwise, immediate response display.
- Help: `http --help` is clean and scannable. Grouped by category.

**websocat** -- WebSocket Swiss knife
- Core pattern: `websocat ws://host/path`. Stdin becomes messages, stdout receives them.
- What feels good: Unix philosophy -- pipe-friendly. `echo "hello" | websocat ws://...` works.
- Key insight: **Bidirectional protocols should still feel like Unix pipes.** Stdin/stdout is the interface.

**netcat** (`nc`) -- The original
- Core pattern: `nc host port`. Two arguments. Nothing else needed.
- What feels good: Absolute minimalism. `nc localhost 8080` -- done. Piping works: `echo "GET /" | nc host 80`.
- Key insight: **The simplest possible interface is `command target port`.** No flags for the common case.

**ngrok** -- Beautiful status
- Core pattern: `ngrok http 3000`. Verb + what you are exposing.
- What feels good: The TUI dashboard. Shows tunnel URL, request count, latency, errors -- all live. You start it and immediately see everything you need.
- Key insight: **Long-running commands should show a live status dashboard, not a wall of logs.** This is the gold standard for `truffle up` and `truffle proxy`.

**bore** -- Minimal ngrok
- Core pattern: `bore local 3000`. Just works.
- Key insight: **If you can remove a flag, remove it.** Bore proves you can do tunnel exposure with almost zero config.

### Mesh/VPN CLIs

**tailscale** -- The benchmark for truffle
- Core pattern: `tailscale [command]`. Simple verb commands: `status`, `ping`, `ssh`, `up`, `down`.
- What feels good: `tailscale status` shows a clean table of peers. `tailscale ping` works like regular ping but for Tailscale nodes. `tailscale up` / `tailscale down` are instantly understandable.
- Discovery: Nodes are addressed by hostname or IP. `tailscale ping laptop` just works if "laptop" is a Tailscale hostname. No configuration needed.
- Status: `tailscale status` is a clean table with hostname, IP, OS, last seen. Simple, scannable.
- Help: `tailscale --help` is brief and well-organized. Each subcommand has its own `--help`.
- Key insight: **`up`/`down`/`status` is the perfect lifecycle triad.** Truffle should steal this directly.

**wireguard** (`wg`) -- Extreme minimalism
- Core pattern: `wg show`, `wg set`, `wg genkey`. Verb-first, no noun.
- What feels good: `wg show` with no arguments shows everything. Colored output with clear sections. Interface name is optional when there is only one.
- Key insight: **When there is only one obvious target, do not require the user to specify it.** `truffle status` should not need `--node self`.

**zerotier-cli** -- What not to do
- Core pattern: `zerotier-cli listpeers`, `zerotier-cli info`. Commands are compound words, not subcommands.
- What feels bad: `listpeers`, `listnetworks`, `listmoons` -- these should be `peers list`, `networks list`. The output is raw JSON by default with no formatting.
- Key insight: **Never dump raw JSON as default output.** Human-readable tables for humans, `--json` for machines.

**nebula** -- Certificate-focused
- Core pattern: `nebula-cert ca`, `nebula-cert sign`. Clean subcommand grouping.
- What feels good: The cert workflow is step-by-step and documented in the help text itself.
- Key insight: **Multi-step workflows should be documented inline in help text,** not just in external docs.

### Modern Developer CLIs

**fly** (Fly.io) -- The conversational CLI
- Core pattern: `fly launch`, `fly deploy`, `fly ssh console`. Verbs that read like actions.
- What feels good: `fly launch` is interactive -- it walks you through setup. `fly status` shows a dashboard. `fly ssh console` drops you into a shell. Commands read like English.
- Key insight: **Commands should be verbs that a product manager would understand.** Not `create-deployment-instance` but `launch`.

**vercel** -- Zero-config philosophy
- Core pattern: `vercel` (no args = deploy). `vercel dev`, `vercel deploy`, `vercel env`.
- What feels good: Running `vercel` with no arguments does the most common thing (deploy). Automatic project detection. Colored, branded output.
- Key insight: **The bare command (no arguments) should do the most useful thing.** For truffle, `truffle` with no args should show status.

**gh** (GitHub CLI) -- The gold standard for subcommand organization
- Core pattern: `gh pr create`, `gh issue list`, `gh repo clone`. Noun-verb pattern with grouping.
- What feels good: Consistent noun-verb pattern. `gh pr list`, `gh pr view`, `gh pr create` -- learn one group, know them all. `--json` flag for machine output. `--web` to open in browser.
- Status: Clean tables with alignment, colored status indicators.
- Help: `gh help pr` shows the group. Each command has examples in help text.
- Key insight: **Group related commands under a noun. Use consistent verbs across groups.** But for truffle, most commands are top-level actions, not CRUD on resources. Use noun-groups only for advanced/compound features like `truffle http serve`.

**docker** -- Verb-noun with depth
- Core pattern: `docker run`, `docker build`, `docker ps`. Top-level verbs, then `docker container ls`, `docker image pull` for explicit noun-verb.
- What feels good: The aliases. `docker ps` = `docker container ls`. Power users and beginners both have a path.
- Key insight: **Provide short aliases for the most common commands.** `truffle ls` is better than `truffle nodes list`.

**kubectl** -- Power at the cost of discoverability
- Core pattern: `kubectl get pods`, `kubectl describe pod nginx`, `kubectl apply -f`. Verb-resource-name.
- What feels good: Once you learn the pattern, it is infinitely extensible. `kubectl get` works with any resource type.
- What feels bad: Steep learning curve. The sheer number of flags. `-o json`, `-o yaml`, `-o wide`, `-o jsonpath=...` -- powerful but overwhelming.
- Key insight: **Truffle should NOT be kubectl.** Truffle has a small, fixed set of operations. Flat command structure is better.

### Chat/Communication CLIs

**mosh** -- Persistent connection
- Core pattern: `mosh user@host`. Drop-in replacement for ssh. Handles roaming, intermittent connectivity.
- Key insight: **Connection commands should handle disconnection gracefully.** `truffle chat` should reconnect automatically.

**tmate** -- Instant sharing
- Core pattern: `tmate`. Run it, get a URL. Share the URL. Done.
- Key insight: **Generate shareable links automatically.** When `truffle expose` starts, print the access URL immediately.

**talk/write** -- Classic Unix chat
- Core pattern: `talk user@host`, `write user`. Split-screen (talk) or line-by-line (write).
- What feels good about talk: The split screen. You see what you type on top, what they type on bottom. Real-time character-by-character.
- What feels good about write: Simplicity. Just `write alice` and start typing. Messages appear on their terminal.
- Key insight: **`truffle chat` should default to `write`-style simplicity (line-by-line) with an option for `talk`-style split screen.** `truffle send` should be like `write` but one-shot.

---

## Part 2: Design Principles

### The Seven Principles of Truffle CLI

**1. "It reads like English."**
Every command should be a sentence fragment that a non-technical person could parse. `truffle send laptop "deploy is done"` reads naturally. `truffle cp report.pdf server:/tmp/` reads naturally. If you have to explain the syntax, the syntax is wrong.

**2. "Beautiful by default, machine-readable on demand."**
Human output uses color, Unicode box-drawing, alignment, and whitespace. Machine output is one `--json` flag away. No raw JSON dumps. No log-style output. If ngrok can make tunnels beautiful, truffle can make mesh networking beautiful.

**3. "Zero-config for the common case."**
`truffle up` should work with no flags. `truffle ls` should work with no flags. `truffle chat laptop` should work with no flags. Configuration exists for edge cases and power users, never for getting started. The first experience should be: install, run, it works.

**4. "Fail with a fix, not a stack trace."**
Every error message must include (a) what went wrong in plain English, (b) why it likely happened, and (c) what to do about it. `Error: Can't reach "laptop". It appears to be offline. Run 'truffle ls' to see which nodes are available.` Not: `Error: ConnectionRefused(Os { code: 111 })`.

**5. "Show, don't tell."**
Long-running operations show live progress. `truffle up` shows a status dashboard. `truffle cp` shows a progress bar. `truffle proxy` shows request counts. If the CLI is doing work, the user should see evidence of that work.

**6. "Names, not addresses."**
Nodes are referred to by name: `truffle ping laptop`, not `truffle ping 100.64.0.3`. IPs work when you need them, but names are the primary interface. Autocomplete helps you discover names. The addressing system is invisible until you need it.

**7. "One command, one job."**
Each command does exactly one thing. `truffle cp` copies files. It does not also start a server, manage transfers, or display dashboards. Composition happens through separate commands, not flags that change behavior.

---

## Part 3: Complete CLI Design

### Global Flags (available on every command)

```
--json           Output as JSON (machine-readable)
--quiet, -q      Suppress all non-essential output
--verbose, -v    Show detailed output (debug info, timings)
--color <when>   Force color: auto, always, never  [default: auto]
--config <path>  Path to config file                [default: ~/.config/truffle/config.toml]
```

### Root Command

```
$ truffle
```

When run with no arguments, behaves like `truffle status`. Shows a quick status dashboard. This follows the Vercel principle: bare command = most useful thing.

```
$ truffle --help

truffle -- Mesh networking for your devices, built on Tailscale.

Usage: truffle [command]

Node:
  up              Start your node and join the mesh
  down            Stop your node and leave the mesh
  status          Show your node's status and connectivity

Discovery:
  ls              List all nodes on your mesh
  ping <node>     Check if a node is reachable

Connectivity:
  tcp <target>    Open a raw TCP connection (like netcat)
  ws <target>     Open a WebSocket connection
  proxy           Forward a remote port to your machine
  expose          Make a local port available on the mesh

Communication:
  chat <node>     Start a live chat with another node
  send <node>     Send a one-shot message

Files:
  cp <src> <dst>  Copy files between nodes (like scp)

Diagnostics:
  doctor          Diagnose connectivity issues
  completion      Generate shell completions

Advanced:
  http            HTTP serving and proxying commands
  store           Cross-device store sync commands

Run 'truffle <command> --help' for details on any command.
```

---

### Node Lifecycle

#### `truffle up`

**Start your node and join the mesh.**

```
truffle up [options]

Options:
  --name <name>         Set this node's display name     [default: hostname]
  --type <type>         Device type: desktop, server, mobile  [default: auto-detected]
  --hostname <prefix>   Tailscale hostname prefix        [default: "truffle"]
  --prefer-primary      Volunteer to be the primary node
  --auth-key <key>      Tailscale auth key (for headless setup)
  --port <port>         Mesh listen port                 [default: 443]
  --background, -b      Run as a background daemon
  --ephemeral           Use an ephemeral Tailscale node (cleaned up on exit)
```

**Example usage and output:**

```
$ truffle up

  truffle
  ───────────────────────────────────────

  Node        james-macbook
  Status      ● online
  IP          100.64.0.3
  DNS         truffle-desktop-a1b2.tailnet.ts.net
  Role        secondary
  Primary     living-room-server (100.64.0.1)
  Mesh        3 nodes online
  Uptime      just started

  Listening for connections...
  Press Ctrl+C to stop, or run 'truffle down' from another terminal.
```

The output is a live TUI that updates. When a new node joins or leaves, the mesh count updates. When the role changes (e.g., this node becomes primary), it updates in place.

**If Tailscale auth is needed:**
```
$ truffle up

  truffle
  ───────────────────────────────────────

  Status      ○ authenticating

  Visit this URL to log in:
  https://login.tailscale.com/a/abc123def456

  Waiting for authentication...
```

Once authenticated, transitions smoothly to the "online" dashboard.

**Edge cases:**
- Already running: `Already running (PID 12345). Use 'truffle down' first, or 'truffle status' to check.`
- No Tailscale: `Tailscale is not installed. Install it from https://tailscale.com/download`
- Sidecar not found: `Can't find the truffle sidecar binary. Run 'truffle doctor' for setup help.`

---

#### `truffle down`

**Stop your node and leave the mesh.**

```
truffle down [options]

Options:
  --force, -f     Force stop even if transfers are in progress
```

**Example:**
```
$ truffle down

  ✓ Notified 3 peers
  ✓ Node stopped

$ truffle down
  Node is not running.
```

**Edge cases:**
- Active file transfer: `A file transfer is in progress (report.pdf → server, 67%). Stop anyway? [y/N]` Use `--force` to skip the prompt.
- Not running: `Node is not running.` (not an error, just informational)

---

#### `truffle status`

**Show your node's status and connectivity.**

```
truffle status [options]

Options:
  --watch, -w     Continuously update (like top)
```

**Example:**
```
$ truffle status

  truffle · james-macbook
  ───────────────────────────────────────

  Status      ● online
  IP          100.64.0.3
  DNS         truffle-desktop-a1b2.tailnet.ts.net
  Role        secondary
  Primary     living-room-server
  Uptime      2h 14m

  Mesh
  ────
  Nodes       3 online, 1 offline
  Messages    847 sent, 1.2k received
  Bandwidth   12.4 MB ↑  38.7 MB ↓

  Services
  ────────
  Proxy       :3000 → laptop:5173 (active, 234 requests)
  HTTP        serving ./public on :8080

  Tailscale
  ─────────
  Version     1.62.0
  Auth        ● authenticated (james@example.com)
  Key expiry  in 89 days
  Connection  direct (no relay)
```

**When node is not running:**
```
$ truffle status

  truffle · james-macbook
  ───────────────────────────────────────

  Status      ○ offline

  Run 'truffle up' to start your node.
```

---

### Discovery

#### `truffle ls`

**List all nodes on your mesh.**

```
truffle ls [options]

Options:
  --all, -a       Include offline nodes
  --long, -l      Show detailed info (IP, OS, latency, capabilities)

Aliases: truffle list, truffle nodes
```

**Example (default -- compact):**
```
$ truffle ls

  NODE                  STATUS     ROLE        LATENCY
  james-macbook         ● online   secondary   —
  living-room-server    ● online   primary     2ms
  work-desktop          ● online   secondary   14ms

  3 nodes online
```

**Example (long format):**
```
$ truffle ls -l

  NODE                  STATUS     ROLE        IP            OS       LATENCY   CAPABILITIES
  james-macbook         ● online   secondary   100.64.0.3    darwin   —         file-transfer, sync
  living-room-server    ● online   primary     100.64.0.1    linux    2ms       file-transfer, sync, http
  work-desktop          ● online   secondary   100.64.0.5    linux    14ms      file-transfer, sync

  3 nodes online
```

**Example (with offline -- `-a`):**
```
$ truffle ls -a

  NODE                  STATUS      ROLE        LATENCY
  james-macbook         ● online    secondary   —
  living-room-server    ● online    primary     2ms
  work-desktop          ● online    secondary   14ms
  old-laptop            ○ offline   —           —

  3 nodes online, 1 offline
```

**Example (JSON):**
```
$ truffle ls --json
[
  {
    "name": "james-macbook",
    "status": "online",
    "role": "secondary",
    "ip": "100.64.0.3",
    "os": "darwin",
    "latency_ms": null,
    "capabilities": ["file-transfer", "sync"]
  },
  ...
]
```

**Edge cases:**
- No nodes: `No other nodes found. Is Tailscale connected? Run 'truffle doctor' to check.`
- Node is not running: `Node is not running. Start it with 'truffle up' first.`

---

#### `truffle ping <node>`

**Check if a node is reachable and measure latency.**

```
truffle ping <node> [options]

Options:
  --count, -c <n>   Number of pings to send  [default: 4]
  --interval <ms>   Interval between pings   [default: 1000]
```

**Example:**
```
$ truffle ping laptop

  PING laptop (100.64.0.3) via Tailscale
  reply from laptop: time=2.1ms (direct)
  reply from laptop: time=1.8ms (direct)
  reply from laptop: time=2.3ms (direct)
  reply from laptop: time=1.9ms (direct)

  --- laptop ping statistics ---
  4 sent, 4 received, 0% loss
  round-trip min/avg/max = 1.8/2.0/2.3 ms
  connection: direct (not relayed)
```

**When relayed:**
```
  reply from laptop: time=47ms (via relay: sea)
```

**Edge cases:**
- Unknown node: `Can't find a node named "lapton". Did you mean "laptop"?` (fuzzy match suggestion)
- Node offline: `laptop is not responding. It was last seen 3 hours ago.`

---

### Connectivity

#### `truffle tcp <target>`

**Open a raw TCP connection to another node (like netcat).**

```
truffle tcp <node>[:<port>] [options]

Options:
  --listen, -l      Listen mode: wait for incoming connections
  --port, -p <port> Port to connect to (alternative to node:port syntax)
```

**Example (connect):**
```
$ truffle tcp server:5432
Connected to server:5432
Type to send data. Press Ctrl+C to disconnect.
```

Stdin goes to the remote, stdout comes back. Fully pipe-compatible:
```
$ echo "SELECT 1;" | truffle tcp server:5432
```

**Example (listen):**
```
$ truffle tcp -l 8080
Listening on :8080 (available to your mesh as james-macbook:8080)
```

**Edge cases:**
- Port not specified and no default: `Which port? Usage: truffle tcp server:5432`
- Connection refused: `Can't connect to server:5432. The port may not be open. Run 'truffle ping server' to check if the node is reachable.`

---

#### `truffle ws <target>`

**Open a WebSocket connection to another node.**

```
truffle ws <node>[:<port>][/path] [options]

Options:
  --listen, -l      Listen mode: start a WebSocket server
  --port, -p <port> Port number
  --text             Send as text frames (default)
  --binary           Send as binary frames
```

**Example:**
```
$ truffle ws server:8080/events
Connected to ws://server:8080/events
Type messages, one per line. Press Ctrl+C to disconnect.

> {"type":"subscribe","channel":"deploy"}
< {"type":"subscribed","channel":"deploy"}
< {"type":"event","data":"build started"}
```

Input lines prefixed with `>`, output with `<`, colored differently. Like websocat but with better formatting.

**Edge cases:**
- Not a WebSocket endpoint: `server:8080/api didn't accept the WebSocket upgrade. Is this a WebSocket endpoint?`

---

#### `truffle proxy <local-port> <node>:<remote-port>`

**Forward a remote port to your machine.**

```
truffle proxy <local-port> <node>:<remote-port> [options]
truffle proxy <node>:<remote-port>                        # auto-assigns local port

Options:
  --bind <addr>     Local bind address  [default: 127.0.0.1]
```

**Example:**
```
$ truffle proxy 5432 server:5432

  truffle proxy
  ───────────────────────────────────────

  Local       127.0.0.1:5432
  Remote      server:5432 (100.64.0.1)
  Status      ● forwarding
  Latency     2ms

  Connections   0 active, 0 total
  Bandwidth     0 B ↑  0 B ↓

  Press Ctrl+C to stop.
```

Live-updating dashboard (ngrok-style). Connection count and bandwidth update in real time.

**Auto-assign local port:**
```
$ truffle proxy server:5432

  Forwarding 127.0.0.1:54321 → server:5432
```

**Edge cases:**
- Local port in use: `Port 5432 is already in use. Try: truffle proxy 15432 server:5432`
- Remote unreachable: `Can't reach server. Run 'truffle ping server' to diagnose.`

---

#### `truffle expose <local-port>`

**Make a local port available to all nodes on your mesh.**

```
truffle expose <local-port> [options]

Options:
  --name <name>       Service name for discovery
  --port <mesh-port>  Port to expose on mesh  [default: same as local-port]
  --allow <nodes>     Comma-separated list of allowed nodes  [default: all]
```

**Example:**
```
$ truffle expose 3000

  truffle expose
  ───────────────────────────────────────

  Local       127.0.0.1:3000
  Mesh        james-macbook:3000
  Access      Any node can reach this via:
              truffle tcp james-macbook:3000
              truffle proxy 3000 james-macbook:3000

  Connections   0 active
  Press Ctrl+C to stop.
```

**Edge cases:**
- Port not open locally: `Nothing is listening on port 3000. Start your service first.` (warning, not error -- the service might start later)
- Already exposed: `Port 3000 is already exposed. Use 'truffle status' to see active services.`

---

### Communication

#### `truffle chat <node>`

**Start a live chat with another node.**

(Deep-dive in Part 4 below.)

```
truffle chat <node> [options]
truffle chat                    # broadcast to all nodes

Options:
  --split, -s       Split-screen mode (like Unix 'talk')
```

---

#### `truffle send <node> <message>`

**Send a one-shot message to another node.**

```
truffle send <node> <message>
truffle send --all <message>          # broadcast to everyone

Options:
  --all, -a         Send to all nodes
  --wait, -w        Wait for and print the reply
  --stdin           Read message from stdin
```

**Example:**
```
$ truffle send laptop "deploy is done"
  ✓ Sent to laptop

$ truffle send --all "rebooting in 5 minutes"
  ✓ Sent to 3 nodes

$ echo "hello" | truffle send laptop --stdin
  ✓ Sent to laptop
```

**With reply:**
```
$ truffle send laptop "are you there?" --wait
  ✓ Sent to laptop
  ← "yes, all good"
```

**Edge cases:**
- Node offline: `laptop is offline. Message will not be delivered. Send anyway? [y/N]` (or add `--queue` for future delivery)
- Empty message: `No message provided. Usage: truffle send laptop "your message"`

---

### File Operations

#### `truffle cp <source> <node>:<path>`

**Copy files between nodes (like scp).**

```
truffle cp <source> <node>:<path>     # upload
truffle cp <node>:<path> <dest>       # download
truffle cp <source> <node>:           # upload, keep filename

Options:
  --recursive, -r    Copy directories recursively
  --resume           Resume an interrupted transfer
  --verify           Verify integrity after transfer (SHA-256)  [default: true]
  --progress          Show progress bar  [default: true in TTY]
```

**Example (upload):**
```
$ truffle cp report.pdf server:/tmp/

  report.pdf → server:/tmp/report.pdf
  ████████████████████████░░░░░░░░  67%  2.4 MB/s  eta 3s
```

**Example (download):**
```
$ truffle cp server:/var/log/app.log ./

  server:/var/log/app.log → ./app.log
  ████████████████████████████████  100%  done (4.2 MB in 1.8s)
  ✓ SHA-256 verified
```

**Example (directory):**
```
$ truffle cp -r ./dist server:/var/www/app/

  Copying 47 files (12.3 MB total)
  ████████████████████████████░░░░  84%  8.1 MB/s  eta 2s
```

**Edge cases:**
- File exists on remote: `report.pdf already exists on server. Overwrite? [y/N]`
- Network interruption: `Transfer interrupted at 67%. Resume with: truffle cp --resume report.pdf server:/tmp/`
- Node offline: `server is offline. Can't transfer files.`
- Path does not exist on remote: `Directory /tmp/nonexistent/ does not exist on server. Create it? [y/N]`

---

### Diagnostics

#### `truffle doctor`

**Diagnose connectivity issues.**

```
truffle doctor [options]

Options:
  --fix             Attempt to fix detected issues
```

**Example (all good):**
```
$ truffle doctor

  truffle doctor
  ───────────────────────────────────────

  ✓ Tailscale installed            v1.62.0
  ✓ Tailscale connected            james@example.com
  ✓ Sidecar binary found           /usr/local/bin/truffle-sidecar
  ✓ Config file valid              ~/.config/truffle/config.toml
  ✓ Mesh reachable                 3 nodes responding
  ✓ Primary node healthy           living-room-server (2ms)
  ✓ Key expiry                     89 days remaining

  All checks passed. Your mesh is healthy.
```

**Example (problems):**
```
$ truffle doctor

  truffle doctor
  ───────────────────────────────────────

  ✓ Tailscale installed            v1.62.0
  ✗ Tailscale not connected
    → Run 'tailscale up' to connect
  ✓ Sidecar binary found           /usr/local/bin/truffle-sidecar
  ✓ Config file valid
  ⚠ Key expiring soon              3 days remaining
    → Run 'tailscale up --reset' to renew your key
  — Mesh reachable                 (skipped: Tailscale not connected)

  1 error, 1 warning. Fix the error first.
```

---

#### `truffle completion`

**Generate shell completions.**

```
truffle completion <shell>

Shells: bash, zsh, fish, powershell
```

**Example:**
```
$ truffle completion zsh > ~/.zfunc/_truffle
$ echo 'fpath=(~/.zfunc $fpath); compinit' >> ~/.zshrc

# Or, one-liner:
$ source <(truffle completion zsh)
```

---

### Advanced: HTTP Subgroup

#### `truffle http serve <dir>`

**Serve a directory as a static website on your mesh.**

```
truffle http serve <dir> [options]

Options:
  --port, -p <port>   Port to serve on  [default: 8080]
  --spa               Enable SPA mode (serve index.html for all routes)
  --name <name>       Service name for discovery
```

**Example:**
```
$ truffle http serve ./dist --spa

  truffle http serve
  ───────────────────────────────────────

  Serving     ./dist (47 files, 12.3 MB)
  Mode        SPA (index.html fallback)
  Local       http://localhost:8080
  Mesh        http://james-macbook:8080

  Requests    0 total
  Press Ctrl+C to stop.
```

---

#### `truffle http proxy <prefix> <target>`

**Proxy HTTP requests by path prefix to a local service.**

```
truffle http proxy <prefix> <target> [options]

Options:
  --port, -p <port>   Mesh-facing port  [default: 443]
```

**Example:**
```
$ truffle http proxy /api localhost:3000

  Proxying /api/* → localhost:3000
  Available at: https://james-macbook.tailnet.ts.net/api/

  Press Ctrl+C to stop.
```

---

### Advanced: Store Subgroup

#### `truffle store sync`

**Manage cross-device store synchronization.**

```
truffle store sync [options]       # show sync status
truffle store sync --reset         # force full resync
truffle store sync --clear         # clear local store

Options:
  --status          Show sync status (default when no flags)
  --reset           Force a full resync from primary
  --clear           Clear the local store
```

**Example:**
```
$ truffle store sync

  Store Sync
  ───────────────────────────────────────

  Status        ● synced
  Last sync     12 seconds ago
  Entries       142 keys across 3 devices
  Primary       living-room-server

  Per-device:
    james-macbook         48 keys   ● current
    living-room-server    67 keys   ● current
    work-desktop          27 keys   ● current
```

---

## Part 4: The Chat Command Deep-Dive

### `truffle chat <node>`

**Start a live chat with another node.**

#### Default Mode: Line-by-line (inspired by Unix `write`)

This is the default because it is simpler, works in any terminal size, and does not require TUI capabilities.

```
$ truffle chat laptop

  Connected to laptop (james-macbook → laptop)
  Type your message and press Enter. Ctrl+C to exit.
  ──────────────────────────────────────────────────

  [14:23] you: hey, is the deploy done?
  [14:23] laptop: yep, just finished. all green.
  [14:24] you: nice, shipping to prod now
  [14:24] laptop: 👍
```

**Design details:**
- Timestamps in `[HH:MM]` format, dimmed color (gray)
- "you" in one color (cyan), remote name in another (green)
- Messages appear inline, scrolling up naturally
- Input is at the bottom, like any chat interface
- No prompt character -- you just type and press Enter
- Empty lines are not sent (prevents accidental sends)

#### Split-Screen Mode: `truffle chat laptop --split`

Inspired by Unix `talk`. The terminal is divided horizontally.

```
$ truffle chat laptop --split

  ┌─ you (james-macbook) ────────────────────────┐
  │ hey, is the deploy done?                      │
  │ nice, shipping to prod now                    │
  │                                               │
  │                                               │
  │                                               │
  │                                               │
  ├─ laptop ─────────────────────────────────────┤
  │ yep, just finished. all green.                │
  │ 👍                                             │
  │                                               │
  │                                               │
  │                                               │
  │                                               │
  └───────────────────────────────────────────────┘
```

Characters appear in real-time as they are typed (like `talk`), not just on Enter.

#### Group Chat: `truffle chat` (no argument)

When run without a node argument, opens a broadcast chat with all online nodes.

```
$ truffle chat

  Mesh chat (3 nodes online). Ctrl+C to exit.
  ──────────────────────────────────────────────────

  [14:23] you: hey everyone, deploying in 5 min
  [14:23] laptop: got it
  [14:23] server: ready
```

#### File Sharing in Chat

You can share files inline using the `/file` command within chat:

```
  [14:25] you: here's the screenshot
  [14:25] you: /file screenshot.png
  [14:25] → Sending screenshot.png (245 KB)...
  [14:25] ✓ screenshot.png sent to laptop
  [14:25] laptop: got it, thanks
```

The recipient sees:
```
  [14:25] james-macbook: here's the screenshot
  [14:25] ← Receiving screenshot.png (245 KB)...
  [14:25] ✓ screenshot.png saved to ~/truffle-received/screenshot.png
```

#### What Happens When the Other Node Is Offline

```
$ truffle chat laptop

  laptop is offline (last seen 3 hours ago).

  Options:
    • Wait for laptop to come online (press Enter)
    • Choose another node: truffle chat server
    • See who's online: truffle ls
```

If you press Enter, truffle waits and connects automatically when laptop appears:
```
  Waiting for laptop to come online...
  ● laptop is online! Connected.
  ──────────────────────────────────────────────────
```

#### Chat Discovery

The chat command uses the same addressing as all other commands. Tab-completion lists online nodes:

```
$ truffle chat [TAB]
laptop    server    work-desktop
```

#### History

Chat history is saved locally at `~/.local/share/truffle/chat/`. You can review it:

```
$ truffle chat laptop --history

  Chat history with laptop (last 7 days)
  ──────────────────────────────────────────────────

  [Mar 18, 14:23] you: hey, is the deploy done?
  [Mar 18, 14:23] laptop: yep, just finished.
  [Mar 19, 09:15] you: morning, any issues overnight?
  [Mar 19, 09:16] laptop: all clean
```

---

## Part 5: Output Design

### Color Palette

Truffle uses a restrained, professional color palette that works on both dark and light terminals:

| Element | Dark terminal | Light terminal |
|---------|--------------|----------------|
| Node names | **bold white** | **bold black** |
| Online indicator | green `●` | green `●` |
| Offline indicator | dim `○` | dim `○` |
| Warning indicator | yellow `⚠` | yellow `⚠` |
| Error indicator | red `✗` | red `✗` |
| Success indicator | green `✓` | green `✓` |
| Dimmed text (labels, timestamps) | gray | gray |
| Your messages | cyan | blue |
| Remote messages | green | green |
| Section headers | **bold** + underline | **bold** + underline |
| Progress bar filled | cyan `█` | blue `█` |
| Progress bar empty | dim `░` | dim `░` |

### `truffle ls` Output

```
  NODE                  STATUS     ROLE        LATENCY
  james-macbook         ● online   secondary   —
  living-room-server    ● online   primary     2ms
  work-desktop          ● online   secondary   14ms

  3 nodes online
```

- Column headers are **dim/gray** (not bold -- they are metadata, not data)
- Status dots are colored: green for online, dim gray for offline
- "primary" is subtly highlighted (bold or different color)
- Latency right-aligned in its column
- Bottom summary line is dimmed
- Columns auto-size to content (no fixed widths)
- Self (current node) has `—` for latency and is listed first

### `truffle status` Output

```
  truffle · james-macbook
  ───────────────────────────────────────

  Status      ● online
  IP          100.64.0.3
  DNS         truffle-desktop-a1b2.tailnet.ts.net
  Role        secondary
  Primary     living-room-server
  Uptime      2h 14m

  Mesh
  ────
  Nodes       3 online, 1 offline
  Messages    847 sent, 1.2k received
  Bandwidth   12.4 MB ↑  38.7 MB ↓

  Services
  ────────
  Proxy       :3000 → laptop:5173 (active, 234 requests)
  HTTP        serving ./public on :8080

  Tailscale
  ─────────
  Version     1.62.0
  Auth        ● authenticated (james@example.com)
  Key expiry  in 89 days
  Connection  direct (no relay)
```

- Title line uses `·` as separator (like macOS window titles)
- Section dividers use `────` (thin horizontal rule), not box-drawing
- Key-value pairs are left-aligned with consistent label width
- The `●` dot is colored (green for good, yellow for warning, red for error)
- Numbers are human-readable (1.2k, 12.4 MB, not 1234 or 12400000)
- Uptime uses friendly units ("2h 14m", "3 days", "just started")

### `truffle proxy` Live Dashboard

```
  truffle proxy
  ───────────────────────────────────────

  Local       127.0.0.1:5432
  Remote      server:5432 (100.64.0.1)
  Status      ● forwarding
  Latency     2ms

  Connections   3 active, 47 total
  Bandwidth     1.2 MB ↑  4.8 MB ↓

  Recent:
  14:23:01  ← new connection from local
  14:23:01  → connected to server:5432
  14:22:45  ✓ connection closed (1.2s, 4.3 KB)

  Press Ctrl+C to stop.
```

- Top section is a static dashboard that updates in place
- Bottom "Recent" section scrolls like a log
- Connection events are timestamped
- Arrows indicate direction: `←` incoming, `→` outgoing
- Clean close `✓`, error close `✗`

### `truffle cp` Progress

```
  report.pdf → server:/tmp/report.pdf
  ████████████████████████░░░░░░░░  67%  2.4 MB/s  eta 3s
```

- Single-line progress bar (like curl with `-#`)
- Source → destination on the first line
- Progress bar uses full-block `█` and light-shade `░`
- Percentage, speed, and ETA on the same line
- On completion:

```
  report.pdf → server:/tmp/report.pdf
  ████████████████████████████████  100%  done (4.2 MB in 1.8s)
  ✓ SHA-256 verified
```

### Error Messages

Every error follows this structure:

```
  ✗ <What went wrong>

    <Why it probably happened>

    <What to do about it>
```

**Examples:**

```
  ✗ Can't reach "laptop"

    The node appears to be offline. It was last seen 3 hours ago.

    Try:
      truffle ls          see who's online
      truffle ping laptop check connectivity
      truffle doctor      diagnose network issues
```

```
  ✗ Port 5432 is already in use

    Another process is listening on 127.0.0.1:5432 (pid 8821: postgres).

    Try:
      truffle proxy 15432 server:5432    use a different local port
```

```
  ✗ Tailscale is not connected

    Your node can't join the mesh without Tailscale.

    Try:
      tailscale up        connect to Tailscale
      truffle doctor      full diagnostic check
```

---

## Part 6: Addressing Scheme

### The Resolution Order

When you type a node reference, truffle resolves it in this order:

1. **Exact mesh node name** -- `laptop` matches the node named "laptop" in the mesh
2. **Alias from config** -- `db` matches an alias defined in `~/.config/truffle/config.toml`
3. **Tailscale hostname prefix** -- `truffle-desktop-a1b2` matches the full Tailscale hostname
4. **Tailscale IP** -- `100.64.0.3` matches by IP address
5. **Full DNS name** -- `truffle-desktop-a1b2.tailnet.ts.net` matches the FQDN

### Port Syntax

Colon-separated, matching scp/ssh/curl convention universally:

```
laptop:5432          # node + port
100.64.0.3:5432      # IP + port
```

There is no space-separated port syntax. Colons are universal and unambiguous.

### Service Syntax

For named services (future feature), use slash:

```
laptop/postgres      # the "postgres" service on laptop
laptop/http          # the "http" service on laptop
```

This reads naturally: "laptop's postgres". It also avoids collision with port syntax.

### Config Aliases

```toml
# ~/.config/truffle/config.toml
[aliases]
db = "living-room-server:5432"
web = "work-desktop:3000"
home = "living-room-server"
```

Then:
```
$ truffle tcp db
# equivalent to: truffle tcp living-room-server:5432

$ truffle proxy 5432 db
# equivalent to: truffle proxy 5432 living-room-server:5432
```

### Ambiguity Handling

If a name could match multiple nodes:

```
$ truffle ping mac

  "mac" matches multiple nodes:
    1. james-macbook
    2. mac-mini-server

  Which one? (1-2, or use the full name)
```

With `--json`, ambiguity is an error (no interactive prompt):

```json
{
  "error": "ambiguous_node",
  "query": "mac",
  "matches": ["james-macbook", "mac-mini-server"]
}
```

### Tab Completion

The completion system knows about all online nodes, aliases, and recently-used targets:

```
$ truffle tcp [TAB]
laptop               living-room-server   work-desktop
db (alias)           web (alias)          home (alias)

$ truffle tcp laptop:[TAB]
laptop:22    laptop:80    laptop:443    laptop:5432
```

Port completion is populated from known exposed services on that node.

---

## Part 7: The "Apple Product" Test

### `truffle up`

- **Junior developer test**: "Start your mesh node" -- yes, anyone understands `up`.
- **Beautiful output**: Live dashboard with status, role, peer count. Passes.
- **Does the obvious thing**: Starts the node. No flags needed. Passes.
- **Fails gracefully**: Tells you what is missing (Tailscale, sidecar) and how to fix it. Passes.
- **Steve Jobs test**: One command to start. Status immediately visible. Nothing to configure. Approved.

### `truffle ls`

- **Junior developer test**: "List nodes" -- like `ls` for files, but for nodes. Intuitive.
- **Beautiful output**: Clean table with colored status dots. Passes.
- **Does the obvious thing**: Shows online nodes. `-a` for all. Passes.
- **Fails gracefully**: If not running, says "start with `truffle up`". Passes.
- **Steve Jobs test**: Three columns. Status dots. Peer count. Nothing extraneous. Approved.

### `truffle chat laptop`

- **Junior developer test**: "Chat with laptop" -- yes, completely obvious.
- **Beautiful output**: Timestamps, colored names, clean layout. Passes.
- **Does the obvious thing**: Opens a chat. Type and press Enter. Passes.
- **Fails gracefully**: If laptop is offline, offers to wait or suggests alternatives. Passes.
- **Steve Jobs test**: It is iMessage for terminals. Approved.

### `truffle cp report.pdf server:/tmp/`

- **Junior developer test**: Anyone who has used `scp` or `cp` knows this syntax instantly.
- **Beautiful output**: Progress bar with speed and ETA. Passes.
- **Does the obvious thing**: Copies the file. Verifies integrity. Passes.
- **Fails gracefully**: Interrupted transfers can be resumed. Existing files prompt before overwrite. Passes.
- **Steve Jobs test**: It just works. Drag, drop, done (but in a terminal). Approved.

### `truffle proxy 5432 server:5432`

- **Junior developer test**: "Forward port 5432 from server to local" -- the arguments tell the story.
- **Beautiful output**: Live dashboard with connection count, bandwidth. Passes.
- **Does the obvious thing**: Forwards the port. Passes.
- **Fails gracefully**: Port in use gets a helpful message with the PID of the occupying process. Passes.
- **Steve Jobs test**: Two numbers and a name. That is the whole interface. Approved.

### `truffle doctor`

- **Junior developer test**: "Check if everything works" -- universally understood metaphor.
- **Beautiful output**: Checklist with green checks and red Xs. Passes.
- **Does the obvious thing**: Runs diagnostics, tells you what is wrong. Passes.
- **Fails gracefully**: It IS the failure-diagnosis tool. Each problem includes the fix. Passes.
- **Steve Jobs test**: "It just tells you what's wrong and how to fix it." Approved.

### `truffle send laptop "deploy is done"`

- **Junior developer test**: "Send a message to laptop" -- reads like English.
- **Beautiful output**: Simple confirmation checkmark. Passes.
- **Does the obvious thing**: Sends the message. Passes.
- **Fails gracefully**: If offline, asks whether to send anyway with an explanation. Passes.
- **Steve Jobs test**: Three words and a message. The minimum possible interface. Approved.

### Commands that need the most care

- **`truffle tcp`** -- The netcat-equivalent has the highest risk of confusing new users because raw TCP is inherently low-level. The key mitigation: when connected, print `Connected to server:5432` and `Type to send data. Press Ctrl+C to disconnect.` -- explicit context for what is happening.
- **`truffle http proxy`** -- The path-prefix concept may confuse people who think in terms of port-forwarding. The key mitigation: the output says exactly what it is doing: `Proxying /api/* -> localhost:3000`.
- **`truffle store sync`** -- The most "developer-facing" command. Acceptable because it is in the `store` subgroup, which self-selects for advanced users.

---

## Summary

The truffle CLI is designed around seven principles: English-readable commands, beautiful default output, zero-config common cases, helpful error messages, live progress feedback, name-based addressing, and single-responsibility commands. It draws from the best of tailscale (lifecycle triad), httpie (colored human output), ngrok (live dashboards), netcat (pipe-friendly connectivity), scp (file copy syntax), gh (subcommand organization), and Unix talk/write (terminal chat). The addressing scheme resolves names before IPs, supports config aliases, handles ambiguity interactively, and uses colon for ports and slash for services. Every command passes the test: would a junior developer understand this without reading docs?
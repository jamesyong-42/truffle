# CLI Quick Start

The `truffle` CLI is the primary way to use truffle. It runs as a background daemon -- start it once with `truffle up`, then use commands like `ls`, `send`, `cp`, and `tcp` to interact with your mesh.

## Commands at a Glance

```
$ truffle --help

truffle -- Mesh networking for your devices, built on Tailscale. (v2 — Node API)

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

Communication:
  send <node>     Send a one-shot message

Files:
  cp <src> <dst>  Copy files between nodes (like scp)

Diagnostics:
  doctor          Diagnose connectivity issues

Run 'truffle <command> --help' for details on any command.
```

## Node Lifecycle

### Start your node

```sh
truffle up
```

This starts the Go sidecar, joins your Tailscale network via `tsnet`, and begins discovering peers via WatchIPNBus. By default it runs in the foreground with a live status dashboard.

Options:
```
--name <name>         Set this node's display name     [default: hostname]
--foreground          Run in foreground (for debugging)
```

### Check status

```sh
truffle status
```

Running `truffle` with no arguments also shows status.

Options:
```
--watch, -w     Continuously update (like top)
--json          Output as JSON
```

### Stop your node

```sh
truffle down
```

Options:
```
--force, -f     Force stop even if transfers are in progress
```

## Discovery

### List nodes

```sh
truffle ls
```

```
  NAME              IP             OS       STATUS
  james-macbook     100.64.0.3     macOS    online
  work-desktop      100.64.0.5     Linux    online
  home-server       100.64.0.7     Linux    online
```

Options:
```
--all, -a       Show offline nodes too
--long, -l      Show detailed info (IP, OS, connection type)
--json          Output as JSON
```

### Ping a node

```sh
truffle ping laptop
```

Nodes are addressed by name (Tailscale hostname). No need to remember IPs.

Options:
```
-c, --count <n>     Number of pings [default: 4]
```

## Communication

### Send a one-shot message

```sh
truffle send laptop "deploy is done"
```

Options:
```
--all, -a       Send to all nodes (broadcast)
--wait, -w      Wait for and print the reply
```

## File Transfer

### Copy files

```sh
# Copy a local file to a remote node
truffle cp ./report.pdf server:/tmp/

# Copy from remote to local
truffle cp server:/var/log/app.log ./
```

The syntax follows `scp` conventions. SHA-256 verification is on by default.

Options:
```
--no-verify     Skip SHA-256 integrity verification after transfer
```

## Connectivity

### Raw TCP connection

```sh
truffle tcp server:8080
```

Works like `netcat` -- stdin is sent, stdout receives. Pipe-friendly.

Options:
```
--check         Only test connectivity, don't open interactive session
```

## Diagnostics

### Doctor

```sh
truffle doctor
```

Checks:
- Tailscale is installed and running
- Sidecar binary is present and executable
- Network connectivity to peers
- Configuration file validity

## Global Flags

Available on every command:

```
--quiet, -q      Suppress all non-essential output
--verbose, -v    Show detailed output (debug info, timings)
--color <when>   Force color: auto, always, never  [default: auto]
--config <path>  Path to config file  [default: ~/.config/truffle/config.toml]
```

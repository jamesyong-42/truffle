# CLI Quick Start

The `truffle` CLI is the primary way to use truffle. It runs as a background daemon -- start it once with `truffle up`, then use commands like `ls`, `send`, `cp`, and `proxy` to interact with your mesh.

## Commands at a Glance

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

## Node Lifecycle

### Start your node

```sh
truffle up
```

This starts the Go sidecar, joins your Tailscale network, and begins discovering peers. By default it runs in the foreground with a live status dashboard.

Options:
```
--name <name>         Set this node's display name     [default: hostname]
--type <type>         Device type: desktop, server, mobile  [default: auto]
--hostname <prefix>   Tailscale hostname prefix        [default: "truffle"]
--auth-key <key>      Tailscale auth key (headless setup)
--port <port>         Mesh listen port                 [default: 443]
--background, -b      Run as a background daemon
--ephemeral           Use an ephemeral Tailscale node
```

### Check status

```sh
truffle status
```

Running `truffle` with no arguments also shows status.

### Stop your node

```sh
truffle down
```

## Discovery

### List nodes

```sh
truffle ls
```

```
  NAME              IP             OS       STATUS
  james-macbook     100.64.0.3     macOS    ● online
  work-desktop      100.64.0.5     Linux    ● online
  home-server       100.64.0.7     Linux    ● online
```

Options:
```
--all, -a       Show offline nodes too
--json          Output as JSON
```

### Ping a node

```sh
truffle ping laptop
```

Nodes are addressed by name (Tailscale hostname). No need to remember IPs.

## Communication

### Send a one-shot message

```sh
truffle send laptop "deploy is done"
```

### Live chat

```sh
truffle chat laptop
```

Opens a persistent bidirectional chat session. Type messages and see replies in real time. Press `Ctrl+C` to exit.

## File Transfer

### Copy files

```sh
# Copy a local file to a remote node
truffle cp ./report.pdf server:/tmp/

# Copy from remote to local
truffle cp server:/var/log/app.log ./

# Copy between two remote nodes
truffle cp server1:/data/export.csv server2:/imports/
```

The syntax follows `scp` conventions. Progress bar is shown by default.

## Port Forwarding

### Expose a local port

```sh
# Make local port 3000 available on the mesh
truffle expose 3000
```

Other nodes can then connect to your port via the mesh.

### Forward a remote port

```sh
# Forward remote node's port 5432 to local port 5432
truffle proxy server:5432
```

## Connectivity

### Raw TCP connection

```sh
truffle tcp server:8080
```

Works like `netcat` -- stdin is sent, stdout receives. Pipe-friendly.

### WebSocket connection

```sh
truffle ws server:8080/path
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

### Shell completions

```sh
# Generate completions for your shell
truffle completion bash >> ~/.bashrc
truffle completion zsh >> ~/.zshrc
truffle completion fish > ~/.config/fish/completions/truffle.fish
```

## Global Flags

Available on every command:

```
--json           Output as JSON (machine-readable)
--quiet, -q      Suppress all non-essential output
--verbose, -v    Show detailed output (debug info, timings)
--color <when>   Force color: auto, always, never  [default: auto]
--config <path>  Path to config file  [default: ~/.config/truffle/config.toml]
```

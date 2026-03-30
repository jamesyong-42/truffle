# CLI Reference

## Interactive TUI

```bash
truffle              # launch interactive TUI (when no subcommand given on a TTY)
```

The TUI provides a Claude Code-inspired terminal interface with:
- Live activity feed (peer events, messages, file transfers)
- Always-visible device panel
- Slash commands with `@` device autocomplete and `/` command autocomplete
- File picker (Tab in `/cp`)
- Accept/reject modal for incoming files
- Toast notifications when scrolled up

### TUI Slash Commands

| Command | Description |
|---------|-------------|
| `/send <msg> @device` | Send a chat message |
| `/broadcast <msg>` | Message all online peers |
| `/cp <path> @device[:/dest]` | Send a file (Tab opens file picker) |
| `/exit` | Quit |

## One-Shot Commands

### Node Lifecycle

```bash
truffle up [--name <name>] [--foreground]
truffle down [--force]
truffle status [--watch] [--json]
truffle update
```

### Discovery & Diagnostics

```bash
truffle ls [--all] [--long] [--json]
truffle ping <node> [-c <count>]
truffle doctor
```

### Communication

```bash
truffle send <node> <message> [--json]
truffle cp <src> <dst> [--no-verify] [--json]
truffle tcp <target> [--check]
```

### Streaming (Agent-Friendly)

```bash
truffle watch [--json] [--filter peer|message|transfer] [--timeout <secs>]
truffle wait <node> [--timeout <secs>] [--json]
truffle recv [--from <node>] [--timeout <secs>] [--json]
```

## Global Flags

| Flag | Description |
|------|-------------|
| `--json` | Output as JSON with versioned envelope |
| `--quiet` | Suppress non-essential output |
| `--verbose` | Show debug info |
| `--color <auto\|always\|never>` | Color mode |

## Exit Codes

| Code | Meaning |
|------|---------|
| 0 | Success |
| 1 | General error |
| 2 | Usage error |
| 3 | Peer not found |
| 4 | Not online |
| 5 | Timeout |
| 6 | Transfer failed |

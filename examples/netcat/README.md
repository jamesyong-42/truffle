# netcat

`netcat`-over-the-mesh: raw TCP between two devices on the tailnet, zero
servers, zero port-forwarding. `mesh.net` mimics `node:net`
([RFC 021](../../docs/rfcs/021-raw-transport-js-api.md) §8), so a Truffle
socket is a real `stream.Duplex` — this example just pipes it to/from
`stdin`/`stdout`, the same trick the real `nc` plays with a plain TCP socket.

## Prerequisites

- Tailscale installed and running on both devices.
- Both devices on the same tailnet, running this example with the **same
  `appId`** (`netcat-demo`, hardcoded in `src/main.ts`) — `appId` scopes
  peer visibility, so two different app ids can't see each other as peers.
- `pnpm install` from the repo root (links `@vibecook/truffle` as a
  workspace dependency).

## Usage

Run from the repo root, on each device:

```bash
# Device A — list peers to find B's name
pnpm --filter @vibecook/example-netcat exec tsx src/main.ts peers

# Device A — listen
pnpm --filter @vibecook/example-netcat exec tsx src/main.ts listen 9000

# Device B — connect (use the deviceName or deviceId printed by `peers`)
pnpm --filter @vibecook/example-netcat exec tsx src/main.ts connect device-a-name 9000
```

Once connected, anything typed on B's stdin shows up on A's stdout, and
vice versa — an interactive chat, just like plain `nc`.

### Commands

| Command | Description |
|---|---|
| `listen <port>` | Start a mesh node and accept one connection on `<port>`. |
| `connect <peer> <port>` | Connect to `<peer>` (device name, device id, or tailnet IP) on `<port>`. |
| `peers` | List known peers (`deviceName`, `deviceId`, `ip`, `online`) and exit. |

All diagnostics (connection status, errors) print to **stderr**; only the
socket's bytes go to **stdout**, so redirection works exactly like real
`nc`:

```bash
# Device A — receive a file
pnpm --filter @vibecook/example-netcat exec tsx src/main.ts listen 9000 > received.png

# Device B — send it
pnpm --filter @vibecook/example-netcat exec tsx src/main.ts connect device-a-name 9000 < photo.png
```

`connect` half-closes its write side (sends FIN) as soon as stdin hits EOF
but keeps reading until the peer closes its side too, so `< photo.png`
finishes sending, then `listen` writes out every received byte and exits
on its own — the same one-shot transfer behavior as `nc -N`.

## Caveats

- Needs two real devices on the same tailnet — this can't be exercised
  against `localhost` (mesh peers are other tailnet devices, not the
  local node itself).
- `listen` accepts a single connection and exits once it ends; run it
  again to accept another.

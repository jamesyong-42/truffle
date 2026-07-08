# ws-chat-over-mesh

Group chat over the tailnet built on `mesh.ws` — the `ws` package running over
mesh TCP ([RFC 021](../../docs/rfcs/021-raw-transport-js-api.md) §8). One device
runs a server that broadcasts each message to everyone connected; the others
connect and chat. No public server, no port forwarding, WireGuard-encrypted
end to end.

`mesh.ws` ships zero new protocol code — it's the ordinary `ws` API:
`mesh.ws.createServer({ port })` returns a real `ws` `WebSocketServer`, and
`mesh.ws.connect(peer, port)` returns a real `ws` `WebSocket`. Only the
transport underneath is the mesh.

## Prerequisites

- Tailscale installed and running on every device.
- All devices on the same tailnet, running this example with the **same
  `appId`** (`ws-chat-demo`, hardcoded in `src/main.ts`) — `appId` scopes peer
  visibility, so two different app ids can't see each other as peers.
- `pnpm install` from the repo root. Note `ws` is an **optional peer
  dependency** of `@vibecook/truffle`: apps that use `mesh.ws` install it
  themselves (this example lists `ws` in its own `package.json`). Skip it and
  `mesh.ws` throws a clear "install the ws package" error.

## Usage

Run from the repo root. Pick one device to host:

```bash
# Device A — list peers to find names, then host the chat
pnpm --filter @vibecook/example-ws-chat-over-mesh exec tsx src/main.ts peers
pnpm --filter @vibecook/example-ws-chat-over-mesh exec tsx src/main.ts serve 9500

# Devices B, C, … — connect (use A's deviceName, device id, or tailnet IP)
pnpm --filter @vibecook/example-ws-chat-over-mesh exec tsx src/main.ts connect device-a-name 9500
```

Type a line on any client and it appears on every other client, prefixed with
the sender's mesh device name. `Ctrl-D` (stdin EOF) or `Ctrl-C` leaves the chat.

### Commands

| Command | Description |
|---|---|
| `serve <port>` | Host the chat: accept clients on `<port>` and broadcast messages between them. |
| `connect <peer> <port>` | Join a chat hosted by `<peer>` (device name, device id, or tailnet IP). |
| `peers` | List known peers (`deviceName`, `deviceId`, `ip`, `online`) and exit. |

## How it works

The server side is a stock `ws` server whose transport is the mesh:

```ts
const server = await mesh.ws.createServer({ port: 9500 });
server.on('connection', (client, req) => {
  client.on('message', (data) => {
    for (const c of server.clients) if (c.readyState === c.OPEN) c.send(data.toString());
  });
});
```

The client side is a stock `ws` client whose transport is the mesh:

```ts
const socket = await mesh.ws.connect('device-a-name', 9500);
socket.on('open', () => socket.send('hello mesh'));
socket.on('message', (data) => console.log(data.toString()));
```

## Notes

- **Peer references with spaces work.** A device name like `"living room pi"`
  isn't a valid URL hostname, but `mesh.ws.connect('living room pi', 9500)`
  still reaches it: the real reference is pinned onto the request and the mesh
  agent dials it verbatim, while the `ws://` URL carries a placeholder host
  used only for the HTTP `Host` header. IPs and space-free names ride the URL
  directly.
- **Same `appId`:** every device must use the same `appId` (`'ws-chat-demo'`
  here) to see each other as peers.
- **Not line-rate:** traffic crosses the userspace tailnet netstack plus a
  loopback hop. Great for chat, control traffic, and app data; not for bulk
  throughput.
```

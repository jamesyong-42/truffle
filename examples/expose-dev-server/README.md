# Expose a local dev server over the mesh

Got something running on `localhost` — a Vite dev server, Grafana, an internal
API, a container — that you want to reach from your phone or share with a
teammate? Publish it to your tailnet in one call. No tunnel service, no public
exposure, no code changes to the thing you're exposing.

```ts
const published = await mesh.serve({ port: 443, target: 'http://localhost:3000' });
console.log(published.url); // https://<your-device>.<tailnet>.ts.net
```

The Go sidecar reverse-proxies the tailnet port to your local process; the
bytes never touch JS. This example starts a stand-in server on
`localhost:3000` so it runs on its own — swap the `target` for your real dev
server (e.g. `http://localhost:5173`).

## Prerequisites

- Node.js 18+
- Tailscale installed and running
- **MagicDNS and HTTPS certificates enabled** on your tailnet (admin console →
  DNS). `serve` defaults to TLS; see the plain-HTTP note below if you don't have
  them.

## Setup (from the repo root)

```bash
pnpm install
pnpm --filter @vibecook/truffle build
```

For a headless device, set a
[Tailscale auth key](https://tailscale.com/kb/1085/auth-keys):

```bash
export TS_AUTHKEY=tskey-auth-...
```

## Run it

```bash
pnpm --filter @vibecook/example-expose-dev-server start
```

It starts the local server, publishes it, and prints the tailnet URL. Open that
URL from any device on your tailnet.

To expose your **own** dev server, start it first, then edit `target` in
`src/main.ts` (and delete the stand-in server) — or just point the example at it.

## Notes

- **Loopback-only by default.** A `target` must resolve to localhost. Pointing
  at a LAN host (`http://192.168.1.10:…`) is refused unless you pass
  `allowNonLoopback: true`, because that would make this node a proxy into its
  local network. Opt in only when you mean to.
- **`target` must be a full `http(s)://` URL.** A bare `localhost:3000` is
  rejected (URLs read the host as a scheme) — include the `http://`.
- **No HTTPS on your tailnet?** Serve plain HTTP on any port:
  `mesh.serve({ port: 8080, target: 'http://localhost:3000', tls: false })`, then
  reach it at `http://<device-name>:8080/`. WireGuard still encrypts the hop.
- **Restrict access:** `allow: ['alice@corp.com']` (Tailscale loginName globs)
  gates the service to specific tailnet users; a non-match gets a 403.
- **Stop it:** `await published.close()` releases the tailnet port. This example
  wires it to Ctrl-C.

See the [Serving HTTP guide](../../docs/guide/serving-http.md) for `mesh.serve`
in full — static directories, mixed path routes, and access control.

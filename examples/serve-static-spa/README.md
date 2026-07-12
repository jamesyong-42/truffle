# Serve an SPA and its API over the mesh

Publish a single-page app **and** its API on one tailnet port — one origin, no
CORS, no Express, no handler code. `mesh.serve({ routes })` mounts several path
prefixes on one listener: here the static SPA at `/` and a reverse-proxied
backend at `/api`. The Go sidecar serves the files and proxies the API; any
Tailscale device (phone browsers included) opens it over a real `https://` URL.

```ts
const site = await mesh.serve({
  port: 443,
  routes: {
    '/api': 'http://localhost:8000',              // reverse-proxied backend
    '/': { dir: './public', fallback: '/index.html' }, // static SPA
  },
});
console.log(site.url); // https://<your-device>.<tailnet>.ts.net
```

Routes match by **longest prefix**, so `/api/*` hits the backend and everything
else the directory. `fallback` is the single-page-app trick: any path the
directory doesn't have (`/about`, `/settings/…`) is served `index.html` instead
of a 404, so your client-side router takes over — even on a hard refresh. This
example's `public/` is a tiny History-API app that calls `/api` and demonstrates
exactly that.

## Prerequisites

- Node.js 18+
- Tailscale installed and running
- **MagicDNS and HTTPS certificates enabled** on your tailnet (admin console →
  DNS). `serve` defaults to TLS, and the certificate needs both. Without them,
  see the plain-HTTP note below.

## Setup (from the repo root)

```bash
pnpm install
pnpm --filter @vibecook/truffle build
```

On first run the node authenticates with Tailscale; the auth URL opens in your
browser. For a headless device, set a
[Tailscale auth key](https://tailscale.com/kb/1085/auth-keys) instead:

```bash
export TS_AUTHKEY=tskey-auth-...
```

## Run it

```bash
pnpm --filter @vibecook/example-serve-static-spa start
```

It starts a small local backend, publishes the SPA and `/api` on one port, and
prints the public URL. Open it on this device, your phone, a teammate's laptop.
The home page shows a live `/api/hello` response — same origin, no CORS.
Navigate to **About** and hit refresh: it still loads, because the directory has
no `/about` file and `fallback` served `index.html`.

## Notes

- **Static serving semantics:** the sidecar uses Go's `http.FileServer` —
  `index.html` at directory roots, ETag / Range / correct mime types for free,
  **no directory listing**, and dotfiles are denied.
- **No HTTPS on your tailnet?** Add `tls: false` and use a normal port
  (`port: 8080`). WireGuard still encrypts the tailnet hop; you just won't get
  the browser padlock or a bare-443 URL. Reach it at `http://<device-name>:8080/`.
- **Restrict who can see it:** pass `allow: ['*@corp.com']` (Tailscale loginName
  globs) to gate the site to specific tailnet users; absent means the whole
  tailnet (subject to your ACLs).
- **Not line-rate:** traffic crosses the userspace tailnet netstack. Great for
  apps, dashboards, and demos; not for bulk throughput.

See the [Serving HTTP guide](../../docs/guide/serving-http.md) for `mesh.serve`
in full — mixed routes, proxying a local process, access control.

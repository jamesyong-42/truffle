# Serve a static SPA over the mesh

Publish a directory of static files — a single-page app, a docs build, anything
— to your whole tailnet with one call. No Express, no handler, no reverse proxy.
The Go sidecar serves the files directly, and any Tailscale device (phone
browsers included) can open the site over a real `https://` URL.

The whole thing is one line:

```ts
const site = await mesh.serve({ port: 443, dir: './public', fallback: '/index.html' });
console.log(site.url); // https://<your-device>.<tailnet>.ts.net
```

`fallback` is the single-page-app trick: any path the directory doesn't have
(`/about`, `/settings/…`) is served `index.html` instead of a 404, so your
client-side router takes over — even on a hard refresh. This example's
`public/` is a tiny History-API app that demonstrates exactly that.

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

It prints the public URL. Open it on this device, your phone, a teammate's
laptop — anything on the tailnet. Navigate to **About** and hit refresh: the
page still loads, because the server missed `/about` and `fallback` served
`index.html`.

## Notes

- **Static serving semantics:** the sidecar uses Go's `http.FileServer` —
  `index.html` at directory roots, ETag / Range / correct mime types for free,
  **no directory listing**, and dotfiles are denied.
- **No HTTPS on your tailnet?** Serve plain HTTP on any port instead:
  `mesh.serve({ port: 8080, dir: './public', fallback: '/index.html', tls: false })`.
  WireGuard still encrypts the tailnet hop; you just won't get the browser
  padlock or a bare-443 URL. Reach it at `http://<device-name>:8080/`.
- **Restrict who can see it:** pass `allow: ['*@corp.com']` (Tailscale loginName
  globs) to gate the site to specific tailnet users; absent means the whole
  tailnet (subject to your ACLs).
- **Not line-rate:** traffic crosses the userspace tailnet netstack. Great for
  apps, dashboards, and demos; not for bulk throughput.

See the [Serving HTTP guide](../../docs/guide/serving-http.md) for `mesh.serve`
in full — mixed routes, proxying a local process, access control.

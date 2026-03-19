Overall verdict: good direction, but not yet the best design Tailscale enables.

At a high level, Truffle is making a lot of the right calls:
•	embedding tsnet instead of reinventing transport,
•	leaning on MagicDNS + WireGuard instead of NAT traversal work,
•	keeping the Go sidecar thin,
•	exposing a higher-level Rust API instead of leaking Tailscale internals.

That said, the RFC still leaves meaningful Tailscale value on the table, and a few design choices are more “works” than “best practice.” The biggest issues are:
1.	you are still treating Tailscale mostly as a dumb encrypted socket layer,
2.	your mesh/discovery model duplicates some things Tailscale can already help with,
3.	your HTTP design is good but needs sharper boundaries around identity, authorization, and service exposure,
4.	the current transport/primary-election design creates failure modes that Tailscale itself does not solve for you.

Bottom line

I would rate it like this:
•	Use of Tailscale fundamentals: strong
•	Use of advanced Tailscale features: moderate, with clear underuse
•	API/library design: promising
•	Operational design: decent, but needs hardening
•	Best-practice alignment: partial, not complete

My conclusion is:
•	Yes, this is a viable architecture.
•	No, it is not yet “making the most out of Tailscale.”
•	No, I would not call it fully best-practice yet for production.
•	Yes, with a few redesigns it can become a very solid library.

⸻

What is strong already

1. Using tsnet as the substrate is the right bet

tsnet is specifically meant to embed a Tailscale node inside a Go program, and Tailscale positions it as a way to give a program its own IP, DNS name, and HTTPS certificate, even allowing multiple services with separate identities/rules on one machine. That fits Truffle’s “embedded networking library” goal very well.  ￼

2. Keeping the Go sidecar thin is good design

Because tsnet is Go-only, a thin sidecar is a reasonable boundary. The sidecar should own:
•	node lifecycle,
•	Tailscale listeners/dials,
•	local API calls like Status, WhoIs, and watchers,
•	the minimum bridging needed into Rust.

That matches the spirit of your layering.

3. The builder-style Rust API is good

The public API is clean and understandable. TruffleRuntime::builder(), runtime.bus(), runtime.file_transfer(), runtime.http() is the right shape for adoption.

4. Your “Truffle is not a framework” positioning is excellent

That is exactly the right product decision. It keeps the library composable and keeps you from overreaching into app structure.

⸻

Where the design is not making full use of Tailscale

1. You are underusing Tailscale identity and authorization

Right now the design still centers too much on:
•	hostname prefix filtering,
•	app-defined capabilities,
•	app-level primary election,
•	app-level message routing,

instead of more deeply using:
•	tags,
•	grants,
•	app capabilities,
•	WhoIs identity,
•	possibly Services for stable service entrypoints.

Tailscale’s grants system supports app capabilities in the policy file, not just raw IP access rules. That means Truffle can move beyond “is this peer in the tailnet?” and into “is this peer allowed to do X in this app?” if you wire the identity data through properly.  ￼

What I would change

Make peer authorization a first-class concept in the RFC:
•	every incoming connection should be resolved with WhoIs,
•	bridge metadata should include node identity and, when relevant, user/profile/capability context,
•	Rust-side handlers should be able to make authorization decisions based on that identity,
•	Truffle should expose hooks like:
•	authorize_http(peer, route)
•	authorize_bus(peer, namespace, msg_type)
•	authorize_file_transfer(peer, file_meta)

That would be a much more “Tailscale-native” design than simple device discovery plus trust.

⸻

2. Hostname-prefix discovery is functional, but not the best long-term design

Using Tailscale peer lists plus a hostname prefix to identify same-app nodes is pragmatic, but it is also brittle.

Problems:
•	hostnames are naming, not identity,
•	collisions and convention drift are easy,
•	it makes multi-app coexistence on the same tailnet messier,
•	it does not map cleanly to policy.

Tailscale is designed around tags and grants for service identity and access control, and tagged/server-style devices are a recommended pattern for auth keys as well. Tagged devices also have key expiry disabled by default after re-authentication, which is helpful for service-like nodes.  ￼

Better design

Keep hostname prefixes only as a convenience hint, but make the real grouping model one of these:
•	Option A: tag-based grouping
•	tag:truffle
•	tag:truffle-myapp
•	possibly per-role tags such as tag:truffle-primary-candidate
•	Option B: explicit app identity in announce protocol + WhoIs verification
•	app ID in the mesh envelope
•	verify the peer node is an allowed Truffle node before accepting its announce

Best version:
•	use tags for coarse trust boundary,
•	use Truffle app ID / protocol version for app membership,
•	use capabilities / policy for fine-grained authorization.

That is much better than hostname-prefix discovery alone.

⸻

3. You are not yet taking enough advantage of Tailscale Services

This is one of the biggest “missed opportunity” areas.

Tailscale now has Services, and tsnet.Server.ListenService is specifically for registering a tsnet application as a Service host. Service hosts must be tagged, approved, and advertise the required ports. Tailscale’s own docs recommend this as the pattern for service registration and discovery in supported cases.  ￼

Why this matters to Truffle

Your current plan says:
•	discover peers,
•	elect a primary,
•	PWA connects to some node,
•	secondary redirects to the primary.

That works, but it is an application-level workaround.

For some use cases, Services may be a better fit:
•	a stable app entrypoint,
•	one DNS/service identity regardless of which node currently serves,
•	cleaner policy around service access,
•	easier future HA semantics.

Important nuance

I would not replace your whole peer mesh with Services.
Services are better for:
•	stable app-facing HTTP entrypoints,
•	“connect to the app” semantics,
•	maybe exposing one logical control endpoint.

They are not necessarily the best primitive for:
•	full peer-to-peer device mesh,
•	per-device sync graph,
•	device-scoped slices.

So my recommendation is:
•	keep direct node-to-node Truffle mesh,
•	consider Services specifically for HTTP/PWA entrypoints, especially when the user experience cares about one stable URL instead of “find primary then redirect.”

That would make the architecture more Tailscale-native.

⸻

4. The design should treat WhoIs as foundational, not just an optimization

Your own gap analysis is correct here: WhoIs is purpose-built for identifying the peer behind an incoming address, while Status() for every connection is the wrong tool. The local API documents Client.WhoIs for IP or IP:port lookup, and WatchIPNBus for live state notifications.  ￼

But I would go further than your RFC does.

WhoIs should drive:
•	incoming bridge identity,
•	authorization,
•	audit logging,
•	route restrictions,
•	file-transfer acceptance policy,
•	future app-capability checks.

This should not be framed as only a performance fix. It is a security and design primitive.

⸻

Where the design has architectural risk

5. The STAR topology + primary election is your biggest non-Tailscale risk

This is the part I am least convinced by.

Tailscale gives you an encrypted, addressable mesh. On top of that, Truffle adds:
•	STAR topology,
•	deterministic primary election,
•	secondary-to-primary routing,
•	primary redirect for browser clients.

That is a lot of app-level coordination logic, and it introduces:
•	a central bottleneck,
•	a central point of failure,
•	failover edge cases,
•	ordering/race issues,
•	reconnect storms around role change,
•	more state to reason about.

My view

This is okay only if you truly need a logical coordinator.

But your RFC currently uses the primary for too many things by default:
•	routing,
•	browser redirection,
•	coordination,
•	implied leadership for app state.

That may be over-centralizing a system that Tailscale already makes naturally peer-to-peer.

Better model

Split these concerns:
•	Peer mesh remains peer-to-peer by default.
•	Leadership/coordinator exists only for features that truly require one.
•	HTTP entrypoint may be separate from coordinator.
•	Bus routing should prefer direct delivery whenever possible.
•	Primary should be advisory, not the universal choke point.

In other words:
•	do not make “there is a primary” equal “all meaningful traffic routes through it.”

For store sync and pub/sub, consider:
•	direct fanout where practical,
•	optional coordinator mode,
•	namespace-specific routing policy.

That would reduce complexity and better match the underlying network.

⸻

6. The transport layer may be too WebSocket-centric

Tailscale is already giving you authenticated, encrypted TCP connectivity. Then Truffle layers:
•	bridge framing,
•	local TCP,
•	WebSocket,
•	JSON envelope,
•	message bus.

That stack is understandable, especially for cross-platform/browser symmetry, but it is also quite layered.

Concern

For native-to-native communication, WebSocket may be more compatibility-driven than necessary.

You are effectively using WebSocket as:
•	a framing layer,
•	a cross-language transport abstraction,
•	a browser-compatible shape.

That is fine, but I would be careful not to let browser needs dictate all runtime transport choices.

Stronger design

Consider explicitly distinguishing:
•	native mesh transport
•	browser transport

For example:
•	native/native could stay on a simpler framed stream protocol over the bridged TCP,
•	browser-facing traffic can keep WebSocket/HTTP.

You do not have to change this immediately, but I would avoid enshrining “everything is a WebSocket” too deeply in the long-term architecture.

⸻

7. Port 443 path-routing is reasonable, but you need clearer service boundaries

Your HTTP router idea is good. Using one TLS listener on :443 and routing by path in Rust is a clean direction, and ListenTLS(":443") is a natural fit for HTTPS services on the tailnet.  ￼

But there are a few caveats.

Caveat A: operational blast radius

If /ws, /proxy/*, /static/*, /pwa/*, and push APIs all share the same listener and router:
•	one bug in the router can affect everything,
•	one slow handler can harm unrelated traffic,
•	auth mistakes have wider impact.

This is fine for simplicity, but only if your internal routing and auth boundaries are very clean.

Caveat B: reverse proxy and app UI on same origin

This is convenient, but it also means:
•	CSRF/origin assumptions matter,
•	route collisions matter,
•	browser cookies/storage concerns may get messy later.

Caveat C: certificate transparency exposure

Tailscale HTTPS certs rely on publicly logged certificates, and the docs explicitly note that machine names in TLS certificates are published to the public certificate transparency ledger. That does not expose the service itself, but it does expose the names.  ￼

So:
•	do not encourage sensitive hostnames,
•	document naming guidance in Truffle,
•	consider a naming scheme that is non-sensitive and non-user-identifying.

That should be called out in the RFC because you are building heavily around HTTPS entrypoints.

⸻

8. Reverse proxy design is good, but path-prefix-only routing is not always enough

Path-based routing on 443 is good for simplicity, but it is not universally ideal.

Cases where it may be limiting:
•	apps that want separate origins,
•	apps that expect absolute root paths,
•	apps that break under prefix rewriting,
•	future service separation by host rather than path.

Better design

Support path-prefix routing first, but leave room for:
•	host-based routing later,
•	optional dedicated listener/service modes later,
•	pass-through mode without aggressive path rewriting.

Do not lock the public API into only path-prefix mental models.

⸻

9. PWA design is decent, but “primary redirect” is not the best UX/control plane story

The PWA flow is workable, but this part feels more like an app-specific workaround than a durable platform primitive.

If the user browses to a node and gets bounced to the primary:
•	it works,
•	but it leaks topology,
•	it adds reconnect churn,
•	it couples browser UX to leadership semantics.

I would prefer one of these:
•	a stable Service entrypoint for the browser-facing app,
•	or a stronger concept of “frontend node” separate from “primary mesh coordinator.”

Again, this is where Tailscale Services may help for the HTTP side even if you keep your peer mesh for everything else.  ￼

⸻

Best-practice review against Tailscale specifically

What aligns with best practices

Good
•	Embedded tsnet node per app instance is aligned with tsnet’s purpose.  ￼
•	Using WhoIs for inbound identity is the right move.  ￼
•	Using WatchIPNBus or equivalent continuous state monitoring is best practice over one-time startup polling.  ￼
•	Supporting ephemeral nodes is a good fit for dev/CI/short-lived workloads, and Tailscale explicitly recommends ephemeral auth keys for those cases.  ￼
•	Using tagged devices for service-like workloads is aligned with Tailscale guidance.  ￼

What does not yet align enough

Weak / missing
•	Not enough use of tags + grants as first-class policy boundaries.
•	Not enough use of app capabilities as authorization input.  ￼
•	Not enough consideration of Services for stable app-facing endpoints.  ￼
•	Too much dependence on hostname prefix conventions.
•	HTTP/PWA design does not yet discuss cert-transparency naming/privacy implications.  ￼

⸻

My concrete recommendations

Priority 1: redesign identity and authorization

Make this a core part of the RFC:
•	all inbound connections go through WhoIs,
•	include node/user identity in bridge metadata,
•	expose authorization hooks in Rust,
•	add optional policy integration for tags/capabilities.

This is the highest-value design improvement.

Priority 2: reduce dependence on the primary

Keep primary election only where needed.
Default to:
•	direct peer communication,
•	direct file transfer,
•	direct namespace fanout where feasible.

Make the primary a coordinator, not the universal traffic hub.

Priority 3: add a “stable entrypoint” story

For browser/PWA access, explicitly evaluate:
•	ListenService for one logical app endpoint,
•	versus current per-node URL + primary redirect.

I would add a design section comparing both.

Priority 4: promote tags from “planned” to “architectural”

Do not leave tags/ACLs as a future nice-to-have.
They should be part of the baseline design:
•	app nodes should be taggable,
•	docs should recommend tagged/pre-approved auth keys for service nodes,
•	policy examples should be part of the RFC.  ￼

Priority 5: separate native transport from browser transport conceptually

Even if implementation stays WebSocket-first today, the architecture should not assume that forever.
Document:
•	browser transport = HTTP/WebSocket,
•	native transport = currently WebSocket, but may evolve.

Priority 6: add hostname/privacy guidance

Because HTTPS certs publish names to CT logs, the RFC should explicitly say:
•	avoid sensitive machine names,
•	prefer neutral generated names for public-cert nodes,
•	document that HTTPS enablement has naming visibility implications.  ￼

⸻

Specific comments on the public API

The public API is mostly strong. A few suggestions:

1. prefer_primary(true) is underspecified

What does that mean operationally?
•	prefer primary for routing?
•	prefer connecting to primary first?
•	prefer browser entrypoint on primary?

This should be narrower or split into explicit options.

2. capabilities(vec![...]) is ambiguous

Are these:
•	app-declared runtime capabilities,
•	Tailscale grant app capabilities,
•	feature flags,
•	advertised handlers?

Rename or clarify. Right now it sounds too close to Tailscale’s “app capabilities” term.  ￼

3. runtime.http().proxy("/", ...) plus serve_static("/", ...)

You need deterministic precedence rules and probably route validation to avoid footguns.

4. runtime.dns_name()

Make it explicit whether this is:
•	node DNS name,
•	service FQDN,
•	currently active browser entrypoint.

That distinction will matter a lot once Services enter the picture.

⸻

Final judgment

Here is my honest summary.

The good news

This RFC is substantively good. It is not a naive design. It shows strong understanding of:
•	what Tailscale gives you,
•	what belongs in Go versus Rust,
•	how to package the library,
•	how to expose a usable runtime API.

The main problem

It still treats Tailscale a bit too much like:
•	secure pipes,
•	peer list,
•	DNS names,
•	certs,

and not enough like:
•	identity fabric,
•	policy engine,
•	service-discovery/control layer.

My recommendation

I would approve the direction, but I would revise the RFC before treating it as the production architecture.

If I were editing it, I would make these three changes mandatory before calling it “best design”:
1.	Identity/authorization via WhoIs + tags/grants/app-capabilities
2.	Re-scope the primary so it is not the default choke point
3.	Add a formal comparison of per-node HTTP entrypoints vs Tailscale Services for browser-facing access

Once those are in place, the design becomes much stronger and much more Tailscale-native.
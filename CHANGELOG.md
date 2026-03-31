# Changelog

## [0.3.16](https://github.com/jamesyong-42/truffle/compare/truffle-v0.3.15...truffle-v0.3.16) (2026-03-31)


### Bug Fixes

* filter KeyEventKind::Press to prevent double input on Windows ([ebf0be3](https://github.com/jamesyong-42/truffle/commit/ebf0be395d4da0c227a94072053fa9d1adc3575b))

## [0.3.15](https://github.com/jamesyong-42/truffle/compare/truffle-v0.3.14...truffle-v0.3.15) (2026-03-31)


### Features

* add SyncedStore and request/reply primitives (RFC 016) ([88f5ea4](https://github.com/jamesyong-42/truffle/commit/88f5ea4b83f3f82834e96ea387cfe4e61bd2ea94))

## [0.3.14](https://github.com/jamesyong-42/truffle/compare/truffle-v0.3.13...truffle-v0.3.14) (2026-03-30)


### Bug Fixes

* exclude Tauri plugin and NAPI from CI tests ([623505b](https://github.com/jamesyong-42/truffle/commit/623505bc058feb8e0c06832c3ec3f4ae638f7621))
* rename connected to ws_connected for explicit WebSocket semantics ([193a827](https://github.com/jamesyong-42/truffle/commit/193a827756098e5ce973d0215b8da7b6867bafbf))

## [0.3.13](https://github.com/jamesyong-42/truffle/compare/truffle-v0.3.12...truffle-v0.3.13) (2026-03-30)


### Features

* rewrite NAPI-RS bindings for current Node API (RFC 015 Phase 1) ([c828d9f](https://github.com/jamesyong-42/truffle/commit/c828d9f6a3e477dc854fce209e3a6815a45e7170))
* rewrite Tauri v2 plugin for current Node API (RFC 015 Phase 2) ([fcf174d](https://github.com/jamesyong-42/truffle/commit/fcf174d2c1a6098e4202ce6b3b1efa380a959100))


### Bug Fixes

* address all NAPI + Tauri binding audit findings ([fea6a92](https://github.com/jamesyong-42/truffle/commit/fea6a926a2260b32182af9524c00cb3fa1abdc80))

## [0.3.12](https://github.com/jamesyong-42/truffle/compare/truffle-v0.3.11...truffle-v0.3.12) (2026-03-30)


### Bug Fixes

* bump Cargo.toml versions to 0.3.12 ([298d300](https://github.com/jamesyong-42/truffle/commit/298d300cfb061931c211be90d7de738fae14aa52))
* complete file transfer UX — hashing/waiting/receive progress ([cb9113c](https://github.com/jamesyong-42/truffle/commit/cb9113c0ee964682b9fb2609ccaf0b711edb7619))

## [0.3.11](https://github.com/jamesyong-42/truffle/compare/truffle-v0.3.10...truffle-v0.3.11) (2026-03-30)


### Bug Fixes

* bump Cargo.toml versions to 0.3.11 ([6ce3693](https://github.com/jamesyong-42/truffle/commit/6ce3693da7a11b210fe87eb93b042ca0021e1b1b))
* revert ARM64 Linux to gnu target (musl cross needs extra tooling) ([e25ede2](https://github.com/jamesyong-42/truffle/commit/e25ede29decfd00668fe17d26c6bfc83d5c0d62c))
* throttle file transfer progress + show hashing phase ([4ed9477](https://github.com/jamesyong-42/truffle/commit/4ed9477ca52521ed70b8c03ac77504e7cb9042de))

## [0.3.10](https://github.com/jamesyong-42/truffle/compare/truffle-v0.3.9...truffle-v0.3.10) (2026-03-30)


### Bug Fixes

* use musl (static linking) for Linux release binaries ([c707275](https://github.com/jamesyong-42/truffle/commit/c707275fbbd36339a8529c3a63455f65f9006125))

## [0.3.9](https://github.com/jamesyong-42/truffle/compare/truffle-v0.3.8...truffle-v0.3.9) (2026-03-30)


### Features

* add file_transfer module to truffle-core (RFC 014 Phase 1) ([da85cec](https://github.com/jamesyong-42/truffle/commit/da85cec6657da7307c0b882adfe2e1d81843c9cc))
* file overwrite confirmation dialog ([20ca606](https://github.com/jamesyong-42/truffle/commit/20ca6068753b78be6eb5e4cca91d2f599945871d))
* interactive file transfer accept/reject modal dialog (RFC 014 Phase 3) ([ceb7cec](https://github.com/jamesyong-42/truffle/commit/ceb7cece93b24f4cc78c11b1024a160dc7ce30cd))
* rich file transfer progress display in TUI feed (RFC 014 Phase 4) ([b0d7b48](https://github.com/jamesyong-42/truffle/commit/b0d7b48fefde92bfad70829281179ab771da146c))


### Bug Fixes

* add debug logging to envelope router and file save path ([397ee95](https://github.com/jamesyong-42/truffle/commit/397ee95e97c4d2cfc29f3b4144a9f80cf98d2958))
* address all RFC 014 audit findings ([7373f80](https://github.com/jamesyong-42/truffle/commit/7373f806530783a5c01f90e533f11438f76c16df))
* auto-open file explorer when /cp command is entered ([045b3b2](https://github.com/jamesyong-42/truffle/commit/045b3b23fd7fbe075cc639e4cecbb3929817bead))
* bump Cargo.toml versions to 0.3.9 ([b75cbda](https://github.com/jamesyong-42/truffle/commit/b75cbda6b047c79419854fcad288f377b4c2f9e8))
* clear activity feed area before rendering file picker overlay ([d37d49a](https://github.com/jamesyong-42/truffle/commit/d37d49abf6862c7b1e929043648422f43f9c61dc))
* dismiss device autocomplete list after selecting a device ([6fa20a3](https://github.com/jamesyong-42/truffle/commit/6fa20a3a2717573492e9482dfb0f19906b7a5c11))
* don't reopen file picker when completing [@device](https://github.com/device) in /cp command ([80d8b2a](https://github.com/jamesyong-42/truffle/commit/80d8b2a7b345da95db9ffbfc6a4eed42cd97971e))
* Esc on file picker resets input to / and shows command list ([c053e29](https://github.com/jamesyong-42/truffle/commit/c053e29de36645c89e726d92ff8c153944da3dde))
* file picker Enter now correctly fills path instead of clearing input ([e5c0373](https://github.com/jamesyong-42/truffle/commit/e5c037365be8803119ea294042ae074e9f894df0))
* file picker opens whenever input is exactly "/cp " ([4b8cb02](https://github.com/jamesyong-42/truffle/commit/4b8cb02cd9cc146eb7c09cfe7b7e223ec1b47df9))
* file transfer bridge half-close and ACK timeout handling ([ee4f0f4](https://github.com/jamesyong-42/truffle/commit/ee4f0f4f75c839fb84b2a301ea5760dbaac7269c))
* file transfer save path resolution and cross-device rename ([a78fa5b](https://github.com/jamesyong-42/truffle/commit/a78fa5b3a710b61eb0882999c75fb9761a4de4df))
* open file picker immediately on Save As ([6336166](https://github.com/jamesyong-42/truffle/commit/6336166b0627e5b9bebd49669ce9a89f56a58e0a))
* TUI Enter key fills autocomplete selection instead of submitting ([7ffaf7c](https://github.com/jamesyong-42/truffle/commit/7ffaf7c3ab94f8d62819df561217a96f514d5f7e))

## [0.3.8](https://github.com/jamesyong-42/truffle/compare/truffle-v0.3.7...truffle-v0.3.8) (2026-03-29)


### Features

* add interactive file picker for /cp command ([e74e3e2](https://github.com/jamesyong-42/truffle/commit/e74e3e2e78f0f0c02737d528de14376c53b0292f))
* first-run onboarding TUI with smart compact naming ([53560a5](https://github.com/jamesyong-42/truffle/commit/53560a5951d529f2162129e155dc37b3649f0408))
* propagate auth events through session layer to TUI ([a8e7bcb](https://github.com/jamesyong-42/truffle/commit/a8e7bcb8c76d6f70b7622a6092387f057b90fafa))


### Bug Fixes

* bump Cargo.toml versions to 0.3.8 ([bdbcc73](https://github.com/jamesyong-42/truffle/commit/bdbcc7394b17445d2d6e4cbb703dd18b5768f630))
* delayed peer re-poll to catch peers missed during TUI startup ([fb45c90](https://github.com/jamesyong-42/truffle/commit/fb45c900d080a29740e3deb29bf71369ca363c7d))

## [0.3.7](https://github.com/jamesyong-42/truffle/compare/truffle-v0.3.6...truffle-v0.3.7) (2026-03-29)


### Bug Fixes

* bump Cargo.toml versions to 0.3.7 ([33e2f96](https://github.com/jamesyong-42/truffle/commit/33e2f967e502b92826c282e3c7e4809b4a341876))
* fast peer offline detection via WantRunning=false on shutdown ([4e54043](https://github.com/jamesyong-42/truffle/commit/4e5404385341036ce4a741b30ce66a39b0cba0ff))
* handle PeerEvent::Updated to track online/offline status changes ([799be7c](https://github.com/jamesyong-42/truffle/commit/799be7cbf051be0b9581c32c58a357cdfb61b1a2))
* populate initial peer list on TUI startup ([6c5d175](https://github.com/jamesyong-42/truffle/commit/6c5d1759955d13136a98376d3129cd62172cc63f))

## [0.3.6](https://github.com/jamesyong-42/truffle/compare/truffle-v0.3.5...truffle-v0.3.6) (2026-03-29)


### Bug Fixes

* bump Cargo.toml versions to 0.3.6 ([86a298f](https://github.com/jamesyong-42/truffle/commit/86a298f1d257fae80e97a2f0b6ee75510ec3a8cd))
* suppress tracing and sidecar logs in TUI mode ([ed88ee3](https://github.com/jamesyong-42/truffle/commit/ed88ee3a03fb09fa5be4bbb5c9bb63940cfb5dcf))

## [0.3.5](https://github.com/jamesyong-42/truffle/compare/truffle-v0.3.4...truffle-v0.3.5) (2026-03-29)


### Features

* Claude Code-inspired TUI visual redesign ([1f3a18b](https://github.com/jamesyong-42/truffle/commit/1f3a18bbc0fa11eef6c248af3e03482c9409e58d))


### Bug Fixes

* add top margin to logo and vertical divider in welcome box ([86cd3ef](https://github.com/jamesyong-42/truffle/commit/86cd3ef4a0b8cbe1481b2eb4e66a4c3987392ec7))
* auto-stop background daemon when TUI launches ([5973065](https://github.com/jamesyong-42/truffle/commit/597306520d369be3352dde89ee931ae9d7117473))
* bump crate versions to 0.3.4 and add crates.io publish workflow ([ecd1b58](https://github.com/jamesyong-42/truffle/commit/ecd1b5864a5a7db91f88c49ba0a3331a48d4258b))
* fix pre-existing CI failures (doc-test type annotation + prettier) ([7e44530](https://github.com/jamesyong-42/truffle/commit/7e4453090f6fa5a1b8a67b600b826885b4a04133))
* redirect stderr to log file in TUI mode ([c975166](https://github.com/jamesyong-42/truffle/commit/c97516613512fc57df0e487e082f8f956e4b2b34))
* use OIDC trusted publishing for crates.io (no API token needed) ([2a0ee74](https://github.com/jamesyong-42/truffle/commit/2a0ee74a3aac041a75f9d1f312856327717a2450))

## [0.3.4](https://github.com/jamesyong-42/truffle/compare/truffle-v0.3.3...truffle-v0.3.4) (2026-03-29)


### Features

* add interactive TUI mode and agent-friendly CLI enhancements (RFC 013) ([dc43b22](https://github.com/jamesyong-42/truffle/commit/dc43b22ee475c87ac967ded5ddf722fc8577abc1))
* add truffle and truffle-sidecar crates for crates.io publishing ([8836ad6](https://github.com/jamesyong-42/truffle/commit/8836ad6161c1db9ff4114f99ab6e7b37c8c15b82))

## [0.3.3](https://github.com/jamesyong-42/truffle/compare/truffle-v0.3.2...truffle-v0.3.3) (2026-03-28)


### Bug Fixes

* add .exe extension to sidecar binary lookup on Windows ([557498b](https://github.com/jamesyong-42/truffle/commit/557498b0f21fc5d013c79aa3a7b01511d1f21a7a))
* bump Cargo.toml versions to 0.3.2 and fix release-please config ([057e017](https://github.com/jamesyong-42/truffle/commit/057e0171b23d9511b087c72edbd18a426fd22731))
* revert release-please extra-files to generic type (cargo-workspace unsupported) ([4b6c669](https://github.com/jamesyong-42/truffle/commit/4b6c6695f8a0129fbe3e43a24010c5b007875a18))

## [0.3.2](https://github.com/jamesyong-42/truffle/compare/truffle-v0.3.1...truffle-v0.3.2) (2026-03-28)


### Features

* add silent background auto-update on truffle up ([0fc532f](https://github.com/jamesyong-42/truffle/commit/0fc532f9fd89a8015b48c953a7514b0105d2d62c))

## [0.3.1](https://github.com/jamesyong-42/truffle/compare/truffle-v0.3.0...truffle-v0.3.1) (2026-03-28)


### Features

* add truffle update self-update command ([8452a8b](https://github.com/jamesyong-42/truffle/commit/8452a8bb798f64339ec906ab62bb74275a090899))


### Bug Fixes

* add --repo flag to gh workflow dispatch in release-please ([a75dd0a](https://github.com/jamesyong-42/truffle/commit/a75dd0aa0a1de4acd1a0c316894c1f26b9c9f46d))
* remove truffle-napi from pnpm workspace (not in Cargo workspace) ([9a90485](https://github.com/jamesyong-42/truffle/commit/9a9048590741af4a47bdd22b4ab8e8dbd2f25728))
* sidecar discovery looks for both 'sidecar-slim' and 'truffle-sidecar' ([173db1a](https://github.com/jamesyong-42/truffle/commit/173db1a4939f8cb303596ff4301584ec67099e5c))

## [0.3.0](https://github.com/jamesyong-42/truffle/compare/truffle-v0.2.5...truffle-v0.3.0) (2026-03-26)


### ⚠ BREAKING CHANGES

* The entire codebase has been rebuilt with a clean 7-layer architecture. Old APIs (BridgeManager, TruffleRuntime, MeshNode, StoreSyncAdapter) are removed. The new Node API is the single public entry point. NAPI-RS and Tauri plugin bindings need updating to the new API.

### Features

* add UDP support to Layer 3 via tsnet ListenPacket relay ([8008f0f](https://github.com/jamesyong-42/truffle/commit/8008f0f5755078915a410b4df86914e5102998c1))
* complete RFC 012 architecture rewrite (v2) ([7a87386](https://github.com/jamesyong-42/truffle/commit/7a873868589a38d4897b0ae9fb8d76578e7995df))
* implement QUIC over tsnet via TsnetUdpSocket (quinn AsyncUdpSocket adapter) ([b404452](https://github.com/jamesyong-42/truffle/commit/b404452c753be45657295f70b613595b81d7d3d8))
* implement real QUIC + UDP transports (RFC 012 Phase 8) ([9362cea](https://github.com/jamesyong-42/truffle/commit/9362ceae77feb8a2356d8ba3365bef1c59bb91f7))
* implement RFC 012 Phase 1 — Layer 3 Network (truffle-core-v2) ([2f3e0c0](https://github.com/jamesyong-42/truffle/commit/2f3e0c0fb0e9a96cf97eee8da9e3d0b4cfea8b28))
* implement RFC 012 Phase 2 — Layer 4 Transport (truffle-core-v2) ([5ce6ab1](https://github.com/jamesyong-42/truffle/commit/5ce6ab1ab52c3bf5f594f811ff48efc02adb8e4f))
* implement RFC 012 Phase 3 — Layer 5 Session (PeerRegistry) ([7d20eac](https://github.com/jamesyong-42/truffle/commit/7d20eac9bb5535c0cd30c56aeefad6abd2340f49))
* implement RFC 012 Phase 4+5 — Layer 6 Envelope + Node API ([78877ac](https://github.com/jamesyong-42/truffle/commit/78877acd0cf206a959e4609a87427f8e112adce8))
* implement RFC 012 Phase 6 — CLI v2 on Node API ([220e244](https://github.com/jamesyong-42/truffle/commit/220e244b3aa6ab836a144132f54b7b41550bc723))
* swap to v2 architecture (RFC 012 Phase 7) ([2f2f310](https://github.com/jamesyong-42/truffle/commit/2f2f3109ad1fad72c8a8440ac4593d6d0b1160a2))


### Bug Fixes

* address Phase 1 audit findings (bind_udp stub, local_identity cache, bridge error handling) ([c35978c](https://github.com/jamesyong-42/truffle/commit/c35978c5fb505c037fe52ec6862d4dbfa4b4ae77))
* auth flow waits for browser approval instead of failing immediately ([c9fe442](https://github.com/jamesyong-42/truffle/commit/c9fe442c270c8a31c826f8d96be782b104ce8505))
* cross-machine transport test — TCP read loop, UDP via tsnet relay, QUIC skip ([b88f009](https://github.com/jamesyong-42/truffle/commit/b88f009c638db8937875899d9574829f952e8c56))
* debug and fix hanging QUIC transport tests ([3db235a](https://github.com/jamesyong-42/truffle/commit/3db235ab7f387581127e53aa7169b41f8eafb2d1))
* Phase 2 audit — split WS stream, fix heartbeat, add Sync bound + tests ([7b36120](https://github.com/jamesyong-42/truffle/commit/7b361201e3b706203a89e919cc709418531967fc))
* Phase 3 audit — reconnect backoff, Left closes WS, concurrent send protection ([a6adaec](https://github.com/jamesyong-42/truffle/commit/a6adaec88a789c5b4ce42618a0fcaed99e7e3553))
* resolve 5 bugs found in real Tailscale e2e test (macOS &lt;-&gt; EC2) ([bdc4a19](https://github.com/jamesyong-42/truffle/commit/bdc4a19a3c0cc9ef07513e46dd377d4489f57e1e))
* UDP relay registration packet so receiver knows Rust peer address ([6b0f998](https://github.com/jamesyong-42/truffle/commit/6b0f998f1dfd1a384c3055e34778c25973482add))
* UDP relay uses 4-byte IPv4 instead of 16-byte IPv4-mapped IPv6 ([a121d39](https://github.com/jamesyong-42/truffle/commit/a121d397afdaba1544854d6881b0c3808e3a2d78))
* update CI workflows and release-please for new crate structure ([7964e71](https://github.com/jamesyong-42/truffle/commit/7964e71daa462fba78f811d632a5e6c38f3602af))

## [0.2.5](https://github.com/jamesyong-42/truffle/compare/truffle-v0.2.4...truffle-v0.2.5) (2026-03-24)


### Bug Fixes

* DialFn lock contention + TcpProxyHandler shutdown + Connection: close ([afa36b6](https://github.com/jamesyong-42/truffle/commit/afa36b649b8895b71e2a18cecb21c0da586b18d9))
* ensure mesh connection before file transfer OFFER/PULL_REQUEST ([0352501](https://github.com/jamesyong-42/truffle/commit/03525019c7f69d996d75ece5a303788196e7777a))
* resolve file transfer blocking_read deadlock + DNS resolution + heartbeat timeouts ([a631d73](https://github.com/jamesyong-42/truffle/commit/a631d73701209cf8256d9c3ea906967b97586225))
* wire file transfer bridge infrastructure for cross-node cp ([3729c6c](https://github.com/jamesyong-42/truffle/commit/3729c6c0f8e0b9ae6370709e0b3e11a0853a2a46))

## [0.2.4](https://github.com/jamesyong-42/truffle/compare/truffle-v0.2.3...truffle-v0.2.4) (2026-03-23)


### Features

* **cli:** bootstrap file transfer subsystem on daemon startup (RFC 011 Phase 1) ([db61168](https://github.com/jamesyong-42/truffle/commit/db6116825d8cb59a3e0d80d2568438d56e9fc60e))
* **file-transfer:** add PULL_REQUEST protocol for downloads (RFC 011 Phase 3) ([59cc3f4](https://github.com/jamesyong-42/truffle/commit/59cc3f44167366aa53a26754878262fad979b68e))
* **file-transfer:** auto-accept CLI-mode transfers (RFC 011 Phase 5) ([42935b4](https://github.com/jamesyong-42/truffle/commit/42935b4e0834d97b73ec1a96c1f118143882a3c9))
* **file-transfer:** cleanup dead code and polish CLI (RFC 011 Phase 6) ([46df5e6](https://github.com/jamesyong-42/truffle/commit/46df5e6c92eb3dc7b8db678f276aafe6126786ba))
* **file-transfer:** implement real upload flow (RFC 011 Phase 2) ([cfbeb52](https://github.com/jamesyong-42/truffle/commit/cfbeb528da175bbe39cf9c315193a3decba11221))
* **file-transfer:** stream progress notifications to CLI (RFC 011 Phase 4) ([ee1c6e8](https://github.com/jamesyong-42/truffle/commit/ee1c6e8190bd81806d77fc6dc67490f357d07f91))


### Bug Fixes

* ls CONNECTION column, tcp --check routing, send name resolution, dial dedup ([dbaac56](https://github.com/jamesyong-42/truffle/commit/dbaac562b1baaec9483330a1fe0c9774c1f1acd3))
* use port 9417 (TCP) for mesh dial instead of 443 (TLS) ([c73cff3](https://github.com/jamesyong-42/truffle/commit/c73cff3e7e9f50bf555a6cb7ebca3cbb0fbdd4ac))

## [0.2.3](https://github.com/jamesyong-42/truffle/compare/truffle-v0.2.2...truffle-v0.2.3) (2026-03-22)


### Features

* add esbuild-style sidecar binary distribution via npm ([3f5da1f](https://github.com/jamesyong-42/truffle/commit/3f5da1fb4455265dd118d7aa88b2eb57d791c267))
* add release-please for automated versioning and releases ([b5cc9db](https://github.com/jamesyong-42/truffle/commit/b5cc9db1a9b101b8f0bff09a2f0ac37b0d9d6efe))
* add Rust core library and Go thin shim (Phase 1+2) ([191a107](https://github.com/jamesyong-42/truffle/commit/191a107f1b59b70654e3438c5baa2125254b3482))
* cross-platform IPC + sidecar discovery (Phase 1+2) ([48ed7f2](https://github.com/jamesyong-42/truffle/commit/48ed7f2f4e706bf1ec4bee076803ab22c90ab5cb))
* cross-platform Phases 3-5 — CI/CD, distribution, testing ([d770fc1](https://github.com/jamesyong-42/truffle/commit/d770fc139ffd51c8e460644e95b9194728208e95))
* implement RFC 005 truffle-core refactor ([eae808b](https://github.com/jamesyong-42/truffle/commit/eae808b0ad42b192477642769f9acfb329d14417))
* **napi:** dial discovered peers and handle outgoing bridge connections ([276c794](https://github.com/jamesyong-42/truffle/commit/276c794944db786297a628658ea4405222c68606))
* **napi:** wire up Go sidecar and BridgeManager in NapiMeshNode ([33d5138](https://github.com/jamesyong-42/truffle/commit/33d5138899cce02d5407e39cba2926d0c457d447))
* rewrite react hooks and cli for NAPI async API ([0f687e6](https://github.com/jamesyong-42/truffle/commit/0f687e6aea058061d05102d7a920dd79c6b285ce))
* **rfc-008-phase2:** sidecar improvements — WhoIs, state monitor, rich peers ([4e8c2a2](https://github.com/jamesyong-42/truffle/commit/4e8c2a2da4760734da78b1bd48f33c10d677969a))
* **rfc-008-phase3:** unified event API + TruffleRuntime builder ([ef90f8f](https://github.com/jamesyong-42/truffle/commit/ef90f8f371067579d00c8fdd836012bca228c3b3))
* **rfc-008-phase4:** HTTP router — reverse proxy, static hosting, path routing ([f2c6c56](https://github.com/jamesyong-42/truffle/commit/f2c6c56c6fdbc515fe4fcc608d341af4644bf4e9))
* **rfc-008-phase5:** PWA handler + Web Push manager ([60e2c52](https://github.com/jamesyong-42/truffle/commit/60e2c522016d7b19db2a945f5bdaa3616d571e98))
* **rfc-008-phase6:** truffle-cli — mesh networking CLI tool ([f0647c3](https://github.com/jamesyong-42/truffle/commit/f0647c379ce5871276539d9ea6d57f7a6766304e))
* **rfc-009-phase1:** wire protocol foundation types ([49bc05d](https://github.com/jamesyong-42/truffle/commit/49bc05df784a71f0fc07c2fc6604c3803a13deae))
* **rfc-009-phase2:** v3 wire codec — frame type byte separates control from data ([cea42eb](https://github.com/jamesyong-42/truffle/commit/cea42eb72f47bfa030e43d8f3c6af45cbb3df470))
* **rfc-009-phase3:** typed dispatch — no more silent drops ([6f24446](https://github.com/jamesyong-42/truffle/commit/6f24446d669241b2a8188d4f66bb3f4b038b265c))
* **rfc-009-phase4:** handshake + version-aware sending ([2e54c2f](https://github.com/jamesyong-42/truffle/commit/2e54c2fcc7edab6f67435be35e0cb2977c9a9ada))
* **rfc-009-phase5+6:** NAPI update + legacy deprecation ([5ded76d](https://github.com/jamesyong-42/truffle/commit/5ded76d6afa5bf73875db039e1d891d7fdcc917f))
* **rfc-010-phase5:** Go sidecar additions — listen, ping, Taildrop ([49e55ea](https://github.com/jamesyong-42/truffle/commit/49e55ea49fc06e0304c8ffbf6b0935e92e9719c1))
* **rfc-010-phase6:** CLI daemon architecture ([23affe9](https://github.com/jamesyong-42/truffle/commit/23affe96330fb088b5b083c139ee05a33cfc1eb4))
* **rfc-010-phase7-12:** complete CLI with all 16 commands ([f7d0f72](https://github.com/jamesyong-42/truffle/commit/f7d0f72ac3c246c392f4f800a597d3435c9ba4f4))
* serve install scripts via GitHub Pages ([55d5521](https://github.com/jamesyong-42/truffle/commit/55d55216f8fce6f93989208a09a187a98a431c70))
* truffle uninstall command + install script --uninstall ([eb26cdf](https://github.com/jamesyong-42/truffle/commit/eb26cdffe6f0b4343aff4c9147016745b0cd22b2))
* truffle update — self-update from GitHub releases ([e33b0ec](https://github.com/jamesyong-42/truffle/commit/e33b0ecaf72977f04e5dda1b8254a6b27c473ac7))
* **truffle:** complete Rust rewrite (RFC 003 Phases 3-8) ([e79feaa](https://github.com/jamesyong-42/truffle/commit/e79feaa12a4ee05c5721ff37d308597b4dea6601))
* zero-config CLI — auto-discover sidecar, auto-create state dir ([0f5a857](https://github.com/jamesyong-42/truffle/commit/0f5a857a4ef89820acf2a9412196abbd21198637))


### Bug Fixes

* add --registry flag and npm version logging for OIDC publish ([117a335](https://github.com/jamesyong-42/truffle/commit/117a33543220344ad564faca79e73d326b8c0aa9))
* **ci:** add Rust toolchain and npm auth to release workflow ([0bc7a76](https://github.com/jamesyong-42/truffle/commit/0bc7a76cdc64e63c94e1400338c2279da79f6378))
* **ci:** add shell: bash to release workflow for Windows compat ([47dcf87](https://github.com/jamesyong-42/truffle/commit/47dcf872924cec32d1ad743de28b0e81c3718d79))
* **ci:** build sidecar to bin/ subdirectory for integration tests ([c54bcce](https://github.com/jamesyong-42/truffle/commit/c54bcce6d16825c523459730b9ceeb6889a48e83))
* **ci:** exclude sidecar integration tests from NAPI workflow ([39bb360](https://github.com/jamesyong-42/truffle/commit/39bb36060820d55d4cb1dba983e36ef2965c86ba))
* **ci:** fix changesets config and workspace dependency ([5a25d6b](https://github.com/jamesyong-42/truffle/commit/5a25d6b09eb3c736f28a63fd2d95fd6a5ea307be))
* **ci:** fix NAPI Build and sidecar workflow failures ([10d8e66](https://github.com/jamesyong-42/truffle/commit/10d8e66539330af756ba76011e258610637fb1b4))
* **ci:** remove --doc flag (can't mix with --lib --bins) ([2f3c6c4](https://github.com/jamesyong-42/truffle/commit/2f3c6c403be3b88ccced122dde0379d2fd3ce77c))
* **ci:** remove pnpm cache from NAPI build/test jobs ([83cc0ca](https://github.com/jamesyong-42/truffle/commit/83cc0ca2b8c35223ca77fe92bc15c58475cd24d8))
* **ci:** switch all publish workflows to OIDC trusted publishing ([c610fed](https://github.com/jamesyong-42/truffle/commit/c610fed9e174c7c43026ac6c0a0d395a025a5087))
* clear NODE_AUTH_TOKEN to allow npm OIDC trusted publishing ([203c43d](https://github.com/jamesyong-42/truffle/commit/203c43da7cd2733efc20da65b3da533db6cf5b56))
* complete auth flow - serde alias for tailscaleIP + auto-request peers ([71583a1](https://github.com/jamesyong-42/truffle/commit/71583a1388fcceb684a763cb61737aa25c45aa0a))
* doc test code examples marked as text (not compiled) ([dd18431](https://github.com/jamesyong-42/truffle/commit/dd18431227add9f53762ded4490732e970c57fc9))
* **election:** detect and resolve split-brain when both nodes claim primary ([8946287](https://github.com/jamesyong-42/truffle/commit/8946287a9ea86b690b9f535089cb46eb0e912269))
* **election:** send election:timeout after grace period to trigger decide_election ([c4e3681](https://github.com/jamesyong-42/truffle/commit/c4e36819521cad5812d15eb863b9c4d4700aa0e0))
* **election:** set_primary() now emits PrimaryElected event ([8859590](https://github.com/jamesyong-42/truffle/commit/8859590a5efb03c662436854c43ad2683c72c1f2))
* exclude RFCs from VitePress build (Rust generics parsed as HTML) ([2492479](https://github.com/jamesyong-42/truffle/commit/24924797a8a5676ac2457e1274ff2cf73838b8f4))
* heartbeat filter incorrectly eating MeshEnvelope messages ([6fc7df7](https://github.com/jamesyong-42/truffle/commit/6fc7df7551a7f39304723dfd748d3bf8efbc6790))
* installer auto-adds to PATH and works immediately ([ee4d007](https://github.com/jamesyong-42/truffle/commit/ee4d0073c9b2f7b817ab9f7cbc439608e90884bb))
* job-level id-token permission + no registry-url for npm OIDC ([7b8c7f1](https://github.com/jamesyong-42/truffle/commit/7b8c7f105b58148784c2df901d0e1a0305de6f93))
* **napi:** defer tokio::spawn in callbacks to avoid runtime panic ([f1f0b8b](https://github.com/jamesyong-42/truffle/commit/f1f0b8bc1a9db8b02836084ab7f9220ccc6a2523))
* remove registry-url from setup-node to enable OIDC trusted publishing ([2cdade6](https://github.com/jamesyong-42/truffle/commit/2cdade6862d4a916e288d2af7bc79e487bd7f4ba))
* remove setup-node .npmrc to let npm use OIDC trusted publishing ([006f0b3](https://github.com/jamesyong-42/truffle/commit/006f0b3d7eb9e5ae86cdf1ccd80cc051655cbf6a))
* rename postinstall to .cjs and remove napi auto-injected optionalDeps ([7774a15](https://github.com/jamesyong-42/truffle/commit/7774a15fbc9da6e278b7f947af9fb78f03ff6da3))
* support MeshNode stop/restart by recreating internal event channels ([b7cbbdc](https://github.com/jamesyong-42/truffle/commit/b7cbbdcdbda8af1b0c7a1cc4f67742b13c357719))
* tauri plugin doc test marked as text (not compiled) ([c938a1b](https://github.com/jamesyong-42/truffle/commit/c938a1b122fb2aa3438b23742423b39695beedc6))
* **test:** change broadcast test msg_type from 'ping' to 'broadcast-test' ([efd9189](https://github.com/jamesyong-42/truffle/commit/efd91893a8da143c5c330c26a5235fb7d7183596))
* truffle up waits for Tailscale to connect before showing status ([1336341](https://github.com/jamesyong-42/truffle/commit/1336341e1f071fad6d05a75b7622be19106381f5))
* **truffle-napi:** complete FileTransferAdapter with action methods ([4e57840](https://github.com/jamesyong-42/truffle/commit/4e57840256b485b915c21fadcbc2fd8b14c8b6a9))
* unset NODE_AUTH_TOKEN env var for npm OIDC trusted publishing ([f8d1d8d](https://github.com/jamesyong-42/truffle/commit/f8d1d8d57f0b520c4e91f83f9ec00c9e19249cfe))
* update pnpm-lock.yaml for CI frozen-lockfile ([07f043f](https://github.com/jamesyong-42/truffle/commit/07f043f19485caa39577b31551aa13480613f322))
* update TS packages for RFC 010 — remove role/election references ([f79ac75](https://github.com/jamesyong-42/truffle/commit/f79ac75c4de9b4fd5b68412b84b85d26b2eaf434))
* use shared "truffle" prefix for peer discovery + periodic polling ([ddc0d92](https://github.com/jamesyong-42/truffle/commit/ddc0d92e8792bcad436c0b998fa60b4417190dca))
* **windows:** fix 4 compilation errors caught by CI ([97070e1](https://github.com/jamesyong-42/truffle/commit/97070e14329040aa4135ee4aceb9c9bd58081252))
* **windows:** use temp_dir() instead of hardcoded /tmp in tests ([757b503](https://github.com/jamesyong-42/truffle/commit/757b5039194e9e262660f61ab376704dc493f4ce))

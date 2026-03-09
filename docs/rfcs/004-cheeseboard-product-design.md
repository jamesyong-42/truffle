# Cheeseboard: Product Design Critique + Enhanced Vision

## A Senior Product Designer's Analysis of RFC-004

**Date:** 2026-03-08
**Status:** Draft
**Companion to:** RFC-004 (Cheeseboard Technical Architecture)

---

## 1. AirDrop Deep Dive -- What Makes It Magic

### The Seven Principles of Invisible Sharing

Apple did not build AirDrop as a "feature." They built it as a **disappearing act** -- the technology gets out of the way so completely that users forget it exists between uses, then rediscover it exactly when they need it.

**Principle 1: Zero-Config Discovery (BLE + WiFi Direct)**
AirDrop uses BLE advertisements to detect nearby Apple devices, then establishes a peer-to-peer WiFi Direct connection for the actual transfer. The user never sees an IP address, a port number, or a pairing code. They see *people* and *devices*.

**Principle 2: Share Sheet Integration (System-Level Presence)**
AirDrop lives inside the OS share sheet. Every app that can share anything can share to AirDrop. The user never opens a separate "AirDrop app." This is the difference between a *utility* and an *infrastructure*.

**Principle 3: Contact-Aware Identity (Faces, Not Device IDs)**
When you AirDrop to someone, you see their contact photo and name, not "iPhone-37A2." Apple links device identity to contact identity through iCloud.

**Principle 4: One-Tap Accept (Notification with Preview)**
The receiver gets a rich notification showing what is being shared -- a thumbnail for photos, a URL for links, a file icon for documents. They tap Accept once.

**Principle 5: Instant Feedback (Progress + Haptics + Sound)**
The circular progress indicator on both devices, the haptic tap on completion, the subtle sound -- these create a sense of physical handoff.

**Principle 6: Auto-Open (Right Content, Right App)**
Received photos go to Photos. Received URLs open in Safari. The receiver does not make a routing decision.

**Principle 7: Universal Clipboard (Complete Invisibility)**
Universal Clipboard has *no UI at all*. You copy on your iPhone, you paste on your Mac. This is the pinnacle of zero-friction design.

### What Cheeseboard Can Steal

| AirDrop Principle | Cheeseboard Can Replicate? | How |
|---|---|---|
| Zero-config discovery | **Partially.** Tailscale handles identity, but setup requires Tailscale auth. Once running, mesh discovery is automatic. | Auto-detect Tailscale. Auto-name device from hostname. Eliminate all manual config. |
| Share sheet integration | **On macOS: yes, eventually.** Via companion Swift helper. | Phase 2+ investment. Start with tray-menu sharing. |
| Contact-aware identity | **Partially.** Tailscale user identity maps devices to one person. | Query Tailscale `whois` for user profile. Display user names with device type icons. |
| One-tap accept | **Yes.** Native OS notifications with action buttons. | Implement actionable notifications with file preview thumbnails. |
| Instant feedback | **Yes.** Tray icon animation, progress in popup, optional sound. | Tray icon pulse, progress bar, macOS system sound on complete. |
| Auto-open | **Yes.** Use `open` (macOS) / `xdg-open` (Linux) / `start` (Windows). | Default to auto-open for trusted devices. |
| Universal Clipboard | **Yes. This must be the launch feature.** | Clipboard sync with zero interaction after first setup. |

### What Cheeseboard Cannot Replicate

- **BLE proximity detection**: No hardware access. Tailscale membership is the trust boundary instead.
- **Native share sheet presence**: Requires platform-specific work outside Tauri (Phase 2+).
- **Contact photos from iCloud**: Tailscale does not provide user avatars. Use identicons or user-set avatars.
- **Haptic feedback**: Desktop devices lack haptic engines. Sound is the analog.

---

## 2. Friction Audit of Current RFC

Scored 1-5 where 1 = nearly invisible, 5 = "most users will abandon here."

### 2.1 Setup Friction: **4/5 -- Needs Major Improvement**

**Current problems:**
- User must have Tailscale installed (prerequisite, unavoidable)
- `mesh_port: 9418` -- user should never see port numbers
- `hostname_prefix: "cheeseboard"` -- internal implementation detail
- No onboarding flow described

**Proposed fix:**
- Auto-detect Tailscale installation
- Eliminate `mesh_port` and `hostname_prefix` from user-visible config
- First launch flow: (1) Welcome screen (2s), (2) Auto-detect device name, (3) Auto-discover other devices, (4) "Copy something to try it." Total: under 20 seconds.

### 2.2 Discovery Friction: **2/5 -- Good, Needs Polish**

- Add "new device" animation that fades after 5s
- If no devices found after 30s, show contextual help
- Auto-hide devices offline for more than 7 days

### 2.3 Sharing Friction (Files): **3/5 -- Acceptable for v1**

- "Send File..." requires 3 deliberate actions minimum
- No right-click context menu integration
- Only files supported, not text snippets or URLs

**Proposed fix:**
- `Cmd+Shift+S` Quick Send dialog
- Make tray popup itself a drag-drop target
- Phase 2: macOS Finder Quick Action

### 2.4 Receiving Friction: **3/5 -- Missing Auto-Open**

- Every file requires explicit accept, even from trusted devices
- No auto-open for received content
- No file preview in accept notification

**Proposed fix:**
- Tiered auto-accept (trusted + small = auto, large = prompt)
- Auto-open by default
- Thumbnail in notification

### 2.5 Clipboard Friction: **2/5 -- Missing Key Features**

- No keyboard shortcut for clipboard history
- No way to paste from a specific device
- No rich content handling
- 500ms polling may feel laggy

**Proposed fix:**
- `Cmd+Shift+V` clipboard history palette
- 250ms polling on macOS (changeCount makes this cheap)
- Rich clipboard: URLs get title+favicon in history

### 2.6 Trust Friction: **3/5 -- Secure but Too Manual**

- `auto_accept_from: Vec<String>` is device IDs in JSON -- no UI
- No concept of "same Tailscale user" trust

**Proposed fix:**
- Same Tailscale user = implicit trust (like same iCloud account)
- Trust managed via UI toggles, not JSON editing
- "Accept & Always Trust" button on first transfer

---

## 3. The "Zero Friction" Vision

### 3.1 First Launch (OOBE)

**Target: Under 20 seconds from app launch to first clipboard sync.**

```
SCREEN 1: "Welcome to Cheeseboard" (auto-advances after 2s)
  [cheese wedge icon, warm amber gradient]
  "Copy anywhere. Paste everywhere."
  [Auto-detecting Tailscale... runs in background]

SCREEN 2: "Your Device"
  Device name: [James's MacBook Pro] -- editable, pre-filled
  "Looking for your other devices..."
  IF devices found: "Found 2 devices!" [Continue]
  IF none found: "No devices yet. Install on other devices." [Skip]

SCREEN 3: (if devices found)
  "Try it out! Copy some text on this device."
  [When copied: "Got it! Now paste on your other device."]
  [Done] -- closes to tray
```

### 3.2 Daily Clipboard Sync

**Indistinguishable from Apple's Universal Clipboard.**

Copy text on Device A. Within 300ms, available on Device B. Paste on Device B. It works. No notification, no popup, no tray icon change. **Invisible.**

**When to break invisibility:**
- Large images (>1 MB): brief tray icon pulse + tooltip
- Sync failure: tray icon turns disconnected
- Password detected: silent skip (no feedback)

**Clipboard History as a First-Class Feature (via StoreSyncAdapter):**

Clipboard history is **shared across all mesh nodes** using truffle-core's `StoreSyncAdapter`.
Each device maintains its own "slice" of entries (things copied on that device), and
StoreSyncAdapter merges all slices so every node sees the full history. New devices
joining the mesh get the complete history automatically via `store:sync:full`.

`Cmd+Shift+V` opens the Cheeseboard clipboard palette:
```
+----------------------------------+
| Clipboard History    [search]    |
|----------------------------------|
| [MB] Hello, this is some te...   |
|      2 min ago                   |
|----------------------------------|
| [DP] https://github.com/...      |
|      GitHub - truffle             |
|      5 min ago                   |
|----------------------------------|
| [pinned] API Key: sk-abc...      |
|      Pinned                      |
+----------------------------------+
```
- Device abbreviations show source (from StoreSyncAdapter device slices)
- URLs show fetched title + favicon
- Click to paste
- Search across all history (all devices)
- Pin frequently used items (synced across devices)
- Mobile web client shows the same shared history

### 3.3 Sharing Files

**Three paths for three contexts:**

1. **Quick Send** (`Cmd+Shift+S`): mini-dialog with clipboard/file picker + device buttons
2. **Drag to tray popup**: drag file over tray icon, popup opens, drop on device
3. **Context Menu** (Phase 2): right-click "Send to Cheeseboard" in Finder

### 3.4 Receiving Files

**Trusted devices (auto-accept):**
1. File arrives silently
2. Passive notification: "Received photo.png" [Open] [Show in Folder]
3. Auto-open in default app

**Untrusted or large files:**
1. Active notification with preview thumbnail
2. [Accept] [Reject] [Accept & Always Trust]
3. Progress → completion → notification with Open

---

## 4. Mobile Strategy: Embedded Web Server + QR Code

### 4.1 Why No Native Mobile Apps

Native iOS and Android apps face severe platform restrictions:
- iOS 16+ requires foreground + explicit permission prompt to read clipboard — invisible sync is impossible
- iOS background execution is heavily restricted — can't maintain mesh connections
- Android 13+ has clipboard access restrictions
- Building and maintaining native apps for iOS + Android is a massive investment for a side project
- App Store review and signing adds friction to shipping

### 4.2 The Simpler Approach: Each Desktop Hosts a Web App

Every running Cheeseboard desktop instance embeds a lightweight web server (axum).
The tray popup shows a QR code. Scan it with any phone camera. Instant access in the browser.

**How it works:**
1. Desktop Cheeseboard starts an HTTP server on a random port
2. QR code encodes `http://<tailscale-ip>:<port>/?token=<256-bit-token>`
3. Phone scans QR, opens in browser
4. Web client connects via WebSocket for real-time events
5. User can: browse shared clipboard history, paste text to desktop, upload files, receive files

**What mobile users can do:**
- **Send text to desktop clipboard**: type or paste on phone → POST to API → desktop writes to OS clipboard
- **Browse clipboard history**: the shared StoreSyncAdapter history from all mesh devices
- **Upload files**: pick from camera roll or files app → upload to desktop → forward to any mesh device
- **Receive files**: WebSocket push when files arrive, with download link

**What mobile users cannot do (and that's OK):**
- Background clipboard sync (iOS restriction — not worth fighting)
- Join the Tailscale mesh directly (need Tailscale app, but the web server handles the relay)

### 4.3 Why This Is Better

- **Zero install**: works on any phone with a camera and browser
- **Zero maintenance**: no App Store submissions, no SDK updates
- **Zero friction**: scan QR, instantly connected
- **Secure**: auth token in URL, traffic over Tailscale (WireGuard encrypted)
- **Cross-platform**: works identically on iOS and Android

---

## 5. Trust & Security Model Redesign

### The Three Tiers

**Tier 1: Same User (Implicit Trust)**
All devices on the same Tailscale user account. Mirrors Apple's "same iCloud account."
- Clipboard: always syncs, no prompts
- Files < 10 MB: auto-accept, passive notification
- Files >= 10 MB: prompt (size confirmation, not trust)

**Tier 2: Known Devices (Explicit Trust)**
Different Tailscale users. Trust granted per-device.
- Clipboard: opt-in per device
- Files: prompt every time, with "Always Trust" option

**Tier 3: Temporary Sharing (Time-Limited)**
"Share with anyone for 10 minutes" mode.
- Clipboard: disabled
- Files: always prompt
- Auto-expires

### Device Pairing

Same user: automatic, passive notification only.
Different user: manual approval with "Accept for 10 minutes" option.

### Content Sensitivity Detection

Beyond macOS ConcealedType:
- Credit card numbers (4 groups of 4 digits)
- API keys (common prefixes: `sk-`, `pk_`, `ghp_`, `AKIA`)
- Private keys (`-----BEGIN RSA PRIVATE KEY-----`)
- SSN-like patterns

Detected sensitive content is **not synced** by default. Configurable.

---

## 6. Killer Features Beyond AirDrop

Things AirDrop **cannot** do:

1. **Cross-platform**: Mac ↔ Windows ↔ Linux
2. **Clipboard history sync**: searchable history across all devices
3. **Pinned snippets**: frequently shared text/URLs persisted across restarts
4. **URL handoff**: copy URL on phone, opens on desktop browser
5. **Screenshot flow**: screenshot on any device, instantly available everywhere
6. **CLI integration**: `cheeseboard send file.txt --to "Desktop PC"`
7. **Works over internet**: not limited to local network (Tailscale)
8. **File transfer resume**: interrupted transfers resume automatically
9. **API/SDK**: other apps can integrate Cheeseboard sharing
10. **Open source**: MIT licensed, auditable, extensible

---

## 7. Visual Design Direction

### App Icon
Geometric cheese wedge, warm amber/gold (#F5A623) on cream (#FFF8E7). 2-3 holes in the wedge subtly suggesting data flow. Professional, not cartoonish.

### Color Palette
```
Primary:     #F5A623  (Warm amber)
Secondary:   #4A90D9  (Cool blue -- trust)
Success:     #7ED321  (Green -- connected)
Error:       #D0021B  (Red -- disconnected)
Dark BG:     #1A1A2E  (Deep navy)
Light BG:    #FFFFFF  (White)
```

### Design Language
macOS-native with glassmorphism. Vibrancy/blur on macOS (NSVisualEffectView via Tauri). System font (SF Pro). Rounded corners (12px). Follow system dark/light mode.

### Animation Principles
- 150-200ms transitions (matching macOS system speed)
- Purposeful only (every animation communicates state change)
- Interruptible
- Tray icon: pulse on sync (300ms)
- Device list: slide-in for new devices (150ms)

### Sound
Optional, disabled by default:
- Clipboard sync: no sound (too frequent)
- File received: system "Ping"
- Transfer complete: system "Hero"

---

## 8. Revised Feature Priority

### MVP (Week 1-2): "The Magic Copy-Paste"

The 30-second demo. The reason to exist.

1. Tray app, auto-detect Tailscale, auto-name device
2. Clipboard text sync (copy on A, paste on B)
3. Minimal tray popup showing devices + sync status
4. **That's it.**

### Phase 2 (Week 3-4): "Actually Useful Daily"

1. Clipboard image sync (screenshots)
2. Password manager detection
3. Clipboard history in tray popup
4. `Cmd+Shift+V` clipboard history palette
5. Tray icon states

### Phase 3 (Week 5-6): "File Drop"

1. File transfer end-to-end
2. Progress indicators
3. Tiered trust / auto-accept
4. Auto-open received files
5. Drop zone window

### Phase 4 (Week 7-8): "Polish + Ship"

1. Settings window
2. Auto-launch at login
3. Pinned clipboard items
4. CLI tool
5. Cross-platform testing
6. Package as DMG

### Phase 5 (Week 9-10): "Mobile Web Access"

1. Embedded axum web server in desktop app
2. QR code in tray popup (scan to connect from phone)
3. Mobile web client: clipboard history, text send, file upload
4. WebSocket for real-time events to phone browser
5. Auth token security

### Phase 6 (Future): "Platform Integration"

1. macOS Finder extension (right-click "Send to Cheeseboard")
2. URL handoff (copy URL on phone, opens on desktop)
3. CLI tool (`cheeseboard send`, `cheeseboard devices`)

### Phase 7 (Future): "Ecosystem"

1. SDK/API for third-party integration
2. Shared scratchpad / pinned snippets sync
3. Content-aware routing (URLs auto-open, images auto-save)
4. Browser extension

### The Killer Demo (30 seconds)

```
[0s]  Two laptops side by side. Cheeseboard tray icons visible.
[3s]  Caption: "Copy on one device."
[5s]  User copies text on laptop A.
[7s]  Caption: "Paste on the other."
[9s]  User pastes on laptop B. Text appears.
[11s] Caption: "Works with images too."
[13s] Screenshot on laptop A.
[15s] Paste on laptop B. Screenshot appears.
[17s] Caption: "And files."
[19s] Right-click file → Send to MacBook Pro.
[23s] File opens automatically on laptop B.
[25s] "Cheeseboard. Copy anywhere. Paste everywhere."
[27s] Logo + GitHub link.
```

---

## 9. Competitive Landscape

| Feature | AirDrop | Universal Clipboard | Quick Share | KDE Connect | LocalSend | **Cheeseboard** |
|---|---|---|---|---|---|---|
| Cross-platform | Apple only | Apple only | Android+Chrome+Win | KDE+Android | All | **All (Tailscale)** |
| Clipboard sync | No | Yes (invisible) | No | Yes | No | **Yes (invisible)** |
| File transfer | Yes | No | Yes | Yes | Yes | **Yes** |
| Zero setup | Yes (BLE) | Yes (iCloud) | Partial | Manual | Zero config | Partial (Tailscale) |
| Works over internet | No | Yes (relay) | No | Plugin | No | **Yes (Tailscale)** |
| Open source | No | No | No | Yes (GPL) | Yes (MIT) | **Yes (MIT)** |
| Clipboard history | No | No | No | Basic | No | **Yes (searchable)** |
| Privacy (no cloud) | Yes (P2P) | No (iCloud) | Yes (P2P) | Yes (P2P) | Yes (P2P) | **Yes (P2P)** |
| CLI integration | No | No | No | D-Bus | No | **Yes** |
| File resume | No | N/A | No | No | No | **Yes** |

### Cheeseboard's Unique Position

**"The cross-platform AirDrop with a clipboard."**

> "Cheeseboard is AirDrop for everyone. Copy on your Mac, paste on your Windows PC. Send files from your phone to your Linux server. All over Tailscale, with zero cloud dependency. Open source, MIT licensed."

The wedge: developers and power users working across macOS/Linux/Windows who can't use AirDrop.

The expansion: general knowledge workers with multiple devices.

The minimum viable product that makes people switch: invisible clipboard sync.

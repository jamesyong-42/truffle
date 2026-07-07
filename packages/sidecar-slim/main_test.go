package main

import (
	"bytes"
	"context"
	"encoding/hex"
	"encoding/json"
	"io"
	"net"
	"runtime/debug"
	"strings"
	"sync"
	"testing"
	"time"
)

// testToken returns a deterministic 32-byte token matching Rust's test_token()
// in bridge/header.rs (bytes 0x00..0x1F).
func testToken() []byte {
	token := make([]byte, 32)
	for i := range token {
		token[i] = byte(i)
	}
	return token
}

// TestGoldenIncomingHeader verifies the binary header format matches
// the Rust truffle-core golden test vectors exactly.
func TestGoldenIncomingHeader(t *testing.T) {
	var buf bytes.Buffer
	token := testToken()

	err := writeHeader(&buf, token, dirIncoming, 443, "", "100.64.0.2:12345", "peer-host.tailnet.ts.net")
	if err != nil {
		t.Fatalf("writeHeader failed: %v", err)
	}

	b := buf.Bytes()

	// Magic
	if got := b[0:4]; !bytes.Equal(got, []byte{0x54, 0x52, 0x46, 0x46}) {
		t.Errorf("magic: got %s, want 54524646", hex.EncodeToString(got))
	}

	// Version
	if b[4] != 0x01 {
		t.Errorf("version: got %02x, want 01", b[4])
	}

	// Session token
	if !bytes.Equal(b[5:37], token) {
		t.Errorf("session token mismatch")
	}

	// Direction = incoming
	if b[37] != 0x01 {
		t.Errorf("direction: got %02x, want 01", b[37])
	}

	// Port = 443 (0x01BB)
	if !bytes.Equal(b[38:40], []byte{0x01, 0xBB}) {
		t.Errorf("port: got %s, want 01bb", hex.EncodeToString(b[38:40]))
	}

	// RequestIdLen = 0
	if !bytes.Equal(b[40:42], []byte{0x00, 0x00}) {
		t.Errorf("request_id_len: got %s, want 0000", hex.EncodeToString(b[40:42]))
	}

	// RemoteAddrLen = 16 ("100.64.0.2:12345")
	if !bytes.Equal(b[42:44], []byte{0x00, 0x10}) {
		t.Errorf("remote_addr_len: got %s, want 0010", hex.EncodeToString(b[42:44]))
	}
	if got := string(b[44:60]); got != "100.64.0.2:12345" {
		t.Errorf("remote_addr: got %q, want %q", got, "100.64.0.2:12345")
	}

	// RemoteDNSNameLen = 24 ("peer-host.tailnet.ts.net")
	if !bytes.Equal(b[60:62], []byte{0x00, 0x18}) {
		t.Errorf("remote_dns_name_len: got %s, want 0018", hex.EncodeToString(b[60:62]))
	}
	if got := string(b[62:86]); got != "peer-host.tailnet.ts.net" {
		t.Errorf("remote_dns_name: got %q, want %q", got, "peer-host.tailnet.ts.net")
	}

	// Total length
	if len(b) != 86 {
		t.Errorf("total length: got %d, want 86", len(b))
	}
}

// TestGoldenOutgoingHeader verifies outgoing header with requestId.
func TestGoldenOutgoingHeader(t *testing.T) {
	var buf bytes.Buffer
	token := testToken()
	requestID := "550e8400-e29b-41d4-a716-446655440000"

	err := writeHeader(&buf, token, dirOutgoing, 9417, requestID, "100.64.0.3:9417", "other-host.tailnet.ts.net")
	if err != nil {
		t.Fatalf("writeHeader failed: %v", err)
	}

	b := buf.Bytes()

	// Direction = outgoing
	if b[37] != 0x02 {
		t.Errorf("direction: got %02x, want 02", b[37])
	}

	// Port = 9417 (0x24C9)
	if !bytes.Equal(b[38:40], []byte{0x24, 0xC9}) {
		t.Errorf("port: got %s, want 24c9", hex.EncodeToString(b[38:40]))
	}

	// RequestIdLen = 36
	if !bytes.Equal(b[40:42], []byte{0x00, 0x24}) {
		t.Errorf("request_id_len: got %s, want 0024", hex.EncodeToString(b[40:42]))
	}

	// RequestId
	reqEnd := 42 + 36
	if got := string(b[42:reqEnd]); got != requestID {
		t.Errorf("request_id: got %q, want %q", got, requestID)
	}

	// Read it back with Rust-compatible offsets
	// After requestId, we have RemoteAddrLen(2) + RemoteAddr + RemoteDNSNameLen(2) + RemoteDNSName
	addrLenOff := reqEnd
	addrLen := int(b[addrLenOff])<<8 | int(b[addrLenOff+1])
	if addrLen != 15 { // "100.64.0.3:9417"
		t.Errorf("remote_addr_len: got %d, want 15", addrLen)
	}
}

// TestGoldenMinimalHeader verifies a header with all empty variable fields.
func TestGoldenMinimalHeader(t *testing.T) {
	var buf bytes.Buffer
	token := make([]byte, 32)
	for i := range token {
		token[i] = 0xAA
	}

	err := writeHeader(&buf, token, dirIncoming, 443, "", "", "")
	if err != nil {
		t.Fatalf("writeHeader failed: %v", err)
	}

	b := buf.Bytes()

	// Minimum header size: 4+1+32+1+2+2+2+2 = 46
	if len(b) != 46 {
		t.Errorf("minimal header length: got %d, want 46", len(b))
	}

	// All length fields should be 0
	if !bytes.Equal(b[40:42], []byte{0, 0}) {
		t.Errorf("request_id_len not 0")
	}
	if !bytes.Equal(b[42:44], []byte{0, 0}) {
		t.Errorf("remote_addr_len not 0")
	}
	if !bytes.Equal(b[44:46], []byte{0, 0}) {
		t.Errorf("remote_dns_name_len not 0")
	}
}

// newTestShim mirrors main()'s shim construction for unit tests: events are
// discarded, the per-subsystem maps are initialized, and the lifecycle context
// is armed from a fresh background context.
func newTestShim() *shim {
	ctx, cancel := context.WithCancel(context.Background())
	return &shim{
		writer:           json.NewEncoder(io.Discard),
		dynamicListeners: make(map[uint16]net.Listener),
		udpRelays:        make(map[uint16]*udpRelay),
		proxies:          make(map[string]*proxyEntry),
		ctx:              ctx,
		cancel:           cancel,
	}
}

// TestMonitorStateExitsAcrossRestart is the goroutine-leak regression: a
// long-lived goroutine spawned in one lifecycle must terminate on stop and stay
// terminated across a restart, rather than being kept alive by the restart's
// re-armed context. It captures the lifecycle ctx pre-stop (as monitorState
// does at spawn), stops (cancelling that ctx), then restarts (re-arming a fresh
// ctx). monitorState run with the captured ctx must observe cancellation and
// exit. Before the fix — where monitorState re-read s.ctx — the equivalent run
// would observe the re-armed live context and block on its 60s ticker, timing
// out here.
func TestMonitorStateExitsAcrossRestart(t *testing.T) {
	s := newTestShim()

	// The lifecycle ctx a monitorState goroutine captures at spawn.
	ctx := s.lifecycleCtx()

	// Stop cancels that lifecycle context.
	s.handleStop()

	// A restart re-arms a fresh lifecycle context.
	s.armLifecycle(testToken(), 9999)

	// lc is nil-safe here: monitorState returns via ctx.Done before its first
	// 60s tick would touch lc.
	done := make(chan struct{})
	go func() {
		s.monitorState(ctx, nil)
		close(done)
	}()

	select {
	case <-done:
	case <-time.After(5 * time.Second):
		t.Fatal("monitorState from previous lifecycle leaked across restart")
	}
}

// TestRestartLifecycleFieldsSynchronized is the data-race regression: the
// ctx/cancel/sessionToken/bridgePort fields are read by spawned goroutines
// (lifecycleCtx/bridgeParams) while the dispatch loop rewrites them on
// stop/start (handleStop/armLifecycle). All access must go through serverMu.
// Under `go test -race`, the pre-fix raw field reads/writes report a data race;
// after the fix this is clean.
func TestRestartLifecycleFieldsSynchronized(t *testing.T) {
	s := newTestShim()

	var wg sync.WaitGroup
	wg.Add(1)
	go func() {
		defer wg.Done()
		for i := 0; i < 1000; i++ {
			_ = s.lifecycleCtx().Err()
			tok, port := s.bridgeParams()
			_ = tok
			_ = port
		}
	}()

	for i := 0; i < 1000; i++ {
		s.handleStop()
		_ = s.armLifecycle(testToken(), 9000)
	}
	wg.Wait()
}

// TestWriteHeaderRejectsOversizeDNS is the RemoteDNSName-cap regression: a DNS
// field one byte over maxRemoteDNSNameLen must be refused, so the sidecar never
// emits a header the paired Rust core is guaranteed to reject. Before the cap
// guard, only the 0xFFFF framing guard applied, so a 4097-byte field wrote
// successfully.
func TestWriteHeaderRejectsOversizeDNS(t *testing.T) {
	var buf bytes.Buffer
	err := writeHeader(&buf, testToken(), dirIncoming, 443, "", "100.64.0.2:1", strings.Repeat("a", maxRemoteDNSNameLen+1))
	if err == nil {
		t.Fatal("writeHeader accepted an oversize RemoteDNSName; want error")
	}
}

// TestWriteHeaderAcceptsDNSAtCap verifies the inclusive boundary: a DNS field of
// exactly maxRemoteDNSNameLen bytes is accepted (Rust caps inclusively too).
func TestWriteHeaderAcceptsDNSAtCap(t *testing.T) {
	var buf bytes.Buffer
	err := writeHeader(&buf, testToken(), dirIncoming, 443, "", "100.64.0.2:1", strings.Repeat("a", maxRemoteDNSNameLen))
	if err != nil {
		t.Fatalf("writeHeader rejected a DNS field at the cap: %v", err)
	}
}

// TestMarshalPeerIdentityDropsOversizeOptionalFields verifies graceful
// degradation: an identity whose profilePicUrl blows past the cap is still
// emitted, with only the offending optional field dropped — dnsName, nodeId, and
// loginName survive so identity verification/display still work.
func TestMarshalPeerIdentityDropsOversizeOptionalFields(t *testing.T) {
	identity := peerIdentityData{
		DNSName:       "peer.tailnet.ts.net",
		NodeID:        "nABC123",
		LoginName:     "alice@example.com",
		DisplayName:   "Alice",
		ProfilePicURL: strings.Repeat("x", 8192),
	}
	out := marshalPeerIdentity(identity)
	if len(out) > maxRemoteDNSNameLen {
		t.Fatalf("marshalPeerIdentity output %d exceeds cap %d", len(out), maxRemoteDNSNameLen)
	}

	var got peerIdentityData
	if err := json.Unmarshal([]byte(out), &got); err != nil {
		t.Fatalf("marshalPeerIdentity produced invalid JSON: %v", err)
	}
	if got.ProfilePicURL != "" {
		t.Errorf("profilePicUrl: got %q, want empty (should have been dropped)", got.ProfilePicURL)
	}
	if got.NodeID != "nABC123" {
		t.Errorf("nodeId: got %q, want nABC123", got.NodeID)
	}
	if got.DNSName != "peer.tailnet.ts.net" {
		t.Errorf("dnsName: got %q, want peer.tailnet.ts.net", got.DNSName)
	}
	if got.LoginName != "alice@example.com" {
		t.Errorf("loginName: got %q, want alice@example.com", got.LoginName)
	}
}

// TestMarshalPeerIdentitySmallIdentityUnchanged verifies the common case: an
// identity that fits within the cap is emitted verbatim with every field intact.
func TestMarshalPeerIdentitySmallIdentityUnchanged(t *testing.T) {
	identity := peerIdentityData{
		DNSName:       "peer.tailnet.ts.net",
		NodeID:        "nABC123",
		LoginName:     "alice@example.com",
		DisplayName:   "Alice",
		ProfilePicURL: "https://example.com/pic.png",
	}
	out := marshalPeerIdentity(identity)

	var got peerIdentityData
	if err := json.Unmarshal([]byte(out), &got); err != nil {
		t.Fatalf("marshalPeerIdentity produced invalid JSON: %v", err)
	}
	if got != identity {
		t.Errorf("identity round-trip mismatch: got %+v, want %+v", got, identity)
	}
}

// TestModulePath guards the go.mod module path against the stale
// claude-code-on-the-go name reappearing.
func TestModulePath(t *testing.T) {
	bi, ok := debug.ReadBuildInfo()
	if !ok {
		t.Skip("no build info embedded in test binary")
	}
	const want = "github.com/jamesyong-42/truffle/packages/sidecar-slim"
	if bi.Main.Path != want {
		t.Errorf("module path = %q, want %q", bi.Main.Path, want)
	}
}

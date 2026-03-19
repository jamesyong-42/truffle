// sidecar-slim is a thin Go shim that wraps tsnet for the Rust truffle-core.
//
// It provides two communication channels:
//   - Command channel: stdin/stdout JSON lines for lifecycle commands
//   - Data bridge: local TCP connections to Rust's bridge port with binary headers
//
// All application logic (WebSocket, mesh, file transfer) lives in Rust.
// This shim only handles tsnet lifecycle and transparent TCP proxying.
package main

import (
	"bufio"
	"context"
	"crypto/tls"
	"encoding/binary"
	"encoding/hex"
	"encoding/json"
	"fmt"
	"io"
	"log"
	"net"
	"os"
	"strings"
	"sync"
	"time"

	"tailscale.com/client/tailscale"
	"tailscale.com/tsnet"
)

// Bridge header constants (must match Rust truffle-core/src/bridge/header.rs)
const (
	headerMagic   = 0x54524646 // "TRFF"
	headerVersion = 0x01

	dirIncoming = 0x01
	dirOutgoing = 0x02

	writeDeadline = 60 * time.Second
)

// Command/Event JSON types
type command struct {
	Command string          `json:"command"`
	Data    json.RawMessage `json:"data,omitempty"`
}

type event struct {
	Event string      `json:"event"`
	Data  interface{} `json:"data,omitempty"`
}

type startData struct {
	Hostname     string   `json:"hostname"`
	StateDir     string   `json:"stateDir"`
	AuthKey      string   `json:"authKey,omitempty"`
	BridgePort   uint16   `json:"bridgePort"`
	SessionToken string   `json:"sessionToken"` // 32-byte hex
	Ephemeral    bool     `json:"ephemeral,omitempty"`
	Tags         []string `json:"tags,omitempty"`
}

type dialData struct {
	RequestID string `json:"requestId"`
	Target    string `json:"target"`
	Port      uint16 `json:"port"`
}

type dialResultData struct {
	RequestID string `json:"requestId"`
	Success   bool   `json:"success"`
	Error     string `json:"error,omitempty"`
}

type statusData struct {
	State       string `json:"state"`
	Hostname    string `json:"hostname,omitempty"`
	DNSName     string `json:"dnsName,omitempty"`
	TailscaleIP string `json:"tailscaleIP,omitempty"`
	Error       string `json:"error,omitempty"`
}

type authRequiredData struct {
	AuthURL string `json:"authUrl"`
}

type peerInfo struct {
	ID           string   `json:"id"`
	Hostname     string   `json:"hostname"`
	DNSName      string   `json:"dnsName"`
	TailscaleIPs []string `json:"tailscaleIPs"`
	Online       bool     `json:"online"`
	OS           string   `json:"os,omitempty"`
	CurAddr      string   `json:"curAddr,omitempty"`
	Relay        string   `json:"relay,omitempty"`
	LastSeen     string   `json:"lastSeen,omitempty"`
	KeyExpiry    string   `json:"keyExpiry,omitempty"`
	Expired      bool     `json:"expired,omitempty"`
}

type peersData struct {
	Peers []peerInfo `json:"peers"`
}

type errorData struct {
	Code    string `json:"code"`
	Message string `json:"message"`
}

// peerIdentityData is the JSON structure encoded into the bridge header's
// remoteDNS field when WhoIs lookup succeeds. Rust deserializes this to
// extract identity information about the connecting peer.
type peerIdentityData struct {
	DNSName       string `json:"dnsName"`
	LoginName     string `json:"loginName,omitempty"`
	DisplayName   string `json:"displayName,omitempty"`
	ProfilePicURL string `json:"profilePicUrl,omitempty"`
	NodeID        string `json:"nodeId,omitempty"`
}

// stateChangeData is the payload for tsnet:stateChange events.
type stateChangeData struct {
	State string `json:"state"`
}

// keyExpiringData is the payload for tsnet:keyExpiring events.
type keyExpiringData struct {
	ExpiresAt  string `json:"expiresAt"`
	ExpiresIn  int64  `json:"expiresInSecs"`
}

// healthWarningData is the payload for tsnet:healthWarning events.
type healthWarningData struct {
	Warnings []string `json:"warnings"`
}

// shim is the main application state.
type shim struct {
	server       *tsnet.Server
	sessionToken []byte // 32 bytes
	bridgePort   uint16

	writeMu sync.Mutex // protects stdout writes
	writer  *json.Encoder

	listenerMu sync.Mutex   // protects listeners
	listeners  []net.Listener // active listeners (TLS :443, TCP :9417)

	ctx    context.Context
	cancel context.CancelFunc
}

func main() {
	log.SetOutput(os.Stderr) // all logs go to stderr; stdout is JSON events only

	ctx, cancel := context.WithCancel(context.Background())
	s := &shim{
		writer: json.NewEncoder(os.Stdout),
		ctx:    ctx,
		cancel: cancel,
	}

	scanner := bufio.NewScanner(os.Stdin)
	scanner.Buffer(make([]byte, 1024*1024), 1024*1024) // 1MB max line

	for scanner.Scan() {
		line := scanner.Text()
		if strings.TrimSpace(line) == "" {
			continue
		}

		var cmd command
		if err := json.Unmarshal([]byte(line), &cmd); err != nil {
			s.sendError("PARSE_ERROR", fmt.Sprintf("failed to parse command: %v", err))
			continue
		}

		switch cmd.Command {
		case "tsnet:start":
			s.handleStart(cmd.Data)
		case "tsnet:stop":
			s.handleStop()
		case "tsnet:getPeers":
			s.handleGetPeers()
		case "bridge:dial":
			s.handleDial(cmd.Data)
		default:
			s.sendError("UNKNOWN_CMD", fmt.Sprintf("unknown command: %s", cmd.Command))
		}
	}

	if err := scanner.Err(); err != nil {
		log.Printf("stdin read error: %v", err)
	}

	// Stdin closed — clean shutdown
	s.handleStop()
}

func (s *shim) handleStart(data json.RawMessage) {
	var d startData
	if err := json.Unmarshal(data, &d); err != nil {
		s.sendError("START_ERROR", fmt.Sprintf("invalid start data: %v", err))
		return
	}

	token, err := hex.DecodeString(d.SessionToken)
	if err != nil || len(token) != 32 {
		s.sendError("START_ERROR", "sessionToken must be 64 hex chars (32 bytes)")
		return
	}
	s.sessionToken = token
	s.bridgePort = d.BridgePort

	s.sendStatus("starting", d.Hostname, "", "", "")

	s.server = &tsnet.Server{
		Hostname:  d.Hostname,
		Dir:       d.StateDir,
		Logf:      log.Printf,
		Ephemeral: d.Ephemeral,
	}
	if d.AuthKey != "" {
		s.server.AuthKey = d.AuthKey
	}
	if len(d.Tags) > 0 {
		s.server.AdvertiseTags = d.Tags
	}

	if err := s.server.Start(); err != nil {
		s.sendStatus("error", "", "", "", err.Error())
		return
	}

	// Wait for running state in background
	go s.waitForRunning(d.Hostname)
}

func (s *shim) waitForRunning(hostname string) {
	lc, err := s.server.LocalClient()
	if err != nil {
		log.Printf("failed to get local client: %v", err)
		s.sendStatus("error", "", "", "", err.Error())
		return
	}

	authURLSent := false
	for {
		select {
		case <-s.ctx.Done():
			return
		default:
		}

		status, err := lc.StatusWithoutPeers(s.ctx)
		if err != nil {
			if s.ctx.Err() != nil {
				return
			}
			log.Printf("status check failed: %v", err)
			time.Sleep(500 * time.Millisecond)
			continue
		}

		if !authURLSent && status.AuthURL != "" {
			s.sendEvent("tsnet:authRequired", authRequiredData{AuthURL: status.AuthURL})
			authURLSent = true
		}

		if status.BackendState == "NeedsMachineAuth" {
			s.sendEvent("tsnet:needsApproval", nil)
		}

		if status.BackendState == "Running" {
			var ip string
			if len(status.TailscaleIPs) > 0 {
				ip = status.TailscaleIPs[0].String()
			}
			dnsName := strings.TrimSuffix(status.Self.DNSName, ".")

			s.sendStatus("running", hostname, dnsName, ip, "")
			s.sendEvent("tsnet:started", statusData{
				State:       "running",
				Hostname:    hostname,
				DNSName:     dnsName,
				TailscaleIP: ip,
			})

			// Start listeners
			go s.listenTLS(lc)
			go s.listenTCP(lc)

			// Start background state monitor
			go s.monitorState(lc)
			return
		}

		time.Sleep(500 * time.Millisecond)
	}
}

// trackListener adds a listener to the tracked set for cleanup on stop.
func (s *shim) trackListener(ln net.Listener) {
	s.listenerMu.Lock()
	defer s.listenerMu.Unlock()
	s.listeners = append(s.listeners, ln)
}

func (s *shim) listenTLS(lc *tailscale.LocalClient) {
	ln, err := s.server.ListenTLS("tcp", ":443")
	if err != nil {
		log.Printf("ListenTLS :443 failed: %v", err)
		s.sendError("LISTEN_ERROR", fmt.Sprintf("ListenTLS :443: %v", err))
		return
	}
	s.trackListener(ln)
	defer ln.Close()

	log.Printf("listening TLS on :443")
	for {
		conn, err := ln.Accept()
		if err != nil {
			if s.ctx.Err() != nil {
				return
			}
			log.Printf("accept :443 error: %v", err)
			continue
		}
		peerIdentity := s.resolvePeerIdentity(lc, conn.RemoteAddr().String())
		go s.bridgeToRust(conn, 443, dirIncoming, "", conn.RemoteAddr().String(), peerIdentity)
	}
}

func (s *shim) listenTCP(lc *tailscale.LocalClient) {
	ln, err := s.server.Listen("tcp", ":9417")
	if err != nil {
		log.Printf("Listen :9417 failed: %v", err)
		s.sendError("LISTEN_ERROR", fmt.Sprintf("Listen :9417: %v", err))
		return
	}
	s.trackListener(ln)
	defer ln.Close()

	log.Printf("listening TCP on :9417")
	for {
		conn, err := ln.Accept()
		if err != nil {
			if s.ctx.Err() != nil {
				return
			}
			log.Printf("accept :9417 error: %v", err)
			continue
		}
		peerIdentity := s.resolvePeerIdentity(lc, conn.RemoteAddr().String())
		go s.bridgeToRust(conn, 9417, dirIncoming, "", conn.RemoteAddr().String(), peerIdentity)
	}
}

func (s *shim) handleStop() {
	if s.cancel != nil {
		s.cancel()
	}

	// Close listeners first so accept loops exit before server teardown
	s.listenerMu.Lock()
	for _, ln := range s.listeners {
		if err := ln.Close(); err != nil {
			log.Printf("listener close error: %v", err)
		}
	}
	s.listeners = nil
	s.listenerMu.Unlock()

	if s.server != nil {
		if err := s.server.Close(); err != nil {
			log.Printf("server close error: %v", err)
		}
		s.server = nil
	}
	s.sendEvent("tsnet:stopped", nil)
}

func (s *shim) handleGetPeers() {
	if s.server == nil {
		s.sendError("NOT_RUNNING", "node not running")
		return
	}

	lc, err := s.server.LocalClient()
	if err != nil {
		s.sendError("PEERS_ERROR", err.Error())
		return
	}

	status, err := lc.Status(s.ctx)
	if err != nil {
		s.sendError("PEERS_ERROR", err.Error())
		return
	}

	var peers []peerInfo
	for _, peer := range status.Peer {
		var ips []string
		for _, ip := range peer.TailscaleIPs {
			ips = append(ips, ip.String())
		}
		p := peerInfo{
			ID:           string(peer.ID),
			Hostname:     peer.HostName,
			DNSName:      strings.TrimSuffix(peer.DNSName, "."),
			TailscaleIPs: ips,
			Online:       peer.Online,
			OS:           peer.OS,
			CurAddr:      peer.CurAddr,
			Relay:        peer.Relay,
			Expired:      peer.Expired,
		}
		if !peer.LastSeen.IsZero() {
			p.LastSeen = peer.LastSeen.UTC().Format(time.RFC3339)
		}
		if peer.KeyExpiry != nil && !peer.KeyExpiry.IsZero() {
			p.KeyExpiry = peer.KeyExpiry.UTC().Format(time.RFC3339)
		}
		peers = append(peers, p)
	}

	s.sendEvent("tsnet:peers", peersData{Peers: peers})
}

func (s *shim) handleDial(data json.RawMessage) {
	var d dialData
	if err := json.Unmarshal(data, &d); err != nil {
		s.sendError("DIAL_ERROR", fmt.Sprintf("invalid dial data: %v", err))
		return
	}

	go func() {
		if s.server == nil {
			s.sendEvent("bridge:dialResult", dialResultData{
				RequestID: d.RequestID,
				Success:   false,
				Error:     "node not running",
			})
			return
		}

		// Dial via tsnet
		addr := fmt.Sprintf("%s:%d", d.Target, d.Port)
		tsnetConn, err := s.server.Dial(s.ctx, "tcp", addr)
		if err != nil {
			s.sendEvent("bridge:dialResult", dialResultData{
				RequestID: d.RequestID,
				Success:   false,
				Error:     err.Error(),
			})
			return
		}

		// For port 443, wrap with TLS
		var conn net.Conn = tsnetConn
		if d.Port == 443 {
			tlsConn := tls.Client(tsnetConn, &tls.Config{
				ServerName: d.Target, // SNI = peer's DNS name
			})
			if err := tlsConn.HandshakeContext(s.ctx); err != nil {
				tsnetConn.Close()
				s.sendEvent("bridge:dialResult", dialResultData{
					RequestID: d.RequestID,
					Success:   false,
					Error:     fmt.Sprintf("TLS handshake failed: %v", err),
				})
				return
			}
			conn = tlsConn
		}

		// Bridge to Rust
		s.bridgeToRust(conn, d.Port, dirOutgoing, d.RequestID, addr, d.Target)
	}()
}

// bridgeToRust connects to Rust's local bridge port, sends the binary header,
// then does bidirectional io.Copy.
func (s *shim) bridgeToRust(tsnetConn net.Conn, port uint16, direction byte, requestID, remoteAddr, remoteDNS string) {
	localConn, err := net.Dial("tcp", fmt.Sprintf("127.0.0.1:%d", s.bridgePort))
	if err != nil {
		log.Printf("bridge connect failed: %v", err)
		tsnetConn.Close()
		// BUG-8: Report failure if this was an outgoing dial
		if direction == dirOutgoing && requestID != "" {
			s.sendEvent("bridge:dialResult", dialResultData{
				RequestID: requestID,
				Success:   false,
				Error:     fmt.Sprintf("bridge connect failed: %v", err),
			})
		}
		return
	}

	// Write binary header
	if err := writeHeader(localConn, s.sessionToken, direction, port, requestID, remoteAddr, remoteDNS); err != nil {
		log.Printf("header write failed: %v", err)
		localConn.Close()
		tsnetConn.Close()
		// BUG-8: Report failure if this was an outgoing dial
		if direction == dirOutgoing && requestID != "" {
			s.sendEvent("bridge:dialResult", dialResultData{
				RequestID: requestID,
				Success:   false,
				Error:     fmt.Sprintf("header write failed: %v", err),
			})
		}
		return
	}

	// Bidirectional copy with close-all pattern
	bridgeCopy(tsnetConn, localConn)
}

// writeHeader writes the bridge binary header per RFC 003.
func writeHeader(w io.Writer, token []byte, direction byte, port uint16, requestID, remoteAddr, remoteDNS string) error {
	reqIDBytes := []byte(requestID)
	addrBytes := []byte(remoteAddr)
	dnsBytes := []byte(remoteDNS)

	// Calculate total size
	size := 4 + 1 + 32 + 1 + 2 + 2 + len(reqIDBytes) + 2 + len(addrBytes) + 2 + len(dnsBytes)
	buf := make([]byte, size)
	offset := 0

	// Magic
	binary.BigEndian.PutUint32(buf[offset:], headerMagic)
	offset += 4

	// Version
	buf[offset] = headerVersion
	offset++

	// Session token (32 bytes)
	copy(buf[offset:], token)
	offset += 32

	// Direction
	buf[offset] = direction
	offset++

	// Service port
	binary.BigEndian.PutUint16(buf[offset:], port)
	offset += 2

	// RequestId
	binary.BigEndian.PutUint16(buf[offset:], uint16(len(reqIDBytes)))
	offset += 2
	copy(buf[offset:], reqIDBytes)
	offset += len(reqIDBytes)

	// RemoteAddr
	binary.BigEndian.PutUint16(buf[offset:], uint16(len(addrBytes)))
	offset += 2
	copy(buf[offset:], addrBytes)
	offset += len(addrBytes)

	// RemoteDNSName
	binary.BigEndian.PutUint16(buf[offset:], uint16(len(dnsBytes)))
	offset += 2
	copy(buf[offset:], dnsBytes)

	_, err := w.Write(buf)
	return err
}

// bridgeCopy does bidirectional io.Copy with the close-all pattern.
// When either copy finishes, both connections are closed.
func bridgeCopy(tsnetConn, localConn net.Conn) {
	var once sync.Once
	closeAll := func() {
		once.Do(func() {
			tsnetConn.Close()
			localConn.Close()
		})
	}
	defer closeAll()

	// Wrap connections with write-deadline-refreshing writers
	tsnetWriter := &deadlineWriter{conn: tsnetConn, timeout: writeDeadline}
	localWriter := &deadlineWriter{conn: localConn, timeout: writeDeadline}

	go func() {
		io.Copy(localWriter, tsnetConn)
		closeAll() // remote EOF -> close both
	}()
	io.Copy(tsnetWriter, localConn)
	// local (Rust) EOF -> close both (handled by defer)
}

// deadlineWriter wraps a net.Conn and refreshes the write deadline on each Write.
type deadlineWriter struct {
	conn    net.Conn
	timeout time.Duration
}

func (w *deadlineWriter) Write(p []byte) (int, error) {
	w.conn.SetWriteDeadline(time.Now().Add(w.timeout))
	return w.conn.Write(p)
}

// resolvePeerIdentity maps a remote address to a PeerIdentity JSON string via WhoIs.
// The JSON is placed into the bridge header's remoteDNS field so Rust can
// extract rich identity info about the connecting peer.
func (s *shim) resolvePeerIdentity(lc *tailscale.LocalClient, remoteAddr string) string {
	whois, err := lc.WhoIs(s.ctx, remoteAddr)
	if err != nil {
		log.Printf("resolvePeerIdentity: WhoIs(%s) failed: %v", remoteAddr, err)
		return ""
	}

	identity := peerIdentityData{}
	if whois.Node != nil {
		identity.DNSName = strings.TrimSuffix(whois.Node.Name, ".")
		identity.NodeID = string(whois.Node.StableID)
	}
	if whois.UserProfile != nil {
		identity.LoginName = whois.UserProfile.LoginName
		identity.DisplayName = whois.UserProfile.DisplayName
		identity.ProfilePicURL = whois.UserProfile.ProfilePicURL
	}

	data, err := json.Marshal(identity)
	if err != nil {
		log.Printf("resolvePeerIdentity: marshal failed: %v", err)
		return ""
	}
	return string(data)
}

// monitorState polls Tailscale status every 60 seconds and emits events for
// state changes, upcoming key expiry, and health warnings.
func (s *shim) monitorState(lc *tailscale.LocalClient) {
	ticker := time.NewTicker(60 * time.Second)
	defer ticker.Stop()

	lastState := "Running"

	for {
		select {
		case <-s.ctx.Done():
			return
		case <-ticker.C:
		}

		status, err := lc.StatusWithoutPeers(s.ctx)
		if err != nil {
			if s.ctx.Err() != nil {
				return
			}
			log.Printf("monitorState: status check failed: %v", err)
			continue
		}

		// Emit tsnet:stateChange if backend state changed
		if status.BackendState != lastState {
			s.sendEvent("tsnet:stateChange", stateChangeData{
				State: status.BackendState,
			})
			lastState = status.BackendState
		}

		// Emit tsnet:keyExpiring if our own key expires within 24 hours
		if status.Self != nil && status.Self.KeyExpiry != nil {
			expiresAt := *status.Self.KeyExpiry
			remaining := time.Until(expiresAt)
			if remaining > 0 && remaining < 24*time.Hour {
				s.sendEvent("tsnet:keyExpiring", keyExpiringData{
					ExpiresAt: expiresAt.UTC().Format(time.RFC3339),
					ExpiresIn: int64(remaining.Seconds()),
				})
			}
		}

		// Emit tsnet:healthWarning if there are health issues
		if len(status.Health) > 0 {
			s.sendEvent("tsnet:healthWarning", healthWarningData{
				Warnings: status.Health,
			})
		}
	}
}

// sendEvent writes a JSON event to stdout.
func (s *shim) sendEvent(eventType string, data interface{}) {
	s.writeMu.Lock()
	defer s.writeMu.Unlock()
	s.writer.Encode(event{Event: eventType, Data: data})
}

// sendStatus is a convenience for sending tsnet:status events.
func (s *shim) sendStatus(state, hostname, dnsName, tailscaleIP, errMsg string) {
	s.sendEvent("tsnet:status", statusData{
		State:       state,
		Hostname:    hostname,
		DNSName:     dnsName,
		TailscaleIP: tailscaleIP,
		Error:       errMsg,
	})
}

// sendError sends a tsnet:error event.
func (s *shim) sendError(code, message string) {
	s.sendEvent("tsnet:error", errorData{Code: code, Message: message})
}

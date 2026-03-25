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
	"net/netip"
	"os"
	"strings"
	"sync"
	"time"

	"tailscale.com/client/tailscale"
	"tailscale.com/ipn"
	"tailscale.com/ipn/ipnstate"
	"tailscale.com/tailcfg"
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

// listenData is the payload for tsnet:listen commands.
type listenData struct {
	Port uint16 `json:"port"`
	TLS  bool   `json:"tls,omitempty"`
}

// listeningData is the payload for tsnet:listening events.
type listeningData struct {
	Port uint16 `json:"port"`
}

// unlistenData is the payload for tsnet:unlisten commands.
type unlistenData struct {
	Port uint16 `json:"port"`
}

// unlistenedData is the payload for tsnet:unlistened events.
type unlistenedData struct {
	Port uint16 `json:"port"`
}

// pingData is the payload for tsnet:ping commands.
type pingData struct {
	Target   string `json:"target"`
	PingType string `json:"pingType,omitempty"` // "TSMP", "Disco", "ICMP" (default: "TSMP")
}

// pingResultData is the payload for tsnet:pingResult events.
type pingResultData struct {
	Target    string  `json:"target"`
	LatencyMs float64 `json:"latencyMs"`
	Direct    bool    `json:"direct"`
	Relay     string  `json:"relay,omitempty"`
	PeerAddr  string  `json:"peerAddr,omitempty"`
	Error     string  `json:"error,omitempty"`
}

// pushFileData is the payload for tsnet:pushFile commands.
type pushFileData struct {
	TargetNodeID string `json:"targetNodeId"`
	FileName     string `json:"fileName"`
	FilePath     string `json:"filePath"`
}

// pushFileResultData is the payload for tsnet:pushFileResult events.
type pushFileResultData struct {
	Success bool   `json:"success"`
	Error   string `json:"error,omitempty"`
}

// waitingFileInfo represents a single waiting file in Taildrop.
type waitingFileInfo struct {
	Name string `json:"name"`
	Size int64  `json:"size"`
}

// waitingFilesResultData is the payload for tsnet:waitingFilesResult events.
type waitingFilesResultData struct {
	Files []waitingFileInfo `json:"files"`
}

// getWaitingFileData is the payload for tsnet:getWaitingFile commands.
type getWaitingFileData struct {
	FileName string `json:"fileName"`
	SavePath string `json:"savePath"`
}

// getWaitingFileResultData is the payload for tsnet:getWaitingFileResult events.
type getWaitingFileResultData struct {
	Success  bool   `json:"success"`
	FileName string `json:"fileName"`
	SavePath string `json:"savePath"`
	Error    string `json:"error,omitempty"`
}

// deleteWaitingFileData is the payload for tsnet:deleteWaitingFile commands.
type deleteWaitingFileData struct {
	FileName string `json:"fileName"`
}

// deleteWaitingFileResultData is the payload for tsnet:deleteWaitingFileResult events.
type deleteWaitingFileResultData struct {
	Success  bool   `json:"success"`
	FileName string `json:"fileName,omitempty"`
	Error    string `json:"error,omitempty"`
}

// stateChangeData is the payload for tsnet:stateChange events.
type stateChangeData struct {
	State string `json:"state"`
}

// watchPeersData is the payload for tsnet:watchPeers commands.
type watchPeersData struct {
	IncludeAll bool `json:"includeAll,omitempty"`
}

// peerChangedData is the payload for tsnet:peerChanged events.
type peerChangedData struct {
	ChangeType string    `json:"changeType"` // "joined", "left", "updated"
	PeerID     string    `json:"peerId"`
	Peer       *peerInfo `json:"peer,omitempty"` // nil for "left" events
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

// listenPacketData is the payload for tsnet:listenPacket commands.
type listenPacketData struct {
	Port uint16 `json:"port"`
}

// listeningPacketData is the payload for tsnet:listeningPacket events.
type listeningPacketData struct {
	Port      uint16 `json:"port"`
	LocalPort uint16 `json:"localPort"`
}

// udpRelay manages a tsnet PacketConn <-> local UDP socket relay.
type udpRelay struct {
	port      uint16         // tsnet-bound port
	localPort uint16         // local relay port (127.0.0.1)
	tsnetConn net.PacketConn // tsnet PacketConn
	localConn net.PacketConn // local UDP socket
	cancel    context.CancelFunc
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

	// dynamicListeners tracks listeners created via tsnet:listen, keyed by port.
	dynamicListenerMu sync.Mutex
	dynamicListeners  map[uint16]net.Listener

	// udpRelays tracks active UDP relays created via tsnet:listenPacket, keyed by port.
	udpRelayMu sync.Mutex
	udpRelays  map[uint16]*udpRelay

	ctx    context.Context
	cancel context.CancelFunc
}

func main() {
	log.SetOutput(os.Stderr) // all logs go to stderr; stdout is JSON events only

	ctx, cancel := context.WithCancel(context.Background())
	s := &shim{
		writer:           json.NewEncoder(os.Stdout),
		dynamicListeners: make(map[uint16]net.Listener),
		udpRelays:        make(map[uint16]*udpRelay),
		ctx:              ctx,
		cancel:           cancel,
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
		case "tsnet:listen":
			s.handleListen(cmd.Data)
		case "tsnet:unlisten":
			s.handleUnlisten(cmd.Data)
		case "tsnet:ping":
			s.handlePing(cmd.Data)
		case "tsnet:watchPeers":
			s.handleWatchPeers(cmd.Data)
		case "tsnet:listenPacket":
			s.handleListenPacket(cmd.Data)
		case "tsnet:pushFile":
			s.handlePushFile(cmd.Data)
		case "tsnet:waitingFiles":
			s.handleWaitingFiles()
		case "tsnet:getWaitingFile":
			s.handleGetWaitingFile(cmd.Data)
		case "tsnet:deleteWaitingFile":
			s.handleDeleteWaitingFile(cmd.Data)
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

	// Close dynamic listeners
	s.dynamicListenerMu.Lock()
	for port, ln := range s.dynamicListeners {
		if err := ln.Close(); err != nil {
			log.Printf("dynamic listener :%d close error: %v", port, err)
		}
	}
	s.dynamicListeners = make(map[uint16]net.Listener)
	s.dynamicListenerMu.Unlock()

	// Close UDP relays
	s.udpRelayMu.Lock()
	for port, relay := range s.udpRelays {
		log.Printf("closing UDP relay :%d", port)
		relay.cancel()
		relay.tsnetConn.Close()
		relay.localConn.Close()
	}
	s.udpRelays = make(map[uint16]*udpRelay)
	s.udpRelayMu.Unlock()

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
			log.Printf("[handleDial] rid=%s FAIL: node not running", d.RequestID)
			s.sendEvent("bridge:dialResult", dialResultData{
				RequestID: d.RequestID,
				Success:   false,
				Error:     "node not running",
			})
			return
		}

		// Dial via tsnet
		addr := fmt.Sprintf("%s:%d", d.Target, d.Port)
		log.Printf("[handleDial] rid=%s dialing %s", d.RequestID, addr)
		tsnetConn, err := s.server.Dial(s.ctx, "tcp", addr)
		if err != nil {
			log.Printf("[handleDial] rid=%s DIAL FAILED: %v", d.RequestID, err)
			s.sendEvent("bridge:dialResult", dialResultData{
				RequestID: d.RequestID,
				Success:   false,
				Error:     err.Error(),
			})
			return
		}
		log.Printf("[handleDial] rid=%s dial succeeded, bridging to Rust", d.RequestID)

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

func (s *shim) handleListen(data json.RawMessage) {
	var d listenData
	if err := json.Unmarshal(data, &d); err != nil {
		s.sendError("LISTEN_ERROR", fmt.Sprintf("invalid listen data: %v", err))
		return
	}

	if s.server == nil {
		s.sendError("NOT_RUNNING", "node not running")
		return
	}

	// Check if already listening on this port
	s.dynamicListenerMu.Lock()
	if _, exists := s.dynamicListeners[d.Port]; exists {
		s.dynamicListenerMu.Unlock()
		s.sendError("LISTEN_ERROR", fmt.Sprintf("already listening on port %d", d.Port))
		return
	}
	s.dynamicListenerMu.Unlock()

	go func() {
		lc, err := s.server.LocalClient()
		if err != nil {
			s.sendError("LISTEN_ERROR", fmt.Sprintf("failed to get local client: %v", err))
			return
		}

		addr := fmt.Sprintf(":%d", d.Port)
		var ln net.Listener

		if d.TLS {
			ln, err = s.server.ListenTLS("tcp", addr)
		} else {
			ln, err = s.server.Listen("tcp", addr)
		}
		if err != nil {
			s.sendError("LISTEN_ERROR", fmt.Sprintf("Listen :%d: %v", d.Port, err))
			return
		}

		s.dynamicListenerMu.Lock()
		s.dynamicListeners[d.Port] = ln
		s.dynamicListenerMu.Unlock()

		// Also track in the main listener list for cleanup on stop
		s.trackListener(ln)

		s.sendEvent("tsnet:listening", listeningData{Port: d.Port})

		proto := "TCP"
		if d.TLS {
			proto = "TLS"
		}
		log.Printf("listening %s on :%d (dynamic)", proto, d.Port)

		for {
			conn, err := ln.Accept()
			if err != nil {
				if s.ctx.Err() != nil {
					return
				}
				// Check if this was a deliberate close (unlisten)
				s.dynamicListenerMu.Lock()
				_, stillActive := s.dynamicListeners[d.Port]
				s.dynamicListenerMu.Unlock()
				if !stillActive {
					return
				}
				log.Printf("accept :%d error: %v", d.Port, err)
				continue
			}
			peerIdentity := s.resolvePeerIdentity(lc, conn.RemoteAddr().String())
			go s.bridgeToRust(conn, d.Port, dirIncoming, "", conn.RemoteAddr().String(), peerIdentity)
		}
	}()
}

func (s *shim) handleUnlisten(data json.RawMessage) {
	var d unlistenData
	if err := json.Unmarshal(data, &d); err != nil {
		s.sendError("UNLISTEN_ERROR", fmt.Sprintf("invalid unlisten data: %v", err))
		return
	}

	s.dynamicListenerMu.Lock()
	ln, exists := s.dynamicListeners[d.Port]
	if !exists {
		s.dynamicListenerMu.Unlock()
		s.sendError("UNLISTEN_ERROR", fmt.Sprintf("no listener on port %d", d.Port))
		return
	}
	delete(s.dynamicListeners, d.Port)
	s.dynamicListenerMu.Unlock()

	if err := ln.Close(); err != nil {
		log.Printf("close listener :%d error: %v", d.Port, err)
	}

	log.Printf("stopped listening on :%d (dynamic)", d.Port)
	s.sendEvent("tsnet:unlistened", unlistenedData{Port: d.Port})
}

func (s *shim) handleListenPacket(data json.RawMessage) {
	var d listenPacketData
	if err := json.Unmarshal(data, &d); err != nil {
		s.sendError("LISTEN_PACKET_ERROR", fmt.Sprintf("invalid listenPacket data: %v", err))
		return
	}

	if s.server == nil {
		s.sendError("NOT_RUNNING", "node not running")
		return
	}

	// Check if already relaying on this port
	s.udpRelayMu.Lock()
	if _, exists := s.udpRelays[d.Port]; exists {
		s.udpRelayMu.Unlock()
		s.sendError("LISTEN_PACKET_ERROR", fmt.Sprintf("already listening UDP on port %d", d.Port))
		return
	}
	s.udpRelayMu.Unlock()

	go func() {
		// Get the tailscale IP for binding
		status, err := s.server.LocalClient()
		if err != nil {
			s.sendError("LISTEN_PACKET_ERROR", fmt.Sprintf("failed to get local client: %v", err))
			return
		}

		st, err := status.StatusWithoutPeers(s.ctx)
		if err != nil {
			s.sendError("LISTEN_PACKET_ERROR", fmt.Sprintf("failed to get status: %v", err))
			return
		}

		if len(st.TailscaleIPs) == 0 {
			s.sendError("LISTEN_PACKET_ERROR", "no Tailscale IPs available")
			return
		}

		tsIP := st.TailscaleIPs[0].String()
		listenAddr := fmt.Sprintf("%s:%d", tsIP, d.Port)

		// Bind tsnet PacketConn
		tsnetPC, err := s.server.ListenPacket("udp", listenAddr)
		if err != nil {
			s.sendError("LISTEN_PACKET_ERROR", fmt.Sprintf("ListenPacket %s: %v", listenAddr, err))
			return
		}

		// Bind local relay UDP socket on ephemeral port
		localPC, err := net.ListenPacket("udp", "127.0.0.1:0")
		if err != nil {
			tsnetPC.Close()
			s.sendError("LISTEN_PACKET_ERROR", fmt.Sprintf("local UDP bind: %v", err))
			return
		}

		localAddr := localPC.LocalAddr().(*net.UDPAddr)
		localPort := uint16(localAddr.Port)

		relayCtx, relayCancel := context.WithCancel(s.ctx)

		relay := &udpRelay{
			port:      d.Port,
			localPort: localPort,
			tsnetConn: tsnetPC,
			localConn: localPC,
			cancel:    relayCancel,
		}

		s.udpRelayMu.Lock()
		s.udpRelays[d.Port] = relay
		s.udpRelayMu.Unlock()

		s.sendEvent("tsnet:listeningPacket", listeningPacketData{
			Port:      d.Port,
			LocalPort: localPort,
		})

		log.Printf("UDP relay started: tsnet %s <-> 127.0.0.1:%d", listenAddr, localPort)

		// We need to track the Rust peer's address so we can relay inbound packets to it.
		// The Rust side "connects" its UDP socket to our local relay, so we learn its
		// address from the first outbound packet it sends.
		var rustAddr net.Addr
		var rustAddrMu sync.Mutex

		// Goroutine: tsnet -> local (inbound datagrams)
		go func() {
			buf := make([]byte, 65536)
			for {
				select {
				case <-relayCtx.Done():
					return
				default:
				}

				n, remoteAddr, err := tsnetPC.ReadFrom(buf)
				if err != nil {
					if relayCtx.Err() != nil {
						return
					}
					log.Printf("UDP relay tsnet read error: %v", err)
					continue
				}

				log.Printf("UDP relay inbound: %d bytes from %v", n, remoteAddr)

				// Parse remote address to get IP and port for the header
				udpAddr, ok := remoteAddr.(*net.UDPAddr)
				if !ok {
					log.Printf("UDP relay: unexpected remote addr type: %T", remoteAddr)
					continue
				}

				ip4 := udpAddr.IP.To4()
				if ip4 == nil {
					log.Printf("UDP relay: non-IPv4 remote addr: %v", udpAddr)
					continue
				}

				// Frame: [4-byte IPv4][2-byte port BE][payload]
				framed := make([]byte, 6+n)
				copy(framed[0:4], ip4)
				binary.BigEndian.PutUint16(framed[4:6], uint16(udpAddr.Port))
				copy(framed[6:], buf[:n])

				rustAddrMu.Lock()
				ra := rustAddr
				rustAddrMu.Unlock()

				if ra == nil {
					log.Printf("UDP relay: no Rust peer address yet, dropping inbound packet from %v", remoteAddr)
					continue
				}

				log.Printf("UDP relay inbound: forwarding %d framed bytes to Rust at %v", len(framed), ra)
				if _, err := localPC.WriteTo(framed, ra); err != nil {
					if relayCtx.Err() != nil {
						return
					}
					log.Printf("UDP relay local write error: %v", err)
				}
			}
		}()

		// Main goroutine: local -> tsnet (outbound datagrams)
		buf := make([]byte, 65536)
		for {
			select {
			case <-relayCtx.Done():
				return
			default:
			}

			n, senderAddr, err := localPC.ReadFrom(buf)
			if err != nil {
				if relayCtx.Err() != nil {
					return
				}
				log.Printf("UDP relay local read error: %v", err)
				continue
			}

			// Remember the Rust peer's address
			rustAddrMu.Lock()
			prevRustAddr := rustAddr
			rustAddr = senderAddr
			rustAddrMu.Unlock()

			if prevRustAddr == nil {
				log.Printf("UDP relay: learned Rust peer address: %v", senderAddr)
			}

			if n < 6 {
				log.Printf("UDP relay: outbound packet too short (%d bytes)", n)
				continue
			}

			// Parse header: [4-byte IPv4][2-byte port BE][payload]
			// IMPORTANT: Use a raw 4-byte net.IP slice instead of net.IPv4()
			// which returns a 16-byte IPv4-mapped IPv6 address (::ffff:x.x.x.x).
			// gvisor's gonet.UDPConn.WriteTo passes the IP to tcpip.AddrFromSlice
			// which treats 16-byte IPs as IPv6. Since our tsnet PacketConn is
			// bound as udp4, writing to an IPv6 dest would fail silently or
			// produce a network-unreachable error.
			targetIP := net.IP(append([]byte(nil), buf[0:4]...))
			targetPort := binary.BigEndian.Uint16(buf[4:6])
			payload := buf[6:n]

			targetAddr := &net.UDPAddr{IP: targetIP, Port: int(targetPort)}
			log.Printf("UDP relay outbound: %d payload bytes -> %v (IP len=%d, raw IP=%x)", len(payload), targetAddr, len(targetIP), []byte(targetIP))
			if _, err := tsnetPC.WriteTo(payload, targetAddr); err != nil {
				if relayCtx.Err() != nil {
					return
				}
				log.Printf("UDP relay tsnet write error to %v: %v", targetAddr, err)
			} else {
				log.Printf("UDP relay outbound: sent %d bytes to %v OK", len(payload), targetAddr)
			}
		}
	}()
}

func (s *shim) handlePing(data json.RawMessage) {
	var d pingData
	if err := json.Unmarshal(data, &d); err != nil {
		s.sendError("PING_ERROR", fmt.Sprintf("invalid ping data: %v", err))
		return
	}

	if s.server == nil {
		s.sendError("NOT_RUNNING", "node not running")
		return
	}

	go func() {
		lc, err := s.server.LocalClient()
		if err != nil {
			s.sendEvent("tsnet:pingResult", pingResultData{
				Target: d.Target,
				Error:  fmt.Sprintf("failed to get local client: %v", err),
			})
			return
		}

		addr, err := netip.ParseAddr(d.Target)
		if err != nil {
			s.sendEvent("tsnet:pingResult", pingResultData{
				Target: d.Target,
				Error:  fmt.Sprintf("failed to parse target IP: %v", err),
			})
			return
		}

		// Map user-facing ping type to tailcfg.PingType.
		// Valid values: "TSMP" (default), "disco", "ICMP", "peerapi".
		var pt tailcfg.PingType
		switch strings.ToUpper(d.PingType) {
		case "DISCO":
			pt = tailcfg.PingDisco
		case "ICMP":
			pt = tailcfg.PingICMP
		case "PEERAPI":
			pt = tailcfg.PingPeerAPI
		case "TSMP", "":
			pt = tailcfg.PingTSMP
		default:
			s.sendEvent("tsnet:pingResult", pingResultData{
				Target: d.Target,
				Error:  fmt.Sprintf("unknown ping type: %s (valid: TSMP, disco, ICMP, peerapi)", d.PingType),
			})
			return
		}

		ctx, cancel := context.WithTimeout(s.ctx, 10*time.Second)
		defer cancel()

		result, err := lc.Ping(ctx, addr, pt)
		if err != nil {
			s.sendEvent("tsnet:pingResult", pingResultData{
				Target: d.Target,
				Error:  fmt.Sprintf("ping failed: %v", err),
			})
			return
		}

		// If the PingResult contains an error string, report it.
		if result.Err != "" {
			s.sendEvent("tsnet:pingResult", pingResultData{
				Target: d.Target,
				Error:  result.Err,
			})
			return
		}

		s.sendEvent("tsnet:pingResult", pingResultData{
			Target:    d.Target,
			LatencyMs: result.LatencySeconds * 1000.0,
			Direct:    result.Endpoint != "" && result.DERPRegionID == 0,
			Relay:     result.DERPRegionCode,
			PeerAddr:  result.Endpoint,
		})
	}()
}

func (s *shim) handleWatchPeers(data json.RawMessage) {
	if s.server == nil {
		s.sendError("NOT_RUNNING", "node not running")
		return
	}

	go func() {
		lc, err := s.server.LocalClient()
		if err != nil {
			s.sendError("WATCH_PEERS_ERROR", fmt.Sprintf("failed to get local client: %v", err))
			return
		}

		// WatchIPNBus gives us real-time notifications about network changes.
		// We watch for engine updates which include peer state changes.
		watcher, err := lc.WatchIPNBus(s.ctx, ipn.NotifyWatchEngineUpdates)
		if err != nil {
			s.sendError("WATCH_PEERS_ERROR", fmt.Sprintf("WatchIPNBus failed: %v", err))
			return
		}
		defer watcher.Close()

		log.Printf("WatchIPNBus started, listening for peer changes")

		// Track known peers by their stable ID to detect joins/leaves/updates.
		// We use the same peerInfo format as getPeers so Rust can deserialize uniformly.
		knownPeers := make(map[string]peerInfo)

		// Seed with current peers via the status API (same as handleGetPeers).
		status, err := lc.Status(s.ctx)
		if err == nil {
			for _, peer := range status.Peer {
				pi := statusPeerToInfo(peer)
				knownPeers[pi.ID] = pi
			}
		}

		for {
			n, err := watcher.Next()
			if err != nil {
				if s.ctx.Err() != nil {
					return // context cancelled, clean shutdown
				}
				log.Printf("WatchIPNBus error: %v", err)
				return
			}

			// When we get a NetMap update, re-fetch the full status and diff
			// against our known peers. This is simpler and more reliable than
			// parsing the NetMap directly, and reuses the same code path as
			// handleGetPeers for consistent output.
			if n.NetMap == nil {
				continue
			}

			newStatus, err := lc.Status(s.ctx)
			if err != nil {
				if s.ctx.Err() != nil {
					return
				}
				log.Printf("WatchIPNBus: status fetch failed: %v", err)
				continue
			}

			// Build current peer map
			currentPeers := make(map[string]peerInfo)
			for _, peer := range newStatus.Peer {
				pi := statusPeerToInfo(peer)
				currentPeers[pi.ID] = pi
			}

			// Detect new peers (joined)
			for id, pi := range currentPeers {
				if _, exists := knownPeers[id]; !exists {
					piCopy := pi
					s.sendEvent("tsnet:peerChanged", peerChangedData{
						ChangeType: "joined",
						PeerID:     id,
						Peer:       &piCopy,
					})
				}
			}

			// Detect removed peers (left)
			for id := range knownPeers {
				if _, exists := currentPeers[id]; !exists {
					s.sendEvent("tsnet:peerChanged", peerChangedData{
						ChangeType: "left",
						PeerID:     id,
					})
				}
			}

			// Detect updated peers
			for id, newPi := range currentPeers {
				if oldPi, exists := knownPeers[id]; exists {
					if watchPeerChanged(oldPi, newPi) {
						piCopy := newPi
						s.sendEvent("tsnet:peerChanged", peerChangedData{
							ChangeType: "updated",
							PeerID:     id,
							Peer:       &piCopy,
						})
					}
				}
			}

			// Update known peers for next iteration
			knownPeers = currentPeers
		}
	}()
}

// statusPeerToInfo converts an ipnstate.PeerStatus to our peerInfo type.
// This uses the same field access as handleGetPeers for consistency.
func statusPeerToInfo(peer *ipnstate.PeerStatus) peerInfo {
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
	return p
}

// watchPeerChanged returns true if any observable property of the peer has changed.
func watchPeerChanged(old, new peerInfo) bool {
	if old.Online != new.Online {
		return true
	}
	if old.CurAddr != new.CurAddr {
		return true
	}
	if old.Relay != new.Relay {
		return true
	}
	if old.Hostname != new.Hostname {
		return true
	}
	if old.DNSName != new.DNSName {
		return true
	}
	if old.Expired != new.Expired {
		return true
	}
	return false
}

func (s *shim) handlePushFile(data json.RawMessage) {
	var d pushFileData
	if err := json.Unmarshal(data, &d); err != nil {
		s.sendError("PUSH_FILE_ERROR", fmt.Sprintf("invalid pushFile data: %v", err))
		return
	}

	if s.server == nil {
		s.sendError("NOT_RUNNING", "node not running")
		return
	}

	go func() {
		lc, err := s.server.LocalClient()
		if err != nil {
			s.sendEvent("tsnet:pushFileResult", pushFileResultData{
				Success: false,
				Error:   fmt.Sprintf("failed to get local client: %v", err),
			})
			return
		}

		f, err := os.Open(d.FilePath)
		if err != nil {
			s.sendEvent("tsnet:pushFileResult", pushFileResultData{
				Success: false,
				Error:   fmt.Sprintf("failed to open file: %v", err),
			})
			return
		}
		defer f.Close()

		fi, err := f.Stat()
		if err != nil {
			s.sendEvent("tsnet:pushFileResult", pushFileResultData{
				Success: false,
				Error:   fmt.Sprintf("failed to stat file: %v", err),
			})
			return
		}

		ctx, cancel := context.WithTimeout(s.ctx, 5*time.Minute)
		defer cancel()

		err = lc.PushFile(ctx, tailcfg.StableNodeID(d.TargetNodeID), fi.Size(), d.FileName, f)
		if err != nil {
			s.sendEvent("tsnet:pushFileResult", pushFileResultData{
				Success: false,
				Error:   fmt.Sprintf("push file failed: %v", err),
			})
			return
		}

		s.sendEvent("tsnet:pushFileResult", pushFileResultData{
			Success: true,
		})
	}()
}

func (s *shim) handleWaitingFiles() {
	if s.server == nil {
		s.sendError("NOT_RUNNING", "node not running")
		return
	}

	go func() {
		lc, err := s.server.LocalClient()
		if err != nil {
			s.sendError("WAITING_FILES_ERROR", fmt.Sprintf("failed to get local client: %v", err))
			return
		}

		files, err := lc.WaitingFiles(s.ctx)
		if err != nil {
			s.sendError("WAITING_FILES_ERROR", fmt.Sprintf("waiting files failed: %v", err))
			return
		}

		var infos []waitingFileInfo
		for _, f := range files {
			infos = append(infos, waitingFileInfo{
				Name: f.Name,
				Size: f.Size,
			})
		}
		if infos == nil {
			infos = []waitingFileInfo{} // ensure non-null JSON array
		}

		s.sendEvent("tsnet:waitingFilesResult", waitingFilesResultData{
			Files: infos,
		})
	}()
}

func (s *shim) handleGetWaitingFile(data json.RawMessage) {
	var d getWaitingFileData
	if err := json.Unmarshal(data, &d); err != nil {
		s.sendError("GET_WAITING_FILE_ERROR", fmt.Sprintf("invalid getWaitingFile data: %v", err))
		return
	}

	if s.server == nil {
		s.sendError("NOT_RUNNING", "node not running")
		return
	}

	go func() {
		lc, err := s.server.LocalClient()
		if err != nil {
			s.sendEvent("tsnet:getWaitingFileResult", getWaitingFileResultData{
				Success:  false,
				FileName: d.FileName,
				SavePath: d.SavePath,
				Error:    fmt.Sprintf("failed to get local client: %v", err),
			})
			return
		}

		rc, _, err := lc.GetWaitingFile(s.ctx, d.FileName)
		if err != nil {
			s.sendEvent("tsnet:getWaitingFileResult", getWaitingFileResultData{
				Success:  false,
				FileName: d.FileName,
				SavePath: d.SavePath,
				Error:    fmt.Sprintf("get waiting file failed: %v", err),
			})
			return
		}
		defer rc.Close()

		outFile, err := os.Create(d.SavePath)
		if err != nil {
			s.sendEvent("tsnet:getWaitingFileResult", getWaitingFileResultData{
				Success:  false,
				FileName: d.FileName,
				SavePath: d.SavePath,
				Error:    fmt.Sprintf("failed to create save file: %v", err),
			})
			return
		}
		defer outFile.Close()

		if _, err := io.Copy(outFile, rc); err != nil {
			s.sendEvent("tsnet:getWaitingFileResult", getWaitingFileResultData{
				Success:  false,
				FileName: d.FileName,
				SavePath: d.SavePath,
				Error:    fmt.Sprintf("failed to write file: %v", err),
			})
			return
		}

		s.sendEvent("tsnet:getWaitingFileResult", getWaitingFileResultData{
			Success:  true,
			FileName: d.FileName,
			SavePath: d.SavePath,
		})
	}()
}

func (s *shim) handleDeleteWaitingFile(data json.RawMessage) {
	var d deleteWaitingFileData
	if err := json.Unmarshal(data, &d); err != nil {
		s.sendError("DELETE_WAITING_FILE_ERROR", fmt.Sprintf("invalid deleteWaitingFile data: %v", err))
		return
	}

	if s.server == nil {
		s.sendError("NOT_RUNNING", "node not running")
		return
	}

	go func() {
		lc, err := s.server.LocalClient()
		if err != nil {
			s.sendEvent("tsnet:deleteWaitingFileResult", deleteWaitingFileResultData{
				Success:  false,
				FileName: d.FileName,
				Error:    fmt.Sprintf("failed to get local client: %v", err),
			})
			return
		}

		err = lc.DeleteWaitingFile(s.ctx, d.FileName)
		if err != nil {
			s.sendEvent("tsnet:deleteWaitingFileResult", deleteWaitingFileResultData{
				Success:  false,
				FileName: d.FileName,
				Error:    fmt.Sprintf("delete waiting file failed: %v", err),
			})
			return
		}

		s.sendEvent("tsnet:deleteWaitingFileResult", deleteWaitingFileResultData{
			Success:  true,
			FileName: d.FileName,
		})
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

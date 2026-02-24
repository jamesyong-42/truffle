// Package ipc handles stdin/stdout JSON communication with the Electron host.
package ipc

import (
	"bufio"
	"encoding/json"
	"fmt"
	"io"
	"os"
	"sync"
)

// Command types from Electron to Go sidecar
const (
	CmdStart     = "tsnet:start"
	CmdStop      = "tsnet:stop"
	CmdStatus    = "tsnet:status"
	CmdWsMessage = "tsnet:wsMessage"
	CmdGetPeers  = "tsnet:getPeers"
	CmdDial        = "tsnet:dial"        // Dial outgoing WebSocket to another device
	CmdDialClose   = "tsnet:dialClose"   // Close an outgoing connection
	CmdDialMessage = "tsnet:dialMessage" // Send message on outgoing connection

	// Reverse proxy commands
	CmdProxyAdd    = "proxy:add"    // Start a new reverse proxy
	CmdProxyRemove = "proxy:remove" // Stop and remove a reverse proxy
	CmdProxyList   = "proxy:list"   // List all active proxies
)

// Event types from Go sidecar to Electron
const (
	EvtStarted        = "tsnet:started"
	EvtStopped        = "tsnet:stopped"
	EvtStatus         = "tsnet:status"
	EvtWsConnect      = "tsnet:wsConnect"
	EvtWsMessage      = "tsnet:wsMessage"
	EvtWsDisconnect   = "tsnet:wsDisconnect"
	EvtError          = "tsnet:error"
	EvtAuthRequired   = "tsnet:authRequired"
	EvtPeers          = "tsnet:peers"
	EvtDialConnected  = "tsnet:dialConnected"  // Outgoing connection established
	EvtDialMessage    = "tsnet:dialMessage"    // Message from outgoing connection
	EvtDialDisconnect = "tsnet:dialDisconnect" // Outgoing connection closed
	EvtDialError      = "tsnet:dialError"      // Outgoing connection error

	// Reverse proxy events
	EvtProxyStarted = "proxy:started" // Proxy successfully started
	EvtProxyStopped = "proxy:stopped" // Proxy stopped
	EvtProxyError   = "proxy:error"   // Proxy error
	EvtProxyList    = "proxy:list"    // List of all proxies
)

// Command represents an incoming IPC command from Electron
type Command struct {
	Command string          `json:"command"`
	Data    json.RawMessage `json:"data,omitempty"`
}

// StartCommand contains parameters for starting the tsnet node
type StartCommand struct {
	Hostname string `json:"hostname"`
	StateDir string `json:"stateDir"`
	AuthKey  string `json:"authKey,omitempty"`
	PWAPath  string `json:"pwaPath,omitempty"` // Path to PWA dist directory to serve
}

// WsMessageCommand contains a WebSocket message to forward
type WsMessageCommand struct {
	ConnectionID string `json:"connectionId"`
	Data         string `json:"data"`
}

// DialCommand contains parameters for dialing an outgoing connection
type DialCommand struct {
	DeviceID string `json:"deviceId"` // Unique ID for this connection
	Hostname string `json:"hostname"` // Tailscale hostname to connect to
	DNSName  string `json:"dnsName"`  // Full MagicDNS name for TLS (e.g., "hostname.tailnet.ts.net")
	Port     int    `json:"port"`     // Port to connect to (default 443)
}

// DialCloseCommand contains parameters for closing an outgoing connection
type DialCloseCommand struct {
	DeviceID string `json:"deviceId"`
}

// DialMessageCommand contains a message to send on an outgoing connection
type DialMessageCommand struct {
	DeviceID string `json:"deviceId"`
	Data     string `json:"data"`
}

// ProxyAddCommand contains parameters for adding a new reverse proxy
type ProxyAddCommand struct {
	ID           string `json:"id"`                     // Unique proxy identifier
	Name         string `json:"name"`                   // Human-readable name
	Port         int    `json:"port"`                   // External port to listen on (e.g., 3001)
	TargetPort   int    `json:"targetPort"`             // Local port to proxy to (e.g., 3000)
	TargetScheme string `json:"targetScheme,omitempty"` // "http" or "https" (default: "http")
}

// ProxyRemoveCommand contains parameters for removing a reverse proxy
type ProxyRemoveCommand struct {
	ID string `json:"id"` // Proxy identifier to remove
}

// Event represents an outgoing IPC event to Electron
type Event struct {
	Event string      `json:"event"`
	Data  interface{} `json:"data,omitempty"`
}

// StatusEventData contains the current status of the tsnet node
type StatusEventData struct {
	State       string `json:"state"`
	Hostname    string `json:"hostname,omitempty"`
	DNSName     string `json:"dnsName,omitempty"`     // Full MagicDNS name (e.g., "hostname.tailnet.ts.net")
	TailscaleIP string `json:"tailscaleIP,omitempty"`
	Error       string `json:"error,omitempty"`
}

// WsConnectEventData contains WebSocket connection info
type WsConnectEventData struct {
	ConnectionID string `json:"connectionId"`
	RemoteAddr   string `json:"remoteAddr"`
}

// WsMessageEventData contains an incoming WebSocket message
type WsMessageEventData struct {
	ConnectionID string `json:"connectionId"`
	Data         string `json:"data"`
}

// WsDisconnectEventData contains WebSocket disconnect info
type WsDisconnectEventData struct {
	ConnectionID string `json:"connectionId"`
	Reason       string `json:"reason,omitempty"`
}

// DialConnectedEventData contains outgoing connection established info
type DialConnectedEventData struct {
	DeviceID   string `json:"deviceId"`
	RemoteAddr string `json:"remoteAddr"`
}

// DialMessageEventData contains a message from an outgoing connection
type DialMessageEventData struct {
	DeviceID string `json:"deviceId"`
	Data     string `json:"data"`
}

// DialDisconnectEventData contains outgoing connection closed info
type DialDisconnectEventData struct {
	DeviceID string `json:"deviceId"`
	Reason   string `json:"reason,omitempty"`
}

// DialErrorEventData contains outgoing connection error info
type DialErrorEventData struct {
	DeviceID string `json:"deviceId"`
	Error    string `json:"error"`
}

// AuthRequiredEventData contains auth URL for Tailscale login
type AuthRequiredEventData struct {
	AuthURL string `json:"authUrl"`
}

// ErrorEventData contains error information
type ErrorEventData struct {
	Code    string `json:"code"`
	Message string `json:"message"`
}

// TailnetPeer represents a peer on the Tailscale network
type TailnetPeer struct {
	ID           string   `json:"id"`
	Hostname     string   `json:"hostname"`
	DNSName      string   `json:"dnsName"`
	TailscaleIPs []string `json:"tailscaleIPs"`
	Online       bool     `json:"online"`
	OS           string   `json:"os,omitempty"`
}

// PeersEventData contains the list of peers on the tailnet
type PeersEventData struct {
	Peers []TailnetPeer `json:"peers"`
}

// ProxyStartedEventData contains info about a successfully started proxy
type ProxyStartedEventData struct {
	ID         string `json:"id"`
	Port       int    `json:"port"`
	TargetPort int    `json:"targetPort"`
	URL        string `json:"url"` // Full URL to access the proxy (e.g., "https://hostname.ts.net:3001")
}

// ProxyStoppedEventData contains info about a stopped proxy
type ProxyStoppedEventData struct {
	ID     string `json:"id"`
	Reason string `json:"reason,omitempty"`
}

// ProxyErrorEventData contains error information for a proxy
type ProxyErrorEventData struct {
	ID    string `json:"id"`
	Error string `json:"error"`
	Code  string `json:"code"` // Error code (e.g., "PORT_IN_USE", "CONNECTION_REFUSED")
}

// ProxyConfig represents a single proxy configuration
type ProxyConfig struct {
	ID           string `json:"id"`
	Name         string `json:"name"`
	Port         int    `json:"port"`
	TargetPort   int    `json:"targetPort"`
	TargetScheme string `json:"targetScheme,omitempty"` // "http" or "https"
	IsActive     bool   `json:"isActive"`
}

// ProxyListEventData contains the list of all proxies
type ProxyListEventData struct {
	Proxies []ProxyConfig `json:"proxies"`
}

// Handler processes incoming commands
type Handler func(cmd Command) error

// Protocol handles IPC communication via stdin/stdout
type Protocol struct {
	reader   *bufio.Reader
	writer   io.Writer
	writeMu  sync.Mutex
	handlers map[string]Handler
	done     chan struct{}
}

// NewProtocol creates a new IPC protocol handler
func NewProtocol() *Protocol {
	return &Protocol{
		reader:   bufio.NewReader(os.Stdin),
		writer:   os.Stdout,
		handlers: make(map[string]Handler),
		done:     make(chan struct{}),
	}
}

// OnCommand registers a handler for a command type
func (p *Protocol) OnCommand(cmdType string, handler Handler) {
	p.handlers[cmdType] = handler
}

// Start begins reading commands from stdin
func (p *Protocol) Start() {
	go p.readLoop()
}

// Stop signals the protocol to stop
func (p *Protocol) Stop() {
	close(p.done)
}

// Send writes an event to stdout
func (p *Protocol) Send(event Event) error {
	p.writeMu.Lock()
	defer p.writeMu.Unlock()

	data, err := json.Marshal(event)
	if err != nil {
		return fmt.Errorf("failed to marshal event: %w", err)
	}

	// Write JSON followed by newline
	_, err = fmt.Fprintf(p.writer, "%s\n", data)
	return err
}

// SendStatus sends a status event
func (p *Protocol) SendStatus(status StatusEventData) error {
	return p.Send(Event{Event: EvtStatus, Data: status})
}

// SendError sends an error event
func (p *Protocol) SendError(code, message string) error {
	return p.Send(Event{Event: EvtError, Data: ErrorEventData{Code: code, Message: message}})
}

// SendWsConnect sends a WebSocket connect event
func (p *Protocol) SendWsConnect(connID, remoteAddr string) error {
	return p.Send(Event{Event: EvtWsConnect, Data: WsConnectEventData{
		ConnectionID: connID,
		RemoteAddr:   remoteAddr,
	}})
}

// SendWsMessage sends a WebSocket message event
func (p *Protocol) SendWsMessage(connID, data string) error {
	return p.Send(Event{Event: EvtWsMessage, Data: WsMessageEventData{
		ConnectionID: connID,
		Data:         data,
	}})
}

// SendWsDisconnect sends a WebSocket disconnect event
func (p *Protocol) SendWsDisconnect(connID, reason string) error {
	return p.Send(Event{Event: EvtWsDisconnect, Data: WsDisconnectEventData{
		ConnectionID: connID,
		Reason:       reason,
	}})
}

// SendAuthRequired sends an auth required event
func (p *Protocol) SendAuthRequired(authURL string) error {
	return p.Send(Event{Event: EvtAuthRequired, Data: AuthRequiredEventData{AuthURL: authURL}})
}

// SendPeers sends a peers event
func (p *Protocol) SendPeers(peers []TailnetPeer) error {
	return p.Send(Event{Event: EvtPeers, Data: PeersEventData{Peers: peers}})
}

// SendDialConnected sends a dial connected event
func (p *Protocol) SendDialConnected(deviceID, remoteAddr string) error {
	return p.Send(Event{Event: EvtDialConnected, Data: DialConnectedEventData{
		DeviceID:   deviceID,
		RemoteAddr: remoteAddr,
	}})
}

// SendDialMessage sends a dial message event
func (p *Protocol) SendDialMessage(deviceID, data string) error {
	return p.Send(Event{Event: EvtDialMessage, Data: DialMessageEventData{
		DeviceID: deviceID,
		Data:     data,
	}})
}

// SendDialDisconnect sends a dial disconnect event
func (p *Protocol) SendDialDisconnect(deviceID, reason string) error {
	return p.Send(Event{Event: EvtDialDisconnect, Data: DialDisconnectEventData{
		DeviceID: deviceID,
		Reason:   reason,
	}})
}

// SendDialError sends a dial error event
func (p *Protocol) SendDialError(deviceID, errMsg string) error {
	return p.Send(Event{Event: EvtDialError, Data: DialErrorEventData{
		DeviceID: deviceID,
		Error:    errMsg,
	}})
}

// SendProxyStarted sends a proxy started event
func (p *Protocol) SendProxyStarted(id string, port, targetPort int, url string) error {
	return p.Send(Event{Event: EvtProxyStarted, Data: ProxyStartedEventData{
		ID:         id,
		Port:       port,
		TargetPort: targetPort,
		URL:        url,
	}})
}

// SendProxyStopped sends a proxy stopped event
func (p *Protocol) SendProxyStopped(id, reason string) error {
	return p.Send(Event{Event: EvtProxyStopped, Data: ProxyStoppedEventData{
		ID:     id,
		Reason: reason,
	}})
}

// SendProxyError sends a proxy error event
func (p *Protocol) SendProxyError(id, errMsg, code string) error {
	return p.Send(Event{Event: EvtProxyError, Data: ProxyErrorEventData{
		ID:    id,
		Error: errMsg,
		Code:  code,
	}})
}

// SendProxyList sends the list of all proxies
func (p *Protocol) SendProxyList(proxies []ProxyConfig) error {
	return p.Send(Event{Event: EvtProxyList, Data: ProxyListEventData{
		Proxies: proxies,
	}})
}

func (p *Protocol) readLoop() {
	for {
		select {
		case <-p.done:
			return
		default:
		}

		line, err := p.reader.ReadString('\n')
		if err != nil {
			if err != io.EOF {
				p.SendError("IPC_READ_ERROR", err.Error())
			}
			return
		}

		var cmd Command
		if err := json.Unmarshal([]byte(line), &cmd); err != nil {
			p.SendError("IPC_PARSE_ERROR", fmt.Sprintf("failed to parse command: %v", err))
			continue
		}

		handler, ok := p.handlers[cmd.Command]
		if !ok {
			p.SendError("IPC_UNKNOWN_CMD", fmt.Sprintf("unknown command: %s", cmd.Command))
			continue
		}

		if err := handler(cmd); err != nil {
			p.SendError("IPC_HANDLER_ERROR", fmt.Sprintf("handler error for %s: %v", cmd.Command, err))
		}
	}
}

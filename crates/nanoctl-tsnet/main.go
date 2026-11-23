package main

import (
	"bufio"
	"bytes"
	"context"
	"crypto/subtle"
	"crypto/tls"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"log"
	"net"
	"net/http"
	"net/http/httputil"
	"net/netip"
	"net/url"
	"os"
	"path/filepath"
	"slices"
	"strings"
	"sync"
	"time"

	"tailscale.com/ipn/ipnstate"
	"tailscale.com/net/tsaddr"
	"tailscale.com/tsnet"
	"tailscale.com/util/dnsname"
)

const (
	bridgeHeader = "X-Nanoctl-Bridge"
	peerHeader   = "X-Nanoctl-Peer"
	maxDatagram  = 2048
)

type settings struct {
	StateDir string
	Web      uint16
	UDP      uint16
	Port     uint16
	Token    string
	HTTPS    bool
}

type snapshot struct {
	Node    *ipnstate.Status
	Ready   bool
	HTTPS   bool
	Port    uint16
	Media   string
	Relay   uint16
	Problem string
}

func main() {
	if len(os.Args) == 2 && os.Args[1] == "--version" {
		fmt.Println("nanoctl-tsnet")
		return
	}
	input := bufio.NewReader(os.Stdin)
	line, err := input.ReadBytes('\n')
	if err != nil {
		log.Fatal(err)
	}
	var cfg settings
	if err := json.Unmarshal(line, &cfg); err != nil {
		log.Fatal("invalid startup settings")
	}
	ctx, cancel := context.WithCancel(context.Background())
	defer cancel()
	go func() {
		// The agent holds this pipe open. Terminate even after an ungraceful parent exit.
		_, _ = io.Copy(io.Discard, input)
		cancel()
	}()
	host, err := os.Hostname()
	if err != nil {
		log.Fatal(err)
	}
	node := &tsnet.Server{
		Dir:      cfg.StateDir,
		Hostname: dnsname.SanitizeHostname("nanoctl-" + host),
		Logf:     func(string, ...any) {},
		UserLogf: func(string, ...any) {}, // Do not log authentication URLs.
	}
	if err := run(ctx, cfg, node, func(addr string) { fmt.Println(addr) }); err != nil {
		log.Fatal(err)
	}
}

func run(ctx context.Context, cfg settings, node *tsnet.Server, announce func(string)) error {
	defer node.Close()
	if len(cfg.Token) != 32 || cfg.Web == 0 || cfg.UDP == 0 || cfg.Port == 0 {
		return errors.New("invalid startup settings")
	}
	client, err := node.LocalClient()
	if err != nil {
		return err
	}
	control, err := net.Listen("tcp4", "127.0.0.1:0")
	if err != nil {
		return err
	}
	defer control.Close()
	udp, err := net.ListenUDP("udp4", &net.UDPAddr{IP: net.IPv4(127, 0, 0, 1)})
	if err != nil {
		return err
	}
	defer udp.Close()
	mux := http.NewServeMux()
	mux.HandleFunc("GET /whois", func(w http.ResponseWriter, req *http.Request) {
		addr, err := netip.ParseAddrPort(req.URL.Query().Get("peer"))
		if err != nil || !tsaddr.IsTailscaleIP(addr.Addr()) {
			http.Error(w, "not a tailnet address", http.StatusBadRequest)
			return
		}
		result, err := client.WhoIs(req.Context(), addr.String())
		reply(w, result, err)
	})
	mux.HandleFunc("GET /peers", func(w http.ResponseWriter, req *http.Request) {
		result, err := client.Status(req.Context())
		reply(w, result, err)
	})
	mux.HandleFunc("GET /probe", func(w http.ResponseWriter, req *http.Request) {
		ip, err := netip.ParseAddr(req.URL.Query().Get("ip"))
		if err != nil || !tsaddr.IsTailscaleIP(ip) {
			http.Error(w, "not a tailnet address", http.StatusBadRequest)
			return
		}
		ctx, cancel := context.WithTimeout(req.Context(), 400*time.Millisecond)
		defer cancel()
		conn, err := node.Dial(ctx, "tcp", netip.AddrPortFrom(ip, cfg.Port).String())
		if err != nil {
			http.Error(w, "peer unavailable", http.StatusBadGateway)
			return
		}
		conn.Close()
		w.WriteHeader(http.StatusNoContent)
	})
	controlServer := &http.Server{
		ReadHeaderTimeout: 2 * time.Second,
		Handler: http.HandlerFunc(func(w http.ResponseWriter, req *http.Request) {
			if subtle.ConstantTimeCompare([]byte(req.Header.Get(bridgeHeader)), []byte(cfg.Token)) != 1 {
				http.Error(w, "forbidden", http.StatusForbidden)
				return
			}
			mux.ServeHTTP(w, req)
		}),
	}
	defer controlServer.Close()
	port := fmt.Sprintf(":%d", cfg.Port)
	web, err := node.Listen("tcp", port)
	if err != nil {
		return err
	}
	defer web.Close()
	go controlServer.Serve(control)
	announce(control.Addr().String())
	viewer := &http.Server{
		Handler:           proxy(cfg.Web, cfg.Token),
		ReadHeaderTimeout: 5 * time.Second,
		MaxHeaderBytes:    16 << 10,
	}
	if cfg.HTTPS {
		viewer.TLSConfig = &tls.Config{
			GetCertificate: client.GetCertificate,
			MinVersion:     tls.VersionTLS12,
		}
	}
	defer viewer.Close()
	webExit := make(chan error, 1)
	go func() {
		if cfg.HTTPS {
			webExit <- viewer.ServeTLS(web, "", "")
		} else {
			webExit <- viewer.Serve(web)
		}
	}()
	path := filepath.Join(cfg.StateDir, "viewer-status.json")
	defer os.Remove(path)
	rustAddr := &net.UDPAddr{IP: net.IPv4(127, 0, 0, 1), Port: int(cfg.UDP)}
	var media *packetRelay
	var requestedCert string
	defer func() {
		if media != nil {
			media.close()
		}
	}()
	tick := time.NewTicker(2 * time.Second)
	defer tick.Stop()
	for {
		status := snapshot{HTTPS: cfg.HTTPS, Port: cfg.Port}
		poll, cancel := context.WithTimeout(ctx, 3*time.Second)
		status.Node, err = client.StatusWithoutPeers(poll)
		cancel()
		var ip netip.Addr
		if err == nil && status.Node.BackendState == "Running" && status.Node.Self != nil {
			for _, candidate := range status.Node.Self.TailscaleIPs {
				if candidate.Is4() && tsaddr.IsTailscaleIP(candidate) {
					ip = candidate
					break
				}
			}
			if !ip.IsValid() {
				status.Problem = "waiting for the embedded Tailscale IPv4 address"
			}
		}
		addr := netip.AddrPortFrom(ip, cfg.Port)
		if media != nil && media.addr != addr {
			media.close()
			media = nil
		}
		if ip.IsValid() && media == nil {
			packet, err := node.ListenPacket("udp4", addr.String())
			if err != nil {
				status.Problem = fmt.Sprintf("cannot listen for tailnet UDP on %s: %v", addr, err)
			} else {
				media = startRelay(packet, udp, addr, rustAddr, cfg.Token)
			}
		}
		if media != nil {
			status.Ready = true
			status.Media = media.addr.String()
			status.Relay = uint16(udp.LocalAddr().(*net.UDPAddr).Port)
			if cfg.HTTPS {
				dns := strings.TrimSuffix(status.Node.Self.DNSName, ".")
				if !slices.Contains(status.Node.CertDomains, dns) {
					status.Problem = "enable HTTPS Certificates under DNS at https://login.tailscale.com/admin/dns"
				} else if dns != requestedCert {
					requestedCert = dns
					go func() {
						certCtx, cancel := context.WithTimeout(ctx, time.Minute)
						defer cancel()
						if _, _, err := client.CertPair(certCtx, dns); err != nil && certCtx.Err() == nil {
							log.Print("embedded Tailscale certificate request failed; see nanoctl setup docs")
						}
					}()
				}
			}
		}
		if status.Node != nil {
			// Only persist the fields needed by the agent and CLI.
			status.Node = &ipnstate.Status{
				BackendState: status.Node.BackendState,
				AuthURL:      status.Node.AuthURL,
				Self:         status.Node.Self,
				User:         status.Node.User,
			}
		}
		if err := writeSnapshot(path, status); err != nil {
			return err
		}
		select {
		case <-ctx.Done():
			return nil
		case err := <-webExit:
			return err
		case <-tick.C:
		}
	}
}

func reply(w http.ResponseWriter, result any, err error) {
	if err != nil {
		http.Error(w, "Tailscale request failed", http.StatusBadGateway)
		return
	}
	w.Header().Set("Content-Type", "application/json")
	_ = json.NewEncoder(w).Encode(result)
}

func proxy(port uint16, token string) http.Handler {
	target := &url.URL{Scheme: "http", Host: fmt.Sprintf("127.0.0.1:%d", port)}
	proxy := &httputil.ReverseProxy{
		Transport: &http.Transport{ResponseHeaderTimeout: 10 * time.Second},
		Rewrite: func(req *httputil.ProxyRequest) {
			req.SetURL(target)
			req.Out.Host = req.In.Host
			req.Out.Header.Set(bridgeHeader, token)
			req.Out.Header.Set(peerHeader, req.In.RemoteAddr)
		},
	}
	return http.HandlerFunc(func(w http.ResponseWriter, req *http.Request) {
		peer, err := netip.ParseAddrPort(req.RemoteAddr)
		if err != nil || !tsaddr.IsTailscaleIP(peer.Addr()) {
			http.Error(w, "not a tailnet peer", http.StatusForbidden)
			return
		}
		proxy.ServeHTTP(w, req)
	})
}

func writeSnapshot(path string, state snapshot) error {
	data, err := json.Marshal(state)
	if err != nil {
		return err
	}
	tmp := path + ".tmp"
	if err := os.WriteFile(tmp, data, 0600); err != nil {
		return err
	}
	return os.Rename(tmp, path)
}

// A frame is token SP address LF payload. It stays on loopback and is never
// exposed to a tailnet peer. Keep the payload as UDP (no head-of-line blocking).
func pack(dst []byte, token, address string, packet []byte) []byte {
	dst = append(dst[:0], token...)
	dst = append(dst, ' ')
	dst = append(dst, address...)
	dst = append(dst, '\n')
	return append(dst, packet...)
}

func unpack(frame []byte, token string) (netip.AddrPort, []byte, bool) {
	header, packet, ok := bytes.Cut(frame, []byte{'\n'})
	if !ok || len(header) <= len(token) || header[len(token)] != ' ' ||
		subtle.ConstantTimeCompare(header[:len(token)], []byte(token)) != 1 ||
		len(packet) > maxDatagram {
		return netip.AddrPort{}, nil, false
	}
	addr, err := netip.ParseAddrPort(string(header[len(token)+1:]))
	return addr, packet, err == nil && addr.Addr().Is4() && tsaddr.IsTailscaleIP(addr.Addr())
}

type packetRelay struct {
	addr   netip.AddrPort
	packet net.PacketConn
	local  *net.UDPConn
	done   sync.WaitGroup
}

func startRelay(packet net.PacketConn, local *net.UDPConn, addr netip.AddrPort, rust *net.UDPAddr, token string) *packetRelay {
	relay := &packetRelay{addr: addr, packet: packet, local: local}
	relay.done.Add(2)
	local.SetReadDeadline(time.Time{})
	go func() {
		defer relay.done.Done()
		var buf [65535]byte
		var frame [maxDatagram + 128]byte
		for {
			n, from, err := packet.ReadFrom(buf[:])
			if err != nil {
				return
			}
			if n <= maxDatagram {
				_, _ = local.WriteToUDP(pack(frame[:0], token, from.String(), buf[:n]), rust)
			}
		}
	}()
	go func() {
		defer relay.done.Done()
		var buf [65535]byte
		expected := netip.AddrPortFrom(rust.AddrPort().Addr().Unmap(), rust.AddrPort().Port())
		for {
			n, from, err := local.ReadFromUDPAddrPort(buf[:])
			if err != nil {
				return
			}
			if from != expected {
				continue
			}
			if peer, packetData, ok := unpack(buf[:n], token); ok {
				_, _ = packet.WriteTo(packetData, net.UDPAddrFromAddrPort(peer))
			}
		}
	}()
	return relay
}

func (r *packetRelay) close() {
	_ = r.local.SetReadDeadline(time.Now())
	_ = r.packet.Close()
	r.done.Wait()
}

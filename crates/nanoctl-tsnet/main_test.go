package main

import (
	"context"
	"encoding/json"
	"net"
	"net/http"
	"net/http/httptest"
	"net/netip"
	"net/url"
	"os"
	"path/filepath"
	"strings"
	"testing"
	"time"

	"tailscale.com/net/netns"
	"tailscale.com/tailcfg"
	"tailscale.com/tsnet"
	"tailscale.com/tstest/integration"
	"tailscale.com/tstest/integration/testcontrol"
	"tailscale.com/types/logger"
)

func TestEmbeddedNodeCarriesHTTPIdentityAndUDP(t *testing.T) {
	t.Setenv("TS_NO_LOGS_NO_SUPPORT", "true")
	netns.SetEnabled(false)
	defer netns.SetEnabled(true)
	ctx, cancel := context.WithTimeout(t.Context(), 35*time.Second)
	defer cancel()
	control := &testcontrol.Server{
		DERPMap:        integration.RunDERPAndSTUN(t, logger.Discard, "127.0.0.1"),
		DNSConfig:      &tailcfg.DNSConfig{Proxied: true},
		MagicDNSDomain: "mock-tailnet.ts.net",
	}
	control.HTTPTestServer = httptest.NewServer(control)
	defer control.HTTPTestServer.Close()
	newNode := func(name string) *tsnet.Server {
		return &tsnet.Server{
			Dir:        t.TempDir(),
			Hostname:   name,
			ControlURL: control.HTTPTestServer.URL,
			Logf:       logger.Discard,
			UserLogf:   logger.Discard,
		}
	}

	const token = "12345678901234567890123456789012"
	type forwarded struct{ Token, Peer, Host string }
	local := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, req *http.Request) {
		_ = json.NewEncoder(w).Encode(forwarded{req.Header.Get(bridgeHeader), req.Header.Get(peerHeader), req.Host})
	}))
	defer local.Close()
	udp, err := net.ListenUDP("udp4", &net.UDPAddr{IP: net.IPv4(127, 0, 0, 1)})
	if err != nil {
		t.Fatal(err)
	}
	defer udp.Close()
	host := newNode("nanoctl")
	cfg := settings{
		StateDir: host.Dir,
		Web:      uint16(local.Listener.Addr().(*net.TCPAddr).Port),
		UDP:      uint16(udp.LocalAddr().(*net.UDPAddr).Port),
		Port:     7337,
		Token:    token,
	}
	addr := make(chan string, 1)
	done := make(chan error, 1)
	go func() { done <- run(ctx, cfg, host, func(address string) { addr <- address }) }()
	defer func() {
		cancel()
		select {
		case err := <-done:
			if err != nil {
				t.Error(err)
			}
		case <-time.After(5 * time.Second):
			t.Error("helper did not stop")
		}
	}()
	var localAPI string
	select {
	case localAPI = <-addr:
	case <-ctx.Done():
		t.Fatal("helper did not announce readiness")
	}
	var status snapshot
	path := filepath.Join(cfg.StateDir, "viewer-status.json")
	for {
		if data, err := os.ReadFile(path); err == nil && json.Unmarshal(data, &status) == nil && status.Ready {
			break
		}
		select {
		case <-ctx.Done():
			t.Fatalf("host never joined the mock tailnet: %+v", status)
		case <-time.After(50 * time.Millisecond):
		}
	}
	if status.Node.User[status.Node.Self.UserID].LoginName == "" {
		t.Fatal("status did not report the host's owner")
	}

	viewer := newNode("viewer")
	defer viewer.Close()
	viewerStatus, err := viewer.Up(ctx)
	if err != nil {
		t.Fatal(err)
	}
	viewerIP, _ := viewer.TailscaleIPs()
	transport := &http.Transport{DialContext: viewer.Dial}
	defer transport.CloseIdleConnections()
	web := &http.Client{Transport: transport}
	req, err := http.NewRequestWithContext(ctx, "GET", "http://"+status.Media+"/v1/info", nil)
	if err != nil {
		t.Fatal(err)
	}
	req.Header.Set(bridgeHeader, "forged")
	req.Header.Set(peerHeader, "100.100.100.100:42")
	req.Host = "nanoctl.mock-tailnet.ts.net:7337"
	res, err := web.Do(req)
	if err != nil {
		t.Fatal(err)
	}
	defer res.Body.Close()
	var got forwarded
	if err := json.NewDecoder(res.Body).Decode(&got); err != nil {
		t.Fatal(err)
	}
	peer, err := netip.ParseAddrPort(got.Peer)
	if err != nil || peer.Addr() != viewerIP || got.Token != token || got.Host != req.Host {
		t.Fatalf("proxy did not preserve actual identity and host: %+v", got)
	}
	whois, err := http.NewRequestWithContext(ctx, "GET", "http://"+localAPI+"/whois?peer="+url.QueryEscape(got.Peer), nil)
	if err != nil {
		t.Fatal(err)
	}
	localClient := &http.Client{Transport: &http.Transport{}}
	defer localClient.CloseIdleConnections()
	response, err := localClient.Do(whois)
	if err != nil {
		t.Fatal(err)
	}
	response.Body.Close()
	if response.StatusCode != http.StatusForbidden {
		t.Fatalf("unauthenticated local API: %s", response.Status)
	}
	whois.Header.Set(bridgeHeader, token)
	response, err = localClient.Do(whois)
	if err != nil {
		t.Fatal(err)
	}
	defer response.Body.Close()
	var identity struct{ UserProfile tailcfg.UserProfile }
	if err := json.NewDecoder(response.Body).Decode(&identity); err != nil ||
		identity.UserProfile.LoginName != viewerStatus.User[viewerStatus.Self.UserID].LoginName {
		t.Fatalf("unexpected tailnet identity: %+v (%v)", identity, err)
	}

	media, err := viewer.Dial(ctx, "udp4", status.Media)
	if err != nil {
		t.Fatal(err)
	}
	defer media.Close()
	deadline, _ := ctx.Deadline()
	_ = media.SetDeadline(deadline)
	_ = udp.SetDeadline(deadline)
	if _, err := media.Write([]byte("from viewer")); err != nil {
		t.Fatal(err)
	}
	buf := make([]byte, 512)
	n, bridge, err := udp.ReadFromUDP(buf)
	if err != nil {
		t.Fatal(err)
	}
	source, packet, ok := unpack(buf[:n], token)
	if !ok || source.Addr() != viewerIP || string(packet) != "from viewer" || bridge.Port != int(status.Relay) {
		t.Fatalf("unexpected UDP frame %q from %s", buf[:n], bridge)
	}
	if _, err := udp.WriteToUDP(pack(nil, token, source.String(), []byte("from agent")), bridge); err != nil {
		t.Fatal(err)
	}
	n, err = media.Read(buf)
	if err != nil || string(buf[:n]) != "from agent" {
		t.Fatalf("UDP return path: %q (%v)", buf[:n], err)
	}
}

func TestProxyRejectsNonTailnetAddresses(t *testing.T) {
	req := httptest.NewRequest("GET", "/v1/info", nil)
	req.RemoteAddr = "192.168.1.2:7337"
	w := httptest.NewRecorder()
	proxy(7337, "token").ServeHTTP(w, req)
	if w.Code != http.StatusForbidden {
		t.Fatalf("got %d", w.Code)
	}
}

func TestUDPFrameRejectsInvalidTokenOrDestination(t *testing.T) {
	for _, frame := range []string{
		"forged 100.100.0.1:123\ndata",
		"secret 127.0.0.1:123\ndata",
		"secret [fd7a:115c:a1e0::123]:123\ndata",
		"secret 100.100.0.1:123\n" + strings.Repeat(".", maxDatagram+1),
	} {
		if _, _, ok := unpack([]byte(frame), "secret"); ok {
			t.Fatal("accepted an invalid UDP frame")
		}
	}
	frame := pack(nil, "secret", "100.100.0.1:123", []byte("binary\x00\n"))
	addr, packet, ok := unpack(frame, "secret")
	if !ok || addr.String() != "100.100.0.1:123" || string(packet) != "binary\x00\n" {
		t.Fatal("UDP frame did not preserve its payload")
	}
}

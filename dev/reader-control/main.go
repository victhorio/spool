// Credential-free control plane for isolated container validation, never deployment.
package main

import (
	"bytes"
	"encoding/json"
	"io"
	"log"
	"net"
	"net/http"
	"net/http/httptest"

	"tailscale.com/derp/derpserver"
	"tailscale.com/tailcfg"
	"tailscale.com/tstest/integration/testcontrol"
	"tailscale.com/types/key"
	"tailscale.com/types/logger"
)

func main() {
	// testcontrol sends this default on every map update, not just the first.
	tailcfg.FilterAllowAll = []tailcfg.FilterRule{{
		SrcIPs: []string{"*"}, IPProto: []int{6},
		DstPorts: []tailcfg.NetPortRange{{
			IP: "*", Ports: tailcfg.PortRange{First: 8090, Last: 8090},
		}},
	}}
	relay := derpserver.New(key.NewNode(), logger.Discard)
	listener, err := net.Listen("tcp", ":8443")
	if err != nil {
		log.Fatal(err)
	}
	tls := httptest.NewUnstartedServer(derpserver.Handler(relay))
	_ = tls.Listener.Close()
	tls.Listener = listener
	tls.StartTLS()
	defer tls.Close()
	defer relay.Close()

	control := &testcontrol.Server{
		ExplicitBaseURL: "http://control:8080",
		DERPMap: &tailcfg.DERPMap{Regions: map[tailcfg.DERPRegionID]*tailcfg.DERPRegion{
			1: {
				RegionID: 1, RegionCode: "local",
				Nodes: []*tailcfg.DERPNode{{
					Name: "local", RegionID: 1, HostName: "control",
					IPv6: "none", STUNPort: -1, DERPPort: 8443, InsecureForTests: true,
				}},
			},
		}},
		Logf: logger.Discard,
	}
	// RequireAuthKey also rejects persisted nodes; require a key only for enrollment.
	control.MaybeRejectRequest = func(r *http.Request) (int, string, string) {
		if r.URL.Path != "/machine/register" {
			return 0, "", ""
		}
		body, err := io.ReadAll(io.LimitReader(r.Body, 1<<20))
		r.Body = io.NopCloser(bytes.NewReader(body))
		var request tailcfg.RegisterRequest
		if err != nil || json.Unmarshal(body, &request) != nil {
			return 400, "", "invalid registration"
		}
		if control.Node(request.NodeKey) == nil &&
			(request.Auth == nil || request.Auth.AuthKey != "synthetic-reader-enrollment-only") {
			return 200, "", `{"Error":"invalid authkey"}`
		}
		return 0, "", ""
	}
	mux := http.NewServeMux()
	mux.HandleFunc("/healthz", func(w http.ResponseWriter, _ *http.Request) { w.WriteHeader(200) })
	mux.HandleFunc("/log/", func(w http.ResponseWriter, r *http.Request) {
		_, _ = io.Copy(io.Discard, r.Body)
		w.WriteHeader(200)
	})
	mux.Handle("/", control)
	log.Fatal(http.ListenAndServe(":8080", mux))
}

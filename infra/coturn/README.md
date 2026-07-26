# coturn production deployment

The container is pinned to the official multi-architecture coturn image by digest and runs as
`nobody` with a read-only root filesystem. Its entrypoint reads the REST auth secret from a mounted
file and writes a mode-0600 runtime configuration into tmpfs, so the secret is absent from the image,
environment, and process arguments.

Each relay needs a stable public IP with one-to-one port mapping. Copy `.env.example` to `.env`, set
the DNS realm, public/external IP, and the host's local listener/relay IP. On a directly addressed
host all three IP values are identical; behind one-to-one NAT, `TURN_EXTERNAL_IP` is
`public/private` and the other two are the private address. Then place these root-readable files in
`secrets/`:

- `turn_auth_secret`: at least 32 random characters, with no trailing blank line;
- `turn_fullchain.pem`: the certificate chain for the realm;
- `turn_privkey.pem`: its private key.

Start with `docker compose up -d --build`, then require a healthy container before adding it to DNS.
The host firewall must allow UDP and TCP 3478, TCP 443 and 5349, and UDP 49152–65535. Port 9641 is a
Prometheus/health endpoint bound to loopback for a host-local collector. The compose file
uses host networking because TURN relay ports require stable one-to-one mappings; it is intended for
a dedicated Linux relay host, not a shared application server.

Place instances in at least two regions and publish both in `TURN_URLS`. Do not put coturn behind an
ordinary HTTP reverse proxy or a load balancer that rewrites relay ports. Drain an instance by
removing it from `TURN_URLS` and DNS, waiting longer than the five-minute credential lifetime plus
the maximum session lifetime, and only then stopping it.

The shared secret must never appear in the web bundle or agent. A trusted control-plane endpoint
mints credentials using the TURN REST convention:

```text
username = "<unix-expiry>:<session-id>"
password = base64(HMAC-SHA1(turn-secret, username))
```

Use a five-minute expiry and authorize issuance only for a live owner/device session. Rotate by
adding a new relay pool/secret, publishing both pools during a bounded overlap, and then draining the
old pool. The denied peer ranges prevent TURN from becoming an SSRF path to private services; adjust
only with a documented network review.

Before production, verify all three paths from outside the relay network:

```sh
turnutils_stunclient -p 3478 turn.example.com
turnutils_uclient -t -S -p 5349 -u "$TURN_USERNAME" -w "$TURN_PASSWORD" turn.example.com
turnutils_uclient -t -S -p 443 -u "$TURN_USERNAME" -w "$TURN_PASSWORD" turn.example.com
```

The final release gate is a browser-to-native session with ICE transport policy `relay`, not merely a
successful STUN binding or allocation.

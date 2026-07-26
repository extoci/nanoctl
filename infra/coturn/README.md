# coturn reference deployment

The configuration is a security baseline, not a complete deployment. Set the public relay/listener
addresses appropriate to the infrastructure, mount certificates and the REST auth secret from a
secret manager, open the listener and relay port ranges, and place instances in at least two
regions.

The shared secret must never appear in the web bundle or agent. A trusted control-plane endpoint
mints credentials using the TURN REST convention:

```text
username = "<unix-expiry>:<session-id>"
password = base64(HMAC-SHA1(turn-secret, username))
```

Use a five-minute expiry and authorize issuance only for a live owner/device session. Rotate by
running old and new secrets during a bounded overlap. The denied peer ranges prevent TURN from
becoming an SSRF path to private services; adjust only with a documented network review.

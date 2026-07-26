# ADR 0003: exact-byte signed agent updates

## Decision

An update manifest is a small JSON envelope containing base64 `payload` and `signature` fields. The
payload is the exact UTF-8 JSON byte sequence signed with Ed25519. Verifiers authenticate those
bytes before parsing them, avoiding ambiguous JSON canonicalization.

The signed payload has a format version, semantic agent version, publication and expiry timestamps,
and one bounded artifact per OS/architecture target. Each artifact binds a credential-free HTTPS
URL, exact byte length, and SHA-256 digest. Agents reject unknown fields, expired or future
manifests, validity windows over 31 days, downgrade/equal versions, duplicate targets, oversized
artifacts, and targets other than their compiled OS and architecture.

Publisher public keys are trust anchors supplied by the signed installer or an explicitly managed
advanced configuration. They are never fetched from the manifest location and private keys never
enter the repository or agent.

## Activation and rollback

The downloader writes only within a private staging directory, enforces both advertised and global
size bounds while streaming, verifies the digest before making a file executable, and fsyncs the
file and directory. Activation retains the previous signed binary, uses same-filesystem atomic
renames, and records a pending health check before restarting.

The new process must pass local startup and control-plane health before the pending record is
committed. Failure or a bounded restart loop restores the retained binary atomically. Windows uses
a separately copied, signed bootstrap helper because an executing image cannot replace itself.
Update activation never changes enrollment credentials or configuration.

## Consequences

Provenance attestations and platform publisher signatures remain independent requirements. A valid
manifest proves authorization by the update publisher; it does not replace Authenticode, Apple
notarization, package-manager verification, the physical release matrix, or rollback testing.

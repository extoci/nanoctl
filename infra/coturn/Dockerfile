FROM coturn/coturn:4.14.0-r0@sha256:0c0e8fc0c263b85a134e9e4242b5e46e1f4c077c5029633511191c05b5c2c814

COPY --chown=65534:65534 turnserver.conf /etc/nanoctl/turnserver.conf
COPY --chown=65534:65534 entrypoint.sh /usr/local/bin/nanoctl-turn-entrypoint

USER 65534:65534
ENTRYPOINT ["/usr/local/bin/nanoctl-turn-entrypoint"]

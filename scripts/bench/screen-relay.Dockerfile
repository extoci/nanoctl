FROM node:24-bookworm-slim AS node
FROM ubuntu:24.04

ENV DEBIAN_FRONTEND=noninteractive
COPY --from=node /usr/local/ /usr/local/
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates procps fonts-liberation \
    && npm install -g screen-relay@0.1.2 \
    && screen-relay install --backend x11 --desktop none --yes \
    && rm -rf /root/.npm /var/lib/apt/lists/*

CMD ["sleep", "infinity"]

# deploy/cliproxy/Dockerfile — adi-cliproxy Fly app.
#
# Wraps upstream CLIProxyAPI image with tailscaled so this proxy joins the
# adi tailnet. adi-zeroclaw-shane and adi-zeroclaw-meg reach it over
# Tailscale at http://adi-cliproxy.<tailnet>.ts.net:8317.

FROM eceasy/cli-proxy-api:v6.9.31 AS cliproxy

FROM debian:bookworm-slim

ARG TAILSCALE_VERSION=1.80.2

RUN apt-get update \
    && apt-get install -y --no-install-recommends \
        ca-certificates curl iptables iproute2 procps tini \
    && curl -fsSL "https://pkgs.tailscale.com/stable/tailscale_${TAILSCALE_VERSION}_amd64.tgz" -o /tmp/tailscale.tgz \
    && tar -xzf /tmp/tailscale.tgz -C /tmp \
    && install -m 0755 "/tmp/tailscale_${TAILSCALE_VERSION}_amd64/tailscaled" /usr/local/bin/tailscaled \
    && install -m 0755 "/tmp/tailscale_${TAILSCALE_VERSION}_amd64/tailscale"  /usr/local/bin/tailscale \
    && rm -rf /tmp/tailscale* /var/lib/apt/lists/*

COPY --from=cliproxy /CLIProxyAPI/CLIProxyAPI /usr/local/bin/cliproxy

COPY config.seed.yaml /opt/cliproxy.config.seed.yaml
COPY entrypoint.sh   /entrypoint.sh
RUN chmod 0755 /entrypoint.sh

ENTRYPOINT ["/usr/bin/tini", "--", "/entrypoint.sh"]

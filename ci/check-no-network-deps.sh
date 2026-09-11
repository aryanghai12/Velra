#!/usr/bin/env bash
# J3: no network-capable crate may be linked into the hook path.
#
# Velra never makes a network request at runtime. This check fails the build if
# an HTTP client, TLS stack, DNS resolver or socket library appears anywhere in
# the normal (non-dev, non-build) dependency graph.

set -euo pipefail

DENY=(
  reqwest hyper hyper-util h2 curl curl-sys ureq isahc surf attohttpc minreq
  native-tls openssl openssl-sys rustls rustls-pki-types tokio-rustls
  async-std tokio mio socket2 tungstenite tokio-tungstenite
  trust-dns-resolver hickory-resolver quinn ws2_32-sys
)

echo "Resolving the normal dependency graph..."
tree="$(cargo tree --workspace -e normal --prefix none --no-dedupe 2>/dev/null | awk '{print $1}' | sort -u)"

failed=0
for crate in "${DENY[@]}"; do
  if grep -qx "$crate" <<<"$tree"; then
    echo "::error::network-capable crate '$crate' is in the runtime dependency graph"
    failed=1
  fi
done

if [ "$failed" -eq 0 ]; then
  echo "OK: no network-capable crate in the runtime dependency graph."
  echo "Runtime dependencies:"
  echo "$tree" | sed 's/^/  /'
fi

exit "$failed"

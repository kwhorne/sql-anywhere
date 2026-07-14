#!/usr/bin/env bash
#
# Smoke-test a published sqld container image: boot it, then prove the HTTP API
# works AND that our vector-native features actually ship by building a DiskANN
# index and running a nearest-neighbour query end to end inside the container.
#
# Usage:
#   scripts/smoke-test-docker.sh [IMAGE]
#
# IMAGE defaults to ghcr.io/kwhorne/sqlanywhere-server:latest. Exits non-zero
# (failing the release) if the container does not come up or returns the wrong
# answer.
set -euo pipefail

IMAGE="${1:-ghcr.io/kwhorne/sqlanywhere-server:latest}"
PORT="${PORT:-18080}"
NAME="sqla-smoke-$$"
BASE="http://127.0.0.1:${PORT}"

cleanup() { docker rm -f "$NAME" >/dev/null 2>&1 || true; }
trap cleanup EXIT

echo "==> Smoke-testing ${IMAGE}"
docker rm -f "$NAME" >/dev/null 2>&1 || true
docker run -d --name "$NAME" -p "${PORT}:8080" "$IMAGE" >/dev/null

# Wait for the HTTP listener (up to ~30s).
echo "==> Waiting for sqld to accept HTTP…"
ready=""
for _ in $(seq 1 30); do
  if curl -fsS -o /dev/null "${BASE}/health" 2>/dev/null \
     || curl -fsS -o /dev/null -X POST "${BASE}/v2/pipeline" \
          -H 'content-type: application/json' \
          -d '{"requests":[{"type":"close"}]}' 2>/dev/null; then
    ready=1
    break
  fi
  sleep 1
done
if [ -z "$ready" ]; then
  echo "FAIL: sqld did not become ready" >&2
  docker logs "$NAME" 2>&1 | tail -20 >&2
  exit 1
fi

# Build a tiny vector index and query it — the whole reason SQL Anywhere exists.
read -r -d '' BODY <<'JSON' || true
{"requests":[
 {"type":"execute","stmt":{"sql":"CREATE TABLE docs(id INTEGER PRIMARY KEY, name TEXT, emb FLOAT32(4))"}},
 {"type":"execute","stmt":{"sql":"INSERT INTO docs VALUES (1,'cat',vector32('[1,0,0,0]')),(2,'dog',vector32('[0.9,0.1,0,0]')),(3,'car',vector32('[0,0,1,0]'))"}},
 {"type":"execute","stmt":{"sql":"CREATE INDEX docs_vec ON docs(sqlanywhere_vector_idx(emb,'metric=cosine'))"}},
 {"type":"execute","stmt":{"sql":"SELECT d.name FROM vector_top_k('docs_vec',vector32('[1,0,0,0]'),2) k JOIN docs d ON d.id=k.id"}},
 {"type":"close"}]}
JSON

echo "==> Running vector search…"
resp="$(curl -fsS -X POST "${BASE}/v2/pipeline" -H 'content-type: application/json' -d "$BODY")"

# Extract the two returned names, whitespace-normalized. Prefer jq, fall back to python3.
if command -v jq >/dev/null 2>&1; then
  got="$(printf '%s' "$resp" | jq -r '[.results[3].response.result.rows[][0].value] | join(",")')"
else
  got="$(printf '%s' "$resp" | python3 -c 'import sys,json;r=json.load(sys.stdin)["results"][3]["response"]["result"]["rows"];print(",".join(x[0]["value"] for x in r))')"
fi

echo "    nearest to [1,0,0,0]: ${got}"
if [ "$got" != "cat,dog" ]; then
  echo "FAIL: expected 'cat,dog', got '${got}'" >&2
  echo "Full response: ${resp}" >&2
  exit 1
fi

echo "PASS: ${IMAGE} boots and serves vector search."

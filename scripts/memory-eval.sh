#!/usr/bin/env bash
# Runs the agent memory eval (crates/tinymemory-integrations/examples/memory_eval)
# against a throwaway CortexDB from integration/cortexdb/, then tears it down.
# Reports land in target/memory-eval/<label>.{md,json} (OUT_DIR overrides
# the directory). See docs/evals/.
#
#   ./scripts/memory-eval.sh                 # deterministic mock models
#   MODELS=openrouter ./scripts/memory-eval.sh   # real models via OpenRouter
#   ./scripts/memory-eval.sh --llm           # extra flags go to the eval
#   KEEP=1 ./scripts/memory-eval.sh          # leave the server running
#   REUSE_IMAGE=1 ./scripts/memory-eval.sh   # use an already-built image
#   CORTEX_FLAGS_FILE=$PWD/integration/cortexdb/flags/no-graph.env \
#     ./scripts/memory-eval.sh               # one CortexDB flag profile
#
# To compare flag profiles, use scripts/memory-flag-sweep.sh.
#
# MODELS=openrouter needs OPENROUTER_API_KEY. It sends the eval's synthetic
# fixtures to OpenRouter for embeddings, extraction and answers. It is not
# cheap: CortexDB runs its extraction model over every stored event, again
# during belief builds, so a full run makes many model calls. The report ends
# with the usage CortexDB accounted for. Run one scenario first (--only) to
# gauge the cost, and mind the key's spending limit: once it is hit, every
# embedding fails and writes never become visible. Override the models with
# CORTEX_EMBEDDING_MODEL, CORTEX_EXTRACTION_MODEL and CORTEX_ANSWER_MODEL.

set -euo pipefail

root="$(cd "$(dirname "$0")/.." && pwd)"
models="${MODELS:-mock}"
port="${CORTEXDB_PORT:-3145}"
label="${LABEL:-cortex-$models}"
out="${OUT_DIR:-$root/target/memory-eval}"
mkdir -p "$out"
export CORTEXDB_PORT="$port"
url="http://127.0.0.1:$port"
compose=(docker compose --project-name "tinymemory-eval-$models-$port" -f "$root/integration/cortexdb/docker-compose.yml")

case "$models" in
  mock) ;;
  openrouter)
    : "${OPENROUTER_API_KEY:?MODELS=openrouter needs OPENROUTER_API_KEY}"
    export CORTEX_INFERENCE_URL="https://openrouter.ai/api/v1"
    export CORTEX_INFERENCE_KEY="$OPENROUTER_API_KEY"
    export CORTEX_EMBEDDING_MODEL="${CORTEX_EMBEDDING_MODEL:-openai/text-embedding-3-large}"
    export CORTEX_EXTRACTION_MODEL="${CORTEX_EXTRACTION_MODEL:-openai/gpt-4.1-mini}"
    export CORTEX_ENRICHMENT_MODEL="${CORTEX_ENRICHMENT_MODEL:-$CORTEX_EXTRACTION_MODEL}"
    export CORTEX_ANSWER_MODEL="${CORTEX_ANSWER_MODEL:-openai/gpt-4.1-mini}"
    export CORTEX_VERIFIER_MODEL="${CORTEX_VERIFIER_MODEL:-$CORTEX_ANSWER_MODEL}"
    ;;
  *)
    echo "MODELS must be mock or openrouter, not $models" >&2
    exit 1
    ;;
esac

# A shared budget file keeps a sequence of live runs below one spending cap.
# The OpenRouter key's usage is cumulative, so unrelated concurrent spend is
# counted conservatively too. Refuse a run when there is less than the
# expected single-run reserve left.
if [ "$models" = openrouter ] && [ -n "${BUDGET_USD:-}" ]; then
  usage=""
  for _ in 1 2 3 4 5; do
    response="$(curl --fail --silent --max-time 10 https://openrouter.ai/api/v1/key \
      -H "Authorization: Bearer $OPENROUTER_API_KEY" || true)"
    usage="$(printf '%s' "$response" | python3 -c \
      'import json,sys; print(json.load(sys.stdin)["data"]["usage"])' 2>/dev/null || true)"
    [ -n "$usage" ] && break
    sleep 2
  done
  if [ -z "$usage" ]; then
    echo "could not read OpenRouter usage for the live budget" >&2
    exit 1
  fi
  start_file="$out/live-budget-start"
  if [ ! -f "$start_file" ]; then
    printf '%s\n' "$usage" > "$start_file"
  fi
  start="$(cat "$start_file")"
  if ! awk -v now="$usage" -v start="$start" -v cap="$BUDGET_USD" \
    -v reserve="${COST_PER_RUN:-0.5}" \
    'BEGIN { exit !((now - start + reserve) <= cap) }'; then
    echo "live eval budget exhausted: spent since start plus reserve exceeds \$$BUDGET_USD" >&2
    exit 1
  fi
fi

# Like cortexdb-live.sh: never reuse or replace a server someone else runs.
if curl --silent --max-time 2 "$url/v1/admin/health" >/dev/null 2>&1; then
  echo "something already serves $url; pick a free CORTEXDB_PORT" >&2
  exit 1
fi

cleanup() {
  result=$?
  if [ -z "${KEEP:-}" ]; then
    "${compose[@]}" down --volumes --remove-orphans >/dev/null 2>&1 || true
  fi
  exit "$result"
}
trap cleanup EXIT

if [ -n "${REUSE_IMAGE:-}" ]; then
  "${compose[@]}" up -d --no-build --wait mock-inference >/dev/null
else
  "${compose[@]}" up -d --build --wait mock-inference >/dev/null
fi
"${compose[@]}" up -d cortex >/dev/null
for _ in $(seq 1 120); do
  if curl --fail --silent "$url/v1/admin/ready" >/dev/null; then
    break
  fi
  sleep 1
done
curl --fail --silent "$url/v1/admin/ready" >/dev/null || {
  echo "CortexDB did not become ready at $url" >&2
  exit 1
}
echo "CortexDB $(curl --silent "$url/v1/admin/health") at $url, models: $models"

CORTEX_DB_URL="$url" CORTEX_DB_KEY="${TINYMEMORY_TEST_CORTEX_KEY:-tinymemory-cortex-test}" \
  cargo run --quiet -p tinymemory-integrations --features full --example memory_eval -- \
  --label "$label" --json "$out/$label.json" "$@" |
  tee "$out/$label.md"

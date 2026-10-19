#!/usr/bin/env bash
# Runs the agent memory eval once per CortexDB flag profile
# (integration/cortexdb/flags/*.env), each against its own throwaway server,
# then compares the runs' KPIs against the baseline profile.
# See docs/evals/cortex-flags.md.
#
#   ./scripts/memory-flag-sweep.sh                      # every profile, mock models
#   ./scripts/memory-flag-sweep.sh baseline no-graph    # just these
#   MODELS=openrouter ./scripts/memory-flag-sweep.sh -- --llm
#   REPEAT=2 PARALLEL=4 ./scripts/memory-flag-sweep.sh  # repeats show the noise
#   ./scripts/memory-flag-sweep.sh -- --only conflicts,surprise
#
# Arguments before `--` name profiles; after it, they go to the eval.
# MODELS, CORTEXDB_IMAGE and the model overrides pass through to
# memory-eval.sh. PARALLEL servers run at once, on ports from BASE_PORT up.
# Reports, logs and summary.md land in OUT_DIR (target/memory-eval/flags).
#
# MODELS=openrouter spends money on every profile: measure the baseline's
# cost first (its report ends with it) and multiply. Before starting, the
# sweep asks OpenRouter what the key has left and refuses when it cannot
# cover COST_PER_RUN (default $0.50, the baseline with --llm on v0.10.4) for
# every run: a key that runs dry mid-sweep answers 403, embeddings stop, and
# every later run fails on its writes. FORCE=1 skips the check.

set -euo pipefail

root="$(cd "$(dirname "$0")/.." && pwd)"
flags="$root/integration/cortexdb/flags"
repeat="${REPEAT:-1}"
parallel="${PARALLEL:-1}"
base_port="${BASE_PORT:-3150}"
out="${OUT_DIR:-$root/target/memory-eval/flags}"

profiles=()
while [ $# -gt 0 ] && [ "$1" != "--" ]; do
  profiles+=("$1")
  shift
done
[ "${1:-}" = "--" ] && shift
if [ ${#profiles[@]} -eq 0 ]; then
  for file in "$flags"/*.env; do
    profiles+=("$(basename "$file" .env)")
  done
fi

# Every run the sweep will make, skipped profiles left out.
planned=0
for profile in "${profiles[@]}"; do
  needs="$(sed -n 's/^# requires: *//p' "$flags/$profile.env" 2>/dev/null)"
  if [ -z "$needs" ] || [ -n "${!needs:-}" ]; then
    planned=$((planned + repeat))
  fi
done
if [ "${MODELS:-mock}" = openrouter ] && [ -z "${FORCE:-}" ]; then
  : "${OPENROUTER_API_KEY:?MODELS=openrouter needs OPENROUTER_API_KEY}"
  left="$(curl --silent --max-time 10 https://openrouter.ai/api/v1/key \
    -H "Authorization: Bearer $OPENROUTER_API_KEY" |
    sed -n 's/.*"limit_remaining":\([0-9.]*\).*/\1/p')"
  need="$(awk -v n="$planned" -v c="${COST_PER_RUN:-0.5}" 'BEGIN { printf "%.2f", n * c }')"
  # An empty answer means the key has no limit (or the check failed).
  if [ -n "$left" ] && awk -v l="$left" -v n="$need" 'BEGIN { exit !(l < n) }'; then
    echo "the key has \$$left left; $planned runs need about \$$need (COST_PER_RUN" \
      "${COST_PER_RUN:-0.5}). Run fewer profiles, wait for the limit to reset, or set FORCE=1." >&2
    exit 1
  fi
  echo "key has \$${left:-unlimited} left; $planned runs need about \$$need"
fi

mkdir -p "$out"
# Build once, so parallel runs do not queue on cargo's lock.
cargo build --quiet -p tinymemory-integrations --features full --example memory_eval

# Runs `profile`, repeat `n`, on `port`.
run_one() {
  local profile="$1" n="$2" port="$3"
  shift 3
  local label="flags-$profile-r$n"
  (
    # Sourced as well as passed to Compose: the profile may set what the
    # compose file interpolates (CORTEX_TOML, CORTEX_VERIFIER_URL).
    set -a
    # shellcheck disable=SC1090
    . "$flags/$profile.env"
    set +a
    CORTEX_FLAGS_FILE="$flags/$profile.env" CORTEXDB_PORT="$port" LABEL="$label" \
      OUT_DIR="$out" "$root/scripts/memory-eval.sh" "$@"
  ) >"$out/$label.log" 2>&1
}

# The next port nothing answers on, from `$1` up.
free_port() {
  local port="$1"
  while curl --silent --max-time 1 "http://127.0.0.1:$port/" >/dev/null 2>&1 ||
    (exec 3<>"/dev/tcp/127.0.0.1/$port") 2>/dev/null; do
    port=$((port + 1))
  done
  echo "$port"
}

jobs_run=()
port="$base_port"
running=0
for profile in "${profiles[@]}"; do
  file="$flags/$profile.env"
  if [ ! -f "$file" ]; then
    echo "no profile $profile in $flags" >&2
    exit 1
  fi
  needs="$(sed -n 's/^# requires: *//p' "$file")"
  if [ -n "$needs" ] && [ -z "${!needs:-}" ]; then
    echo "skip $profile: needs $needs"
    continue
  fi
  for n in $(seq 1 "$repeat"); do
    label="flags-$profile-r$n"
    rm -f "$out/$label.json"
    port="$(free_port "$port")"
    echo "run  $label on :$port"
    (run_one "$profile" "$n" "$port" "$@" && echo "done $label" || {
      echo "FAIL $label (see $out/$label.log)"
      exit 1
    }) &
    jobs_run+=("$label")
    port=$((port + 1))
    running=$((running + 1))
    if [ "$running" -ge "$parallel" ]; then
      wait -n || true
      running=$((running - 1))
    fi
  done
done
while [ "$running" -gt 0 ]; do
  wait -n || true
  running=$((running - 1))
done

# A run failed when it left no report.
reports=()
failed=()
for label in "${jobs_run[@]}"; do
  if [ -f "$out/$label.json" ]; then
    reports+=("$out/$label.json")
  else
    failed+=("$label")
  fi
done
if [ ${#reports[@]} -eq 0 ]; then
  echo "no run finished; see the logs in $out" >&2
  exit 1
fi
cargo run --quiet -p tinymemory-integrations --features full --example memory_eval -- \
  compare "${reports[@]}" | tee "$out/summary.md"
echo
echo "wrote $out/summary.md"
if [ ${#failed[@]} -gt 0 ]; then
  echo "failed: ${failed[*]}; see their logs in $out" >&2
  exit 1
fi

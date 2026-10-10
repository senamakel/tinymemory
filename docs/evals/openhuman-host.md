# OpenHuman host profile

`memory_eval --host openhuman` replays the existing scripted scenarios through
the parts of OpenHuman's turn hook that affect the hot path. It uses the same
engine and probes as the default profile, so pack accuracy can be compared
without changing the stored fixture data. It is a mirror inside TinyMemory;
there is no dependency on `openhuman-core`.

The source references below refer to OpenHuman commit `cf16716f4f`. Update
them when the host's defaults or hook behavior changes.

| Behavior | Eval mirror | OpenHuman source |
| --- | --- | --- |
| 1200-token budget, 8 learnings, 6 brain, 6 history, 0 team, beliefs every 10 turns | `main.rs`, OpenHuman policy | `crates/openhuman-core/src/config/schema/memory.rs:278-336` |
| Dated pre-turn hook, ready date hint, empty pack after 1500 ms | `agent.rs`, `ScriptedAgent::user` | `crates/openhuman-core/src/memory/lifecycle/hooks.rs:225-285` |
| Timed-out task continues in the background and can still log the turn | `agent.rs`, `ScriptedAgent::flush` | `crates/openhuman-core/src/memory/lifecycle/hooks.rs:190-225` |
| User and reply indices `2n` and `2n+1` | `agent.rs`, `ScriptedAgent::user` | `crates/openhuman-core/src/memory/lifecycle/hooks.rs:245-255,414-435` |
| Tool-result lines, whitespace folding, 240-character cap | `agent.rs`, `logged_reply` | `crates/openhuman-core/src/memory/lifecycle/hooks.rs:385-406` |

The profile reports pre-turn p50, p95, p99 and timeout rate. A timeout leaves
the simulated model with an empty pack. `--loop-guard` reads its script from
`examples/memory_eval/data/loop_guard.json`, replays 500 turns, and exports
all stored items to detect any `<memory-context>` tag copied into memory. It
also reports pack size and repeated bullet-line rates for the first and last
50 turns. The command fails if any stored item contains the tag.

```sh
cargo run -p tinymemory-integrations --features full --example memory_eval -- \
  --engine reference --host openhuman --only none --loop-guard \
  --json target/memory-eval/openhuman-loop-reference.json

./scripts/memory-eval.sh --host openhuman --only tool_heavy,compaction \
  --json target/memory-eval/openhuman-cortex.json
```

The 500-turn loop guard is separate from the standard scenario run because it
adds 1000 stored turns and many belief builds. The profile currently uses the
scripted answerer and the existing Rust scenario definitions. The live model
step, memory-tool calls, persisted job queue, ingestion/import paths, JSON
scenario format for the full suite, and 8000 ms compaction deadline in issue
[#251](https://github.com/tinyhumansai/tinymemory/issues/251) remain to be
implemented. The OpenHuman scrub guard is also outside this profile; the
scripted fixtures contain no sensitive data.

## Measured runs, 2026-10-10

| Run | Scope | Recall / synthesis pack hit | Synthesis model answer | Pre-turn p95 | Timeout rate | CortexDB cost |
| --- | --- | --- | --- | --- | --- | --- |
| `openhuman-cortex-mock` | `tool_heavy,compaction`, 8 probes | 8/8 · 8/8 | no answer model | 48 ms | 0/27 scripted turns | $0.000 |
| `openhuman-cortex-mock-full` | all 12 scenarios, 53 probes | 41/52 · 40/52 | no answer model | 358 ms | 0/175 calls | $0.000 |
| `openhuman-cortex-openrouter` repeat 1 | `tool_heavy`, 6 probes | 0/6 · 5/6 | 4/6 | 1502 ms | 10/19 calls | $0.050 |
| repeat 2 | same | 0/6 · 5/6 | 5/6 | 1502 ms | 11/19 calls | $0.012 |
| repeat 3 | same | 0/6 · 5/6 | 4/6 | 1508 ms | 12/19 calls | $0.030 |

The three live runs used CortexDB v0.10.4, OpenRouter embeddings and
extraction, and `openai/gpt-4.1-mini` answers. The answer model cost another
$0.002 per run. The timeout rate counts seven scripted turns and twelve
pre-turn probes across both phases. All six recall-phase probes missed the
deadline in every repeat. The 19-call sample gives a timeout range of
53–63%, well above the proposed 1% gate. The synthesis pack found five of
six expected answers each time; the absent one was the paraphrase asking who
to contact about the regression. CortexDB model cost varied from $0.012 to
$0.050, so a single run would hide substantial cost noise.

The reference-engine loop guard ran all 500 turns with 500 nonempty packs,
zero timeouts and zero stored `<memory-context>` tags. Mean pack size was
114 tokens in the first 50 turns and 130 in the last 50. Repeated bullet-line
rates were 82.4% and 85.7%; this fixture intentionally repeats the same
acknowledgement, so the rate is a measurement of that repetition rather than
a leakage count.

//! The scenarios: what gets written, then what gets asked.
//!
//! Each scenario runs below its own layout root, so scenarios never see each
//! other. A `tenant` other than [`MAIN`] gets a sibling root, to check that
//! one root never sees another's memory.
//!
//! Every probe states what a correct pack contains (`expect`), what it may
//! hold but must not prefer (`stale`, superseded values), and what it must
//! never hold (`forbidden`, another tenant's data or turns still in the
//! prompt). Probes are tagged `Lexical` when the question shares its key
//! words with the stored text, and `Paraphrase` when it does not, so keyword
//! retrieval and semantic retrieval can be told apart.

use tinymemory_api::LearningKind;
use tinymemory_tools::BrainSource;

use crate::agent::ToolStep;

/// The default tenant.
pub(crate) const MAIN: &str = "main";

/// Something written before the probes run.
pub(crate) enum Step {
    /// A seeded set of documents written in batches for the scale sweep.
    BulkDocuments {
        count: usize,
        needle_at: usize,
        seed: u64,
    },
    /// A brain document.
    Doc {
        tenant: &'static str,
        source: BrainSource,
        title: &'static str,
        text: &'static str,
    },
    /// A learning stored at the root.
    Learning {
        kind: LearningKind,
        text: &'static str,
        confidence: f32,
    },
    /// A thread of user turns, each with the tool calls the agent makes.
    Chat {
        tenant: &'static str,
        agent: &'static str,
        thread: String,
        /// Days after the run's epoch the thread starts: orders threads in
        /// time.
        day: i64,
        turns: Vec<(String, Vec<ToolStep>)>,
    },
}

/// How a probe reads memory.
#[derive(Debug, Clone)]
pub(crate) enum Via {
    /// A new thread's first `pre_turn`.
    Ask,
    /// `start_session`, resuming `thread` (or none) for `focus` (or none).
    Resume {
        thread: Option<&'static str>,
        focus: Option<&'static str>,
    },
    /// `recall_for_compaction` of `thread`, dropping `dropped`.
    Compact {
        thread: &'static str,
        dropped: Vec<String>,
    },
    /// The next `pre_turn` of `thread` at `turn_index`, with the turns from
    /// `in_prompt_from` still in the prompt.
    Continue {
        thread: &'static str,
        turn_index: u32,
        in_prompt_from: u32,
    },
    /// A `context.md` with one brief, `heading`, answering the probe's
    /// question over the whole layout: the initial context a host compiles
    /// once per session.
    ContextDoc { heading: &'static str },
}

/// Whether a question shares its key words with the stored text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Style {
    /// It does.
    Lexical,
    /// It does not: only meaning connects them.
    Paraphrase,
}

/// One question and what a correct pack holds.
#[derive(Debug, Clone)]
pub(crate) struct Probe {
    pub(crate) id: &'static str,
    pub(crate) tenant: &'static str,
    pub(crate) agent: &'static str,
    pub(crate) via: Via,
    pub(crate) question: &'static str,
    pub(crate) style: Style,
    pub(crate) expect: Vec<&'static str>,
    pub(crate) stale: Vec<&'static str>,
    pub(crate) forbidden: Vec<&'static str>,
    /// Other wordings a correct answer may use instead of every `expect`
    /// string ("Thursday" for "Thursdays"). They grade answers, not packs.
    pub(crate) accept: Vec<&'static str>,
}

impl Probe {
    fn new(id: &'static str, agent: &'static str, question: &'static str, style: Style) -> Self {
        Self {
            id,
            tenant: MAIN,
            agent,
            via: Via::Ask,
            question,
            style,
            expect: Vec::new(),
            stale: Vec::new(),
            forbidden: Vec::new(),
            accept: Vec::new(),
        }
    }

    fn expect(mut self, expect: &[&'static str]) -> Self {
        self.expect = expect.to_vec();
        self
    }

    fn stale(mut self, stale: &[&'static str]) -> Self {
        self.stale = stale.to_vec();
        self
    }

    fn accept(mut self, accept: &[&'static str]) -> Self {
        self.accept = accept.to_vec();
        self
    }

    fn forbid(mut self, forbidden: &[&'static str]) -> Self {
        self.forbidden = forbidden.to_vec();
        self
    }

    fn via(mut self, via: Via) -> Self {
        self.via = via;
        self
    }

    fn tenant(mut self, tenant: &'static str) -> Self {
        self.tenant = tenant;
        self
    }
}

/// A named scenario.
pub(crate) struct Scenario {
    pub(crate) name: &'static str,
    pub(crate) about: &'static str,
    pub(crate) steps: Vec<Step>,
    pub(crate) probes: Vec<Probe>,
}

/// Every scenario, in run order.
pub(crate) fn all() -> Vec<Scenario> {
    vec![
        brain_lookup(),
        restart_recall(),
        contradictions(),
        tool_heavy(),
        coding_session(),
        task_drift(),
        team_handoff(),
        compaction(),
        long_compaction(),
        isolation(),
        needle_in_noise(),
        learnings(),
        learning_from_feedback(),
        surprise(),
        conflicts(),
    ]
}

/// A controlled needle sweep. The same seed produces the same decoys at
/// every size, while the chosen position moves the answer-bearing item.
pub(crate) fn scaled(count: usize, position: &str, seed: u64) -> Result<Scenario, String> {
    if !matches!(count, 100 | 1_000 | 10_000) {
        return Err("--scale-events must be 100, 1000, or 10000".into());
    }
    let needle_at = match position {
        "early" => count / 10,
        "middle" => count / 2,
        "late" => count - count / 10 - 1,
        _ => return Err("--scale-position must be early, middle, or late".into()),
    };
    Ok(Scenario {
        name: "needle_scale",
        about: "One owner hidden among seeded near-duplicate support documents",
        steps: vec![Step::BulkDocuments {
            count,
            needle_at,
            seed,
        }],
        probes: vec![
            Probe::new(
                "owner-lexical",
                "scale-agent",
                "Who owns orbital cache invalidation?",
                Style::Lexical,
            )
            .expect(&["Mira Solis"]),
            Probe::new(
                "owner-paraphrase",
                "scale-agent",
                "Which engineer handles the satellite cache refresh rule?",
                Style::Paraphrase,
            )
            .expect(&["Mira Solis"]),
        ],
    })
}

/// A stable distractor or the answer-bearing document for a scale run.
pub(crate) fn scale_document(index: usize, needle_at: usize, seed: u64) -> String {
    if index == needle_at {
        return format!("Case {index}: Mira Solis owns orbital cache invalidation.");
    }
    let shuffled = (index as u64)
        .wrapping_mul(6_364_136_223_846_793_005)
        .wrapping_add(seed);
    let teams = [
        "billing",
        "search",
        "mobile",
        "storage",
        "checkout",
        "analytics",
    ];
    let owners = ["Nadia", "Owen", "Priya", "Rafael", "Tess", "Uma"];
    let n = (shuffled >> 32) as usize;
    format!(
        "Case {index}: {} owns {} cache review. The refresh checklist is in playbook {}.",
        owners[n % owners.len()],
        teams[(n / 7) % teams.len()],
        n % 997
    )
}

#[cfg(test)]
#[path = "scenarios_tests.rs"]
mod tests;

/// User turns with no tool calls.
fn said(lines: &[&str]) -> Vec<(String, Vec<ToolStep>)> {
    lines
        .iter()
        .map(|line| ((*line).to_string(), Vec::new()))
        .collect()
}

/// A tool step.
const fn tool(name: &'static str, result: &'static str) -> ToolStep {
    ToolStep { name, result }
}

fn doc(source: BrainSource, title: &'static str, text: &'static str) -> Step {
    Step::Doc {
        tenant: MAIN,
        source,
        title,
        text,
    }
}

fn chat(
    agent: &'static str,
    thread: &'static str,
    day: i64,
    turns: Vec<(String, Vec<ToolStep>)>,
) -> Step {
    Step::Chat {
        tenant: MAIN,
        agent,
        thread: thread.to_string(),
        day,
        turns,
    }
}

fn brain_lookup() -> Scenario {
    use Style::{Lexical, Paraphrase};
    Scenario {
        name: "brain_lookup",
        about: "Company documents from six sources, asked about by an agent that never saw them",
        steps: vec![
            doc(
                BrainSource::Pdf,
                "Refund policy",
                "Refunds settle within five business days of approval. Enterprise customers \
                 get a dedicated support channel.",
            ),
            doc(
                BrainSource::Markdown,
                "On-call handbook",
                "The on-call rotation hands over every Monday at 09:00 UTC. Pages that are not \
                 acknowledged within 15 minutes escalate to the engineering manager.",
            ),
            doc(
                BrainSource::Notion,
                "Pricing FAQ",
                "The Team plan costs 40 dollars per seat per month. Annual billing gets two \
                 months free.",
            ),
            doc(
                BrainSource::Github,
                "deploy/README.md",
                "Production deploys run from the release branch through the ship-it workflow. \
                 Rollbacks use make rollback ENV=prod.",
            ),
            doc(
                BrainSource::Web,
                "Status page",
                "Scheduled maintenance windows are on Sundays between 02:00 and 04:00 UTC.",
            ),
            doc(
                BrainSource::Markdown,
                "Security policy",
                "Customer data must never leave the eu-central-1 region. Access keys rotate \
                 every 90 days.",
            ),
        ],
        probes: vec![
            Probe::new(
                "refund-days",
                "support-01",
                "How many business days do refunds take to settle?",
                Lexical,
            )
            .expect(&["five business days"])
            .accept(&["5 business days"]),
            Probe::new(
                "refund-paraphrase",
                "support-01",
                "If we give money back to a customer, when does it land?",
                Paraphrase,
            )
            .expect(&["five business days"])
            .accept(&["5 business days"]),
            Probe::new(
                "handover",
                "support-01",
                "When does the on-call rotation hand over?",
                Lexical,
            )
            .expect(&["Monday at 09:00"])
            .accept(&["Monday"]),
            Probe::new(
                "page-escalation",
                "support-01",
                "Who gets woken up if nobody answers an alert?",
                Paraphrase,
            )
            .expect(&["engineering manager"]),
            Probe::new(
                "seat-price",
                "support-01",
                "How much does the Team plan cost per seat?",
                Lexical,
            )
            .expect(&["40 dollars"])
            .accept(&["$40", "40 USD"]),
            Probe::new(
                "undo-release",
                "support-01",
                "How do I undo a bad release?",
                Paraphrase,
            )
            .expect(&["make rollback"]),
            Probe::new(
                "maintenance",
                "support-01",
                "When are the scheduled maintenance windows?",
                Lexical,
            )
            .expect(&["Sundays"])
            .accept(&["Sunday"]),
            Probe::new(
                "credential-rotation",
                "support-01",
                "How often must we change our credentials?",
                Paraphrase,
            )
            .expect(&["90 days"]),
        ],
    }
}

fn restart_recall() -> Scenario {
    use Style::{Lexical, Paraphrase};
    Scenario {
        name: "restart_recall",
        about: "A user introduces themselves; the agent restarts and must remember them",
        steps: vec![chat(
            "assistant-01",
            "intro",
            0,
            said(&[
                "Hi, I'm Dana and I lead the payments team.",
                "I'm based in Lisbon, so my timezone is WET.",
                "I prefer answers as short bullet points, no long essays.",
                "Our project codename is Bluefin.",
                "We ship to production on Thursdays.",
                "Thanks, that's all for now.",
            ]),
        )],
        probes: vec![
            Probe::new(
                "resume-cold",
                "assistant-01",
                "What is the project codename?",
                Lexical,
            )
            .via(Via::Resume {
                thread: None,
                focus: None,
            })
            .expect(&["Bluefin"]),
            Probe::new(
                "resume-focused",
                "assistant-01",
                "What is the user's timezone?",
                Lexical,
            )
            .via(Via::Resume {
                thread: None,
                focus: Some("the user's timezone and location"),
            })
            .expect(&["WET"])
            .accept(&["Western European", "Lisbon"]),
            Probe::new(
                "name-team",
                "assistant-01",
                "What's my name, and which team do I lead?",
                Lexical,
            )
            .expect(&["Dana", "payments"]),
            Probe::new(
                "timezone",
                "assistant-01",
                "Which time zone should meetings with me be scheduled in?",
                Paraphrase,
            )
            .expect(&["WET"])
            .accept(&["Western European", "Lisbon"]),
            Probe::new(
                "codename",
                "assistant-01",
                "What's our project codename?",
                Lexical,
            )
            .expect(&["Bluefin"]),
            Probe::new(
                "release-day",
                "assistant-01",
                "Which weekday do our releases go out?",
                Paraphrase,
            )
            .expect(&["Thursdays"])
            .accept(&["Thursday"]),
            Probe::new(
                "format",
                "assistant-01",
                "How should you format replies for me?",
                Paraphrase,
            )
            .expect(&["bullet points"])
            .accept(&["bullet"]),
        ],
    }
}

fn contradictions() -> Scenario {
    use Style::{Lexical, Paraphrase};
    Scenario {
        name: "contradictions",
        about: "Facts change across three sessions; the newest value must win",
        steps: vec![
            chat(
                "ops-01",
                "week-1",
                0,
                said(&[
                    "Our production region is us-east-1.",
                    "The monthly cloud budget is 5000 dollars.",
                    "Standup is on Tuesdays at 10:00.",
                    "The main database is Postgres 14.",
                ]),
            ),
            chat(
                "ops-01",
                "week-2",
                7,
                said(&[
                    "Update: we migrated the production region to eu-west-2 last night.",
                    "Finance raised the monthly cloud budget to 8000 dollars.",
                ]),
            ),
            chat(
                "ops-01",
                "week-3",
                14,
                said(&[
                    "Standup moved to Thursdays at 10:00.",
                    "Correction: the monthly cloud budget got cut back to 6500 dollars.",
                ]),
            ),
        ],
        probes: vec![
            Probe::new(
                "region",
                "ops-01",
                "Which production region are we in?",
                Lexical,
            )
            .expect(&["eu-west-2"])
            .stale(&["us-east-1"]),
            Probe::new(
                "budget",
                "ops-01",
                "What is the monthly cloud budget?",
                Lexical,
            )
            .expect(&["6500"])
            .accept(&["6,500"])
            .stale(&["5000", "8000"]),
            Probe::new("standup", "ops-01", "When is standup?", Lexical)
                .expect(&["Thursdays"])
                .accept(&["Thursday"])
                .stale(&["Tuesdays"]),
            Probe::new(
                "spend-limit",
                "ops-01",
                "How much can we spend on infrastructure each month?",
                Paraphrase,
            )
            .expect(&["6500"])
            .accept(&["6,500"])
            .stale(&["5000", "8000"]),
            Probe::new("database", "ops-01", "Which database do we run?", Lexical)
                .expect(&["Postgres 14"]),
            Probe::new(
                "resume-latest",
                "ops-01",
                "What is the monthly cloud budget?",
                Lexical,
            )
            .via(Via::Resume {
                thread: None,
                focus: None,
            })
            .expect(&["6500"])
            .accept(&["6,500"])
            .stale(&["5000", "8000"]),
        ],
    }
}

fn tool_heavy() -> Scenario {
    use Style::{Lexical, Paraphrase};
    let turns = vec![
        (
            "The checkout service is returning 500s, can you look?".to_string(),
            vec![
                tool(
                    "search_logs",
                    "error code E4031: connection pool exhausted in checkout-db",
                ),
                tool(
                    "get_deploy_status",
                    "checkout version 2.14.3 deployed 40 minutes ago by ci-bot",
                ),
                tool("get_metrics", "checkout p99 latency 2.8s, up from 180ms"),
                tool(
                    "list_alerts",
                    "2 firing: CheckoutErrorRate, DbPoolSaturation",
                ),
            ],
        ),
        (
            "Which tests cover the connection pool?".to_string(),
            vec![
                tool(
                    "grep_repo",
                    "pool_size=10 set in services/checkout/config/db.toml",
                ),
                tool(
                    "run_tests",
                    "checkout::db: 3 passed, 1 failed: test_pool_saturation",
                ),
                tool("read_file", "db.toml: max_overflow=0, timeout=5s"),
            ],
        ),
        (
            "Roll it back please.".to_string(),
            vec![
                tool("rollback", "checkout rolled back to 2.14.2"),
                tool("get_metrics", "checkout p99 back to 190ms"),
                tool("create_ticket", "ticket OPS-7781 opened for pool sizing"),
                tool(
                    "notify_channel",
                    "posted incident summary to #checkout-oncall",
                ),
            ],
        ),
        (
            "Find the commit that caused it.".to_string(),
            vec![
                tool("git_log", "commit 9f2c1ab lowered pool_size from 50 to 10"),
                tool("git_blame", "change authored by jmiller in PR 4412"),
                tool("get_pr", "PR 4412 approved by one reviewer, merged Friday"),
            ],
        ),
        ("Thanks, that's it.".to_string(), Vec::new()),
    ];
    Scenario {
        name: "tool_heavy",
        about: "An incident debugged through 17 tool calls; the facts live in tool results",
        steps: vec![
            chat("coder-42", "incident-1", 0, turns),
            chat(
                "coder-42",
                "chores",
                1,
                said(&[
                    "Bump the lint config to the new rules.",
                    "Rename the billing module to invoicing.",
                ]),
            ),
        ],
        probes: vec![
            Probe::new(
                "error-code",
                "coder-42",
                "What error code did the checkout logs show?",
                Lexical,
            )
            .expect(&["E4031"]),
            Probe::new("failing-test", "coder-42", "Which test failed?", Lexical)
                .expect(&["test_pool_saturation"]),
            Probe::new(
                "rollback-version",
                "coder-42",
                "Which version did checkout roll back to?",
                Lexical,
            )
            .expect(&["2.14.2"]),
            Probe::new(
                "ticket",
                "coder-42",
                "What ticket tracks the pool sizing?",
                Lexical,
            )
            .expect(&["OPS-7781"]),
            Probe::new(
                "culprit-commit",
                "coder-42",
                "Which change caused the outage?",
                Paraphrase,
            )
            .expect(&["9f2c1ab"]),
            Probe::new(
                "who-to-ask",
                "coder-42",
                "Who should I talk to about the regression?",
                Paraphrase,
            )
            .expect(&["jmiller"]),
        ],
    }
}

fn coding_session() -> Scenario {
    use Style::{Lexical, Paraphrase};
    Scenario {
        name: "coding_session",
        about: "A coding decision, failing test, and file path found only in tool results",
        steps: vec![chat(
            "coder-42",
            "retry-fix",
            0,
            vec![
                (
                    "Find why the API retries a refused connection.".into(),
                    vec![
                        tool("read_file", "src/client/retry.rs: retry_on_connect=false"),
                        tool(
                            "run_tests",
                            "FAILED test_retries_refused_connection at retry_tests.rs:81",
                        ),
                    ],
                ),
                (
                    "Try the proposed change and inspect the diff.".into(),
                    vec![
                        tool(
                            "git_diff",
                            "retry_on_connect=true caused duplicate writes after ambiguous timeout",
                        ),
                        tool("run_tests", "FAILED test_does_not_repeat_unknown_write"),
                    ],
                ),
                (
                    "Revert that change and keep the safe path.".into(),
                    vec![tool(
                        "git_diff",
                        "reverted retry_on_connect=true; kept writes single-attempt",
                    )],
                ),
            ],
        )],
        probes: vec![
            Probe::new(
                "retry-file",
                "coder-42",
                "Which file contains the retry policy?",
                Lexical,
            )
            .expect(&["src/client/retry.rs"]),
            Probe::new(
                "failed-test",
                "coder-42",
                "Which test exposed the unsafe retry?",
                Paraphrase,
            )
            .expect(&["test_does_not_repeat_unknown_write"]),
            Probe::new(
                "why-revert",
                "coder-42",
                "Why did we back out the connection retry?",
                Paraphrase,
            )
            .expect(&["duplicate writes"]),
        ],
    }
}

fn task_drift() -> Scenario {
    use Style::{Lexical, Paraphrase};
    Scenario {
        name: "task_drift",
        about: "A two-week task changes owner and plan while old status remains searchable",
        steps: vec![
            chat(
                "planner-07",
                "migration",
                0,
                said(&[
                    "The ledger migration is owned by Anna. Plan A is a Friday cutover.",
                    "The blocker is an unverified backfill checksum.",
                ]),
            ),
            chat(
                "planner-07",
                "migration-update",
                7,
                said(&[
                    "The ledger migration moved to Ravi. Plan B is a staged Monday cutover.",
                    "The backfill checksum is verified; the current blocker is partner signoff.",
                ]),
            ),
            chat(
                "planner-07",
                "migration-resume",
                14,
                said(&[
                    "Resume the ledger migration. Ravi still owns it; partner signoff remains open.",
                ]),
            ),
        ],
        probes: vec![
            Probe::new(
                "current-owner",
                "planner-07",
                "Who owns the ledger migration now?",
                Lexical,
            )
            .expect(&["Ravi"])
            .stale(&["Anna"]),
            Probe::new(
                "current-blocker",
                "planner-07",
                "What still blocks the cutover?",
                Paraphrase,
            )
            .expect(&["partner signoff"])
            .stale(&["unverified backfill checksum"]),
            Probe::new(
                "changed-plan",
                "planner-07",
                "What is the new release approach?",
                Paraphrase,
            )
            .expect(&["staged Monday cutover"])
            .stale(&["Friday cutover"]),
        ],
    }
}

fn long_compaction() -> Scenario {
    use Style::Lexical;
    let mut turns = said(&["The archive key is indigo-cedar; retain it after compaction."]);
    turns.extend((1..200).map(|n| {
        (
            format!("Routine planning note {n}: check the agenda."),
            Vec::new(),
        )
    }));
    let first: Vec<String> = turns
        .iter()
        .take(100)
        .map(|(text, _)| text.clone())
        .collect();
    let second: Vec<String> = turns
        .iter()
        .take(180)
        .map(|(text, _)| text.clone())
        .collect();
    Scenario {
        name: "long_compaction",
        about: "Two compaction windows over a 200-exchange thread",
        steps: vec![chat("planner-07", "archive", 0, turns)],
        probes: vec![
            Probe::new(
                "first-compaction",
                "planner-07",
                "What archive key must survive?",
                Lexical,
            )
            .via(Via::Compact {
                thread: "archive",
                dropped: first,
            })
            .expect(&["indigo-cedar"]),
            Probe::new(
                "second-compaction",
                "planner-07",
                "What archive key must survive?",
                Lexical,
            )
            .via(Via::Compact {
                thread: "archive",
                dropped: second,
            })
            .expect(&["indigo-cedar"]),
            Probe::new(
                "resumed-window",
                "planner-07",
                "What archive key must survive?",
                Lexical,
            )
            .via(Via::Continue {
                thread: "archive",
                turn_index: 400,
                in_prompt_from: 392,
            })
            .expect(&["indigo-cedar"])
            .forbid(&["planning note 198", "planning note 199"]),
        ],
    }
}

fn team_handoff() -> Scenario {
    use Style::{Lexical, Paraphrase};
    Scenario {
        name: "team_handoff",
        about: "Support learns of a bug; a coding agent must see it without being told",
        steps: vec![chat(
            "support-01",
            "ticket-9",
            0,
            vec![
                (
                    "Customer Acme Corp reports duplicated invoices since Monday.".to_string(),
                    vec![tool(
                        "lookup_account",
                        "Acme Corp is on the Enterprise plan, account id ACC-2209",
                    )],
                ),
                (
                    "They were charged twice for September.".to_string(),
                    Vec::new(),
                ),
            ],
        )],
        probes: vec![
            Probe::new(
                "duplicate-invoices",
                "coder-42",
                "Is any customer reporting duplicated invoices?",
                Lexical,
            )
            .expect(&["Acme"]),
            Probe::new(
                "account-id",
                "coder-42",
                "What is Acme Corp's account id?",
                Lexical,
            )
            .expect(&["ACC-2209"]),
            Probe::new(
                "billed-twice",
                "coder-42",
                "Has anyone been billed two times for the same month?",
                Paraphrase,
            )
            .expect(&["charged twice"])
            .accept(&["twice"]),
        ],
    }
}

/// The 16 filler turns of the compaction scenario, after its four facts.
fn agenda() -> Vec<String> {
    (1..=16)
        .map(|n| {
            format!("Next, agenda item {n}: assign an owner and a deadline for workstream {n}.")
        })
        .collect()
}

fn compaction() -> Scenario {
    use Style::Lexical;
    let facts = [
        "The offsite is in Porto on 12 May.",
        "The offsite budget is 20000 euros.",
        "Catering is booked with Taberna Azul.",
        "We need a vegetarian option for 9 people.",
    ];
    let mut turns = said(&facts);
    turns.extend(agenda().into_iter().map(|line| (line, Vec::new())));
    // 20 exchanges: turns 0..40. The first 4 exchanges are the facts.
    let dropped: Vec<String> = facts
        .iter()
        .map(|fact| (*fact).to_string())
        .chain(agenda().into_iter().take(8))
        .collect();
    Scenario {
        name: "compaction",
        about: "A 20-exchange planning thread whose early facts have left the prompt",
        steps: vec![chat("planner-07", "offsite", 0, turns)],
        probes: vec![
            Probe::new(
                "carry-over",
                "planner-07",
                "Where is the offsite and who caters it?",
                Lexical,
            )
            .via(Via::Compact {
                thread: "offsite",
                dropped,
            })
            .expect(&["Porto", "Taberna Azul"]),
            Probe::new(
                "out-of-window",
                "planner-07",
                "How many vegetarian meals do we need?",
                Lexical,
            )
            .via(Via::Continue {
                thread: "offsite",
                turn_index: 40,
                in_prompt_from: 32,
            })
            .expect(&["9 people"])
            .accept(&["9 vegetarian", "nine vegetarian", "9 meals"])
            // Turns 32..40 are agenda items 13 to 16: still in the prompt.
            .forbid(&[
                "agenda item 13",
                "agenda item 14",
                "agenda item 15",
                "agenda item 16",
            ]),
        ],
    }
}

fn isolation() -> Scenario {
    use Style::Lexical;
    Scenario {
        name: "isolation",
        about: "Two tenants side by side: neither may see the other's brain or turns",
        steps: vec![
            Step::Doc {
                tenant: "acme",
                source: BrainSource::Markdown,
                title: "M&A memo",
                text: "The acquisition target is Zephyr Labs, under the code name Kestrel.",
            },
            Step::Chat {
                tenant: "acme",
                agent: "cfo-bot",
                thread: "q3".to_string(),
                day: 0,
                turns: said(&["Our Q3 revenue was 4.2 million dollars."]),
            },
            Step::Doc {
                tenant: "globex",
                source: BrainSource::Markdown,
                title: "Office handbook",
                text: "Lunch is served at noon in the atrium.",
            },
            Step::Chat {
                tenant: "globex",
                agent: "cfo-bot",
                thread: "q3".to_string(),
                day: 0,
                turns: said(&["Our Q3 revenue was strong this year."]),
            },
        ],
        probes: vec![
            Probe::new(
                "own-tenant",
                "cfo-bot",
                "What is the acquisition target?",
                Lexical,
            )
            .tenant("acme")
            .expect(&["Zephyr"]),
            Probe::new(
                "other-tenant-brain",
                "cfo-bot",
                "What is the acquisition target?",
                Lexical,
            )
            .tenant("globex")
            .forbid(&["Zephyr", "Kestrel"]),
            Probe::new(
                "other-tenant-turns",
                "cfo-bot",
                "What was our Q3 revenue?",
                Lexical,
            )
            .tenant("globex")
            .expect(&["strong"])
            .forbid(&["4.2 million"]),
        ],
    }
}

fn needle_in_noise() -> Scenario {
    use Style::{Lexical, Paraphrase};
    let topics = [
        "resetting a password",
        "exporting invoices to CSV",
        "changing the billing email",
        "adding a teammate",
        "enabling two-factor login",
        "downgrading a plan",
    ];
    let mut steps: Vec<Step> = (0..24)
        .map(|n| {
            let topic = topics[n % topics.len()];
            Step::Chat {
                tenant: MAIN,
                agent: "support-02",
                thread: format!("noise-{n}"),
                day: 0,
                turns: vec![(
                    format!(
                        "Customer {} asked about {topic}; I sent the help article.",
                        100 + n
                    ),
                    Vec::new(),
                )],
            }
        })
        .collect();
    steps.insert(
        12,
        chat(
            "support-02",
            "vip",
            0,
            said(&["Heads up: the VIP account Orion Freight must always be routed to Priya."]),
        ),
    );
    Scenario {
        name: "needle_in_noise",
        about: "One routing rule hidden among 24 routine support threads",
        steps,
        probes: vec![
            Probe::new(
                "needle",
                "support-02",
                "Who handles the Orion Freight account?",
                Lexical,
            )
            .expect(&["Priya"]),
            Probe::new(
                "needle-paraphrase",
                "support-02",
                "Which teammate gets our big logistics client?",
                Paraphrase,
            )
            .expect(&["Priya"]),
        ],
    }
}

fn learnings() -> Scenario {
    use Style::{Lexical, Paraphrase};
    Scenario {
        name: "learnings",
        about: "Explicit learnings must lead the pack, ahead of raw history",
        steps: vec![
            Step::Learning {
                kind: LearningKind::Procedure,
                text: "Always confirm the customer's plan before quoting a price.",
                confidence: 0.9,
            },
            Step::Learning {
                kind: LearningKind::Preference,
                text: "The team prefers metric units in every report.",
                confidence: 0.8,
            },
            chat(
                "sales-03",
                "quote-1",
                0,
                said(&["I quoted Initech 12 seats at the list price."]),
            ),
        ],
        probes: vec![
            Probe::new(
                "quote-procedure",
                "sales-03",
                "Can you quote a price for 12 seats?",
                Lexical,
            )
            .expect(&["confirm the customer's plan"])
            .accept(&[
                "confirm your plan",
                "confirm the plan",
                "confirm which plan",
                "confirm the customer",
            ]),
            Probe::new(
                "units",
                "sales-03",
                "Should the report use miles or kilometres?",
                Paraphrase,
            )
            .expect(&["metric units"])
            .accept(&["kilomet", "metric"]),
        ],
    }
}

/// The `context.md` brief that should carry standing instructions.
const PREFERENCES: Via = Via::ContextDoc {
    heading: "Preferences and standing instructions",
};

/// The `context.md` brief that should carry surprises.
const EVENTS: Via = Via::ContextDoc {
    heading: "Recent important events",
};

/// A fresh session's initial context: `start_session` with nothing to go on.
const COLD_START: Via = Via::Resume {
    thread: None,
    focus: None,
};

fn learning_from_feedback() -> Scenario {
    use Style::{Lexical, Paraphrase};
    let turns = vec![
        (
            "Install the dependencies for the web app.".to_string(),
            vec![tool(
                "run_shell",
                "npm install failed: this repo uses pnpm workspaces (found pnpm-lock.yaml)",
            )],
        ),
        (
            "Right, in this repo always use pnpm, never npm.".to_string(),
            Vec::new(),
        ),
        (
            "Now deploy the staging build.".to_string(),
            vec![tool(
                "run_shell",
                "kubectl apply -f k8s/staging refused: not logged in to the cluster",
            )],
        ),
        (
            "Don't run kubectl yourself. We always deploy with make ship ENV=staging.".to_string(),
            Vec::new(),
        ),
        (
            "Also: open pull requests ready for review, never as drafts.".to_string(),
            Vec::new(),
        ),
    ];
    Scenario {
        name: "learning_from_feedback",
        about: "The agent makes mistakes, the user corrects it; the corrections must stick",
        steps: vec![
            chat("coder-42", "setup", 0, turns),
            chat(
                "coder-42",
                "chores",
                1,
                said(&["Bump the eslint config.", "Rename utils.ts to format.ts."]),
            ),
        ],
        probes: vec![
            Probe::new(
                "install-cold",
                "coder-42",
                "Which package manager should I use in this repo?",
                Paraphrase,
            )
            .via(COLD_START)
            .expect(&["pnpm"]),
            Probe::new(
                "install-context",
                "coder-42",
                "What preferences and standing instructions has the user given?",
                Lexical,
            )
            .via(PREFERENCES)
            .expect(&["pnpm", "make ship"])
            .accept(&["pnpm"]),
            Probe::new(
                "install",
                "coder-42",
                "How do I install the dependencies?",
                Lexical,
            )
            .expect(&["pnpm"]),
            Probe::new(
                "deploy",
                "coder-42",
                "What's the command to deploy staging?",
                Lexical,
            )
            .expect(&["make ship"])
            .stale(&["kubectl apply"]),
            Probe::new(
                "draft-pr",
                "coder-42",
                "Should my new pull request be a draft?",
                Paraphrase,
            )
            .expect(&["ready for review"])
            .accept(&[
                "not as a draft",
                "never as a draft",
                "never as drafts",
                "not a draft",
            ]),
        ],
    }
}

fn surprise() -> Scenario {
    use Style::{Lexical, Paraphrase};
    Scenario {
        name: "surprise",
        about: "A long-stable routine breaks and a metric spikes; the surprise must surface",
        steps: vec![
            chat(
                "ops-01",
                "baseline",
                0,
                said(&[
                    "The nightly backup has succeeded every night for over a year.",
                    "Checkout's error rate always sits under 0.1%.",
                ]),
            ),
            chat(
                "ops-01",
                "morning-check",
                5,
                vec![
                    (
                        "Check last night's backup.".to_string(),
                        vec![tool(
                            "backup_status",
                            "FAILED: disk full on backup-02, the first failure in 412 days",
                        )],
                    ),
                    (
                        "That's a surprise. Keep an eye on backup-02.".to_string(),
                        Vec::new(),
                    ),
                    (
                        "How is checkout doing?".to_string(),
                        vec![tool(
                            "get_metrics",
                            "checkout error rate 4.7% since 02:00, about 47 times its usual level",
                        )],
                    ),
                ],
            ),
        ],
        probes: vec![
            Probe::new(
                "unusual-cold",
                "ops-01",
                "Is anything unusual going on?",
                Paraphrase,
            )
            .via(COLD_START)
            .expect(&["backup-02"])
            .accept(&["disk full", "backup failed", "4.7%"]),
            Probe::new(
                "events-context",
                "ops-01",
                "What important events happened recently?",
                Paraphrase,
            )
            .via(EVENTS)
            .expect(&["backup-02"])
            .accept(&["disk full", "backup failed", "4.7%"]),
            Probe::new(
                "backup",
                "ops-01",
                "Did last night's backup succeed?",
                Lexical,
            )
            .expect(&["FAILED"])
            .accept(&["fail", "did not succeed", "didn't succeed"]),
            Probe::new(
                "checkout",
                "ops-01",
                "Is anything off with checkout?",
                Paraphrase,
            )
            .expect(&["4.7%"])
            .accept(&["error rate"]),
            Probe::new(
                "surprising",
                "ops-01",
                "What surprised us recently?",
                Paraphrase,
            )
            .expect(&["backup-02"])
            .accept(&["disk full", "backup failed", "4.7%"]),
        ],
    }
}

fn conflicts() -> Scenario {
    use Style::{Lexical, Paraphrase};
    Scenario {
        name: "conflicts",
        about: "Sources disagree (a policy, a colleague, an agent); the disagreement must show",
        steps: vec![
            doc(
                BrainSource::Pdf,
                "Refund policy",
                "Refunds settle within five business days of approval.",
            ),
            chat(
                "support-01",
                "finance-sync",
                0,
                said(&["Finance told me refunds now take ten business days."]),
            ),
            chat(
                "sales-02",
                "acme-call",
                1,
                said(&[
                    "I promised Acme their refund lands in seven days.",
                    "Acme is on the Enterprise plan.",
                ]),
            ),
            chat(
                "support-01",
                "acme-ticket",
                2,
                said(&["Acme downgraded to the Team plan last week."]),
            ),
        ],
        probes: vec![
            Probe::new(
                "refund-days",
                "support-01",
                "How long do refunds take?",
                Lexical,
            )
            .expect(&["five", "ten"])
            .accept(&["conflict", "inconsistent", "disagree", "differ"]),
            Probe::new(
                "refund-cold",
                "support-01",
                "How long do refunds take?",
                Lexical,
            )
            .via(COLD_START)
            .expect(&["five", "ten"])
            .accept(&["conflict", "inconsistent", "disagree", "differ"]),
            Probe::new("acme-plan", "support-01", "Which plan is Acme on?", Lexical)
                .expect(&["Team plan"])
                .stale(&["Enterprise"]),
            Probe::new(
                "promise",
                "support-01",
                "Did anyone promise Acme a refund timeline that disagrees with policy?",
                Paraphrase,
            )
            .expect(&["seven days"])
            .accept(&["7 days"]),
        ],
    }
}

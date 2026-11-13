//! A scripted cast, for proving a surface without a live harness.
//!
//! One cast per backend instance. Answers advance the script instead of
//! reaching a model, so the UI can be exercised — a prompt card really does
//! clear, and a tool row really does start running — without a provider key.
//! Nothing here is a mock of a protocol: it is the same `Agent` shape an
//! Operator or Pi adapter produces.

use crate::{
    Activity, ActivityKind, Agent, AgentBackend, Answer, Binding, Choice, Error, Goal, Owner, Page,
    Phase, Prompt, PromptKind, Role, Scope, ScopeKind, Status, Turn, Usage,
};

/// A fixed epoch so a captured frame shows stable timestamps. Real adapters
/// report the harness's own clock.
pub const EPOCH_MS: u64 = 1_766_000_000_000;

pub struct FixtureAgents {
    agents: Vec<Agent>,
    transcript: Vec<(String, Vec<Turn>)>,
    revision: u64,
    /// Advanced by each action, so "now" moves when the person does something.
    now_ms: u64,
    /// Launch intent a demo host can set, the way a window manager would when it
    /// opens one window per agent.
    binding: Binding,
}

impl Default for FixtureAgents {
    fn default() -> Self {
        Self::new()
    }
}

impl FixtureAgents {
    pub fn new() -> Self {
        let mut cast = Self {
            agents: Vec::new(),
            transcript: Vec::new(),
            revision: 1,
            now_ms: EPOCH_MS + 6 * 60_000,
            binding: Binding::default(),
        };
        cast.push(
            "op-7f3a",
            "operator",
            "compaction lane",
            "/Users/ajmwagar/src/operator",
            "deepseek-v4.1-flash",
            "ajmwagar",
            Phase::NeedsYou,
            Some(Goal {
                text: "Make the tool-output lane fire in serve mode".into(),
                set_at_ms: EPOCH_MS,
            }),
            Usage {
                context_tokens: 74_210,
                context_window: 200_000,
                input_tokens: 512_883,
                output_tokens: 42_107,
                turns: 19,
            },
            vec![
                activity(ActivityKind::You, "serve hooks", Status::Ok, EPOCH_MS, Some(1_400)),
                activity(ActivityKind::Model, "plan", Status::Ok, 2_100, Some(6_800)),
                activity(ActivityKind::Files, "src/server.rs", Status::Ok, 9_000, Some(700)),
                activity(ActivityKind::Files, "src/agent.rs", Status::Ok, 9_800, Some(1_150)),
                activity(ActivityKind::Shell, "cargo test", Status::Ok, 11_000, Some(41_000)),
                activity(
                    ActivityKind::Hook,
                    "finish_status blocked",
                    Status::Blocked,
                    52_000,
                    Some(0),
                ),
            ],
            vec![Prompt {
                id: "p1".into(),
                kind: PromptKind::Approval,
                title: "Push this branch to origin?".into(),
                body: Some(
                    "The lane works and the suite is green. The branch is local only, \
                     and the finish contract asks before it publishes."
                        .into(),
                ),
                details: Some(
                    "git push -u origin feat/serve-hooks\n\n 3 files changed, 41 insertions(+), 6 deletions(-)"
                        .into(),
                ),
                origin: Some("bash".into()),
                choices: vec![
                    Choice {
                        id: "approve".into(),
                        label: "Push".into(),
                        detail: Some("origin/feat/serve-hooks".into()),
                        danger: true,
                    },
                    Choice {
                        id: "deny".into(),
                        label: "Keep it local".into(),
                        detail: None,
                        danger: false,
                    },
                ],
                allows_text: true,
                asked_at_ms: EPOCH_MS + 52_000,
                deadline_at_ms: None,
            }],
        );

        cast.push(
            "op-2b91",
            "operator",
            "arch mirror",
            "/Users/ajmwagar/src/operator",
            "deepseek-v4.1-flash",
            "ajmwagar",
            Phase::Working,
            Some(Goal {
                text: "Bring ARCHITECTURE.md back in line with agent.rs".into(),
                set_at_ms: EPOCH_MS,
            }),
            Usage {
                context_tokens: 38_940,
                context_window: 200_000,
                input_tokens: 190_442,
                output_tokens: 18_003,
                turns: 8,
            },
            vec![
                activity(
                    ActivityKind::You,
                    "recheck the drift list",
                    Status::Ok,
                    EPOCH_MS,
                    Some(900),
                ),
                activity(
                    ActivityKind::Memory,
                    "AGENTS.md",
                    Status::Ok,
                    1_000,
                    Some(200),
                ),
                activity(
                    ActivityKind::Files,
                    "ARCHITECTURE.md",
                    Status::Ok,
                    2_000,
                    Some(5_600),
                ),
                Activity {
                    kind: ActivityKind::Shell,
                    label: "cargo test -p operator".into(),
                    detail: Some("162 passing".into()),
                    status: Status::Running,
                    started_at_ms: 8_400,
                    finished_at_ms: None,
                },
            ],
            Vec::new(),
        );

        cast.push(
            "pi-9c04",
            "pi",
            "bead triage",
            "/Users/ajmwagar/src/marbles",
            "claude-sonnet-4-6",
            "ajmwagar",
            Phase::Thinking,
            Some(Goal {
                text: "Close or merge the nine stale marbles".into(),
                set_at_ms: EPOCH_MS,
            }),
            Usage {
                context_tokens: 121_500,
                context_window: 200_000,
                input_tokens: 402_118,
                output_tokens: 29_884,
                turns: 12,
            },
            vec![
                activity(
                    ActivityKind::You,
                    "stale beads",
                    Status::Ok,
                    EPOCH_MS,
                    Some(700),
                ),
                activity(
                    ActivityKind::Network,
                    "github.com",
                    Status::Ok,
                    900,
                    Some(2_300),
                ),
                activity(
                    ActivityKind::Files,
                    "marbles.toml",
                    Status::Ok,
                    3_400,
                    Some(400),
                ),
                Activity {
                    kind: ActivityKind::Model,
                    label: "sift 12 issues".into(),
                    detail: Some("9 stale, 3 live".into()),
                    status: Status::Running,
                    started_at_ms: 4_200,
                    finished_at_ms: None,
                },
            ],
            Vec::new(),
        );

        cast.push(
            "pi-4d18",
            "pi",
            "ui spike",
            "/Users/ajmwagar/src/spores",
            "kimi-k2",
            "sam",
            Phase::NeedsYou,
            Some(Goal {
                text: "Land the creature board without a second renderer".into(),
                set_at_ms: EPOCH_MS,
            }),
            Usage {
                context_tokens: 18_002,
                context_window: 131_072,
                input_tokens: 44_210,
                output_tokens: 9_600,
                turns: 4,
            },
            vec![
                activity(
                    ActivityKind::You,
                    "draw the board",
                    Status::Ok,
                    EPOCH_MS,
                    Some(600),
                ),
                activity(
                    ActivityKind::Skill,
                    "spore-ui",
                    Status::Ok,
                    800,
                    Some(1_100),
                ),
                activity(
                    ActivityKind::Agent,
                    "sketch skins",
                    Status::Ok,
                    2_000,
                    Some(28_000),
                ),
            ],
            vec![Prompt {
                id: "p2".into(),
                kind: PromptKind::Question,
                title: "Rotate the staging deploy key now, or wait for the window?".into(),
                body: Some(
                    "Nothing calls it, and the Lua host never loads it. Removing it is a \
                     two-line diff; leaving it means the next branch inherits a second \
                     layout path."
                        .into(),
                ),
                details: Some("deploy/staging.env\n  DEPLOY_KEY   expires in 2h\n  last rotated 29d ago".into()),
                origin: Some("finish_status".into()),
                choices: Vec::new(),
                allows_text: true,
                asked_at_ms: EPOCH_MS + 31_000,
                deadline_at_ms: None,
            }],
        );

        cast.push(
            "op-58ce",
            "operator",
            "release check",
            "/Users/ajmwagar/src/soil",
            "deepseek-v4.1-flash",
            "ajmwagar",
            Phase::Finished,
            Some(Goal {
                text: "Verify the 0.2.0 cut builds on all three hosts".into(),
                set_at_ms: EPOCH_MS,
            }),
            Usage {
                context_tokens: 9_004,
                context_window: 200_000,
                input_tokens: 88_102,
                output_tokens: 6_440,
                turns: 6,
            },
            vec![
                activity(
                    ActivityKind::You,
                    "cut 0.2.0",
                    Status::Ok,
                    EPOCH_MS,
                    Some(500),
                ),
                activity(
                    ActivityKind::Shell,
                    "cargo build --release",
                    Status::Ok,
                    600,
                    Some(96_000),
                ),
                activity(
                    ActivityKind::Shell,
                    "cargo build --target wasm32-wasip2",
                    Status::Ok,
                    97_000,
                    Some(31_000),
                ),
                activity(
                    ActivityKind::Model,
                    "report",
                    Status::Ok,
                    129_000,
                    Some(3_000),
                ),
            ],
            Vec::new(),
        );

        cast.transcripts();
        cast
    }

    /// Set the launch intent the way a host opening a dedicated window would:
    /// this surface is for one agent, in one view.
    pub fn bind(mut self, agent: Option<String>, view: Option<String>) -> Self {
        self.binding = Binding { agent, view };
        self
    }

    #[allow(clippy::too_many_arguments)]
    fn push(
        &mut self,
        id: &str,
        harness: &str,
        title: &str,
        workspace: &str,
        model: &str,
        owner: &str,
        phase: Phase,
        goal: Option<Goal>,
        usage: Usage,
        activity: Vec<Activity>,
        prompts: Vec<Prompt>,
    ) {
        // Read the counters before the mutable borrow of the cast: a fixture
        // that stamps a half-written revision is worse than none.
        let revision = self.revision;
        let updated_at_ms = self.now_ms;
        self.agents.push(Agent {
            id: id.into(),
            harness: harness.into(),
            title: title.into(),
            workspace: Some(workspace.into()),
            model: Some(model.into()),
            phase,
            goal,
            owner: Some(Owner {
                actor: owner.into(),
                display_name: Some(owner.into()),
            }),
            activity,
            prompts,
            usage,
            revision,
            started_at_ms: EPOCH_MS,
            updated_at_ms,
        });
    }

    /// Conversation for the focus view: short on purpose, because the point of
    /// the fixture is the shape of the surface, not a transcript to read.
    fn transcripts(&mut self) {
        let casts: [(&str, [(&str, Role); 4]); 5] = [
            (
                "op-7f3a",
                [
                    (
                        "The tool-output lane is dead in serve mode: policy_hooks is bound to _policy_hooks.",
                        Role::User,
                    ),
                    (
                        "Found it. `server.rs` drops the hook list, so fail-open conjunct 2 is permanently false over ACP.",
                        Role::Assistant,
                    ),
                    (
                        "Fixed and covered by a serve-mode test. Suite is green.",
                        Role::Assistant,
                    ),
                    (
                        "finish_status(blocked): the branch is local; pushing it needs your approval.",
                        Role::Tool,
                    ),
                ],
            ),
            (
                "op-2b91",
                [
                    (
                        "ARCHITECTURE.md cites line numbers that moved two refactors ago.",
                        Role::User,
                    ),
                    (
                        "Re-deriving every citation from the current tree, then rewriting the drift section.",
                        Role::Assistant,
                    ),
                    (
                        "Running the suite before I touch the prose.",
                        Role::Assistant,
                    ),
                    ("cargo test -p operator  (running)", Role::Tool),
                ],
            ),
            (
                "pi-9c04",
                [
                    (
                        "Nine marbles have gone quiet since September. Triage them.",
                        Role::User,
                    ),
                    (
                        "Pulling each thread and the last commit that touched it.",
                        Role::Assistant,
                    ),
                    (
                        "Nine stale, three live: the stale ones all point at the retired ClickHouse path.",
                        Role::Assistant,
                    ),
                    (
                        "fetch github.com/FuturePresentLabs/marbles  (200)",
                        Role::Tool,
                    ),
                ],
            ),
            (
                "pi-4d18",
                [
                    (
                        "Draw the creature board. Keep the renderer count at one.",
                        Role::User,
                    ),
                    (
                        "GPUI in the guest, no host widgets, no second compositor.",
                        Role::Assistant,
                    ),
                    (
                        "Board, stage and focus share one layout pass; skins are pure functions of vitals.",
                        Role::Assistant,
                    ),
                    ("finish_status(decision): rotate the staging key?", Role::Tool),
                ],
            ),
            (
                "op-58ce",
                [
                    ("Cut 0.2.0 and prove it on macOS, Linux, wasm.", Role::User),
                    (
                        "Three release builds, then the manifest check.",
                        Role::Assistant,
                    ),
                    (
                        "All three link; the wasm target is 41 KiB smaller than 0.1.9.",
                        Role::Assistant,
                    ),
                    ("finish_status(done)", Role::Tool),
                ],
            ),
        ];
        self.transcript = casts
            .into_iter()
            .map(|(id, turns)| {
                let turns = turns
                    .into_iter()
                    .enumerate()
                    .map(|(index, (text, role))| Turn {
                        role,
                        text: text.into(),
                        at_ms: EPOCH_MS + index as u64 * 60_000,
                    })
                    .collect();
                (id.to_string(), turns)
            })
            .collect();
    }

    fn find(&mut self, id: &str) -> Result<&mut Agent, Error> {
        self.agents
            .iter_mut()
            .find(|agent| agent.id == id)
            .ok_or(Error::NotFound)
    }

    /// Every mutation goes through here so a revision always moves and a
    /// surface can tell a stale frame from a fresh one.
    fn touch(&mut self, id: &str) {
        self.revision += 1;
        self.now_ms += 1_000;
        let revision = self.revision;
        let now_ms = self.now_ms;
        if let Ok(agent) = self.find(id) {
            agent.revision = revision;
            agent.updated_at_ms = now_ms;
            // The invariant holds even in a fixture: a prompt is the only way
            // to need a person, and clearing one leaves the phase behind it.
            agent.phase = if agent.prompts.is_empty() {
                match agent.phase {
                    Phase::NeedsYou => Phase::Idle,
                    other => other,
                }
            } else {
                Phase::NeedsYou
            };
        }
    }
}

fn activity(
    kind: ActivityKind,
    label: &str,
    status: Status,
    started_at_ms: u64,
    duration_ms: Option<u64>,
) -> Activity {
    Activity {
        kind,
        label: label.into(),
        detail: None,
        status,
        started_at_ms: EPOCH_MS + started_at_ms,
        finished_at_ms: duration_ms.map(|d| EPOCH_MS + started_at_ms + d),
    }
}

impl AgentBackend for FixtureAgents {
    fn list(&mut self) -> Vec<Agent> {
        let mut agents = self.agents.clone();
        // Host order: a person is the scarce resource, so an agent that needs
        // one outranks an agent that is merely busy.
        agents.sort_by_key(|agent| {
            (
                agent.phase != Phase::NeedsYou,
                u64::MAX - agent.updated_at_ms,
            )
        });
        agents
    }

    fn get(&mut self, id: &str) -> Option<Agent> {
        self.agents.iter().find(|agent| agent.id == id).cloned()
    }

    fn binding(&mut self) -> Binding {
        self.binding.clone()
    }

    /// A handover machine: the wall shows this desktop's agents, and some of
    /// them belong to somebody else.
    fn scope(&mut self) -> Scope {
        Scope {
            kind: ScopeKind::Desktop,
            id: Some("desk-ajmwagar".into()),
            label: Some("this desktop".into()),
            owner: Some(Owner {
                actor: "ajmwagar".into(),
                display_name: Some("ajmwagar".into()),
            }),
        }
    }

    fn transcript(&mut self, id: &str, _before: Option<u64>, limit: u32) -> Result<Page, Error> {
        let Some((_, turns)) = self.transcript.iter().find(|(key, _)| key == id) else {
            return Err(Error::NotFound);
        };
        let limit = limit.clamp(1, 64) as usize;
        let start = turns.len().saturating_sub(limit);
        Ok(Page {
            turns: turns[start..].to_vec(),
            before: (start > 0).then_some(turns[start].at_ms),
        })
    }

    fn answer(&mut self, id: &str, prompt_id: &str, answer: Answer) -> Result<(), Error> {
        let now_ms = self.now_ms;
        let agent = self.find(id)?;
        let Some(index) = agent.prompts.iter().position(|p| p.id == prompt_id) else {
            return Err(Error::Stale);
        };
        if answer.choice.is_none() && answer.text.as_deref().unwrap_or("").trim().is_empty() {
            return Err(Error::Invalid);
        }
        let prompt = agent.prompts.remove(index);
        let approved = answer.choice.as_deref() == Some("approve");
        match prompt.kind {
            PromptKind::Approval if approved => {
                agent.phase = Phase::Working;
                agent.activity.push(Activity {
                    kind: ActivityKind::Shell,
                    label: "git push".into(),
                    detail: Some("waiting for origin".into()),
                    status: Status::Running,
                    started_at_ms: now_ms,
                    finished_at_ms: None,
                });
            }
            PromptKind::Approval => {
                agent.phase = Phase::Idle;
                agent.activity.push(Activity {
                    kind: ActivityKind::Shell,
                    label: "git push".into(),
                    detail: Some("declined".into()),
                    status: Status::Blocked,
                    started_at_ms: now_ms,
                    finished_at_ms: Some(now_ms),
                });
            }
            _ => {
                // A question has no answer channel of its own: the words go
                // back as the next user message, which is what the Operator
                // adapter does with a `blocked` report too.
                agent.phase = Phase::Thinking;
                let text = answer
                    .text
                    .filter(|text| !text.trim().is_empty())
                    .or_else(|| {
                        answer.choice.clone().and_then(|choice| {
                            prompt
                                .choices
                                .iter()
                                .find(|candidate| candidate.id == choice)
                                .map(|candidate| candidate.label.clone())
                        })
                    })
                    .unwrap_or_default();
                agent.activity.push(Activity {
                    kind: ActivityKind::You,
                    label: truncate(&text, 48),
                    detail: None,
                    status: Status::Ok,
                    started_at_ms: now_ms,
                    finished_at_ms: Some(now_ms),
                });
            }
        }
        self.touch(id);
        Ok(())
    }

    fn send(&mut self, id: &str, text: &str) -> Result<(), Error> {
        if text.trim().is_empty() {
            return Err(Error::Invalid);
        }
        let now_ms = self.now_ms;
        let agent = self.find(id)?;
        if !agent.prompts.is_empty() {
            return Err(Error::Invalid);
        }
        agent.phase = Phase::Thinking;
        agent.activity.push(Activity {
            kind: ActivityKind::You,
            label: truncate(text.trim(), 48),
            detail: None,
            status: Status::Ok,
            started_at_ms: now_ms,
            finished_at_ms: Some(now_ms),
        });
        if let Some((_, turns)) = self.transcript.iter_mut().find(|(key, _)| key == id) {
            turns.push(Turn {
                role: Role::User,
                text: text.trim().to_string(),
                at_ms: now_ms,
            });
        }
        self.touch(id);
        Ok(())
    }

    fn interrupt(&mut self, id: &str) -> Result<(), Error> {
        let now_ms = self.now_ms;
        let agent = self.find(id)?;
        agent.phase = Phase::Paused;
        for row in &mut agent.activity {
            if row.status == Status::Running {
                row.status = Status::Blocked;
                row.finished_at_ms = Some(now_ms);
            }
        }
        self.touch(id);
        Ok(())
    }
}

/// A label is a label: the fixture keeps it to one short line the way an
/// adapter has to.
fn truncate(text: &str, max: usize) -> String {
    let mut out: String = text.chars().take(max).collect();
    if text.chars().count() > max {
        out.push('\u{2026}');
    }
    out
}

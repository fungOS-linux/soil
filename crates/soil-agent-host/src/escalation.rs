//! What happens when nobody answers.
//!
//! A prompt with no deadline is a deadlock: the agent will not move and the
//! person may be asleep. This wrapper puts a policy around any backend — hand
//! the question to another agent (a PM), and if that agent does not decide
//! either, take the safest option on the person's behalf.
//!
//! Policy, not orchestration: the wrapper never decides *what* is right, it
//! only decides *when* waiting is over. Everything it does is one of the three
//! calls the contract already has.

use std::collections::HashSet;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::{
    Agent, AgentBackend, Answer, Binding, Change, Error, Page, Prompt, Scope,
};

/// When a quiet prompt becomes somebody else's problem.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Escalation {
    /// How long a prompt may sit unanswered before it is handed on.
    pub after: Duration,
    /// Who receives it. `None` still applies the deadline, so a view can show
    /// "escalates in 40s" before anyone is delegated to.
    pub to: Option<String>,
    /// How long the delegate has before the safest option is applied.
    pub grace: Duration,
}

impl Default for Escalation {
    fn default() -> Self {
        Self {
            after: Duration::from_secs(90),
            to: None,
            grace: Duration::from_secs(60),
        }
    }
}

impl Escalation {
    /// Read the policy from the environment, so a host can be configured
    /// without a rebuild.
    pub fn from_environment() -> Self {
        let seconds = |key: &str, fallback: Duration| {
            std::env::var(key)
                .ok()
                .and_then(|value| value.trim().parse::<u64>().ok())
                .map(Duration::from_secs)
                .unwrap_or(fallback)
        };
        let default = Self::default();
        Self {
            after: seconds("SOIL_ESCALATE_AFTER_SECS", default.after),
            to: std::env::var("SOIL_ESCALATE_TO")
                .ok()
                .map(|value| value.trim().to_string())
                .filter(|value| !value.is_empty()),
            grace: seconds("SOIL_ESCALATE_GRACE_SECS", default.grace),
        }
    }
}

/// A backend under an escalation policy.
pub struct Escalating<B> {
    inner: B,
    policy: Escalation,
    /// (agent, prompt) pairs already handed on, so a slow delegate is asked
    /// once rather than every tick.
    handed_on: HashSet<(String, String)>,
    /// What the policy did, newest last, so a surface can say "answered by
    /// policy" instead of pretending the person did.
    pub decisions: Vec<String>,
}

impl<B: AgentBackend> Escalating<B> {
    pub fn new(inner: B, policy: Escalation) -> Self {
        Self {
            inner,
            policy,
            handed_on: HashSet::new(),
            decisions: Vec::new(),
        }
    }

    pub fn policy(&self) -> &Escalation {
        &self.policy
    }

    /// Stamp a deadline on every open prompt and act on the ones whose time is
    /// up. Called from every read, because the contract has no timer.
    fn enforce(&mut self, agents: &mut [Agent]) {
        let now = now_ms();
        let mut work = Vec::new();
        for agent in agents.iter_mut() {
            for prompt in agent.prompts.iter_mut() {
                let deadline = prompt
                    .deadline_at_ms
                    .unwrap_or(prompt.asked_at_ms.saturating_add(self.policy.after.as_millis() as u64));
                prompt.deadline_at_ms = Some(deadline);
                if now >= deadline {
                    work.push((agent.id.clone(), agent.title.clone(), prompt.clone(), deadline));
                }
            }
        }
        for (agent_id, title, prompt, deadline) in work {
            let decided = now >= deadline.saturating_add(self.policy.grace.as_millis() as u64);
            if decided {
                self.decide(&agent_id, &title, &prompt);
            } else if let Some(to) = self.policy.to.clone()
                && self.handed_on.insert((agent_id.clone(), prompt.id.clone()))
            {
                let question = format!(
                    "{title} ({agent_id}) cannot proceed: {}",
                    prompt.title
                );
                let _ = self.inner.send(&to, &question);
                self.decisions.push(format!(
                    "handed {agent_id} to {to} after {}s",
                    self.policy.after.as_secs()
                ));
            }
        }
    }

    /// Take the safest option, or stop the turn when there is nothing to pick.
    fn decide(&mut self, agent_id: &str, title: &str, prompt: &Prompt) {
        if !self.handed_on.insert((agent_id.to_string(), prompt.id.clone())) {
            // Already handed on; that is expected. Only decide once.
        }
        let safe = prompt
            .choices
            .iter()
            .find(|choice| {
                let label = choice.label.to_ascii_lowercase();
                !choice.danger
                    && (label.contains("reject") || label.contains("deny") || label.contains("cancel"))
            })
            .or_else(|| prompt.choices.iter().find(|choice| !choice.danger));
        let outcome = match safe {
            Some(choice) => self
                .inner
                .answer(
                    agent_id,
                    &prompt.id,
                    Answer {
                        choice: Some(choice.id.clone()),
                        text: None,
                    },
                )
                .map(|()| format!("answered {agent_id} with {}", choice.label)),
            None => self
                .inner
                .interrupt(agent_id)
                .map(|()| format!("interrupted {agent_id}: no safe option")),
        };
        match outcome {
            Ok(note) => self.decisions.push(format!("{title}: {note}")),
            Err(error) => self
                .decisions
                .push(format!("{title}: policy could not decide ({error:?})")),
        }
    }
}

impl<B: AgentBackend> AgentBackend for Escalating<B> {
    fn list(&mut self) -> Vec<Agent> {
        let mut agents = self.inner.list();
        self.enforce(&mut agents);
        agents
    }

    fn get(&mut self, id: &str) -> Option<Agent> {
        let mut agents: Vec<Agent> = self.inner.get(id).into_iter().collect();
        self.enforce(&mut agents);
        agents.pop()
    }

    fn binding(&mut self) -> Binding {
        self.inner.binding()
    }

    fn scope(&mut self) -> Scope {
        self.inner.scope()
    }

    fn transcript(&mut self, id: &str, before: Option<u64>, limit: u32) -> Result<Page, Error> {
        self.inner.transcript(id, before, limit)
    }

    fn answer(&mut self, id: &str, prompt_id: &str, answer: Answer) -> Result<(), Error> {
        self.inner.answer(id, prompt_id, answer)
    }

    fn send(&mut self, id: &str, text: &str) -> Result<(), Error> {
        self.inner.send(id, text)
    }

    fn interrupt(&mut self, id: &str) -> Result<(), Error> {
        self.inner.interrupt(id)
    }

    fn next(&mut self, timeout_ms: u32) -> Option<Change> {
        let change = self.inner.next(timeout_ms);
        // A change is the moment a new prompt arrived; the policy acts on the
        // next read, which every surface performs.
        if let Some(change) = change {
            let mut agents = self.inner.list();
            self.enforce(&mut agents);
            return Some(change);
        }
        None
    }
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        Agent, Answer, Binding, Change, Choice, Error, Page, Phase, Prompt, PromptKind, Usage,
    };

    #[derive(Default)]
    struct Fake {
        agents: Vec<Agent>,
        sent: Vec<(String, String)>,
        answered: Vec<(String, String, Option<String>)>,
        interrupted: Vec<String>,
    }

    impl AgentBackend for Fake {
        fn list(&mut self) -> Vec<Agent> {
            self.agents.clone()
        }
        fn get(&mut self, id: &str) -> Option<Agent> {
            self.agents.iter().find(|agent| agent.id == id).cloned()
        }
        fn binding(&mut self) -> Binding {
            Binding::default()
        }
        fn transcript(&mut self, _: &str, _: Option<u64>, _: u32) -> Result<Page, Error> {
            Err(Error::Unsupported)
        }
        fn answer(&mut self, id: &str, prompt_id: &str, answer: Answer) -> Result<(), Error> {
            self.answered
                .push((id.to_string(), prompt_id.to_string(), answer.choice));
            Ok(())
        }
        fn send(&mut self, id: &str, text: &str) -> Result<(), Error> {
            self.sent.push((id.to_string(), text.to_string()));
            Ok(())
        }
        fn interrupt(&mut self, id: &str) -> Result<(), Error> {
            self.interrupted.push(id.to_string());
            Ok(())
        }
        fn next(&mut self, _: u32) -> Option<Change> {
            None
        }
    }

    fn asked(deadline: Option<u64>, choices: Vec<Choice>) -> Agent {
        Agent {
            id: "a1".into(),
            harness: "acp".into(),
            title: "Deploy".into(),
            workspace: None,
            model: None,
            phase: Phase::NeedsYou,
            goal: None,
            owner: None,
            activity: Vec::new(),
            prompts: vec![Prompt {
                id: "p1".into(),
                kind: PromptKind::Approval,
                title: "push to main".into(),
                body: None,
                details: None,
                origin: None,
                choices,
                allows_text: false,
                asked_at_ms: 0,
                deadline_at_ms: deadline,
            }],
            usage: Usage::default(),
            revision: 0,
            started_at_ms: 0,
            updated_at_ms: 0,
        }
    }

    fn choice(id: &str, label: &str, danger: bool) -> Choice {
        Choice {
            id: id.into(),
            label: label.into(),
            detail: None,
            danger,
        }
    }

    #[test]
    fn a_deadline_is_stamped_so_a_view_can_count_down() {
        let fake = Fake {
            agents: vec![asked(None, vec![])],
            ..Default::default()
        };
        let mut backend = Escalating::new(
            fake,
            Escalation {
                after: Duration::from_secs(30),
                to: None,
                grace: Duration::from_secs(10),
            },
        );
        let agents = backend.list();
        assert_eq!(agents[0].prompts[0].deadline_at_ms, Some(30_000));
    }

    #[test]
    fn a_prompt_past_its_deadline_is_handed_to_the_delegate_once() {
        let fake = Fake {
            agents: vec![asked(Some(now_ms() - 1_000), vec![choice("ok", "Allow", false)])],
            ..Default::default()
        };
        let mut backend = Escalating::new(
            fake,
            Escalation {
                after: Duration::from_secs(1),
                to: Some("pm".into()),
                grace: Duration::from_secs(600),
            },
        );
        backend.list();
        backend.list();
        assert_eq!(backend.inner.sent.len(), 1, "handed on once");
        assert!(backend.inner.sent[0].1.contains("Deploy"));
        assert!(backend.inner.answered.is_empty(), "the delegate still has time");
    }

    #[test]
    fn past_the_grace_the_safest_option_is_taken() {
        let fake = Fake {
            agents: vec![asked(
                Some(now_ms() - 10_000),
                vec![
                    choice("allow-all", "Always allow", true),
                    choice("reject", "Reject", false),
                ],
            )],
            ..Default::default()
        };
        let mut backend = Escalating::new(
            fake,
            Escalation {
                after: Duration::from_secs(1),
                to: None,
                grace: Duration::from_secs(1),
            },
        );
        backend.list();
        assert_eq!(
            backend.inner.answered,
            vec![("a1".to_string(), "p1".to_string(), Some("reject".to_string()))]
        );
    }

    #[test]
    fn with_no_option_the_turn_is_interrupted_rather_than_faked() {
        let fake = Fake {
            agents: vec![asked(Some(now_ms() - 10_000), vec![choice("go", "Publish", true)])],
            ..Default::default()
        };
        let mut backend = Escalating::new(
            fake,
            Escalation {
                after: Duration::from_secs(1),
                to: None,
                grace: Duration::from_secs(1),
            },
        );
        backend.list();
        assert_eq!(backend.inner.interrupted, vec!["a1".to_string()]);
        assert!(backend.inner.answered.is_empty());
    }
}

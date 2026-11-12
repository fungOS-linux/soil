//! The host side of `soil:agent`: one normalized agent, and the seam a harness
//! adapter implements.
//!
//! These are plain records, not generated WIT types. A presenter maps them at
//! the boundary (`CatalogHost` follows the same rule), so an adapter never
//! depends on the ABI and a WIT change cannot silently alter a protocol client.
//!
//! The contract carries facts, not moods: no energy, health or mood lives here.
//! A view derives those from `usage`, `activity` and `prompts`.

pub mod fixture;

pub use fixture::FixtureAgents;

/// Why an observation or an answer was refused. Mirrors `soil:agent/types.error`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    Denied,
    NotFound,
    NotReady,
    Invalid,
    OverBudget,
    Stale,
    Unsupported,
    Io,
}

/// Where an agent is in its turn.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phase {
    Connecting,
    Idle,
    Thinking,
    Working,
    /// The only phase that carries prompts, and the only phase a non-empty
    /// prompt list may appear in.
    NeedsYou,
    Finished,
    Failed,
    Paused,
}

/// The station vocabulary of an agent's timeline. The adapter classifies; the
/// view only draws.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ActivityKind {
    You,
    Model,
    Memory,
    Files,
    Shell,
    Network,
    Browser,
    Agent,
    Skill,
    Hook,
    Other,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Status {
    Running,
    Ok,
    Failed,
    Blocked,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Activity {
    pub kind: ActivityKind,
    /// A name, never content: a tool name, a path's tail, a host.
    pub label: String,
    pub detail: Option<String>,
    pub status: Status,
    pub started_at_ms: u64,
    pub finished_at_ms: Option<u64>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PromptKind {
    Question,
    Approval,
    Choice,
}

/// One option of a `Prompt`. Named `Choice` in Rust because `Option` is
/// already a word here.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Choice {
    pub id: String,
    pub label: String,
    pub detail: Option<String>,
    /// Publishes, deletes, spends, or widens scope.
    pub danger: bool,
}

/// One thing the agent cannot decide alone.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Prompt {
    pub id: String,
    pub kind: PromptKind,
    pub title: String,
    pub body: Option<String>,
    /// Preformatted evidence: a command, a diff, a failing test.
    pub details: Option<String>,
    /// What raised it: a tool name, a harness step.
    pub origin: Option<String>,
    pub choices: Vec<Choice>,
    pub allows_text: bool,
    pub asked_at_ms: u64,
    pub deadline_at_ms: Option<u64>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Role {
    User,
    Assistant,
    Tool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Turn {
    pub role: Role,
    pub text: String,
    pub at_ms: u64,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Usage {
    pub context_tokens: u64,
    /// Zero when the adapter does not know the window; a view must not invent
    /// one, it draws an unknown meter instead.
    pub context_window: u64,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub turns: u32,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Goal {
    pub text: String,
    pub set_at_ms: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Agent {
    pub id: String,
    /// Adapter identity: "operator", "pi".
    pub harness: String,
    pub title: String,
    pub workspace: Option<String>,
    pub model: Option<String>,
    pub phase: Phase,
    pub goal: Option<Goal>,
    pub activity: Vec<Activity>,
    pub prompts: Vec<Prompt>,
    pub usage: Usage,
    pub revision: u64,
    pub started_at_ms: u64,
    pub updated_at_ms: u64,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Answer {
    pub choice: Option<String>,
    pub text: Option<String>,
}

/// What the host opened a surface for.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Binding {
    pub agent: Option<String>,
    /// A hint the Spore may ignore: views are the Spore's vocabulary.
    pub view: Option<String>,
}

/// One page of conversation, newest last.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Page {
    pub turns: Vec<Turn>,
    /// Cursor for the next older page.
    pub before: Option<u64>,
}

/// One change since a subscription opened.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Change {
    /// Boxed: an agent is a few hundred bytes, and a change queue should not
    /// pay for the difference on every `Removed`.
    Upserted(Box<Agent>),
    Removed(String),
}

/// The seam a harness adapter implements.
///
/// Every method is fallible only where a caller could ask for something wrong:
/// `list` and `binding` answer for the host itself, so they cannot be refused.
/// `send` is refused while a prompt is open — an answer is not a message.
pub trait AgentBackend: Send {
    /// Host order: agents that need a person first, then most recently active.
    fn list(&mut self) -> Vec<Agent>;
    fn get(&mut self, id: &str) -> Option<Agent>;
    fn binding(&mut self) -> Binding;
    fn transcript(&mut self, id: &str, before: Option<u64>, limit: u32) -> Result<Page, Error>;
    fn answer(&mut self, id: &str, prompt_id: &str, answer: Answer) -> Result<(), Error>;
    fn send(&mut self, id: &str, text: &str) -> Result<(), Error>;
    fn interrupt(&mut self, id: &str) -> Result<(), Error>;

    /// The next change, waiting up to `timeout_ms`. `None` means "nothing yet",
    /// never "nothing will ever happen". A backend with no stream returns
    /// `None` immediately and a guest re-reads `list` after its own actions.
    fn next(&mut self, timeout_ms: u32) -> Option<Change> {
        let _ = timeout_ms;
        None
    }
}

/// A host with no adapter attached. Every read fails loudly rather than
/// returning an empty wall that looks like "no agents are running".
pub struct NoAgents;

impl AgentBackend for NoAgents {
    fn list(&mut self) -> Vec<Agent> {
        Vec::new()
    }

    fn get(&mut self, _id: &str) -> Option<Agent> {
        None
    }

    fn binding(&mut self) -> Binding {
        Binding::default()
    }

    fn transcript(&mut self, _id: &str, _before: Option<u64>, _limit: u32) -> Result<Page, Error> {
        Err(Error::Denied)
    }

    fn answer(&mut self, _id: &str, _prompt_id: &str, _answer: Answer) -> Result<(), Error> {
        Err(Error::Denied)
    }

    fn send(&mut self, _id: &str, _text: &str) -> Result<(), Error> {
        Err(Error::Denied)
    }

    fn interrupt(&mut self, _id: &str) -> Result<(), Error> {
        Err(Error::Denied)
    }
}

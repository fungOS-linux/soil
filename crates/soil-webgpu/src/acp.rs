//! An ACP client, so one adapter covers every harness that speaks the protocol.
//!
//! ACP is the wire the harnesses already have: `operator`, `pi`, the editor
//! agents, the `delta-acp` sidecar. Mapping it once, here, means a view never
//! learns which harness is underneath, and adding a harness is a command line
//! rather than a crate.
//!
//! The client is deliberately thin: it spawns the agent, correlates JSON-RPC
//! over newline-delimited frames, folds `session/update` into the records
//! `soil:agents` already defines, and parks `session/request_permission` as a
//! prompt until a person answers it.

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde_json::{Value, json};
use soil_agent_host::{
    Activity, ActivityKind, Agent, AgentBackend, Answer, Binding, Change, Choice, Error, Goal,
    Page, Phase, Prompt, PromptKind, Role, Status, Turn, Usage,
};

/// How long an ordinary request may take before it is treated as failed. A
/// prompt is exempt: a turn runs for minutes.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

/// How much of a turn a view keeps. The stage draws a ring, not a transcript.
const ACTIVITY_LIMIT: usize = 12;

/// How many transcript turns one session keeps in memory. A page is what a
/// focus view draws; the rest stays in whatever store the harness owns.
const TRANSCRIPT_LIMIT: usize = 400;

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// One thing the agent sent that the backend has not turned into a change yet.
#[derive(Debug)]
enum Event {
    Update {
        session: String,
        update: Value,
    },
    Permission {
        session: String,
        prompt: Prompt,
    },
    PromptDone {
        session: String,
        stop: Option<String>,
    },
}

/// The connection to one ACP agent process.
struct Acp {
    stdin: Mutex<Option<ChildStdin>>,
    child: Mutex<Child>,
    next_id: AtomicU64,
    pending: Mutex<HashMap<u64, Sender<Result<Value, String>>>>,
    /// Prompt id → the JSON-RPC request that asked for it.
    permissions: Mutex<HashMap<String, u64>>,
    events: Sender<Event>,
}

impl Acp {
    fn spawn(command: &str) -> Result<(Arc<Self>, Receiver<Event>), String> {
        let mut parts = command.split_whitespace();
        let program = parts.next().ok_or("the ACP command is empty")?;
        let mut child = Command::new(program)
            .args(parts)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .map_err(|error| format!("cannot start {program}: {error}"))?;
        let stdin = child.stdin.take().ok_or("the ACP agent took no input")?;
        let stdout = child
            .stdout
            .take()
            .ok_or("the ACP agent opened no output")?;
        let (events, inbox) = mpsc::channel();
        let acp = Arc::new(Self {
            stdin: Mutex::new(Some(stdin)),
            child: Mutex::new(child),
            next_id: AtomicU64::new(1),
            pending: Mutex::new(HashMap::new()),
            permissions: Mutex::new(HashMap::new()),
            events,
        });
        let reader = Arc::clone(&acp);
        thread::Builder::new()
            .name("acp-reader".into())
            .spawn(move || reader.read(stdout))
            .map_err(|error| format!("cannot start the ACP reader: {error}"))?;
        Ok((acp, inbox))
    }

    /// Route every frame the agent sends: a response settles a pending request,
    /// a permission request becomes a prompt, anything else is an update.
    fn read(&self, stdout: impl std::io::Read) {
        for line in BufReader::new(stdout).lines() {
            let Ok(line) = line else { break };
            let Ok(frame) = serde_json::from_str::<Value>(line.trim()) else {
                continue;
            };
            let method = frame.get("method").and_then(Value::as_str);
            if let Some(method) = method {
                let params = frame.get("params").cloned().unwrap_or_else(|| json!({}));
                let session = params
                    .get("sessionId")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string();
                if method == "session/request_permission" {
                    let Some(request) = frame.get("id").and_then(Value::as_u64) else {
                        continue;
                    };
                    let Some(prompt) = prompt_from_permission(request, &params) else {
                        continue;
                    };
                    self.permissions
                        .lock()
                        .expect("permissions lock")
                        .insert(prompt.id.clone(), request);
                    let _ = self.events.send(Event::Permission { session, prompt });
                } else if let Some(update) = params.get("update") {
                    let _ = self.events.send(Event::Update {
                        session,
                        update: update.clone(),
                    });
                }
                continue;
            }
            let Some(id) = frame.get("id").and_then(Value::as_u64) else {
                continue;
            };
            let Some(waiting) = self.pending.lock().expect("pending lock").remove(&id) else {
                continue;
            };
            let settled = match frame.get("error") {
                Some(error) => Err(error
                    .get("message")
                    .and_then(Value::as_str)
                    .unwrap_or("the agent refused")
                    .to_string()),
                None => Ok(frame.get("result").cloned().unwrap_or(Value::Null)),
            };
            let _ = waiting.send(settled);
        }
    }

    fn write(&self, frame: &Value) -> Result<(), String> {
        let mut stdin = self.stdin.lock().expect("stdin lock");
        let stdin = stdin.as_mut().ok_or("the ACP agent is gone")?;
        serde_json::to_writer(&mut *stdin, frame).map_err(|error| error.to_string())?;
        stdin.write_all(b"\n").map_err(|error| error.to_string())?;
        stdin.flush().map_err(|error| error.to_string())
    }

    fn request(&self, method: &str, params: Value, timeout: Duration) -> Result<Value, String> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let (reply, answer) = mpsc::channel();
        self.pending.lock().expect("pending lock").insert(id, reply);
        self.write(&json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}))?;
        match answer.recv_timeout(timeout) {
            Ok(settled) => settled,
            Err(_) => {
                self.pending.lock().expect("pending lock").remove(&id);
                Err(format!("{method} timed out"))
            }
        }
    }

    fn notify(&self, method: &str, params: Value) -> Result<(), String> {
        self.write(&json!({"jsonrpc": "2.0", "method": method, "params": params}))
    }

    /// Answer a parked permission request with the option a person chose, or
    /// cancel it when they chose nothing.
    fn answer(&self, prompt_id: &str, choice: Option<&str>) -> Result<(), Error> {
        let request = self
            .permissions
            .lock()
            .expect("permissions lock")
            .remove(prompt_id)
            .ok_or(Error::NotFound)?;
        let outcome = match choice {
            Some(option) => json!({"outcome": {"outcome": "selected", "optionId": option}}),
            None => json!({"outcome": {"outcome": "cancelled"}}),
        };
        self.write(&json!({"jsonrpc": "2.0", "id": request, "result": outcome}))
            .map_err(|_| Error::Io)
    }

    fn events(&self) -> Sender<Event> {
        self.events.clone()
    }
}

impl Drop for Acp {
    fn drop(&mut self) {
        if let Ok(mut child) = self.child.lock() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

/// A cast of agents read from one ACP agent process.
pub struct AcpBackend {
    acp: Arc<Acp>,
    events: Receiver<Event>,
    agents: Vec<Agent>,
    /// Text that arrived on the stream, per session. ACP has no history call:
    /// `session/load` replays, and this is where that replay lands.
    turns: HashMap<String, Vec<Turn>>,
    binding: Binding,
    started_at_ms: u64,
}

impl AcpBackend {
    /// Start the agent and handshake. A command that cannot be started, or that
    /// refuses `initialize`, is an error rather than an empty wall of agents.
    pub fn spawn(command: &str) -> Result<Self, String> {
        let (acp, events) = Acp::spawn(command)?;
        acp.request(
            "initialize",
            json!({"protocolVersion": 1, "clientCapabilities": {}}),
            REQUEST_TIMEOUT,
        )
        .map_err(|error| format!("{command}: {error}"))?;
        Ok(Self {
            acp,
            events,
            agents: Vec::new(),
            turns: HashMap::new(),
            binding: Binding::default(),
            started_at_ms: now_ms(),
        })
    }

    /// Bind the surface this runtime was opened for. Views are the Spore's
    /// vocabulary; a hint the backend may echo or ignore.
    pub fn bind(mut self, agent: Option<String>, view: Option<String>) -> Self {
        self.binding = Binding { agent, view };
        self
    }

    /// Read the roster once. `session/list` is the ACP extension `operator` and
    /// `delta-acp` both answer; an agent without it shows only what streams.
    fn refresh(&mut self) {
        let Ok(listed) = self.acp.request("session/list", json!({}), REQUEST_TIMEOUT) else {
            return;
        };
        let sessions = listed
            .get("sessions")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let mut agents: Vec<Agent> = sessions
            .iter()
            .filter_map(|session| agent_from_session(session, self.started_at_ms))
            .collect();
        agents.sort_by_key(|agent| std::cmp::Reverse(agent.updated_at_ms));
        if !agents.is_empty() {
            self.agents = agents;
        }
    }

    fn agent(&mut self, id: &str) -> &mut Agent {
        if let Some(index) = self.agents.iter().position(|agent| agent.id == id) {
            return &mut self.agents[index];
        }
        self.agents.push(agent_stub(id, self.started_at_ms));
        let last = self.agents.len() - 1;
        &mut self.agents[last]
    }
}

impl AgentBackend for AcpBackend {
    fn list(&mut self) -> Vec<Agent> {
        if self.agents.is_empty() {
            self.refresh();
        }
        let mut agents = self.agents.clone();
        // Host order: whoever needs a person first, then most recently active.
        agents.sort_by(|a, b| {
            let needs = b.prompts.len().cmp(&a.prompts.len());
            needs.then(b.updated_at_ms.cmp(&a.updated_at_ms))
        });
        agents
    }

    fn get(&mut self, id: &str) -> Option<Agent> {
        if self.agents.is_empty() {
            self.refresh();
        }
        self.agents.iter().find(|agent| agent.id == id).cloned()
    }

    fn binding(&mut self) -> Binding {
        self.binding.clone()
    }

    fn transcript(&mut self, id: &str, before: Option<u64>, limit: u32) -> Result<Page, Error> {
        // Serve what the stream has delivered. A session that has not streamed
        // yet is an empty page, not a refusal: `session/load` is what fills it.
        let turns = self.turns.get(id).map(Vec::as_slice).unwrap_or_default();
        Ok(page(turns, before, limit))
    }

    fn answer(&mut self, id: &str, prompt_id: &str, answer: Answer) -> Result<(), Error> {
        self.acp.answer(prompt_id, answer.choice.as_deref())?;
        let agent = self.agent(id);
        agent.prompts.retain(|prompt| prompt.id != prompt_id);
        if agent.prompts.is_empty() && agent.phase == Phase::NeedsYou {
            agent.phase = Phase::Working;
        }
        agent.revision += 1;
        agent.updated_at_ms = now_ms();
        Ok(())
    }

    fn send(&mut self, id: &str, text: &str) -> Result<(), Error> {
        if self
            .agents
            .iter()
            .any(|agent| agent.id == id && !agent.prompts.is_empty())
        {
            // An answer is not a message: the host refuses one while a prompt
            // is open.
            return Err(Error::Invalid);
        }
        let acp = Arc::clone(&self.acp);
        let events = self.acp.events();
        let session = id.to_string();
        let prompt = json!({
            "sessionId": session,
            "prompt": [{"type": "text", "text": text}],
        });
        thread::Builder::new()
            .name(format!("acp-prompt-{session}"))
            .spawn(move || {
                let stop = acp
                    .request("session/prompt", prompt, Duration::from_secs(3600))
                    .ok()
                    .and_then(|result| {
                        result
                            .get("stopReason")
                            .and_then(Value::as_str)
                            .map(str::to_string)
                    });
                let _ = events.send(Event::PromptDone { session, stop });
            })
            .map_err(|_| Error::Io)?;
        let agent = self.agent(id);
        agent.phase = Phase::Thinking;
        agent.revision += 1;
        agent.updated_at_ms = now_ms();
        Ok(())
    }

    fn interrupt(&mut self, id: &str) -> Result<(), Error> {
        self.acp
            .notify("session/cancel", json!({"sessionId": id}))
            .map_err(|_| Error::Io)
    }

    fn next(&mut self, timeout_ms: u32) -> Option<Change> {
        let event = self
            .events
            .recv_timeout(Duration::from_millis(u64::from(timeout_ms)))
            .ok()?;
        let change = match event {
            Event::Update { session, update } => {
                if let Some((role, text)) = chunk_text(&update) {
                    let turns = self.turns.entry(session.clone()).or_default();
                    turns.push(Turn {
                        role,
                        text,
                        at_ms: now_ms(),
                    });
                    if turns.len() > TRANSCRIPT_LIMIT {
                        let excess = turns.len() - TRANSCRIPT_LIMIT;
                        turns.drain(..excess);
                    }
                }
                let agent = self.agent(&session);
                apply_update(agent, &update);
                agent.clone()
            }
            Event::Permission { session, prompt } => {
                let agent = self.agent(&session);
                agent.prompts.retain(|open| open.id != prompt.id);
                agent.prompts.push(prompt);
                agent.phase = Phase::NeedsYou;
                agent.revision += 1;
                agent.updated_at_ms = now_ms();
                agent.clone()
            }
            Event::PromptDone { session, stop } => {
                let agent = self.agent(&session);
                agent.phase = match stop.as_deref() {
                    Some("refusal") | Some("max_tokens") => Phase::Failed,
                    _ => Phase::Idle,
                };
                agent.revision += 1;
                agent.updated_at_ms = now_ms();
                agent.clone()
            }
        };
        Some(Change::Upserted(Box::new(change)))
    }
}

fn agent_from_session(session: &Value, started_at_ms: u64) -> Option<Agent> {
    let id = session.get("sessionId")?.as_str()?;
    let title = session
        .get("title")
        .and_then(Value::as_str)
        .filter(|title| !title.trim().is_empty())
        .unwrap_or("agent");
    let goal = session
        .get("summary")
        .and_then(Value::as_str)
        .filter(|summary| !summary.trim().is_empty())
        .map(|summary| Goal {
            text: summary.to_string(),
            set_at_ms: started_at_ms,
        });
    Some(Agent {
        id: id.to_string(),
        harness: "acp".to_string(),
        title: title.to_string(),
        workspace: session
            .get("workspace")
            .or_else(|| session.get("cwd"))
            .and_then(Value::as_str)
            .map(str::to_string),
        model: session
            .get("model")
            .and_then(Value::as_str)
            .map(str::to_string),
        phase: Phase::Idle,
        goal,
        owner: None,
        activity: Vec::new(),
        prompts: Vec::new(),
        usage: Usage::default(),
        revision: 0,
        started_at_ms,
        updated_at_ms: started_at_ms,
    })
}

fn agent_stub(id: &str, started_at_ms: u64) -> Agent {
    Agent {
        id: id.to_string(),
        harness: "acp".to_string(),
        title: id.chars().take(12).collect(),
        workspace: None,
        model: None,
        phase: Phase::Thinking,
        goal: None,
        owner: None,
        activity: Vec::new(),
        prompts: Vec::new(),
        usage: Usage::default(),
        revision: 0,
        started_at_ms,
        updated_at_ms: started_at_ms,
    }
}

/// Fold one `session/update` into the agent it belongs to. The adapter
/// classifies; the view only draws.
fn apply_update(agent: &mut Agent, update: &Value) {
    let kind = update
        .get("sessionUpdate")
        .and_then(Value::as_str)
        .unwrap_or_default();
    match kind {
        "agent_message_chunk" | "agent_thought_chunk" | "user_message_chunk" => {
            agent.phase = if kind == "user_message_chunk" {
                Phase::Thinking
            } else {
                Phase::Working
            };
        }
        "tool_call" | "tool_call_update" => {
            let label = update
                .get("title")
                .and_then(Value::as_str)
                .unwrap_or("tool")
                .to_string();
            let status = tool_status(update);
            let at_ms = now_ms();
            if let Some(existing) = agent
                .activity
                .iter_mut()
                .rev()
                .find(|activity| activity.label == label && activity.finished_at_ms.is_none())
            {
                existing.status = status;
                existing.finished_at_ms = (status != Status::Running).then_some(at_ms);
            } else {
                agent.activity.push(Activity {
                    kind: tool_kind(update),
                    label,
                    detail: update
                        .get("rawInput")
                        .map(|input| input.to_string())
                        .filter(|input| input != "null"),
                    status,
                    started_at_ms: at_ms,
                    finished_at_ms: (status != Status::Running).then_some(at_ms),
                });
                if agent.activity.len() > ACTIVITY_LIMIT {
                    agent.activity.remove(0);
                }
            }
            if status == Status::Running {
                agent.phase = Phase::Working;
            }
        }
        "available_commands_update" | "plan" => {}
        _ => {}
    }
    agent.revision += 1;
    agent.updated_at_ms = now_ms();
}

/// One page of a session's transcript, newest last.
fn page(turns: &[Turn], before: Option<u64>, limit: u32) -> Page {
    let end = before
        .map(|cursor| cursor as usize)
        .unwrap_or(turns.len())
        .min(turns.len());
    let start = end.saturating_sub(limit as usize);
    Page {
        turns: turns[start..end].to_vec(),
        before: (start > 0).then_some(start as u64),
    }
}

/// The text of one transcript chunk, and who spoke it.
fn chunk_text(update: &Value) -> Option<(Role, String)> {
    let role = match update.get("sessionUpdate")?.as_str()? {
        "user_message_chunk" => Role::User,
        "agent_message_chunk" | "agent_thought_chunk" => Role::Assistant,
        _ => return None,
    };
    let text = update
        .get("content")
        .and_then(|content| content.get("text"))
        .and_then(Value::as_str)
        .filter(|text| !text.trim().is_empty())?;
    Some((role, text.to_string()))
}

fn tool_kind(update: &Value) -> ActivityKind {
    match update
        .get("kind")
        .and_then(Value::as_str)
        .unwrap_or_default()
    {
        "read" | "search" => ActivityKind::Files,
        "edit" | "delete" | "move" => ActivityKind::Files,
        "execute" => ActivityKind::Shell,
        "think" => ActivityKind::Model,
        "fetch" => ActivityKind::Network,
        _ => ActivityKind::Other,
    }
}

fn tool_status(update: &Value) -> Status {
    match update
        .get("status")
        .and_then(Value::as_str)
        .unwrap_or_default()
    {
        "pending" => Status::Running,
        "in_progress" => Status::Running,
        "completed" => Status::Ok,
        "failed" => Status::Failed,
        _ => Status::Running,
    }
}

/// Turn a parked `session/request_permission` into the prompt a person answers.
/// `allow_always` widens authority, so it carries `danger`.
fn prompt_from_permission(request: u64, params: &Value) -> Option<Prompt> {
    let tool = params.get("toolCall")?;
    let title = tool
        .get("title")
        .and_then(Value::as_str)
        .unwrap_or("approval");
    let choices: Vec<Choice> = params
        .get("options")
        .and_then(Value::as_array)
        .map(|options| {
            options
                .iter()
                .filter_map(|option| {
                    let id = option.get("optionId").and_then(Value::as_str)?;
                    Some(Choice {
                        id: id.to_string(),
                        label: option
                            .get("name")
                            .and_then(Value::as_str)
                            .unwrap_or(id)
                            .to_string(),
                        detail: None,
                        danger: option
                            .get("kind")
                            .and_then(Value::as_str)
                            .is_some_and(|kind| kind == "allow_always"),
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    Some(Prompt {
        id: request.to_string(),
        kind: PromptKind::Approval,
        title: title.to_string(),
        body: None,
        details: tool.get("rawInput").map(|input| input.to_string()),
        origin: tool.get("kind").and_then(Value::as_str).map(str::to_string),
        choices,
        allows_text: false,
        asked_at_ms: now_ms(),
        deadline_at_ms: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_session_becomes_an_agent_with_its_goal() {
        let agent = agent_from_session(
            &json!({
                "sessionId": "s1",
                "title": "Agent surfaces",
                "summary": "One normalized agent.",
                "workspace": "/w",
            }),
            7,
        )
        .expect("a session with an id");
        assert_eq!(agent.id, "s1");
        assert_eq!(agent.title, "Agent surfaces");
        assert_eq!(
            agent.goal.as_ref().map(|goal| goal.text.as_str()),
            Some("One normalized agent.")
        );
        assert_eq!(agent.workspace.as_deref(), Some("/w"));
        assert_eq!(agent.phase, Phase::Idle);
        assert!(agent_from_session(&json!({"title": "no id"}), 0).is_none());
    }

    #[test]
    fn an_update_without_a_session_still_folds() {
        let mut agent = agent_stub("s1", 0);
        apply_update(
            &mut agent,
            &json!({"sessionUpdate": "agent_message_chunk", "content": {"type": "text", "text": "hi"}}),
        );
        assert_eq!(agent.phase, Phase::Working);
        assert_eq!(agent.revision, 1);
    }

    #[test]
    fn a_tool_call_and_its_update_are_one_station() {
        let mut agent = agent_stub("s1", 0);
        apply_update(
            &mut agent,
            &json!({"sessionUpdate": "tool_call", "toolCallId": "t", "title": "cargo test", "kind": "execute", "status": "in_progress"}),
        );
        assert_eq!(agent.activity.len(), 1);
        assert_eq!(agent.activity[0].kind, ActivityKind::Shell);
        assert_eq!(agent.activity[0].status, Status::Running);
        assert_eq!(agent.activity[0].finished_at_ms, None);

        apply_update(
            &mut agent,
            &json!({"sessionUpdate": "tool_call_update", "toolCallId": "t", "title": "cargo test", "kind": "execute", "status": "completed"}),
        );
        assert_eq!(agent.activity.len(), 1, "the update refines the station");
        assert_eq!(agent.activity[0].status, Status::Ok);
        assert!(agent.activity[0].finished_at_ms.is_some());
    }

    #[test]
    fn activity_is_bounded() {
        let mut agent = agent_stub("s1", 0);
        for index in 0..(ACTIVITY_LIMIT + 4) {
            apply_update(
                &mut agent,
                &json!({"sessionUpdate": "tool_call", "title": format!("tool {index}"), "status": "completed"}),
            );
        }
        assert_eq!(agent.activity.len(), ACTIVITY_LIMIT);
    }

    #[test]
    fn chunks_become_transcript_turns() {
        let (role, text) = chunk_text(&json!({
            "sessionUpdate": "agent_message_chunk",
            "content": {"type": "text", "text": "on it"},
        }))
        .expect("a chunk");
        assert_eq!(role, Role::Assistant);
        assert_eq!(text, "on it");
        let (role, _) = chunk_text(&json!({
            "sessionUpdate": "user_message_chunk",
            "content": {"type": "text", "text": "hello"},
        }))
        .expect("a chunk");
        assert_eq!(role, Role::User);
        assert!(chunk_text(&json!({"sessionUpdate": "tool_call"})).is_none());
        assert!(
            chunk_text(&json!({
                "sessionUpdate": "agent_message_chunk",
                "content": {"type": "text", "text": "  "},
            }))
            .is_none()
        );
    }

    #[test]
    fn a_page_walks_backwards_through_the_transcript() {
        let turns: Vec<Turn> = (0..5)
            .map(|index| Turn {
                role: Role::User,
                text: format!("turn {index}"),
                at_ms: 0,
            })
            .collect();
        let newest = page(&turns, None, 2);
        assert_eq!(newest.turns.len(), 2);
        assert_eq!(newest.turns[1].text, "turn 4");
        assert_eq!(newest.before, Some(3));

        let older = page(&turns, newest.before, 3);
        assert_eq!(older.turns.len(), 3);
        assert_eq!(older.turns[0].text, "turn 0");
        assert_eq!(older.before, None, "the oldest page ends the walk");
    }

    #[test]
    fn a_permission_request_is_a_prompt_with_real_options() {
        let prompt = prompt_from_permission(
            9,
            &json!({
                "sessionId": "s1",
                "toolCall": {"toolCallId": "t", "title": "Write README", "kind": "edit", "rawInput": {"path": "README.md"}},
                "options": [
                    {"optionId": "allow-once", "name": "Allow once", "kind": "allow_once"},
                    {"optionId": "allow-always", "name": "Always allow", "kind": "allow_always"},
                    {"optionId": "reject", "name": "Reject", "kind": "reject_once"},
                ],
            }),
        )
        .expect("a tool call");
        assert_eq!(prompt.id, "9");
        assert_eq!(prompt.kind, PromptKind::Approval);
        assert_eq!(prompt.title, "Write README");
        assert_eq!(prompt.choices.len(), 3);
        assert!(!prompt.choices[0].danger);
        assert!(prompt.choices[1].danger, "always-allow widens authority");
        assert_eq!(prompt.origin.as_deref(), Some("edit"));
        assert!(prompt.details.is_some());
        assert!(prompt_from_permission(1, &json!({})).is_none());
    }
}

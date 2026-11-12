# Agents

Status: contract. `wit/soil-agents.wit` is reviewed; the first host is the
`soil-webgpu` fixture backend, which proves the guest ABI against a scripted
cast and talks to no real harness. The Operator and Pi adapters are the next
slice (see "Adapters" below).

## The one concept

Every harness has its own words: Operator has turns, tools, a `finish_status`
terminal and an ACP server; Pi has sessions, steps and an RPC/JSON mode; a
Claude Code plugin sees hooks and permissions. Those are transport and protocol
differences, not product differences. A display that shows what an agent is
doing should not have to be written once per harness.

`soil:agents` is that single concept: **one normalized agent**, and the set of them a surface may see — identity, phase,
goal, activity, prompts, usage — plus the three things a person may do to it
(answer, send, interrupt). An **adapter** translates one harness into it. Views
are written against the contract and never learn which harness is underneath.

Two interfaces, so the authority splits with the grant:

| Interface | Needs | Functions |
|---|---|---|
| `observe` | `soil:agents.observe` | `agents`, `get`, `intent`, `transcript`, `subscribe` |
| `control` | `soil:agents.control` | `respond`, `send`, `interrupt` |

A read-only wall imports `observe` alone. A window that can answer imports both.

## Facts, not moods

The contract carries what happened, not how it feels. There is no `energy`,
`mood`, or `hunger` field, because those are opinions about a session, and two
views that disagree about an opinion produce two different creatures for the
same agent. A view that wants a creature derives vitals from the facts it
already has:

| Vital | Derived from |
|---|---|
| energy | `usage.context-tokens / usage.context-window` |
| health | the status of recent `activity` |
| hunger | `prompts` (how much it needs you) |
| growth | `usage.turns` |

The same rule keeps the *look* out of the contract: a skin is a view, and the
next skin is not an ABI change.

## The one invariant

`phase = needs-you` **if and only if** `prompts` is non-empty.

Every way an agent can need a person funnels into one record, so a surface
renders one card shape instead of one per harness:

| Harness event | `prompt-kind` |
|---|---|
| permission request (a hook, a sandbox gate) | `approval` |
| `finish_status(status = decision)` | `choice` |
| `finish_status(status = blocked)` | `question` |
| an interactive question tool | `question` or `choice` |

An adapter whose harness has no answer channel still satisfies the invariant. It
synthesizes a prompt from the terminal report and implements `respond` by sending
the person's words back through the channel that harness does have — for Operator
that is the next `session/prompt`. Answering is idempotent: the prompt id makes a
second answer `stale` rather than a second action.

## Adapters

An adapter is a host concern, for three reasons:

1. **No ambient authority.** A guest cannot open a socket or spawn a process, and
   it should not be given one to watch an agent.
2. **One classifier.** Deciding that `grep -rn` is Files and `cargo build` is
   Shell belongs in one place per harness, not in every view.
3. **The harness protocol is not a UI contract.** ACP versions and Pi's RPC
   frames move; the WIT does not.

`crates/soil-agent-host` holds the seam: a plain `Agent` record, an
`AgentBackend` trait, and backends. `crates/soil-webgpu` maps those records to
the generated WIT types, because hosts do not depend on generated types
(`CatalogHost` and `TerminalHost` follow the same rule).

| Backend | Speaks | State it needs |
|---|---|---|
| `FixtureAgents` | a scripted cast | none; answers advance the script |
| Operator | ACP JSON-RPC (`session/status`, `session/history`) plus the SSE stream | the server origin and a session id |
| Pi | its JSON/RPC mode | the process or endpoint, and a session id |

Operator's `session/status` already carries the facts this contract wants:
`running`, `context { tokens, window }`, `turn`, `finish`, `usage`, `model`,
`workspace`. `finalize`/`finish` maps directly onto `phase`, and
`finish_status(blocked|decision)` is the synthesized prompt. Pi's JSON events
carry steps and usage; its adapter classifies steps into the same activity
stations.

## Bounded by the host

Nothing here is an unbounded pipe:

- `activity` and `prompts` are short lists; an adapter keeps the last N.
- `transcript` is paged with a cursor and a host-capped `limit`.
- `details` on a prompt is preformatted evidence (a command, a diff, a failing
  test), capped like a tool preview, never a credential.
- `label` is a name, never content: two path segments, a host, a tool name.
- `workspace` may be redacted by host policy.

## Not in this contract

These are separate authorities, and a surface that answers questions should not
have them:

- **Spawn.** Starting a session is a process, and process authority is not a UI
  grant. It arrives as `soil:agent.spawn` if a host wants to offer it.
- **Configure.** Choosing a model, editing policies, or trusting a workspace is
  a different decision with a different lifetime.
- **Mutate history.** The transcript is an observation. Fork, rewind, and edits
  go through the harness's own session surface.

## The first surface

[`spores/colony`](https://github.com/fungOS-linux/spores/tree/main/spores/colony)
is the proving Spore: one window per agent, a board of every agent, and a focus
view with the transcript and a composer. It imports `observe` and `control` and
knows nothing about Operator or Pi beyond the `harness` string it prints.

## Open questions

- Whether `respond` should carry a deadline or the host should expire prompts on
  its own clock. Today the harness withdraws a prompt; the surface never guesses.
- Whether one surface may hold two bindings (a split view) or a host opens two
  surfaces. Leases are one-per-Spore today, so this is a host question.
- Whether `usage` should carry cost. It is provider- and plan-dependent, and a
  wrong cost is worse than no cost.

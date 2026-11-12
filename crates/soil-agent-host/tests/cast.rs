//! The contract's one invariant, checked against the cast a surface is proved
//! with: a prompt is the only way an agent needs a person.

use soil_agent_host::{AgentBackend, Answer, Error, FixtureAgents, Phase};

#[test]
fn needing_a_person_and_having_a_prompt_are_the_same_fact() {
    let mut cast = FixtureAgents::new();
    let agents = cast.list();
    assert!(!agents.is_empty());
    for agent in &agents {
        assert_eq!(
            agent.phase == Phase::NeedsYou,
            !agent.prompts.is_empty(),
            "{} breaks the needs-you invariant",
            agent.id
        );
    }
}

#[test]
fn the_host_orders_a_person_before_the_busy() {
    let mut cast = FixtureAgents::new();
    let agents = cast.list();
    let first_idle = agents.iter().position(|a| a.phase != Phase::NeedsYou);
    let last_needy = agents.iter().rposition(|a| a.phase == Phase::NeedsYou);
    if let (Some(idle), Some(needy)) = (first_idle, last_needy) {
        assert!(
            needy < idle,
            "a busy agent outranked one that needs a person"
        );
    }
}

#[test]
fn an_answer_resolves_its_prompt_and_moves_the_revision() {
    let mut cast = FixtureAgents::new();
    let agent = cast
        .list()
        .into_iter()
        .find(|agent| agent.phase == Phase::NeedsYou)
        .expect("the cast has an agent that needs a person");
    let prompt = agent.prompts[0].clone();
    let choice = prompt.choices.first().map(|choice| choice.id.clone());

    cast.answer(
        &agent.id,
        &prompt.id,
        Answer {
            choice: choice.clone(),
            text: choice.is_none().then(|| "remove it".to_string()),
        },
    )
    .expect("the first answer is accepted");

    let after = cast.get(&agent.id).expect("the agent is still there");
    assert!(after.prompts.is_empty());
    assert_ne!(after.phase, Phase::NeedsYou);
    assert!(after.revision > agent.revision);

    let again = cast.answer(&agent.id, &prompt.id, Answer::default());
    assert_eq!(again, Err(Error::Stale), "a second answer is stale");
}

#[test]
fn a_message_does_not_race_an_open_prompt() {
    let mut cast = FixtureAgents::new();
    let agent = cast
        .list()
        .into_iter()
        .find(|agent| !agent.prompts.is_empty())
        .expect("the cast has an open prompt");
    assert_eq!(
        cast.send(&agent.id, "also: rename the helper"),
        Err(Error::Invalid)
    );
}

#[test]
fn an_interrupt_pauses_and_leaves_the_agent_observable() {
    let mut cast = FixtureAgents::new();
    let agent = cast
        .list()
        .into_iter()
        .find(|agent| agent.phase == Phase::Working)
        .expect("the cast has a working agent");
    cast.interrupt(&agent.id).expect("interrupt is supported");
    let after = cast.get(&agent.id).expect("the agent is still there");
    assert_eq!(after.phase, Phase::Paused);
    assert!(
        after
            .activity
            .iter()
            .all(|row| row.finished_at_ms.is_some()),
        "an interrupted turn leaves no row claiming to be running"
    );
}

#[test]
fn nothing_is_visible_without_an_adapter() {
    let mut host = soil_agent_host::NoAgents;
    assert!(host.list().is_empty());
    assert_eq!(host.send("x", "hello"), Err(Error::Denied));
}

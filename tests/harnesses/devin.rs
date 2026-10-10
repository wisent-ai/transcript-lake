//! Independent expectations for an existing native Devin conversation graph.
use serde::Deserialize;
use serde_json::Value;

#[derive(Deserialize)]
pub struct Message {
    event_type: String,
    text: String,
}

#[derive(Deserialize)]
pub struct Chain {
    session_id: String,
    chain: String,
    messages: Vec<Message>,
}

#[derive(Deserialize)]
pub struct Expected {
    main: Vec<Message>,
    subagent: Chain,
    branch: Chain,
}

fn messages(events: &[&Value], expected: &[Message], session: &str) {
    assert!(!expected.is_empty(), "supply native messages for {session}");
    let mut wanted: Vec<_> = expected
        .iter()
        .map(|message| (message.event_type.as_str(), message.text.as_str()))
        .collect();
    let mut observed: Vec<_> = events
        .iter()
        .map(|event| {
            (
                event["event_type"].as_str().expect("canonical event type"),
                event["text"].as_str().expect("canonical message text"),
            )
        })
        .collect();
    wanted.sort_unstable();
    observed.sort_unstable();
    assert_eq!(observed, wanted, "wrong native chain content: {session}");
}

fn session_events<'a>(persisted: &'a [Value], session: &str) -> Vec<&'a Value> {
    persisted
        .iter()
        .filter(|event| event["runtime"] == "devin" && event["session_id"] == session)
        .collect()
}

pub fn verify(persisted: &[Value], session: &str, expected: &Expected) {
    let main = session_events(persisted, session);
    messages(&main, &expected.main, session);
    for event in main {
        assert!(event["extra"].get("parent_session").is_none());
        assert!(event["extra"].get("chain").is_none());
    }
    assert_ne!(expected.subagent.session_id, expected.branch.session_id);
    assert!(expected.branch.chain.starts_with("branch-"));
    for chain in [&expected.subagent, &expected.branch] {
        assert!(!chain.chain.is_empty(), "supply the native chain identity");
        assert_eq!(chain.session_id, format!("{session}:{}", chain.chain));
        assert_ne!(chain.session_id, session);
        let events = session_events(persisted, &chain.session_id);
        messages(&events, &chain.messages, &chain.session_id);
        for event in events {
            assert_eq!(event["extra"]["parent_session"], session);
            assert_eq!(event["extra"]["chain"], chain.chain);
        }
    }
}

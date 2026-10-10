//! The conversation chains of one Devin session, rebuilt from its node graph.
//!
//! `message_nodes` is not the conversation: every node names its parent, the
//! agent rebuilds its context chain whenever its system prefix changes (so
//! several nodes repeat one `message_id`), a subagent runs its own chain
//! rooted on a null parent and declared by a `subagent_heads` row, and a
//! rewind leaves a chain behind that no head names. Reading the table in
//! node order flattened all of that into one session and emitted every copy.
//!
//! The live conversation is the chain `sessions.main_chain_id` heads, read
//! head first so the newest copy of a repeated message is the one kept. Each
//! declared subagent head names a side chain; what is left afterwards is a
//! rewound branch, named by its root-most node nobody else covered. A node a
//! chain already covered, or a message a chain already kept, is never emitted
//! again as new branch content. A node id is a session's own: the store
//! numbers nodes per session and two sessions may use the same id, so every
//! chain here is built from one session's nodes alone (`mod.rs` reads them
//! `WHERE session_id = ?1`) and an id never reaches across sessions.

use std::collections::{BTreeMap, BTreeSet, HashSet};

/// One node as the store holds it: its parent link, the message's own id
/// (when the message carries one) and whether it is rebuilt scaffolding.
#[derive(Debug, Clone)]
pub struct Node {
    pub parent: Option<i64>,
    pub message_id: Option<String>,
    pub scaffolding: bool,
}

/// How a chain is known to the store.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Identity {
    /// The chain `sessions.main_chain_id` heads.
    Main,
    /// The chain a `subagent_heads` row declares, by its agent id.
    Subagent(String),
    /// A chain no head names, by its root-most node no other chain covered.
    Branch(i64),
}

/// One chain's nodes in conversation order, root first, after deduplication.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Chain {
    pub identity: Identity,
    pub nodes: Vec<i64>,
}

/// The parent walk from `head` up to the chain's root: `path[0]` is `head`
/// itself, the last element is the root (the node whose parent is null), so
/// the walk is head first, the reverse of conversation order. A parent that
/// names no node is the store contradicting itself, and a link that returns
/// to a node already walked is a cycle: both are refused by name.
fn walk(nodes: &BTreeMap<i64, Node>, head: i64) -> Result<Vec<i64>, String> {
    let mut path = Vec::new();
    let mut seen = HashSet::new();
    let mut at = Some(head);
    while let Some(id) = at {
        if !seen.insert(id) {
            return Err(format!(
                "node {id} is its own ancestor: the parent links from node {head} form a cycle"
            ));
        }
        let Some(node) = nodes.get(&id) else {
            return Err(format!(
                "node {id} is named as a parent on the chain from node {head} but no such \
                 node is stored"
            ));
        };
        path.push(id);
        at = node.parent;
    }
    Ok(path)
}

/// Keeps, from a head-first walk (`walk`'s order: head at index zero, root
/// last), every node whose message no chain kept before and that no chain
/// covered before; marks all of them covered. Walking head first is what
/// makes the newest copy of a repeated message the one kept; the kept list
/// is then reversed, so the chain it returns runs root first, in
/// conversation order.
fn keep(
    nodes: &BTreeMap<i64, Node>,
    head_first: &[i64],
    covered: &mut BTreeSet<i64>,
    kept_messages: &mut HashSet<String>,
) -> Vec<i64> {
    let mut kept = Vec::new();
    for &id in head_first {
        if !covered.insert(id) {
            continue;
        }
        let node = &nodes[&id];
        if node.scaffolding {
            continue;
        }
        if let Some(message) = &node.message_id {
            if !kept_messages.insert(message.clone()) {
                continue;
            }
        }
        kept.push(id);
    }
    kept.reverse();
    kept
}

/// Every chain of one session, in the order they are emitted: the main chain,
/// then each declared subagent's chain by agent id, then each rewound branch
/// by its root-most uncovered node. `main_head` is `sessions.main_chain_id`,
/// absent while a session has no live chain yet.
pub fn chains(
    nodes: &BTreeMap<i64, Node>,
    main_head: Option<i64>,
    subagent_heads: &BTreeMap<String, i64>,
) -> Result<Vec<Chain>, String> {
    let mut covered = BTreeSet::new();
    let mut kept_messages = HashSet::new();
    let mut found = Vec::new();
    if let Some(head) = main_head {
        if !nodes.contains_key(&head) {
            return Err(format!(
                "sessions.main_chain_id names node {head}, which the session's message_nodes \
                 do not hold"
            ));
        }
        let head_first = walk(nodes, head)?;
        found.push(Chain {
            identity: Identity::Main,
            nodes: keep(nodes, &head_first, &mut covered, &mut kept_messages),
        });
    }
    for (agent, &head) in subagent_heads {
        if !nodes.contains_key(&head) {
            return Err(format!(
                "subagent_heads names node {head} for agent {agent}, which the session's \
                 message_nodes do not hold"
            ));
        }
        let head_first = walk(nodes, head)?;
        found.push(Chain {
            identity: Identity::Subagent(agent.clone()),
            nodes: keep(nodes, &head_first, &mut covered, &mut kept_messages),
        });
    }
    let parents: HashSet<i64> = nodes
        .iter()
        .filter(|(id, _)| !covered.contains(id))
        .filter_map(|(_, node)| node.parent)
        .collect();
    let leaves: Vec<i64> = nodes
        .keys()
        .filter(|id| !covered.contains(id) && !parents.contains(id))
        .copied()
        .collect();
    for leaf in leaves {
        if covered.contains(&leaf) {
            continue;
        }
        let head_first = walk(nodes, leaf)?;
        let Some(&root) = head_first.iter().rev().find(|id| !covered.contains(id)) else {
            continue;
        };
        found.push(Chain {
            identity: Identity::Branch(root),
            nodes: keep(nodes, &head_first, &mut covered, &mut kept_messages),
        });
    }
    Ok(found)
}

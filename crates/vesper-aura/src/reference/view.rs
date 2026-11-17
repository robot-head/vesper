//! Rust build of view validation (frozen, #910): the Vesper build is
//! `validate` in the Loom package in vesper/.
use crate::{Event, Key, Kind, Limits, Node, Property, Value, ViewError, ViewTree};
use std::collections::{BTreeMap, BTreeSet};

impl Kind {
    pub(crate) fn has_children(self) -> bool {
        self == Self::Container
    }
}

impl Limits {
    pub(crate) fn check(&self) -> Result<(), ViewError> {
        if self.max_nodes == 0 || self.max_depth == 0 || self.max_patches == 0 {
            return Err(ViewError::InvalidLimits);
        }
        Ok(())
    }
}

pub(crate) struct Validated<'a> {
    pub nodes: BTreeMap<Key, &'a Node>,
    pub parents: BTreeMap<Key, Key>,
    pub order: Vec<Key>,
}

pub(crate) fn validate<'a>(
    tree: &'a ViewTree,
    limits: &Limits,
) -> Result<Validated<'a>, ViewError> {
    limits.check()?;
    if tree.nodes.len() > limits.max_nodes {
        return Err(ViewError::Budget("nodes"));
    }
    let mut nodes = BTreeMap::new();
    let mut parents = BTreeMap::new();
    let mut total_text = 0usize;
    // Every accepted tree must remain fully unmountable under this budget.
    let mut teardown = 1usize;
    for node in &tree.nodes {
        teardown = teardown
            .checked_add(1)
            .and_then(|n| n.checked_add(node.properties.len()))
            .and_then(|n| n.checked_add(node.subscriptions.len()))
            .and_then(|n| n.checked_add(node.children.len()))
            .ok_or(ViewError::Budget("teardown patches"))?;
        if teardown > limits.max_patches {
            return Err(ViewError::Budget("teardown patches"));
        }
        if nodes.insert(node.key, node).is_some() {
            return Err(ViewError::DuplicateKey(node.key));
        }
        if node.children.len() > limits.max_children {
            return Err(ViewError::Budget("children"));
        }
        if node.properties.len() > limits.max_properties {
            return Err(ViewError::Budget("properties"));
        }
        if node.subscriptions.len() > limits.max_subscriptions {
            return Err(ViewError::Budget("subscriptions"));
        }
        if !node.kind.has_children() && !node.children.is_empty() {
            return Err(ViewError::ChildrenOnLeaf(node.key));
        }
        for (&property, value) in &node.properties {
            let valid = property_valid(node.kind, property, value);
            if !valid {
                return Err(ViewError::PropertyType {
                    key: node.key,
                    property,
                });
            }
            if let Value::Text(text) = value {
                if text.len() > limits.max_text_bytes {
                    return Err(ViewError::Budget("property text"));
                }
                total_text = total_text
                    .checked_add(text.len())
                    .ok_or(ViewError::Budget("total text"))?;
                if total_text > limits.max_total_text_bytes {
                    return Err(ViewError::Budget("total text"));
                }
            }
        }
        for &event in node.subscriptions.keys() {
            let valid = event_valid(node.kind, event);
            if !valid {
                return Err(ViewError::UnsupportedEvent {
                    key: node.key,
                    event,
                });
            }
        }
    }
    if !nodes.contains_key(&tree.root) {
        return Err(ViewError::MissingRoot(tree.root));
    }
    for node in &tree.nodes {
        for &child in &node.children {
            if !nodes.contains_key(&child) {
                return Err(ViewError::MissingChild {
                    parent: node.key,
                    child,
                });
            }
            if parents.insert(child, node.key).is_some() {
                return Err(ViewError::MultipleParents(child));
            }
        }
    }
    if parents.contains_key(&tree.root) {
        return Err(ViewError::RootHasParent);
    }
    let mut order = Vec::new();
    let mut seen = BTreeSet::new();
    let mut stack = vec![(tree.root, 1usize)];
    while let Some((key, depth)) = stack.pop() {
        if depth > limits.max_depth {
            return Err(ViewError::Budget("depth"));
        }
        if !seen.insert(key) {
            return Err(ViewError::CycleOrDisconnected);
        }
        order.push(key);
        for &child in nodes[&key].children.iter().rev() {
            stack.push((child, depth + 1));
        }
    }
    if seen.len() != tree.nodes.len() {
        return Err(ViewError::CycleOrDisconnected);
    }
    Ok(Validated {
        nodes,
        parents,
        order,
    })
}

pub(crate) fn property_valid(kind: Kind, property: Property, value: &Value) -> bool {
    match (property, value) {
        (Property::Text, Value::Text(_)) => kind != Kind::Container,
        (Property::Value, Value::Text(_)) => kind == Kind::Input,
        (Property::Enabled | Property::Visible, Value::Bool(_)) => true,
        (Property::Width, Value::Int(width)) => *width >= 0,
        _ => false,
    }
}
pub(crate) fn event_valid(kind: Kind, event: Event) -> bool {
    matches!(
        (kind, event),
        (Kind::Button, Event::Click | Event::Focused)
            | (Kind::Input, Event::Changed | Event::Focused)
    )
}

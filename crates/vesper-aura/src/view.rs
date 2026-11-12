use std::collections::{BTreeMap, BTreeSet};

/// Keys are unique throughout one tree. The same key and widget kind retain
/// native state across property changes, reorders and reparenting.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Key(pub u64);

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Kind {
    Container,
    Text,
    Button,
    Input,
}
impl Kind {
    pub(crate) fn has_children(self) -> bool {
        self == Self::Container
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Property {
    Text,
    Value,
    Enabled,
    Visible,
    Width,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Value {
    Text(String),
    Bool(bool),
    Int(i64),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Event {
    Click,
    Changed,
    Focused,
}

/// Embedding-host callback identity. This is metadata, never a raw code pointer.
/// Changing a handler disconnects the old registration before subscribing anew.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct HandlerKey(pub u64);

/// Flat declarative input: no recursive Rust ownership or implicit event wiring.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Node {
    pub key: Key,
    pub kind: Kind,
    pub properties: BTreeMap<Property, Value>,
    pub subscriptions: BTreeMap<Event, HandlerKey>,
    pub children: Vec<Key>,
}
impl Node {
    pub fn new(key: Key, kind: Kind) -> Self {
        Self {
            key,
            kind,
            properties: BTreeMap::new(),
            subscriptions: BTreeMap::new(),
            children: Vec::new(),
        }
    }
    pub fn property(mut self, name: Property, value: Value) -> Self {
        self.properties.insert(name, value);
        self
    }
    pub fn on(mut self, event: Event, handler: HandlerKey) -> Self {
        self.subscriptions.insert(event, handler);
        self
    }
    pub fn children(mut self, children: impl IntoIterator<Item = Key>) -> Self {
        self.children = children.into_iter().collect();
        self
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ViewTree {
    pub root: Key,
    pub nodes: Vec<Node>,
}
impl ViewTree {
    pub fn new(root: Key, nodes: Vec<Node>) -> Self {
        Self { root, nodes }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Limits {
    pub max_nodes: usize,
    pub max_children: usize,
    pub max_depth: usize,
    pub max_properties: usize,
    pub max_subscriptions: usize,
    pub max_text_bytes: usize,
    pub max_total_text_bytes: usize,
    pub max_patches: usize,
}
impl Default for Limits {
    fn default() -> Self {
        Self {
            max_nodes: 4096,
            max_children: 1024,
            max_depth: 64,
            max_properties: 5,
            max_subscriptions: 3,
            max_text_bytes: 4096,
            max_total_text_bytes: 1 << 20,
            max_patches: 100_000,
        }
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

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ViewError {
    InvalidLimits,
    Budget(&'static str),
    DuplicateKey(Key),
    MissingRoot(Key),
    MissingChild { parent: Key, child: Key },
    MultipleParents(Key),
    RootHasParent,
    CycleOrDisconnected,
    ChildrenOnLeaf(Key),
    PropertyType { key: Key, property: Property },
    UnsupportedEvent { key: Key, event: Event },
    IdentityExhausted,
    Closed,
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

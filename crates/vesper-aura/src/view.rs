use std::collections::BTreeMap;

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

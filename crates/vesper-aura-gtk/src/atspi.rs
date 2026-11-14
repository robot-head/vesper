//! A small AT-SPI 2 client over GIO D-Bus, for accessibility tree checks.
//!
//! GTK 4 publishes its accessibility tree on the AT-SPI bus (at-spi2-core's
//! `org.a11y.Bus`). [`AtspiClient`] reads that tree the way a screen reader
//! does: `org.a11y.Bus.GetAddress` on the session bus, then the
//! `org.a11y.atspi.Accessible` interface (`GetChildren`, `GetRoleName`,
//! `GetState`, the `Name` property) on the accessibility bus.
//!
//! The calls are blocking. Querying this process's own GTK must happen off
//! the GTK main thread while that thread keeps iterating GLib (see
//! [`crate::mainloop::run_off_thread`]); GIO connections are thread-safe.

use gtk4::{gio, glib, prelude::*};
use std::time::Duration;

const ACCESSIBLE: &str = "org.a11y.atspi.Accessible";
const REGISTRY: &str = "org.a11y.atspi.Registry";
const ROOT: &str = "/org/a11y/atspi/accessible/root";

/// AT-SPI state bits used by the checks (`AtspiStateType`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum State {
    Editable = 7,
    Enabled = 8,
    Focusable = 11,
    Focused = 12,
    Sensitive = 24,
    Showing = 25,
    Visible = 30,
}

/// Failures talking to AT-SPI.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AtspiError {
    /// No session bus, or no `org.a11y.Bus` (at-spi2-core not installed).
    Unavailable(String),
    /// A D-Bus call failed.
    Call(String),
    /// A reply did not have the AT-SPI signature.
    Malformed(&'static str),
    /// No registered application with that name.
    NoSuchApplication(String),
    /// The tree exceeded `max_nodes` or `max_depth`.
    TooLarge,
}

/// One accessible object.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AtspiNode {
    pub role: String,
    pub name: String,
    pub states: u64,
    pub children: Vec<AtspiNode>,
}

impl AtspiNode {
    pub fn has(&self, state: State) -> bool {
        self.states & (1u64 << state as u8) != 0
    }
    /// Depth-first search for the first node with `role` and `name`.
    pub fn find(&self, role: &str, name: &str) -> Option<&AtspiNode> {
        if self.role == role && self.name == name {
            return Some(self);
        }
        self.children.iter().find_map(|c| c.find(role, name))
    }
    /// All nodes with `role`, depth-first.
    pub fn all(&self, role: &str) -> Vec<&AtspiNode> {
        let mut out = Vec::new();
        self.walk(&mut |n| {
            if n.role == role {
                out.push(n);
            }
        });
        out
    }
    fn walk<'a>(&'a self, f: &mut impl FnMut(&'a AtspiNode)) {
        f(self);
        for c in &self.children {
            c.walk(f);
        }
    }
}

/// An accessibility problem found by [`audit`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Issue {
    pub role: String,
    pub problem: &'static str,
}

/// Roles a user operates directly; each needs an accessible name
/// (WCAG 2.2 success criterion 4.1.2, "Name, Role, Value").
pub const INTERACTIVE_ROLES: &[&str] = &["button", "push button", "text box", "entry", "check box"];

/// Check that every showing interactive object has a nonempty name.
pub fn audit(root: &AtspiNode) -> Vec<Issue> {
    let mut issues = Vec::new();
    root.walk(&mut |n| {
        if INTERACTIVE_ROLES.contains(&n.role.as_str())
            && n.has(State::Showing)
            && n.name.trim().is_empty()
        {
            issues.push(Issue {
                role: n.role.clone(),
                problem: "interactive object without an accessible name",
            });
        }
    });
    issues
}

/// A connection to the AT-SPI accessibility bus.
pub struct AtspiClient {
    bus: gio::DBusConnection,
    timeout_ms: i32,
}

impl AtspiClient {
    /// Connect through the session bus's `org.a11y.Bus`.
    pub fn connect(timeout: Duration) -> Result<Self, AtspiError> {
        let timeout_ms = i32::try_from(timeout.as_millis()).unwrap_or(i32::MAX);
        let session = gio::bus_get_sync(gio::BusType::Session, gio::Cancellable::NONE)
            .map_err(|e| AtspiError::Unavailable(e.to_string()))?;
        let reply = session
            .call_sync(
                Some("org.a11y.Bus"),
                "/org/a11y/bus",
                "org.a11y.Bus",
                "GetAddress",
                None,
                Some(glib::VariantTy::new("(s)").expect("static type")),
                gio::DBusCallFlags::NONE,
                timeout_ms,
                gio::Cancellable::NONE,
            )
            .map_err(|e| AtspiError::Unavailable(e.to_string()))?;
        let address: String = reply
            .child_value(0)
            .get()
            .ok_or(AtspiError::Malformed("GetAddress"))?;
        let bus = gio::DBusConnection::for_address_sync(
            &address,
            gio::DBusConnectionFlags::AUTHENTICATION_CLIENT
                | gio::DBusConnectionFlags::MESSAGE_BUS_CONNECTION,
            None::<&gio::DBusAuthObserver>,
            gio::Cancellable::NONE,
        )
        .map_err(|e| AtspiError::Unavailable(e.to_string()))?;
        Ok(Self { bus, timeout_ms })
    }

    fn call(
        &self,
        dest: &str,
        path: &str,
        iface: &str,
        method: &str,
        args: Option<&glib::Variant>,
        reply: &str,
    ) -> Result<glib::Variant, AtspiError> {
        self.bus
            .call_sync(
                Some(dest),
                path,
                iface,
                method,
                args,
                Some(glib::VariantTy::new(reply).map_err(|_| AtspiError::Malformed("type"))?),
                gio::DBusCallFlags::NONE,
                self.timeout_ms,
                gio::Cancellable::NONE,
            )
            .map_err(|e| AtspiError::Call(format!("{method} on {path}: {e}")))
    }

    fn children(&self, dest: &str, path: &str) -> Result<Vec<(String, String)>, AtspiError> {
        let reply = self.call(dest, path, ACCESSIBLE, "GetChildren", None, "(a(so))")?;
        let array = reply.child_value(0);
        (0..array.n_children())
            .map(|i| {
                let item = array.child_value(i);
                let bus: String = item
                    .child_value(0)
                    .get()
                    .ok_or(AtspiError::Malformed("so"))?;
                let path = item
                    .child_value(1)
                    .str()
                    .ok_or(AtspiError::Malformed("so"))?
                    .to_string();
                Ok((bus, path))
            })
            .collect()
    }

    fn name(&self, dest: &str, path: &str) -> Result<String, AtspiError> {
        let reply = self.call(
            dest,
            path,
            "org.freedesktop.DBus.Properties",
            "Get",
            Some(&(ACCESSIBLE, "Name").to_variant()),
            "(v)",
        )?;
        let inner = reply
            .child_value(0)
            .as_variant()
            .ok_or(AtspiError::Malformed("Name"))?;
        inner.get::<String>().ok_or(AtspiError::Malformed("Name"))
    }

    fn node(
        &self,
        dest: &str,
        path: &str,
        depth: usize,
        budget: &mut usize,
        max_depth: usize,
    ) -> Result<AtspiNode, AtspiError> {
        if *budget == 0 || depth > max_depth {
            return Err(AtspiError::TooLarge);
        }
        *budget -= 1;
        let role: String = self
            .call(dest, path, ACCESSIBLE, "GetRoleName", None, "(s)")?
            .child_value(0)
            .get()
            .ok_or(AtspiError::Malformed("GetRoleName"))?;
        let states = self.call(dest, path, ACCESSIBLE, "GetState", None, "(au)")?;
        let words = states.child_value(0);
        let mut bits = 0u64;
        for i in 0..words.n_children().min(2) {
            let word: u32 = words
                .child_value(i)
                .get()
                .ok_or(AtspiError::Malformed("au"))?;
            bits |= u64::from(word) << (32 * i);
        }
        let name = self.name(dest, path)?;
        let children = self
            .children(dest, path)?
            .into_iter()
            .map(|(bus, child)| self.node(&bus, &child, depth + 1, budget, max_depth))
            .collect::<Result<_, _>>()?;
        Ok(AtspiNode {
            role,
            name,
            states: bits,
            children,
        })
    }

    /// Names of the applications registered with the AT-SPI registry.
    pub fn applications(&self) -> Result<Vec<String>, AtspiError> {
        self.children(REGISTRY, ROOT)?
            .iter()
            .map(|(bus, path)| self.name(bus, path))
            .collect()
    }

    /// The accessibility tree of the registered application `name`.
    pub fn application(
        &self,
        name: &str,
        max_nodes: usize,
        max_depth: usize,
    ) -> Result<AtspiNode, AtspiError> {
        for (bus, path) in self.children(REGISTRY, ROOT)? {
            if self.name(&bus, &path)? == name {
                let mut budget = max_nodes;
                return self.node(&bus, &path, 0, &mut budget, max_depth);
            }
        }
        Err(AtspiError::NoSuchApplication(name.to_string()))
    }
}

//! Rust build of the identity counter (frozen, #910).
use crate::{SlotId, SubscriptionId, WidgetId};

#[derive(Debug)]
pub(crate) struct Ids {
    pub session: u64,
    pub serial: u64,
}
impl Ids {
    fn next(&mut self) -> Option<u64> {
        let n = self.serial;
        self.serial = n.checked_add(1)?;
        Some(n)
    }
    pub fn widget(&mut self) -> Option<WidgetId> {
        Some(WidgetId {
            session: self.session,
            serial: self.next()?,
        })
    }
    pub fn slot(&mut self) -> Option<SlotId> {
        Some(SlotId {
            session: self.session,
            serial: self.next()?,
        })
    }
    pub fn subscription(&mut self) -> Option<SubscriptionId> {
        Some(SubscriptionId {
            session: self.session,
            serial: self.next()?,
        })
    }
}

//! A bounded, deterministic virtual message network (`net-v1`).
//!
//! Endpoints are `u64` addresses owned by simulated processes. A message sent
//! from `a` to `b` is in flight for the link delay. Policy:
//! - A send over a partitioned link, or to an endpoint whose process is down,
//!   is dropped at send time; the sender still gets its message id, as with
//!   a datagram the network silently loses.
//! - On arrival, a message is dropped if the link is partitioned *then* or the
//!   destination process is down; otherwise it goes to the oldest waiting
//!   receive request of the destination, or to its bounded inbox.
//! - A crash discards the endpoint's inbox and waiting receivers. Messages
//!   already in flight to it are not recalled; they are delivered to the
//!   restarted process if it is up when they arrive.
//! - Capacity is reserved at send time: in-flight messages to an endpoint plus
//!   its inbox never exceed `max_inbox`, and the network never holds more than
//!   `max_in_flight` messages. Exceeding a bound is a budget error before
//!   anything is retained, never a silent drop.
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet, VecDeque};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Message {
    pub id: u64,
    pub from: u64,
    pub to: u64,
    /// Canonical JSON of the checked message value.
    pub payload: String,
    pub deliver_at_ns: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum DropReason {
    Partitioned,
    DestinationDown,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NetError {
    MessageTooLarge { bytes: u64, limit: u64 },
    InFlightFull { limit: u64 },
    InboxFull { endpoint: u64, limit: u64 },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NetLimits {
    pub max_in_flight: u64,
    pub max_inbox: u64,
    pub max_message_bytes: u64,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Endpoint {
    pub inbox: VecDeque<Message>,
    /// Waiting receive requests, oldest first.
    pub waiters: VecDeque<u64>,
    /// Messages in flight to this endpoint.
    pub reserved: u64,
}

/// What a send did, before the caller schedules any arrival.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Sent {
    InFlight(Message),
    Dropped(Message, DropReason),
}

/// What an arrival did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Arrival {
    /// Handed to this waiting receive request.
    Delivered(Message, u64),
    Queued(Message),
    Dropped(Message, DropReason),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Network {
    limits: NetLimits,
    default_delay_ns: u64,
    delays: BTreeMap<(u64, u64), u64>,
    endpoints: BTreeMap<u64, Endpoint>,
    partitions: BTreeSet<(u64, u64)>,
    in_flight: BTreeMap<u64, Message>,
    next_message: u64,
}

fn link(a: u64, b: u64) -> (u64, u64) {
    (a.min(b), a.max(b))
}

impl Network {
    pub fn new(
        limits: NetLimits,
        default_delay_ns: u64,
        delays: BTreeMap<(u64, u64), u64>,
        endpoints: impl IntoIterator<Item = u64>,
    ) -> Self {
        Self {
            limits,
            default_delay_ns,
            delays,
            endpoints: endpoints
                .into_iter()
                .map(|e| (e, Endpoint::default()))
                .collect(),
            partitions: BTreeSet::new(),
            in_flight: BTreeMap::new(),
            next_message: 1,
        }
    }
    pub fn has_endpoint(&self, endpoint: u64) -> bool {
        self.endpoints.contains_key(&endpoint)
    }
    pub fn endpoint(&self, endpoint: u64) -> Option<&Endpoint> {
        self.endpoints.get(&endpoint)
    }
    pub fn endpoints(&self) -> &BTreeMap<u64, Endpoint> {
        &self.endpoints
    }
    pub fn in_flight(&self) -> &BTreeMap<u64, Message> {
        &self.in_flight
    }
    pub fn partitions(&self) -> &BTreeSet<(u64, u64)> {
        &self.partitions
    }
    pub fn is_partitioned(&self, a: u64, b: u64) -> bool {
        self.partitions.contains(&link(a, b))
    }
    pub fn delay(&self, from: u64, to: u64) -> u64 {
        self.delays
            .get(&(from, to))
            .copied()
            .unwrap_or(self.default_delay_ns)
    }
    /// Returns whether the partition is new.
    pub fn partition(&mut self, a: u64, b: u64) -> bool {
        self.partitions.insert(link(a, b))
    }
    /// Returns whether a partition was removed.
    pub fn heal(&mut self, a: u64, b: u64) -> bool {
        self.partitions.remove(&link(a, b))
    }

    /// Validate bounds and either put the message in flight or drop it.
    /// `to` must be a known endpoint; `destination_up` is its process state.
    pub fn send(
        &mut self,
        from: u64,
        to: u64,
        payload: String,
        now_ns: u64,
        destination_up: bool,
    ) -> Result<Sent, NetError> {
        let bytes = payload.len() as u64;
        if bytes > self.limits.max_message_bytes {
            return Err(NetError::MessageTooLarge {
                bytes,
                limit: self.limits.max_message_bytes,
            });
        }
        let id = self.next_message;
        let deliver_at_ns = now_ns.saturating_add(self.delay(from, to));
        let message = Message {
            id,
            from,
            to,
            payload,
            deliver_at_ns,
        };
        let drop = if self.is_partitioned(from, to) {
            Some(DropReason::Partitioned)
        } else if !destination_up {
            Some(DropReason::DestinationDown)
        } else {
            None
        };
        if let Some(reason) = drop {
            self.next_message += 1;
            return Ok(Sent::Dropped(message, reason));
        }
        if self.in_flight.len() as u64 >= self.limits.max_in_flight {
            return Err(NetError::InFlightFull {
                limit: self.limits.max_in_flight,
            });
        }
        let endpoint = &self.endpoints[&to];
        if endpoint.reserved + endpoint.inbox.len() as u64 >= self.limits.max_inbox {
            return Err(NetError::InboxFull {
                endpoint: to,
                limit: self.limits.max_inbox,
            });
        }
        self.next_message += 1;
        self.endpoints.get_mut(&to).unwrap().reserved += 1;
        self.in_flight.insert(id, message.clone());
        Ok(Sent::InFlight(message))
    }

    /// The message's link delay elapsed. Returns `None` for an unknown id.
    pub fn arrive(&mut self, id: u64, destination_up: bool) -> Option<Arrival> {
        let message = self.in_flight.remove(&id)?;
        let partitioned = self.is_partitioned(message.from, message.to);
        let endpoint = self.endpoints.get_mut(&message.to).unwrap();
        endpoint.reserved -= 1;
        Some(if partitioned {
            Arrival::Dropped(message, DropReason::Partitioned)
        } else if !destination_up {
            Arrival::Dropped(message, DropReason::DestinationDown)
        } else if let Some(request) = endpoint.waiters.pop_front() {
            Arrival::Delivered(message, request)
        } else {
            endpoint.inbox.push_back(message.clone());
            Arrival::Queued(message)
        })
    }

    /// Take the oldest queued message for an immediate receive.
    pub fn take_queued(&mut self, endpoint: u64) -> Option<Message> {
        self.endpoints.get_mut(&endpoint)?.inbox.pop_front()
    }
    pub fn wait(&mut self, endpoint: u64, request: u64) {
        self.endpoints
            .get_mut(&endpoint)
            .unwrap()
            .waiters
            .push_back(request);
    }
    /// Remove a waiting request (for a timeout). Returns whether it waited.
    pub fn cancel_wait(&mut self, endpoint: u64, request: u64) -> bool {
        let waiters = &mut self.endpoints.get_mut(&endpoint).unwrap().waiters;
        match waiters.iter().position(|r| *r == request) {
            Some(i) => {
                waiters.remove(i);
                true
            }
            None => false,
        }
    }
    /// Crash: drop the endpoint's inbox and waiters; returns both.
    pub fn crash(&mut self, endpoint: u64) -> (Vec<Message>, Vec<u64>) {
        let e = self.endpoints.get_mut(&endpoint).unwrap();
        (
            std::mem::take(&mut e.inbox).into(),
            std::mem::take(&mut e.waiters).into(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn net() -> Network {
        Network::new(
            NetLimits {
                max_in_flight: 2,
                max_inbox: 2,
                max_message_bytes: 8,
            },
            5,
            [((1, 2), 9)].into(),
            [1, 2, 3],
        )
    }

    #[test]
    fn delays_partitions_and_waiting_receivers() {
        let mut n = net();
        let Sent::InFlight(m) = n.send(1, 2, "1".into(), 10, true).unwrap() else {
            panic!()
        };
        assert_eq!(m.deliver_at_ns, 19);
        let Sent::InFlight(back) = n.send(2, 1, "2".into(), 10, true).unwrap() else {
            panic!()
        };
        assert_eq!(back.deliver_at_ns, 15);
        n.wait(2, 40);
        assert_eq!(n.arrive(m.id, true), Some(Arrival::Delivered(m, 40)));
        // A partition after the send drops the message at arrival time.
        assert!(n.partition(2, 1));
        assert!(matches!(
            n.arrive(back.id, true),
            Some(Arrival::Dropped(_, DropReason::Partitioned))
        ));
        assert!(matches!(
            n.send(1, 2, "3".into(), 20, true),
            Ok(Sent::Dropped(_, DropReason::Partitioned))
        ));
        assert!(n.heal(1, 2));
        assert!(!n.heal(1, 2));
        assert!(matches!(
            n.send(1, 3, "4".into(), 20, false),
            Ok(Sent::Dropped(_, DropReason::DestinationDown))
        ));
        assert_eq!(n.arrive(99, true), None);
    }

    #[test]
    fn bounds_are_checked_before_anything_is_retained() {
        let mut n = net();
        assert!(matches!(
            n.send(1, 2, "123456789".into(), 0, true),
            Err(NetError::MessageTooLarge { bytes: 9, .. })
        ));
        n.send(1, 3, "a".into(), 0, true).unwrap();
        n.send(1, 3, "b".into(), 0, true).unwrap();
        assert!(matches!(
            n.send(1, 3, "c".into(), 0, true),
            Err(NetError::InFlightFull { .. })
        ));
        let ids: Vec<u64> = n.in_flight().keys().copied().collect();
        for id in ids {
            assert!(matches!(n.arrive(id, true), Some(Arrival::Queued(_))));
        }
        assert!(matches!(
            n.send(2, 3, "d".into(), 0, true),
            Err(NetError::InboxFull { endpoint: 3, .. })
        ));
        assert_eq!(n.take_queued(3).unwrap().payload, "a");
        n.wait(3, 7);
        let (inbox, waiters) = n.crash(3);
        assert_eq!((inbox.len(), waiters), (1, vec![7]));
        assert!(!n.cancel_wait(3, 7));
    }
}

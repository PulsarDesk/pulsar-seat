// SPDX-License-Identifier: GPL-3.0-only
//! Event hub: numbers every event (`seq`), keeps a backlog of [`seat_proto::EVENT_BACKLOG`]
//! frames for `poll_events`, and pushes frames to subscribed pipe connections.
//!
//! Scoping (architecture §5): `host` topic events reach only the verified seat host of the
//! event's seat; `agent` topic events only the agent of the event's session. Other topics
//! go to every subscriber that asked for them (the request itself was authorised).
//!
//! Every subscriber has a bounded queue ([`SUBSCRIBER_QUEUE`]). A subscriber that does not
//! keep up (its client stopped reading) is dropped when its queue is full, so a client
//! can never make the service buffer events without limit. The pipe layer then closes the
//! connection (the channel ends); the client re-subscribes and catches up with
//! `poll_events` from the backlog.

use seat_core::{Clock, SeatNumber};
use seat_proto::{EVENT_BACKLOG, Event, EventBatch, EventFrame, Topic};
use std::collections::VecDeque;
use std::sync::mpsc::{Receiver, SyncSender, TrySendError, sync_channel};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

/// Frames queued per subscriber before it counts as lagging and is dropped.
pub const SUBSCRIBER_QUEUE: usize = 4096;

/// Who receives scoped events.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Scope {
    /// Verified seat host of this seat.
    pub host_seat: Option<SeatNumber>,
    /// Agent of this session.
    pub agent_session: Option<u32>,
    /// Verified kiosk shell of this seat: its seat's `input_packets` only (no host
    /// commands). Added 2026-11-13.
    pub kiosk_seat: Option<SeatNumber>,
}

impl Scope {
    /// Whether a frame may go to a client with this scope.
    pub fn allows(&self, ev: &Event) -> bool {
        match ev {
            Event::InputPackets { seat, .. } => self.host_seat == Some(*seat) || self.kiosk_seat == Some(*seat),
            Event::HostCommand { seat, .. } => self.host_seat == Some(*seat),
            Event::AgentTask { session_id, .. } => self.agent_session == Some(*session_id),
            _ => true,
        }
    }
}

struct Sub {
    topics: Vec<Topic>,
    scope: Scope,
    tx: SyncSender<EventFrame>,
}

struct Inner {
    seq: u64,
    backlog: VecDeque<EventFrame>,
    subs: Vec<Sub>,
}

pub struct EventHub {
    inner: Mutex<Inner>,
    cv: Condvar,
    clock: Arc<dyn Clock>,
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

fn wanted(topics: &[Topic], ev: &Event) -> bool {
    topics.is_empty() || topics.contains(&ev.topic())
}

impl EventHub {
    pub fn new(clock: Arc<dyn Clock>) -> Self {
        Self {
            inner: Mutex::new(Inner { seq: 0, backlog: VecDeque::with_capacity(EVENT_BACKLOG), subs: Vec::new() }),
            cv: Condvar::new(),
            clock,
        }
    }

    /// Publish one event; returns its `seq`.
    pub fn publish(&self, event: Event) -> u64 {
        let mut g = lock(&self.inner);
        g.seq += 1;
        let frame = EventFrame { seq: g.seq, at_ms: self.clock.now_ms(), event };
        // High-rate input packets are pushed but not kept in the backlog.
        if !matches!(frame.event, Event::InputPackets { .. }) {
            if g.backlog.len() == EVENT_BACKLOG {
                g.backlog.pop_front();
            }
            g.backlog.push_back(frame.clone());
        }
        g.subs.retain(|s| {
            if !wanted(&s.topics, &frame.event) || !s.scope.allows(&frame.event) {
                return true;
            }
            match s.tx.try_send(frame.clone()) {
                Ok(()) => true,
                Err(TrySendError::Full(_)) => {
                    crate::log::warn("event subscriber lagging: queue full, subscription dropped");
                    false
                }
                Err(TrySendError::Disconnected(_)) => false,
            }
        });
        let seq = g.seq;
        drop(g);
        self.cv.notify_all();
        seq
    }

    pub fn last_seq(&self) -> u64 {
        lock(&self.inner).seq
    }

    /// Register a push subscription. Dropping the receiver ends it (pruned on the next
    /// publish). Empty `topics` = every topic the scope allows.
    pub fn subscribe(&self, topics: Vec<Topic>, scope: Scope) -> Receiver<EventFrame> {
        self.subscribe_bounded(topics, scope, SUBSCRIBER_QUEUE)
    }

    /// [`Self::subscribe`] with an explicit queue size (tests).
    pub fn subscribe_bounded(&self, topics: Vec<Topic>, scope: Scope, queue: usize) -> Receiver<EventFrame> {
        let (tx, rx) = sync_channel(queue.max(1));
        lock(&self.inner).subs.push(Sub { topics, scope, tx });
        rx
    }

    #[cfg(test)]
    pub fn subscriber_count(&self) -> usize {
        lock(&self.inner).subs.len()
    }

    /// Frames with `seq > after` matching `topics` + `scope`, waiting up to `wait` when
    /// there are none yet.
    pub fn poll(&self, after: u64, topics: &[Topic], scope: Scope, wait: Duration) -> EventBatch {
        let deadline = Instant::now() + wait;
        let mut g = lock(&self.inner);
        loop {
            let events: Vec<EventFrame> = g
                .backlog
                .iter()
                .filter(|f| f.seq > after && wanted(topics, &f.event) && scope.allows(&f.event))
                .cloned()
                .collect();
            let now = Instant::now();
            if !events.is_empty() || now >= deadline {
                return EventBatch { events, last_seq: g.seq };
            }
            let (ng, _) = self.cv.wait_timeout(g, deadline - now).unwrap_or_else(|e| e.into_inner());
            g = ng;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use seat_core::mock::MockClock;
    use seat_proto::HostCommand;

    fn hub() -> EventHub {
        EventHub::new(Arc::new(MockClock::default()))
    }

    #[test]
    fn seq_backlog_and_poll() {
        let h = hub();
        assert_eq!(h.publish(Event::ConfigChanged { revision: 1 }), 1);
        h.publish(Event::DeviceRemoved { key: seat_core::DeviceKey::new("kbd;x").unwrap() });
        let b = h.poll(0, &[Topic::Status], Scope::default(), Duration::ZERO);
        assert_eq!(b.events.len(), 1);
        assert_eq!(b.last_seq, 2);
        assert!(h.poll(2, &[], Scope::default(), Duration::from_millis(10)).events.is_empty());
    }

    #[test]
    fn poll_wakes_on_publish() {
        let h = Arc::new(hub());
        let h2 = h.clone();
        let t = std::thread::spawn(move || h2.poll(0, &[], Scope::default(), Duration::from_secs(5)));
        std::thread::sleep(Duration::from_millis(50));
        h.publish(Event::ConfigChanged { revision: 2 });
        assert_eq!(t.join().unwrap().events.len(), 1);
    }

    #[test]
    fn host_events_are_scoped() {
        let h = hub();
        let two = SeatNumber::new(2).unwrap();
        let three = SeatNumber::new(3).unwrap();
        let rx2 = h.subscribe(vec![Topic::Host], Scope { host_seat: Some(two), ..Scope::default() });
        let rx_any = h.subscribe(vec![Topic::Host], Scope::default());
        let kiosk3 = h.subscribe(vec![Topic::Host], Scope { kiosk_seat: Some(three), ..Scope::default() });
        h.publish(Event::HostCommand { seat: two, command: HostCommand::Exit });
        h.publish(Event::HostCommand { seat: three, command: HostCommand::Exit });
        h.publish(Event::InputPackets { seat: three, packets: vec![] });
        h.publish(Event::InputPackets { seat: two, packets: vec![] });
        assert_eq!(rx2.try_iter().count(), 2);
        assert_eq!(rx_any.try_iter().count(), 0);
        // A kiosk shell gets its own seat's packets only, never host commands.
        let k: Vec<_> = kiosk3.try_iter().collect();
        assert_eq!(k.len(), 1);
        assert!(matches!(k[0].event, Event::InputPackets { seat, .. } if seat == three));
        drop(kiosk3);
        drop(rx2);
        h.publish(Event::HostCommand { seat: two, command: HostCommand::Exit });
        h.publish(Event::InputPackets { seat: three, packets: vec![] });
        assert_eq!(h.subscriber_count(), 1);
    }

    #[test]
    fn lagging_subscriber_is_dropped_not_buffered() {
        let h = hub();
        let slow = h.subscribe_bounded(vec![], Scope::default(), 3);
        for r in 0..10 {
            h.publish(Event::ConfigChanged { revision: r });
        }
        assert_eq!(h.subscriber_count(), 0);
        // What was queued before the drop is still readable, then the channel ends.
        assert_eq!(slow.iter().count(), 3);
    }
}

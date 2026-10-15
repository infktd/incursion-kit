//! In-process server-sent-events hub: topic subscriptions and event fan-out.
//!
//! Replaces the legacy waitlist's separate SSE server. Topic and event names are part of the wire
//! contract with the frontend and are kept exactly as before, typos included (see [`topics`] and
//! [`events`]).
//!
//! Delivery semantics match the legacy server: no persistence, no replay, no event ids. Clients
//! treat most events as "something changed, refetch", and refetch everything when they
//! (re)connect.
//!
//! # Slow consumers
//!
//! Each subscriber has a bounded buffer ([`Hub::DEFAULT_BUFFER`] events). Publishing never blocks
//! or waits: if a subscriber's buffer is full, that subscriber is **disconnected** — its stream
//! yields what it already buffered and then ends. The HTTP response finishes, the browser's
//! `EventSource` reconnects, and the page refetches its state. This bounds memory per client and
//! keeps one stuck connection from slowing everybody else down, at the cost of a refetch for the
//! slow client (which would otherwise have missed events anyway).

use std::collections::{HashMap, HashSet};
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::task::{Context, Poll};

use futures::Stream;
use serde::Serialize;
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;

/// Topic names used by the API.
pub mod topics {
    /// Announcement changes, for everyone. (Misspelled in the legacy protocol; keep it.)
    pub const ANNOUNCEMENTS: &str = "announcments";
    /// Waitlist changes, for everyone.
    pub const WAITLIST: &str = "waitlist";
    /// Fleet composition changes, for accounts with `fleet-view`.
    pub const FLEET_COMP: &str = "fleet_comp";
    /// Incursion tracker changes, for everyone (served on the public `/api/incursions/stream`
    /// and `/api/public/stream`, and on the authenticated stream with `?public=1`). Not part of
    /// the legacy catalogue.
    pub const INCURSIONS: &str = "incursions";
    /// Public waitlist status changes, for everyone (served on the public `/api/public/stream`,
    /// and on the authenticated stream with `?public=1`). Not part of the legacy catalogue.
    pub const STATUS: &str = "status";
    /// Community data file changes, for everyone (served on the authenticated and the public
    /// stream). Not part of the legacy catalogue.
    pub const DATA: &str = "data";
    /// Wiki changes readable by everyone: public, published pages and sections (served on the
    /// authenticated and the public stream). Not part of the legacy catalogue.
    pub const WIKI: &str = "wiki";
    /// Wiki changes of pages anonymous visitors can't see (drafts, login- or permission-gated
    /// pages), on the authenticated stream only. Not part of the legacy catalogue.
    pub const WIKI_RESTRICTED: &str = "wiki_restricted";

    /// The topics of the public stream (`/api/public/stream`); the authenticated stream adds
    /// them with `?public=1`, so a logged-in page needs one connection.
    pub const PUBLIC: [&str; 4] = [STATUS, INCURSIONS, DATA, WIKI];

    /// The per-account topic: `account;<account_id>` (account id = main character id).
    pub fn account(account_id: i64) -> String {
        format!("account;{account_id}")
    }
}

/// Event names used by the API.
pub mod events {
    /// On [`super::topics::WAITLIST`]: JSON `{"waitlist_id": i64}`; clients refetch.
    pub const WAITLIST_UPDATE: &str = "waitlist_update";
    /// On [`super::topics::FLEET_COMP`]: JSON `{"fleet_id": i64}`; clients refetch.
    pub const COMP_UPDATE: &str = "comp_update";
    /// On `account;<id>`: plain text invite notification.
    pub const WAKEUP: &str = "wakeup";
    /// On `account;<id>`: JSON `{"message": String}`.
    pub const MESSAGE: &str = "message";
    /// On [`super::topics::ANNOUNCEMENTS`]: JSON list of all active announcements.
    pub const ANNOUNCEMENT_NEW: &str = "announcment;new";
    /// On [`super::topics::ANNOUNCEMENTS`]: JSON list of all active announcements.
    pub const ANNOUNCEMENT_UPDATED: &str = "announcment;updated";
    /// On [`super::topics::INCURSIONS`]: JSON `{"spawn_ids": [i64], "changes": [...],
    /// "influence_changed": [i64]}`; clients refetch `/api/incursions`.
    pub const INCURSIONS_UPDATE: &str = "incursions_update";
    /// On [`super::topics::STATUS`]: JSON `{"waitlist_id": i64}`; clients refetch
    /// `/api/public/status`.
    pub const STATUS_UPDATE: &str = "status_update";
    /// On [`super::topics::DATA`]: JSON `{"file": "fits.dat"}` (`null` after a reload of every
    /// file); clients refetch what they show from the data files (`/api/fittings`, …).
    pub const DATA_UPDATE: &str = "data_update";
    /// On [`super::topics::WIKI`] and [`super::topics::WIKI_RESTRICTED`]: JSON
    /// `{"slug": "guides/newbro" or null, "section": key or null, "change": "updated"}`; clients
    /// showing that page (or the index, for any event) refetch.
    pub const WIKI_UPDATE: &str = "wiki_update";
}

/// One published event, shared between all subscribers that receive it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Event {
    pub topic: String,
    /// SSE event name (`event:` field).
    pub name: String,
    /// SSE payload (`data:` field): JSON text or plain text, depending on the event.
    pub data: String,
}

type SubscriberId = u64;
type Sender = mpsc::Sender<Arc<Event>>;

#[derive(Default)]
struct Registry {
    /// topic → subscribers interested in it.
    topics: HashMap<String, HashMap<SubscriberId, Sender>>,
    /// subscriber → its topics (for cleanup).
    subscribers: HashMap<SubscriberId, Vec<String>>,
}

impl Registry {
    fn remove(&mut self, id: SubscriberId) {
        for topic in self.subscribers.remove(&id).unwrap_or_default() {
            if let Some(subs) = self.topics.get_mut(&topic) {
                subs.remove(&id);
                if subs.is_empty() {
                    self.topics.remove(&topic);
                }
            }
        }
    }
}

struct Inner {
    registry: Mutex<Registry>,
    next_id: AtomicU64,
    buffer: usize,
}

impl Inner {
    fn registry(&self) -> MutexGuard<'_, Registry> {
        // The registry stays consistent even if a holder panicked (all updates are single
        // map operations), so recover from poisoning instead of propagating it.
        self.registry.lock().unwrap_or_else(|e| e.into_inner())
    }
}

/// Topic-based fan-out of events to live subscribers. Cheap to clone.
#[derive(Clone)]
pub struct Hub {
    inner: Arc<Inner>,
}

impl Default for Hub {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Debug for Hub {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Hub")
            .field("subscribers", &self.subscriber_count())
            .finish()
    }
}

impl Hub {
    /// Events buffered per subscriber before it is considered too slow and disconnected.
    pub const DEFAULT_BUFFER: usize = 64;

    pub fn new() -> Self {
        Self::with_buffer(Self::DEFAULT_BUFFER)
    }

    /// A hub whose subscribers buffer at most `buffer` events (minimum 1).
    pub fn with_buffer(buffer: usize) -> Self {
        Self {
            inner: Arc::new(Inner {
                registry: Mutex::new(Registry::default()),
                next_id: AtomicU64::new(1),
                buffer: buffer.max(1),
            }),
        }
    }

    /// Subscribe to `topics`. The stream ends when the subscriber is disconnected for being too
    /// slow; dropping the stream unsubscribes.
    pub fn subscribe(&self, topics: Vec<String>) -> Subscription {
        let id = self.inner.next_id.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = mpsc::channel(self.inner.buffer);
        let topics: Vec<String> = topics
            .into_iter()
            .collect::<HashSet<_>>()
            .into_iter()
            .collect();

        let mut registry = self.inner.registry();
        for topic in &topics {
            registry
                .topics
                .entry(topic.clone())
                .or_default()
                .insert(id, tx.clone());
        }
        registry.subscribers.insert(id, topics);
        drop(registry);

        Subscription {
            stream: ReceiverStream::new(rx),
            guard: SubscriptionGuard {
                id,
                hub: Arc::downgrade(&self.inner),
            },
        }
    }

    /// Publish a raw (already encoded) event to everyone subscribed to `topic`.
    ///
    /// Never blocks. Subscribers whose buffer is full are disconnected (see the crate docs).
    /// Event names must not contain line breaks; such events are dropped with an error log.
    pub fn publish(&self, topic: &str, name: &str, data: impl Into<String>) {
        if name.contains(['\r', '\n']) {
            tracing::error!(
                topic,
                name,
                "dropping live event: event name contains a newline"
            );
            return;
        }

        let mut registry = self.inner.registry();
        let Some(subscribers) = registry.topics.get(topic) else {
            return;
        };

        let event = Arc::new(Event {
            topic: topic.to_owned(),
            name: name.to_owned(),
            data: data.into(),
        });

        let mut dropped = Vec::new();
        for (id, tx) in subscribers {
            match tx.try_send(event.clone()) {
                Ok(()) => {}
                Err(mpsc::error::TrySendError::Full(_)) => {
                    tracing::debug!(subscriber = id, topic, "disconnecting slow live subscriber");
                    dropped.push(*id);
                }
                Err(mpsc::error::TrySendError::Closed(_)) => dropped.push(*id),
            }
        }
        for id in dropped {
            registry.remove(id);
        }
    }

    /// Publish `data` serialized as JSON.
    pub fn publish_json<T: Serialize + ?Sized>(
        &self,
        topic: &str,
        name: &str,
        data: &T,
    ) -> Result<(), serde_json::Error> {
        let encoded = serde_json::to_string(data)?;
        self.publish(topic, name, encoded);
        Ok(())
    }

    /// A waitlist changed (entries, open state, name, linked fleets): `waitlist_update` on
    /// [`topics::WAITLIST`] and `status_update` on [`topics::STATUS`], both with the JSON data
    /// `{"waitlist_id": <id>}`.
    pub fn waitlist_changed(&self, waitlist_id: i64) {
        let data = format!("{{\"waitlist_id\":{waitlist_id}}}");
        self.publish(topics::WAITLIST, events::WAITLIST_UPDATE, data.clone());
        self.publish(topics::STATUS, events::STATUS_UPDATE, data);
    }

    /// A community data file changed (`file`, e.g. `fits.dat`), or every file was reloaded
    /// (`None`): `data_update` on [`topics::DATA`] with the JSON data `{"file": <name or null>}`.
    pub fn data_changed(&self, file: Option<&str>) {
        let data = serde_json::json!({ "file": file }).to_string();
        self.publish(topics::DATA, events::DATA_UPDATE, data);
    }

    /// Number of currently connected subscribers.
    pub fn subscriber_count(&self) -> usize {
        self.inner.registry().subscribers.len()
    }

    /// Disconnect every current subscriber (their streams end after draining). Used on shutdown so
    /// open event streams don't keep the HTTP server from finishing; clients reconnect elsewhere.
    pub fn disconnect_all(&self) {
        let mut registry = self.inner.registry();
        registry.topics.clear();
        registry.subscribers.clear();
    }
}

/// A live subscription: a stream of events for the subscribed topics.
pub struct Subscription {
    stream: ReceiverStream<Arc<Event>>,
    guard: SubscriptionGuard,
}

impl Subscription {
    /// Internal id, useful for logging.
    pub fn id(&self) -> u64 {
        self.guard.id
    }
}

impl Stream for Subscription {
    type Item = Arc<Event>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        Pin::new(&mut self.stream).poll_next(cx)
    }
}

struct SubscriptionGuard {
    id: SubscriberId,
    hub: std::sync::Weak<Inner>,
}

impl Drop for SubscriptionGuard {
    fn drop(&mut self) {
        if let Some(inner) = self.hub.upgrade() {
            inner.registry().remove(self.id);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::StreamExt;
    use std::time::Duration;

    fn t(topics: &[&str]) -> Vec<String> {
        topics.iter().map(|s| s.to_string()).collect()
    }

    async fn next(sub: &mut Subscription) -> Option<Arc<Event>> {
        tokio::time::timeout(Duration::from_secs(1), sub.next())
            .await
            .expect("timed out waiting for event")
    }

    #[tokio::test]
    async fn waitlist_changed_reaches_both_topics() {
        let hub = Hub::new();
        let mut sub = hub.subscribe(t(&[topics::WAITLIST, topics::STATUS]));
        hub.waitlist_changed(3);
        let first = next(&mut sub).await.unwrap();
        assert_eq!(
            (
                first.topic.as_str(),
                first.name.as_str(),
                first.data.as_str()
            ),
            (
                topics::WAITLIST,
                events::WAITLIST_UPDATE,
                r#"{"waitlist_id":3}"#
            )
        );
        let second = next(&mut sub).await.unwrap();
        assert_eq!(
            (
                second.topic.as_str(),
                second.name.as_str(),
                second.data.as_str()
            ),
            (
                topics::STATUS,
                events::STATUS_UPDATE,
                r#"{"waitlist_id":3}"#
            )
        );
    }

    #[tokio::test]
    async fn delivers_only_subscribed_topics() {
        let hub = Hub::new();
        let mut a = hub.subscribe(t(&[topics::WAITLIST, &topics::account(1)]));
        let mut b = hub.subscribe(t(&[topics::WAITLIST, &topics::account(2)]));

        hub.publish(&topics::account(1), events::WAKEUP, "hello");
        hub.publish_json(
            topics::WAITLIST,
            events::WAITLIST_UPDATE,
            &serde_json::json!({"waitlist_id": 1}),
        )
        .unwrap();
        hub.publish(topics::FLEET_COMP, events::COMP_UPDATE, "{}");

        let first = next(&mut a).await.unwrap();
        assert_eq!(first.name, "wakeup");
        assert_eq!(first.data, "hello");
        assert_eq!(first.topic, "account;1");
        let second = next(&mut a).await.unwrap();
        assert_eq!(second.name, "waitlist_update");
        assert_eq!(second.data, r#"{"waitlist_id":1}"#);

        let only = next(&mut b).await.unwrap();
        assert_eq!(only.name, "waitlist_update");
        assert!(
            tokio::time::timeout(Duration::from_millis(50), b.next())
                .await
                .is_err(),
            "b must not receive other topics"
        );
    }

    #[tokio::test]
    async fn duplicate_topics_deliver_once() {
        let hub = Hub::new();
        let mut a = hub.subscribe(t(&["x", "x"]));
        hub.publish("x", "e", "1");
        hub.publish("x", "e", "2");
        assert_eq!(next(&mut a).await.unwrap().data, "1");
        assert_eq!(next(&mut a).await.unwrap().data, "2");
    }

    #[tokio::test]
    async fn dropping_unsubscribes() {
        let hub = Hub::new();
        let a = hub.subscribe(t(&["x", "y"]));
        let _b = hub.subscribe(t(&["x"]));
        assert_eq!(hub.subscriber_count(), 2);
        drop(a);
        assert_eq!(hub.subscriber_count(), 1);
        assert!(!hub.inner.registry().topics.contains_key("y"));
    }

    #[tokio::test]
    async fn slow_subscriber_is_disconnected_after_draining() {
        let hub = Hub::with_buffer(2);
        let mut slow = hub.subscribe(t(&["x"]));
        let mut fast = hub.subscribe(t(&["x"]));

        hub.publish("x", "e", "1");
        hub.publish("x", "e", "2");
        assert_eq!(next(&mut fast).await.unwrap().data, "1");
        assert_eq!(next(&mut fast).await.unwrap().data, "2");
        // `slow` is full now; the third event disconnects it but still reaches `fast`.
        hub.publish("x", "e", "3");
        assert_eq!(next(&mut fast).await.unwrap().data, "3");
        assert_eq!(hub.subscriber_count(), 1);

        assert_eq!(next(&mut slow).await.unwrap().data, "1");
        assert_eq!(next(&mut slow).await.unwrap().data, "2");
        assert!(
            next(&mut slow).await.is_none(),
            "stream ends after the buffered events"
        );
    }

    #[tokio::test]
    async fn legacy_names_are_preserved() {
        assert_eq!(topics::ANNOUNCEMENTS, "announcments");
        assert_eq!(events::ANNOUNCEMENT_NEW, "announcment;new");
        assert_eq!(events::ANNOUNCEMENT_UPDATED, "announcment;updated");
        assert_eq!(topics::account(42), "account;42");
    }

    #[tokio::test]
    async fn rejects_event_names_with_newlines() {
        let hub = Hub::new();
        let mut a = hub.subscribe(t(&["x"]));
        hub.publish("x", "bad\nname", "1");
        hub.publish("x", "good", "2");
        assert_eq!(next(&mut a).await.unwrap().name, "good");
    }

    #[tokio::test]
    async fn disconnect_all_ends_streams() {
        let hub = Hub::new();
        let mut a = hub.subscribe(t(&["x"]));
        hub.publish("x", "e", "1");
        hub.disconnect_all();
        assert_eq!(hub.subscriber_count(), 0);
        assert_eq!(next(&mut a).await.unwrap().data, "1");
        assert!(next(&mut a).await.is_none());
        drop(a); // dropping after disconnect is harmless
    }

    #[test]
    fn publish_without_subscribers_is_a_no_op() {
        Hub::new().publish("nobody", "e", "data");
    }
}

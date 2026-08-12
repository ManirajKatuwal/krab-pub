//! # WebSocket Ergonomic Layer
//!
//! Framework-level WebSocket primitives built on Axum's WebSocket support.
//!
//! ## Usage
//!
//! ```rust
//! use krab_core::ws::{WsMessage, WsRoom};
//!
//! let room = WsRoom::new("chat");
//!
//! // Subscribers receive everything broadcast after they subscribe.
//! let mut rx = room.subscribe();
//! let delivered = room.broadcast(WsMessage::text("Hello everyone!"));
//!
//! assert_eq!(delivered, 1);
//! assert_eq!(rx.try_recv().unwrap().to_text(), "Hello everyone!");
//! ```
//!
//! Use [`WsRoomManager`] to keep a set of named rooms:
//!
//! ```rust
//! use krab_core::ws::WsRoomManager;
//!
//! let rt = tokio::runtime::Runtime::new().unwrap();
//! rt.block_on(async {
//!     let manager = WsRoomManager::new();
//!     let _room = manager.room("chat").await;
//!     assert_eq!(manager.room_names().await, vec!["chat".to_string()]);
//! });
//! ```

use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use tokio::sync::{broadcast, RwLock};

/// Environment variable capping the number of rooms a [`WsRoomManager`] will
/// create. `0` or unset means unlimited. Read once when the manager is built.
pub const KRAB_WS_MAX_ROOMS: &str = "KRAB_WS_MAX_ROOMS";

// ── WsMessage ───────────────────────────────────────────────────────────────

/// A WebSocket message wrapper.
#[derive(Debug, Clone)]
pub enum WsMessage {
    /// Text message.
    Text(String),
    /// Binary message.
    Binary(Vec<u8>),
    /// Close frame.
    Close,
}

impl WsMessage {
    /// Create a text message.
    pub fn text(msg: impl Into<String>) -> Self {
        Self::Text(msg.into())
    }

    /// Create a binary message.
    pub fn binary(data: Vec<u8>) -> Self {
        Self::Binary(data)
    }

    /// Create a JSON message from a serializable value.
    #[cfg(any(feature = "rest", feature = "db-postgres", feature = "db-sqlite"))]
    pub fn json_value(value: &serde_json::Value) -> Self {
        Self::Text(serde_json::to_string(value).unwrap_or_default())
    }

    /// Convert to a text representation for sending.
    pub fn to_text(&self) -> String {
        match self {
            WsMessage::Text(t) => t.clone(),
            WsMessage::Binary(b) => format!("[binary: {} bytes]", b.len()),
            WsMessage::Close => String::new(),
        }
    }

    /// Returns true if this is a close message.
    pub fn is_close(&self) -> bool {
        matches!(self, WsMessage::Close)
    }
}

// ── WsRoom ──────────────────────────────────────────────────────────────────

/// A named WebSocket room supporting pub/sub broadcasting.
///
/// Clients join rooms and receive all messages broadcast to that room.
/// Connection lifetime is tracked with [`WsRoom::join`], whose returned
/// [`WsConnectionGuard`] decrements the count on drop — including when the
/// owning task panics or is aborted, so counts cannot leak.
#[derive(Debug, Clone)]
pub struct WsRoom {
    /// Name of the room.
    pub name: String,
    /// Broadcast sender.
    tx: broadcast::Sender<WsMessage>,
    /// Number of active connections.
    connection_count: Arc<AtomicUsize>,
}

impl WsRoom {
    /// Create a new room with the given name and default capacity.
    pub fn new(name: impl Into<String>) -> Self {
        Self::with_capacity(name, 256)
    }

    /// Create a new room with the given name and channel capacity.
    pub fn with_capacity(name: impl Into<String>, capacity: usize) -> Self {
        let (tx, _) = broadcast::channel(capacity);
        Self {
            name: name.into(),
            tx,
            connection_count: Arc::new(AtomicUsize::new(0)),
        }
    }

    /// Broadcast a message to all subscribers in this room.
    pub fn broadcast(&self, msg: WsMessage) -> usize {
        self.tx.send(msg).unwrap_or(0)
    }

    /// Subscribe to this room (returns a receiver).
    pub fn subscribe(&self) -> broadcast::Receiver<WsMessage> {
        self.tx.subscribe()
    }

    /// Join the room, incrementing the connection count.
    ///
    /// The returned guard decrements the count when dropped, so the count
    /// stays correct even if the connection task panics or is aborted. Hold
    /// the guard for the lifetime of the connection.
    pub fn join(&self) -> WsConnectionGuard {
        self.connection_count.fetch_add(1, Ordering::Relaxed);
        WsConnectionGuard {
            connection_count: Arc::clone(&self.connection_count),
        }
    }

    /// Track a new connection.
    #[deprecated(
        since = "0.3.0",
        note = "use `WsRoom::join()`; its guard decrements on drop and cannot leak counts on panic or abort"
    )]
    pub async fn connect(&self) {
        self.connection_count.fetch_add(1, Ordering::Relaxed);
    }

    /// Track a disconnection.
    #[deprecated(
        since = "0.3.0",
        note = "use `WsRoom::join()`; dropping the returned guard replaces manual disconnect()"
    )]
    pub async fn disconnect(&self) {
        let _ = self
            .connection_count
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |count| {
                Some(count.saturating_sub(1))
            });
    }

    /// Get the number of active connections.
    pub fn connections(&self) -> usize {
        self.connection_count.load(Ordering::Relaxed)
    }
}

/// RAII guard for a tracked room connection, returned by [`WsRoom::join`].
///
/// Decrements the room's connection count on drop, which runs on normal
/// disconnect, panic unwind, and task abort alike.
#[derive(Debug)]
pub struct WsConnectionGuard {
    connection_count: Arc<AtomicUsize>,
}

impl Drop for WsConnectionGuard {
    fn drop(&mut self) {
        let _ = self
            .connection_count
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |count| {
                Some(count.saturating_sub(1))
            });
    }
}

// ── WsRoomManager ───────────────────────────────────────────────────────────

/// Error returned by [`WsRoomManager::try_room`] when a new room cannot be
/// created.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WsRoomError {
    /// The configured room cap has been reached; no new room was created.
    RoomCapReached {
        /// The cap in effect.
        cap: usize,
    },
}

impl std::fmt::Display for WsRoomError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            WsRoomError::RoomCapReached { cap } => {
                write!(f, "websocket room cap of {cap} reached")
            }
        }
    }
}

impl std::error::Error for WsRoomError {}

/// Manages multiple named WebSocket rooms.
///
/// The number of rooms can be capped: [`WsRoomManager::new`] reads
/// `KRAB_WS_MAX_ROOMS` once at construction (`0` or unset = unlimited), and
/// [`WsRoomManager::with_max_rooms`] sets the cap in code. Empty rooms can be
/// reclaimed with [`WsRoomManager::reap_empty`].
#[derive(Debug, Clone)]
pub struct WsRoomManager {
    rooms: Arc<RwLock<HashMap<String, WsRoom>>>,
    /// Maximum number of rooms; `0` means unlimited.
    max_rooms: usize,
}

impl Default for WsRoomManager {
    fn default() -> Self {
        Self::new()
    }
}

impl WsRoomManager {
    /// Create a new room manager.
    ///
    /// The room cap is read once from the `KRAB_WS_MAX_ROOMS` environment
    /// variable; `0`, unset, or unparseable values mean unlimited.
    pub fn new() -> Self {
        Self::with_max_rooms(max_rooms_from_env())
    }

    /// Create a room manager with an explicit room cap (`0` = unlimited),
    /// ignoring `KRAB_WS_MAX_ROOMS`.
    pub fn with_max_rooms(max_rooms: usize) -> Self {
        Self {
            rooms: Arc::new(RwLock::new(HashMap::new())),
            max_rooms,
        }
    }

    /// The configured room cap (`0` = unlimited).
    pub fn max_rooms(&self) -> usize {
        self.max_rooms
    }

    /// Get or create a room, failing if creating it would exceed the cap.
    ///
    /// Fetching an existing room never fails, even at the cap.
    pub async fn try_room(&self, name: &str) -> Result<WsRoom, WsRoomError> {
        {
            let rooms = self.rooms.read().await;
            if let Some(room) = rooms.get(name) {
                return Ok(room.clone());
            }
        }

        let mut rooms = self.rooms.write().await;
        // Re-check under the write lock: another task may have created it.
        if let Some(room) = rooms.get(name) {
            return Ok(room.clone());
        }
        if self.max_rooms > 0 && rooms.len() >= self.max_rooms {
            return Err(WsRoomError::RoomCapReached {
                cap: self.max_rooms,
            });
        }
        let room = WsRoom::new(name);
        rooms.insert(name.to_string(), room.clone());
        Ok(room)
    }

    /// Get or create a room.
    ///
    /// Infallible for compatibility: if the room cap is reached, this logs a
    /// `ws_room_cap_reached` warning and returns a *detached* room that is not
    /// registered with the manager — broadcasts on it only reach subscribers
    /// obtained from that same returned handle. Use
    /// [`try_room`](Self::try_room) to observe the cap as an error instead.
    pub async fn room(&self, name: &str) -> WsRoom {
        match self.try_room(name).await {
            Ok(room) => room,
            Err(WsRoomError::RoomCapReached { cap }) => {
                tracing::warn!(room = name, cap, "ws_room_cap_reached");
                WsRoom::new(name)
            }
        }
    }

    /// List all active room names.
    pub async fn room_names(&self) -> Vec<String> {
        self.rooms.read().await.keys().cloned().collect()
    }

    /// Remove an empty room.
    pub async fn remove_room(&self, name: &str) -> bool {
        let mut rooms = self.rooms.write().await;
        if let Some(room) = rooms.get(name) {
            if room.connections() == 0 {
                rooms.remove(name);
                return true;
            }
        }
        false
    }

    /// Remove every room with zero connections, returning how many were
    /// removed. Call periodically (or after disconnects) to keep the room map
    /// bounded.
    pub async fn reap_empty(&self) -> usize {
        let mut rooms = self.rooms.write().await;
        let before = rooms.len();
        rooms.retain(|_, room| room.connections() > 0);
        before - rooms.len()
    }

    /// Get total connections across all rooms.
    pub async fn total_connections(&self) -> usize {
        let rooms = self.rooms.read().await;
        rooms.values().map(|room| room.connections()).sum()
    }
}

fn max_rooms_from_env() -> usize {
    parse_max_rooms(std::env::var(KRAB_WS_MAX_ROOMS).ok().as_deref())
}

fn parse_max_rooms(raw: Option<&str>) -> usize {
    let Some(raw) = raw else {
        return 0;
    };
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return 0;
    }
    match trimmed.parse::<usize>() {
        Ok(value) => value,
        Err(_) => {
            tracing::warn!(
                value = raw,
                "krab_ws_max_rooms_invalid_treated_as_unlimited"
            );
            0
        }
    }
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ws_message_text() {
        let msg = WsMessage::text("hello");
        assert_eq!(msg.to_text(), "hello");
        assert!(!msg.is_close());
    }

    #[test]
    fn ws_message_close() {
        let msg = WsMessage::Close;
        assert!(msg.is_close());
    }

    #[test]
    fn ws_room_broadcast() {
        let room = WsRoom::new("test");
        let mut rx = room.subscribe();

        room.broadcast(WsMessage::text("hello"));

        let received = rx.try_recv().unwrap();
        assert_eq!(received.to_text(), "hello");
    }

    #[test]
    fn ws_room_multiple_subscribers() {
        let room = WsRoom::new("multi");
        let mut rx1 = room.subscribe();
        let mut rx2 = room.subscribe();

        let count = room.broadcast(WsMessage::text("broadcast"));
        assert_eq!(count, 2);

        assert_eq!(rx1.try_recv().unwrap().to_text(), "broadcast");
        assert_eq!(rx2.try_recv().unwrap().to_text(), "broadcast");
    }

    #[test]
    fn ws_room_join_guard_tracks_connections() {
        let room = WsRoom::new("tracked");
        assert_eq!(room.connections(), 0);

        let guard_a = room.join();
        let guard_b = room.join();
        assert_eq!(room.connections(), 2);

        drop(guard_a);
        assert_eq!(room.connections(), 1);
        drop(guard_b);
        assert_eq!(room.connections(), 0);
    }

    #[tokio::test]
    #[allow(deprecated)]
    async fn ws_room_deprecated_connect_disconnect_still_count() {
        let room = WsRoom::new("legacy");
        room.connect().await;
        room.connect().await;
        assert_eq!(room.connections(), 2);

        room.disconnect().await;
        assert_eq!(room.connections(), 1);
        room.disconnect().await;
        room.disconnect().await; // extra disconnect saturates at zero
        assert_eq!(room.connections(), 0);
    }

    #[tokio::test]
    async fn ws_room_guard_releases_count_on_task_abort() {
        let room = WsRoom::new("aborted");
        let room_for_task = room.clone();

        let handle = tokio::spawn(async move {
            let _guard = room_for_task.join();
            std::future::pending::<()>().await;
        });

        // Wait until the task has joined the room.
        while room.connections() == 0 {
            tokio::task::yield_now().await;
        }
        assert_eq!(room.connections(), 1);

        handle.abort();
        let join_result = handle.await;
        assert!(join_result.unwrap_err().is_cancelled());
        assert_eq!(room.connections(), 0);
    }

    #[tokio::test]
    async fn ws_room_guard_releases_count_on_task_panic() {
        let room = WsRoom::new("panicked");
        let room_for_task = room.clone();

        let handle = tokio::spawn(async move {
            let _guard = room_for_task.join();
            panic!("connection task blew up");
        });

        let join_result = handle.await;
        assert!(join_result.unwrap_err().is_panic());
        assert_eq!(room.connections(), 0);
    }

    #[tokio::test]
    async fn ws_room_manager_creates_rooms() {
        let manager = WsRoomManager::new();

        let room1 = manager.room("chat").await;
        let _room2 = manager.room("notifications").await;

        let names = manager.room_names().await;
        assert!(names.contains(&"chat".to_string()));
        assert!(names.contains(&"notifications".to_string()));

        // Same room returned on second call
        let room1_again = manager.room("chat").await;
        assert_eq!(room1.name, room1_again.name);
    }

    #[tokio::test]
    async fn ws_room_manager_remove_empty() {
        let manager = WsRoomManager::new();
        let _room = manager.room("temp").await;

        assert!(manager.remove_room("temp").await);
        assert!(manager.room_names().await.is_empty());
    }

    #[tokio::test]
    async fn ws_room_manager_total_connections() {
        let manager = WsRoomManager::with_max_rooms(0);
        let r1 = manager.room("a").await;
        let r2 = manager.room("b").await;

        let _g1 = r1.join();
        let _g2 = r2.join();
        let _g3 = r2.join();

        assert_eq!(manager.total_connections().await, 3);
    }

    #[tokio::test]
    async fn ws_room_manager_reap_empty_removes_only_empty_rooms() {
        let manager = WsRoomManager::with_max_rooms(0);
        let busy = manager.room("busy").await;
        let _idle = manager.room("idle").await;
        let _empty = manager.room("empty").await;

        let guard = busy.join();
        assert_eq!(manager.reap_empty().await, 2);

        let names = manager.room_names().await;
        assert_eq!(names, vec!["busy".to_string()]);

        drop(guard);
        assert_eq!(manager.reap_empty().await, 1);
        assert!(manager.room_names().await.is_empty());
    }

    #[tokio::test]
    async fn ws_room_manager_try_room_enforces_cap() {
        let manager = WsRoomManager::with_max_rooms(1);
        assert_eq!(manager.max_rooms(), 1);

        let first = manager.try_room("first").await.expect("under cap");
        assert_eq!(first.name, "first");

        let err = manager.try_room("second").await.unwrap_err();
        assert_eq!(err, WsRoomError::RoomCapReached { cap: 1 });

        // Existing rooms are still retrievable at the cap.
        let again = manager.try_room("first").await.expect("existing room");
        assert_eq!(again.name, "first");
    }

    #[tokio::test]
    async fn ws_room_manager_room_returns_detached_fallback_at_cap() {
        let manager = WsRoomManager::with_max_rooms(1);
        let _registered = manager.room("registered").await;

        let detached = manager.room("overflow").await;
        assert_eq!(detached.name, "overflow");

        // The fallback room is not registered with the manager.
        assert_eq!(manager.room_names().await, vec!["registered".to_string()]);

        // It is still functional for local pub/sub on the same handle.
        let mut rx = detached.subscribe();
        detached.broadcast(WsMessage::text("still works"));
        assert_eq!(rx.try_recv().unwrap().to_text(), "still works");
    }

    #[tokio::test]
    async fn ws_room_manager_cap_frees_slots_after_reap() {
        let manager = WsRoomManager::with_max_rooms(1);
        let _room = manager.room("a").await;
        assert!(manager.try_room("b").await.is_err());

        assert_eq!(manager.reap_empty().await, 1);
        assert!(manager.try_room("b").await.is_ok());
    }

    #[test]
    fn parse_max_rooms_handles_unset_empty_invalid_and_values() {
        assert_eq!(parse_max_rooms(None), 0);
        assert_eq!(parse_max_rooms(Some("")), 0);
        assert_eq!(parse_max_rooms(Some("   ")), 0);
        assert_eq!(parse_max_rooms(Some("0")), 0);
        assert_eq!(parse_max_rooms(Some("64")), 64);
        assert_eq!(parse_max_rooms(Some(" 8 ")), 8);
        assert_eq!(parse_max_rooms(Some("not-a-number")), 0);
        assert_eq!(parse_max_rooms(Some("-3")), 0);
    }
}

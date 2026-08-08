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
use std::sync::Arc;
use tokio::sync::{broadcast, RwLock};

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
    #[cfg(any(feature = "rest", feature = "db"))]
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
#[derive(Debug, Clone)]
pub struct WsRoom {
    /// Name of the room.
    pub name: String,
    /// Broadcast sender.
    tx: broadcast::Sender<WsMessage>,
    /// Number of active connections.
    connection_count: Arc<RwLock<usize>>,
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
            connection_count: Arc::new(RwLock::new(0)),
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

    /// Track a new connection.
    pub async fn connect(&self) {
        let mut count = self.connection_count.write().await;
        *count += 1;
    }

    /// Track a disconnection.
    pub async fn disconnect(&self) {
        let mut count = self.connection_count.write().await;
        *count = count.saturating_sub(1);
    }

    /// Get the number of active connections.
    pub async fn connections(&self) -> usize {
        *self.connection_count.read().await
    }
}

// ── WsRoomManager ───────────────────────────────────────────────────────────

/// Manages multiple named WebSocket rooms.
#[derive(Debug, Clone, Default)]
pub struct WsRoomManager {
    rooms: Arc<RwLock<HashMap<String, WsRoom>>>,
}

impl WsRoomManager {
    /// Create a new room manager.
    pub fn new() -> Self {
        Self::default()
    }

    /// Get or create a room.
    pub async fn room(&self, name: &str) -> WsRoom {
        let rooms = self.rooms.read().await;
        if let Some(room) = rooms.get(name) {
            return room.clone();
        }
        drop(rooms);

        let mut rooms = self.rooms.write().await;
        rooms
            .entry(name.to_string())
            .or_insert_with(|| WsRoom::new(name))
            .clone()
    }

    /// List all active room names.
    pub async fn room_names(&self) -> Vec<String> {
        self.rooms.read().await.keys().cloned().collect()
    }

    /// Remove an empty room.
    pub async fn remove_room(&self, name: &str) -> bool {
        let mut rooms = self.rooms.write().await;
        if let Some(room) = rooms.get(name) {
            if room.connections().await == 0 {
                rooms.remove(name);
                return true;
            }
        }
        false
    }

    /// Get total connections across all rooms.
    pub async fn total_connections(&self) -> usize {
        let rooms = self.rooms.read().await;
        let mut total = 0;
        for room in rooms.values() {
            total += room.connections().await;
        }
        total
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

    #[tokio::test]
    async fn ws_room_connection_tracking() {
        let room = WsRoom::new("tracked");
        assert_eq!(room.connections().await, 0);

        room.connect().await;
        room.connect().await;
        assert_eq!(room.connections().await, 2);

        room.disconnect().await;
        assert_eq!(room.connections().await, 1);
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
        let manager = WsRoomManager::new();
        let r1 = manager.room("a").await;
        let r2 = manager.room("b").await;

        r1.connect().await;
        r2.connect().await;
        r2.connect().await;

        assert_eq!(manager.total_connections().await, 3);
    }
}

use axum::extract::ws::{Message as AxumWsMessage, WebSocket, WebSocketUpgrade};
use axum::Json;
use futures_util::{SinkExt, StreamExt};
use krab_core::ws::WsMessage;
use serde::Deserialize;
use serde_json::json;

use crate::app_state::ws_manager;

#[derive(Debug, Deserialize)]
pub(crate) struct WsPublishPayload {
    pub(crate) message: String,
}

pub(crate) async fn ws_publish_handler(
    Json(payload): Json<WsPublishPayload>,
) -> Json<serde_json::Value> {
    let room = ws_manager().room("chat").await;
    let delivered = room.broadcast(WsMessage::text(payload.message));
    Json(json!({
        "status": "published",
        "delivered": delivered
    }))
}

pub(crate) async fn ws_chat_handler(ws: WebSocketUpgrade) -> impl axum::response::IntoResponse {
    ws.on_upgrade(handle_ws_chat_socket)
}

pub(crate) async fn handle_ws_chat_socket(socket: WebSocket) {
    let room = ws_manager().room("chat").await;
    room.connect().await;
    let mut subscription = room.subscribe();
    let (mut sender, mut receiver) = socket.split();
    let room_for_sender = room.clone();

    let send_task = tokio::spawn(async move {
        while let Ok(message) = subscription.recv().await {
            match message {
                WsMessage::Text(text) => {
                    if sender.send(AxumWsMessage::Text(text.into())).await.is_err() {
                        break;
                    }
                }
                WsMessage::Binary(bin) => {
                    if sender
                        .send(AxumWsMessage::Binary(bin.into()))
                        .await
                        .is_err()
                    {
                        break;
                    }
                }
                WsMessage::Close => {
                    let _ = sender.send(AxumWsMessage::Close(None)).await;
                    break;
                }
            }
        }
        room_for_sender.disconnect().await;
    });

    while let Some(Ok(incoming)) = receiver.next().await {
        match incoming {
            AxumWsMessage::Text(text) => {
                room.broadcast(WsMessage::text(text.to_string()));
            }
            AxumWsMessage::Binary(bin) => {
                room.broadcast(WsMessage::Binary(bin.to_vec()));
            }
            AxumWsMessage::Close(_) => {
                room.broadcast(WsMessage::Close);
                break;
            }
            _ => {}
        }
    }

    send_task.abort();
    room.disconnect().await;
}

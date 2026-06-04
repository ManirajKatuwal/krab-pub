use std::collections::HashSet;
use std::sync::{Arc, OnceLock};

use krab_core::http::{HasRuntimeState, RuntimeState};
use krab_core::isr::IsrCache;
use krab_core::ws::WsRoomManager;
use reqwest::Client;
use serde::{Deserialize, Serialize};

use crate::protocol_client::ProtocolAwareClient;

#[derive(Clone, Serialize, Deserialize)]
pub(crate) struct CachedHttpPayload {
    pub(crate) body: String,
    pub(crate) content_type: String,
}

#[derive(Clone)]
pub(crate) struct AppState {
    pub(crate) runtime: RuntimeState,
    pub(crate) http_client: Client,
    pub(crate) auth_base_url: String,
    pub(crate) users_base_url: String,
    pub(crate) protocol_client: Arc<ProtocolAwareClient>,
    pub(crate) isr_cache: IsrCache,
    pub(crate) isr_revalidating: Arc<tokio::sync::Mutex<HashSet<String>>>,
    pub(crate) hmr_rx: tokio::sync::watch::Receiver<u64>,
}

pub(crate) fn ws_manager() -> &'static WsRoomManager {
    static WS_MANAGER: OnceLock<WsRoomManager> = OnceLock::new();
    WS_MANAGER.get_or_init(WsRoomManager::new)
}

impl HasRuntimeState for AppState {
    fn runtime_state(&self) -> &RuntimeState {
        &self.runtime
    }
}

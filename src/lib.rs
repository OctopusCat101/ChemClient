//! Official API 4 async functions run under the persistent host driver.
use exports::astrobox::psys_plugin_v4::{event, lifecycle};
use std::io::Read;

mod netease;
mod protocol;
mod state;
mod transcode;
mod transfer;
mod ui;
wit_bindgen::generate!({ path: "wit", world: "chem-client", generate_all });
struct ChemClient;
impl lifecycle::Guest for ChemClient {
    async fn on_load() {
        let mut cookie = String::new();
        if let Ok(file) = std::fs::File::open("data/netease.cookie") {
            let _ = file.take(65537).read_to_string(&mut cookie);
            if cookie.len() > 65536 {
                cookie.clear();
            }
        }
        state::with(|s| {
            s.cookie = cookie;
            s.status = "启用手环上的 NoirMusic，再点连接".into();
        });
    }
}
impl event::Guest for ChemClient {
    async fn on_event(kind: event::EventType, payload: String) -> String {
        match kind {
            event::EventType::TransportPacket => transfer::received(&payload).await,
            event::EventType::Timer => {
                // Host timers carry their identity and application payload.
                // Compression ticks must not multiply pending BLE polls.
                let message = serde_json::from_str::<serde_json::Value>(&payload).ok();
                let tag = message
                    .as_ref()
                    .and_then(|v| v["payload"].as_str())
                    .unwrap_or(&payload);
                match tag {
                    "music-compression" => transcode::tick().await,
                    "music-link" => transfer::tick().await,
                    _ => {}
                }
            }
            event::EventType::DeviceAction => {
                if !state::snapshot().active {
                    if let Err(error) = transfer::discover_devices().await {
                        state::error(error);
                    }
                    ui::rerender();
                }
            }
            _ => {}
        }
        String::new()
    }
    async fn on_ui_event(id: String, _: event::UiEvent, payload: String) -> String {
        ui::event(&id, &payload).await;
        String::new()
    }
    async fn on_ui_render(root: String) {
        // Reopening the UI never waits for permissions, a dialog, or Bluetooth.
        ui::render(&root);
    }
    async fn on_card_render(_: String) {}
}
export!(ChemClient);

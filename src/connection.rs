//! Reuse the host connection, then discover services without a separate UI flow.
use crate::{astrobox::psys_host_v4::device, modules, state, transfer, ui};
use std::{sync::{Mutex, OnceLock}, time::{Duration, Instant}};

struct Retry { service: Instant, music: Instant }
static RETRY: OnceLock<Mutex<Retry>> = OnceLock::new();
fn retries() -> &'static Mutex<Retry> {
    RETRY.get_or_init(|| Mutex::new(Retry { service: Instant::now(), music: Instant::now() }))
}
pub fn attempted() {
    retries().lock().unwrap_or_else(|e| e.into_inner()).service = Instant::now();
}
pub fn opened() {
    retries().lock().unwrap_or_else(|e| e.into_inner()).service = Instant::now() - Duration::from_secs(16);
}

pub use crate::deadline::bounded;

pub async fn tick() {
    let s = state::snapshot();
    if s.active || s.installing || s.discovering || s.manager_busy
        || state::action_busy() || transfer::busy() { return; }
    let (service, music) = {
        let mut last = retries().lock().unwrap_or_else(|e| e.into_inner());
        let interval = if s.addr.is_empty() { 3 } else if !s.manager_ready { 1 } else if s.boot_status >> 8 != 0 { 2 } else { 15 };
        let service = last.service.elapsed() >= Duration::from_secs(interval);
        let music = !s.direct && s.modules.iter().any(|m| m.importable())
            && last.music.elapsed() >= Duration::from_secs(1);
        if service { last.service = Instant::now(); }
        if music { last.music = Instant::now(); }
        (service, music)
    };
    if service && s.addr.is_empty() {
        state::with(|s| s.discovering = true);
        let devices = bounded(5000, device::get_connected_device_list()).await;
        state::with(|s| s.discovering = false);
        if devices.is_ok_and(|d| !d.is_empty()) { refresh(false).await; }
    } else if service && !s.manager_ready {
        let _ = modules::connect_device(&s.addr).await;
    } else if service {
        let _ = modules::refresh_background().await;
    } else if music && s.manager_ready {
        let _ = transfer::connect_device(&s.addr).await;
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum Phase {
    #[default]
    Checking,
    Modules,
    Installer(String),
    Ready,
    Failed(String),
}
impl Phase {
    pub fn busy(&self) -> bool {
        matches!(self, Self::Checking | Self::Modules)
    }
}

// Prefer the current band; a stable fallback avoids changing devices merely
// because the host returned the same connected list in a different order.
fn select_device(current: &str, devices: &[(String, String)]) -> Option<String> {
    devices
        .iter()
        .find(|d| d.0 == current)
        .or_else(|| devices.iter().min_by(|a, b| a.0.cmp(&b.0)))
        .map(|d| d.0.clone())
}

fn apply_devices(
    s: &mut state::State,
    devices: Vec<(String, String)>,
    force: bool,
) -> Option<String> {
    let selected = select_device(&s.addr, &devices);
    s.connected_devices = devices.iter().map(|d| d.0.clone()).collect();
    s.devices = devices;
    let Some(addr) = selected else {
        if !s.addr.is_empty() { s.connection_epoch = s.connection_epoch.wrapping_add(1); }
        s.addr.clear();
        s.device_name.clear();
        s.direct = false;
        s.manager_ready = false;
        s.manager_busy = false;
        s.modules.clear();
        s.module_jobs.clear();
        s.install_id.clear();
        s.cloud_modules.clear();
        s.installer_action = None;
        s.target.clear();
        s.native_delivery = false;
        s.bulk_delivery = false;
        s.safe_mode = false; s.safe_reason = 0; s.boot_status = 0; s.safe_exit_pending = false;
        s.tab = "modules".into();
        s.status.clear();
        s.connection = Phase::Failed("请先在 Astro 连接手环，然后刷新。".into());
        return None;
    };
    if s.addr != addr {
        s.connection_epoch = s.connection_epoch.wrapping_add(1);
        // Clear the old band's data before awaiting a new receive subscription.
        s.addr = addr.clone();
        s.modules.clear();
        s.module_jobs.clear();
        s.install_id.clear();
        s.cloud_modules.clear();
        s.target.clear();
        s.native_delivery = false;
        s.bulk_delivery = false;
        s.safe_mode = false; s.safe_reason = 0; s.boot_status = 0; s.safe_exit_pending = false;
        s.module_detail.clear();
        s.installer_action = None;
        s.delete_module.clear();
        s.manager_ready = false;
        s.manager_busy = false;
        s.direct = false;
        s.tab = "modules".into();
    }
    s.device_name = s
        .devices
        .iter()
        .find(|d| d.0 == addr)
        .map(|d| d.1.clone())
        .unwrap_or_default();
    if s.addr == addr
        && s.manager_ready
        && !force
        && (s.direct || !s.modules.iter().any(|m| m.importable()))
    {
        s.connection = Phase::Ready;
        return None;
    }
    s.connection = Phase::Modules;
    s.status.clear();
    Some(addr)
}

/// Startup, host device events and the single Refresh button share this path.
pub async fn refresh(force: bool) {
    if crate::cloud_download::busy() { return; }
    let busy = transfer::busy() || (!force && state::action_busy());
    let start = state::with(|s| begin_check(s, busy));
    if !start {
        return;
    }
    ui::rerender();
    let devices = match bounded(5000, device::get_connected_device_list()).await {
        Ok(devices) => devices,
        Err(e) => { state::with(|s| s.discovering = false); failed(e); return; }
    }
        .into_iter()
        .map(|d| (d.addr, d.name))
        .collect();
    let previous = state::snapshot().addr;
    let addr = state::with(|s| apply_devices(s, devices, force));
    let current = state::snapshot();
    if current.connected_devices.is_empty() || addr.as_ref().is_some_and(|a| a != &previous) {
        modules::reset_connection();
        crate::bootstrap::reset();
        transfer::reset_connection();
    }
    if let Some(addr) = addr {
        if let Err(error) = crate::bootstrap::start(&addr).await {
            failed(error);
        }
    }
    state::with(|s| s.discovering = false);
    ui::rerender();
}

pub async fn device_event() {
    let current = state::snapshot();
    let Ok(devices) = bounded(5000, device::get_connected_device_list()).await else { return; };
    if state::snapshot().connection_epoch != current.connection_epoch { return; }
    if devices.iter().any(|device|device.addr==current.addr) {
        if !current.manager_ready && !current.manager_busy && !current.active && !current.installing {
            let _ = modules::connect_device(&current.addr).await;
        }
        return;
    }
    crate::cloud_download::cancel(true);
    modules::reset_connection();
    if crate::delivery::busy() { crate::delivery::fail("手环已断开，安装未完成，请重新连接后重试".into()); }
    transfer::disconnected();
    state::with(|s| {
        s.connection_epoch = s.connection_epoch.wrapping_add(1);
        s.connected_devices = devices.iter().map(|d| d.addr.clone()).collect();
        s.manager_ready = false;
        s.manager_busy = false;
        s.direct = false;
        s.recheck_connection = true;
    });
    refresh(false).await;
}

fn begin_check(s: &mut state::State, busy: bool) -> bool {
    if busy || s.active || s.installing || s.discovering || s.manager_busy {
        s.recheck_connection = true;
        return false;
    }
    s.recheck_connection = false;
    s.discovering = true;
    if !s.manager_ready { s.connection = Phase::Checking; }
    true
}

// A host disconnect/reconnect during import is checked as soon as the current
// operation finishes, rather than resetting its pending request or file handle.
pub async fn check_pending() {
    if state::with(|s| s.recheck_connection) {
        refresh(false).await;
    }
}

pub async fn modules_ready() {
    let s = state::snapshot();
    state::with(|s| s.connection = Phase::Ready);
    if s.modules.iter().any(|m| m.importable()) {
        if !s.direct && !s.active && !transfer::busy() {
            let _ = transfer::connect_device(&s.addr).await;
        }
    } else {
        state::with(|s| s.direct = false);
    }
    state::with(|s| {
        if s.direct || !s.modules.iter().any(|m| m.importable()) {
            s.connection = Phase::Ready;
        }
    });
}

pub fn failed(message: String) {
    state::with(|s| {
        s.connection = Phase::Failed(message);
        s.status.clear();
    });
    ui::rerender();
}

#[cfg(test)]
mod tests {
    use super::*;
    fn music() -> modules::Module {
        modules::Module {
            id: "bluetooth_audio".into(),
            state: 8,
            intent: 1,
            ..Default::default()
        }
    }
    fn devices() -> Vec<(String, String)> {
        vec![("B".into(), "Band B".into()), ("A".into(), "Band A".into())]
    }
    #[test]
    fn startup_selects_connected_band_without_a_user_action() {
        let mut s = state::State::default();
        assert_eq!(
            apply_devices(&mut s, devices(), false).as_deref(),
            Some("A")
        );
        assert_eq!(s.connection, Phase::Modules);
        assert_eq!(s.connected_devices, ["B", "A"]);
    }
    #[test]
    fn disconnect_clears_stale_services_and_refresh_retries_automatically() {
        let mut s = state::State {
            addr: "A".into(),
            direct: true,
            manager_ready: true,
            modules: vec![music()],
            ..Default::default()
        };
        assert!(apply_devices(&mut s, vec![], false).is_none());
        assert!(matches!(s.connection, Phase::Failed(_)));
        assert!(!s.direct && !s.manager_ready && s.modules.is_empty() && s.addr.is_empty());
        assert_eq!(apply_devices(&mut s, devices(), true).as_deref(), Some("A"));
        assert_eq!(s.connection, Phase::Modules);
    }
    #[test]
    fn device_events_keep_the_current_band_and_import_page() {
        let mut s = state::State {
            addr: "B".into(),
            tab: "local".into(),
            direct: true,
            manager_ready: true,
            modules: vec![music()],
            ..Default::default()
        };
        assert!(apply_devices(&mut s, devices(), false).is_none());
        assert_eq!(s.connection, Phase::Ready);
        assert_eq!(s.tab, "local");
        assert_eq!(apply_devices(&mut s, devices(), true).as_deref(), Some("B"));
    }
    #[test]
    fn missing_or_disabled_music_does_not_block_module_management() {
        let mut s = state::State {
            addr: "A".into(),
            manager_ready: true,
            ..Default::default()
        };
        assert!(apply_devices(&mut s, devices(), false).is_none());
        s.modules = vec![music()];
        // A failed HELLO is retried on the next host event or Refresh.
        assert_eq!(
            apply_devices(&mut s, devices(), false).as_deref(),
            Some("A")
        );
        for (intent, state, error) in [(0, 8, 0), (2, 8, 0), (1, 14, 0), (1, 8, -1)] {
            s.modules[0].intent = intent;
            s.modules[0].state = state;
            s.modules[0].error = error;
            assert!(apply_devices(&mut s, devices(), false).is_none());
            assert_eq!(s.connection, Phase::Ready);
        }
    }
    #[test]
    fn connection_event_during_import_is_deferred_until_idle() {
        let mut s = state::State {
            active: true,
            direct: true,
            sent: 100,
            connection: Phase::Ready,
            ..Default::default()
        };
        assert!(!begin_check(&mut s, false));
        assert!(s.recheck_connection && s.active && s.direct);
        assert_eq!(s.sent, 100);
        assert_eq!(s.connection, Phase::Ready);
        s.active = false;
        assert!(!begin_check(&mut s, true));
        assert!(begin_check(&mut s, false));
        assert!(!s.recheck_connection);
        // A second refresh cannot race registration/device discovery.
        assert!(!begin_check(&mut s, false));
    }
    #[test]
    fn changing_bands_never_leaves_old_module_actions_available() {
        let mut s = state::State {
            addr: "C".into(),
            manager_ready: true,
            direct: true,
            modules: vec![music()],
            module_detail: "bluetooth_audio".into(),
            tab: "local".into(),
            ..Default::default()
        };
        assert_eq!(
            apply_devices(&mut s, devices(), false).as_deref(),
            Some("A")
        );
        assert_eq!(s.addr, "A");
        assert_eq!(s.device_name, "Band A");
        assert!(!s.direct && !s.manager_ready && s.modules.is_empty());
        assert!(s.module_detail.is_empty());
        assert_eq!(s.tab, "modules");
    }
}

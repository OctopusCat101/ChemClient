//! Probe the resident Chem service first; offer the installer only on failure.
use crate::{
    astrobox::psys_host_v4::{queue, timer, transport, watchface},
    cloud, cloud_download,
    connection::Phase,
    modules, protocol, state, ui,
};
use std::{
    sync::{Mutex, OnceLock},
    time::Instant,
};
#[derive(Clone)]
struct Waiting {
    addr: String,
    target: cloud::Target,
    queued: bool,
    since: Instant,
    inspect: bool,
    inspected: bool,
}
impl Waiting {
    fn probe_failed(&mut self) -> bool {
        if self.queued {
            return false;
        }
        self.inspect = !self.inspected;
        self.since.elapsed().as_secs() < 180
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InstallerAction {
    Install,
    Open,
}
#[derive(Clone)]
struct Candidate {
    addr: String,
    target: cloud::Target,
    path: Option<String>,
}
#[derive(Default)]
struct Bootstrap {
    candidate: Option<Candidate>,
    waiting: Option<Waiting>,
    timer: bool,
    polling: bool,
    device_info: Option<Vec<u8>>,
    catalog: Option<cloud::Catalog>,
}
impl Bootstrap {
    fn take_ready(&mut self) -> Option<(Vec<u8>, cloud::Catalog)> {
        if self.device_info.is_none() || self.catalog.is_none() {
            return None;
        }
        Some((
            self.device_info.take().unwrap(),
            self.catalog.take().unwrap(),
        ))
    }
}
static BOOT: OnceLock<Mutex<Bootstrap>> = OnceLock::new();
fn with<R>(f: impl FnOnce(&mut Bootstrap) -> R) -> R {
    f(&mut BOOT
        .get_or_init(|| Mutex::new(Bootstrap::default()))
        .lock()
        .unwrap_or_else(|e| e.into_inner()))
}
pub fn reset() {
    with(|b| {
        // Astro may still be reading a user-submitted queue file after a timeout.
        let queued = b.waiting.as_ref().is_some_and(|w| w.queued);
        b.waiting = None;
        if let Some(c) = b.candidate.take() {
            if !queued {
                if let Some(path) = c.path {
                    let _ = std::fs::remove_file(path);
                }
            }
        }
        b.polling = false;
        b.device_info = None;
        b.catalog = None;
    });
}
pub fn connected() {
    with(|b| b.waiting = None);
    state::with(|s| {
        s.installer_action = None;
        s.status.clear();
    });
}
pub fn probe_failed(_error: &str) -> bool {
    let retrying = with(|b| b.waiting.as_mut().is_some_and(Waiting::probe_failed));
    if retrying {
        state::with(|s| {
            s.connection = Phase::Installer(if s.installer_action.is_some() {
                "Chem 尚未运行，请打开安装器并点击运行".into()
            } else {
                "暂未连接 Chem，正在重试…".into()
            });
            // Transport details belong in diagnostics, not the connection UI.
            s.status.clear();
        });
        ui::rerender();
    }
    retrying
}
fn identify(packet: &[u8], catalog: &cloud::Catalog) -> Result<cloud::Target, String> {
    if packet.len() > 8192 {
        return Err("设备信息响应过大".into());
    }
    let info = protocol::fields(protocol::fields(packet, 4).ok_or("未返回设备信息")?, 3)
        .ok_or("未返回设备型号")?;
    fn string<'a>(b: &'a [u8], field: u64, name: &str) -> Result<&'a str, String> {
        let value = protocol::fields(b, field)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| format!("手环未返回{name}，请刷新重试"))?;
        std::str::from_utf8(value).map_err(|_| format!("手环{name}编码无效"))
    }
    // Xiaomi System.DeviceInfo: serial=1, firmware=2, unknown=3, model=4.
    // There is no required product field 5. Match the reported model itself.
    let firmware = string(info, 2, "固件版本")?;
    let model = string(info, 4, "型号")?;
    catalog
        .targets
        .iter()
        .find(|t| {
            t.firmware == firmware && t.device_ids.iter().any(|id| id.eq_ignore_ascii_case(model))
        })
        .cloned()
        .ok_or_else(|| format!("暂不支持此手环固件：{model} {firmware}"))
}
pub async fn start(addr: &str) -> Result<(), String> {
    reset();
    let epoch = state::snapshot().connection_epoch;
    state::with(|s| {
        s.connection = Phase::Checking;
        s.installer_action = None;
        s.manager_ready = false;
        s.direct = false;
        s.native_delivery = false;
        s.cloud_modules.clear();
        s.cloud_notice.clear();
        s.target.clear();
        s.firmware_sha256.clear();
        s.expected_revision = 0;
        s.resident_revision = 0;
        s.resident_target.clear();
        s.resident_capabilities = 0;
    });
    ui::rerender();
    if let Err(e) = cloud_download::catalog(addr) {
        state::with(|s| s.cloud_notice = e);
    }
    // Local service discovery must not wait for a cloud mirror or DeviceInfo.
    if let Err(e) = modules::connect_device(addr).await {
        state::with(|s| s.cloud_notice = e);
    }
    if state::snapshot().connection_epoch != epoch {
        return Ok(());
    }
    let reply = match crate::connection::bounded(
        5000,
        transport::request(addr.to_string(), vec![8, 2, 16, 2, 34, 0]),
    )
    .await
    .and_then(|r| r.map_err(|e| format!("读取手环型号失败：{e}")))
    {
        Ok(reply) => reply,
        Err(e) => {
            state::with(|s| s.cloud_notice = e);
            return Ok(());
        }
    };
    if state::snapshot().addr != addr || state::snapshot().connection_epoch != epoch {
        return Ok(());
    }
    with(|b| b.device_info = Some(reply));
    prepare_ready(addr).await
}
pub async fn catalog_ready(
    addr: &str,
    result: Result<cloud::Catalog, String>,
) -> Result<(), String> {
    let s = state::snapshot();
    if s.addr != addr || !s.device_connected() {
        return Ok(());
    }
    let catalog = match result {
        Ok(catalog) => catalog,
        Err(e) => {
            state::with(|s| s.cloud_notice = format!("获取云端清单失败，可刷新重试：{e}"));
            return Ok(());
        }
    };
    with(|b| b.catalog = Some(catalog));
    prepare_ready(addr).await?;
    ui::rerender();
    Ok(())
}
async fn prepare_ready(addr: &str) -> Result<(), String> {
    let Some((reply, catalog)) = with(Bootstrap::take_ready) else {
        return Ok(());
    };
    let target = identify(&reply, &catalog)?;
    let s = state::snapshot();
    if !s.resident_target.is_empty() && s.resident_target != target.target {
        modules::reset_connection();
        state::with(|s| {
            s.manager_ready = false;
            s.manager_busy = false;
            s.direct = false;
        });
        return Err("手环框架与当前固件目标不一致".into());
    }
    state::with(|s| {
        s.target = target.target.clone();
        s.firmware_sha256 = target.firmware_sha256.clone();
        s.expected_revision = target.loader.version;
        s.cloud_modules = target.modules.clone();
        s.native_delivery = !s.resident_target.is_empty() && s.resident_capabilities & 1 != 0;
    });
    with(|b| {
        b.candidate = Some(Candidate {
            addr: addr.into(),
            target: target.clone(),
            path: None,
        })
    });
    if s.manager_ready {
        return Ok(());
    }
    state::with(|s| {
        s.installer_action = None;
        s.connection = Phase::Modules;
    });
    // A resident service works with any watchface. Only inspect/open the
    // installer after a real service probe fails; never gate probing on it.
    wait(addr, target, false);
    if !state::snapshot().manager_busy {
        modules::connect_device(addr).await?;
    }
    ui::rerender();
    Ok(())
}
fn installer_action(installed: Option<bool>) -> Option<InstallerAction> {
    match installed {
        None => Some(InstallerAction::Install),
        Some(false) => Some(InstallerAction::Open),
        Some(true) => None,
    }
}
fn wait(addr: &str, target: cloud::Target, queued: bool) {
    with(|b| {
        b.waiting = Some(Waiting {
            addr: addr.into(),
            target,
            queued,
            since: Instant::now(),
            inspect: false,
            inspected: false,
        });
        if !b.timer {
            timer::set_interval(2000, "chem-bootstrap");
            b.timer = true;
        }
    });
}
fn download() -> Result<(), String> {
    let s = state::snapshot();
    if s.installer_action != Some(InstallerAction::Install) {
        return Err("请刷新安装器状态".into());
    }
    let c = with(|b| b.candidate.clone()).ok_or("请先刷新设备信息")?;
    if c.addr != s.addr || !s.connected_devices.contains(&c.addr) {
        return Err("手环连接已变化，请刷新".into());
    }
    cloud_download::loader(&c.addr, &c.target.loader.artifact)?;
    state::with(|s| s.installer_action = None);
    Ok(())
}
pub async fn download_finished(result: Result<String, String>) {
    match result {
        Ok(path) => {
            with(|b| {
                if let Some(c) = &mut b.candidate {
                    c.path = Some(path);
                }
            });
            state::with(|s| {
                s.installer_action = Some(InstallerAction::Install);
                s.connection = Phase::Installer("正在安装 Chem…".into());
                s.status.clear();
            });
            ui::rerender();
            if let Err(error) = install().await {
                download_failed(error);
            }
        }
        Err(error) => download_failed(error),
    }
}
pub fn download_failed(error: String) {
    state::with(|s| {
        s.installer_action = Some(InstallerAction::Install);
        s.connection = Phase::Installer("安装未完成，可点击安装重试".into());
        s.status = error;
    });
}
pub async fn install() -> Result<(), String> {
    let s = state::snapshot();
    let c = with(|b| b.candidate.clone()).ok_or("请先刷新设备信息")?;
    if c.addr != s.addr || !s.connected_devices.contains(&c.addr) {
        return Err("手环连接已变化，请刷新".into());
    }
    match s.installer_action {
        Some(InstallerAction::Install) => {
            let Some(path) = c.path else {
                return download();
            };
            // This flow starts only after the user clicks Install.
            state::with(|s| {
                s.installer_action = None;
                s.connection = Phase::Installer("正在安装 Chem…".into());
            });
            ui::rerender();
            queue::add_resource_to_queue(queue::ResourceType::Watchface, &path);
            state::with(|s| {
                s.installer_action = None;
                s.connection = Phase::Installer(
                    "已提交安装，请在 Astro 上传队列开始上传，正在等待安装完成…".into(),
                );
            });
            wait(&c.addr, c.target, true);
        }
        Some(InstallerAction::Open) => {
            watchface::set_current_watchface(c.addr.clone(), c.target.loader.face_id.clone())
                .await
                .map_err(|e| format!("打开安装器失败：{e}"))?;
            state::with(|s| {
                s.installer_action = None;
                s.connection = Phase::Installer("请在手环安装器点击“运行”，正在等待连接…".into());
            });
            wait(&c.addr, c.target, false);
            modules::connect_device(&c.addr).await?;
        }
        _ => return Err("请刷新安装器状态后点击安装".into()),
    }
    Ok(())
}
pub async fn tick() {
    let wait = with(|b| {
        if b.polling {
            return None;
        }
        let w = b.waiting.clone()?;
        b.polling = true;
        Some(w)
    });
    let Some(w) = wait else {
        return;
    };
    let s = state::snapshot();
    if s.addr != w.addr || !s.connected_devices.contains(&w.addr) {
        reset();
        return;
    }
    if w.since.elapsed().as_secs() > if w.queued { 600 } else { 180 } {
        reset();
        crate::connection::failed(
            if w.queued {
                "尚未完成安装器上传，请检查 Astro 上传队列后刷新"
            } else {
                "尚未连接 Chem，请在手环运行安装器后刷新"
            }
            .into(),
        );
        return;
    }
    if !s.active && !s.installing && !s.manager_busy && s.download.is_none() {
        if w.inspect && !w.inspected {
            match watchface::get_watchface_list(w.addr.clone()).await {
                Ok(faces) => {
                    if state::snapshot().addr != w.addr || state::snapshot().manager_ready {
                        with(|b| b.polling = false);
                        return;
                    }
                    let action = installer_action(
                        faces
                            .iter()
                            .find(|f| f.id == w.target.loader.face_id)
                            .map(|f| f.is_current),
                    );
                    state::with(|s| {
                        s.installer_action = action;
                        s.connection = Phase::Installer(
                            match action {
                                Some(InstallerAction::Install) => "需要安装或更新 Chem，请点击安装",
                                Some(InstallerAction::Open) => "请打开 Chem 安装器并点击运行",
                                None => "请在手环运行 Chem，正在等待连接…",
                            }
                            .into(),
                        );
                    });
                    with(|b| {
                        if let Some(w) = &mut b.waiting {
                            w.inspected = true;
                            w.inspect = false;
                        }
                    });
                }
                Err(_) => {} // Service retries remain independent of watchface RPC.
            }
        }
        if w.queued {
            match watchface::get_watchface_list(w.addr.clone()).await {
                Ok(faces) => {
                    if let Some(f) = faces.iter().find(|f| f.id == w.target.loader.face_id) {
                        with(|b| {
                            if let Some(c) = &mut b.candidate {
                                if let Some(path) = c.path.take() {
                                    let _ = std::fs::remove_file(path);
                                }
                            }
                        });
                        let result = if f.is_current {
                            Ok(())
                        } else {
                            watchface::set_current_watchface(w.addr.clone(), f.id.clone()).await
                        };
                        match result {
                            Ok(()) => {
                                with(|b| {
                                    if let Some(w) = &mut b.waiting {
                                        w.queued = false;
                                        w.since = Instant::now();
                                    }
                                });
                                state::with(|s| {
                                    s.connection = Phase::Installer(
                                        "安装器已就绪，请在手环点击“运行”，正在等待连接…".into(),
                                    )
                                });
                                let _ = modules::connect_device(&w.addr).await;
                            }
                            Err(e) => {
                                reset();
                                crate::connection::failed(format!("切换安装器失败：{e}"));
                            }
                        }
                    }
                }
                Err(e) => state::with(|s| s.status = format!("等待安装器：{e}")),
            }
        }
    }
    with(|b| b.polling = false);
    ui::rerender();
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn parallel_discovery_waits_for_both_results_in_either_order() {
        for catalog_first in [false, true] {
            let mut b = Bootstrap::default();
            if catalog_first {
                b.catalog = Some(cloud::fixture().unwrap());
            } else {
                b.device_info = Some(packet("3.1.35", "n66tc"));
            }
            assert!(b.take_ready().is_none());
            if catalog_first {
                b.device_info = Some(packet("3.1.35", "n66tc"));
            } else {
                b.catalog = Some(cloud::fixture().unwrap());
            }
            let (reply, catalog) = b.take_ready().unwrap();
            assert!(identify(&reply, &catalog).is_ok());
            assert!(b.take_ready().is_none());
        }
    }
    #[test]
    fn installer_inspection_waits_for_a_failed_service_probe() {
        let mut w = Waiting {
            addr: "band".into(),
            target: cloud::fixture().unwrap().targets.remove(0),
            queued: false,
            since: Instant::now(),
            inspect: false,
            inspected: false,
        };
        assert!(!w.inspect);
        assert!(w.probe_failed());
        assert!(w.inspect);
        w.inspected = true;
        w.inspect = false;
        assert!(w.probe_failed());
        assert!(!w.inspect);
        w.queued = true;
        assert!(!w.probe_failed());
        w.queued = false;
        w.since = Instant::now() - std::time::Duration::from_secs(181);
        assert!(!w.probe_failed());
    }
    fn packet(firmware: &str, model: &str) -> Vec<u8> {
        fn field(id: u8, value: &[u8]) -> Vec<u8> {
            assert!(value.len() < 128);
            let mut b = vec![id << 3 | 2, value.len() as u8];
            b.extend_from_slice(value);
            b
        }
        let mut info = field(2, firmware.as_bytes());
        info.extend(field(4, model.as_bytes()));
        field(4, &field(3, &info))
    }
    #[test]
    fn missing_or_old_installer_needs_a_user_action() {
        assert_eq!(installer_action(None), Some(InstallerAction::Install));
        assert_eq!(installer_action(Some(false)), Some(InstallerAction::Open));
        assert_eq!(installer_action(Some(true)), None);
    }
    #[test]
    fn exact_device_and_firmware_gate_cloud_loader() {
        let c = cloud::fixture().unwrap();
        assert!(identify(&packet("3.1.35", "n66tc"), &c).is_ok());
        assert!(identify(&packet("3.1.35", "miwear.watch.n66tc"), &c).is_ok());
        assert!(identify(&packet("3.1.35", "M2346B1"), &c).is_ok());
        assert!(identify(&packet("3.1.32", "n66tc"), &c).is_err());
        assert!(identify(&packet("3.1.35", "n66nfc"), &c).is_err());
        assert!(identify(&packet("3.1.35", "miwear.watch.n66nfc"), &c).is_err());
        assert!(identify(&packet("3.1.32", "M2346B1"), &c).is_err());
        assert!(identify(&packet("3.1.35", "M9999B1"), &c).is_err());
        assert!(identify(&[8, 2], &c).is_err());
    }

    #[test]
    fn standard_device_info_has_no_product_field() {
        // Wire fixture from the System/DeviceInfo schema: command 2/2,
        // firmware field 2 and model field 4, with field 5 entirely absent.
        let reply = b"\x08\x02\x10\x02\x22\x1e\x1a\x1c\x12\x063.1.35\x22\x12miwear.watch.n66tc";
        let c = cloud::fixture().unwrap();
        let target = identify(reply, &c).unwrap();
        assert_eq!(target.target, "xiaomi-band-9-3.1.35");
        assert_eq!(target.firmware, "3.1.35");
    }

    #[test]
    fn band9_reported_hardware_model_without_product_field_is_supported() {
        // Actual reported identity: firmware=3.1.35, model=M2346B1.
        // Exact firmware C38CAB8 selects this model for factory.board.id=TC;
        // C38D668 puts the selected string in System.DeviceInfo.model (4).
        let reply = b"\x08\x02\x10\x02\x22\x13\x1a\x11\x12\x063.1.35\x22\x07M2346B1";
        let target = identify(reply, &cloud::fixture().unwrap()).unwrap();
        assert_eq!(target.target, "xiaomi-band-9-3.1.35");
    }

    #[test]
    fn incomplete_identity_is_rejected_with_the_missing_field_named() {
        let c = cloud::fixture().unwrap();
        assert!(
            identify(&packet("", "n66tc"), &c)
                .unwrap_err()
                .contains("固件版本")
        );
        assert!(
            identify(&packet("3.1.35", ""), &c)
                .unwrap_err()
                .contains("型号")
        );
        // Unknown fields cannot override an unsupported model.
        let reply = b"\x22\x19\x1a\x17\x12\x063.1.35\x22\x06n66nfc\x2a\x05n66tc";
        assert!(identify(reply, &c).is_err());
    }
}

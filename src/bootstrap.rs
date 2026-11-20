//! Discover the resident Chem service and cloud module/version metadata.
use crate::{
    astrobox::psys_host_v4::transport, cloud, cloud_download, connection::Phase, modules, protocol,
    state, ui,
};
use std::{
    sync::{Mutex, OnceLock},
    time::Instant,
};
#[derive(Default)]
struct Bootstrap {
    waiting_since: Option<Instant>,
    device_info: Option<Vec<u8>>,
    catalog: Option<cloud::Catalog>,
    /// Catalog lookup finished unsuccessfully (offline / timeout / bad payload).
    /// Identity then comes from the built-in exact-target table, so a locally
    /// imported .chem still works without ChemCloud.
    catalog_failed: bool,
    /// A precise "unsupported device" reason, kept so a later service-probe
    /// timeout cannot replace it with a misleading "no Chem reply" message.
    identification_error: Option<String>,
}
impl Bootstrap {
    fn take_ready(&mut self) -> Option<(Vec<u8>, cloud::Catalog)> {
        if self.device_info.is_none() {
            return None;
        }
        let catalog = if let Some(c) = self.catalog.take() {
            c
        } else if self.catalog_failed {
            /* An empty catalog makes cloud::identify_target() fall back to the
             * built-in/local exact-target identities (target + firmware digest
             * needed to verify a local .chem receipt). */
            cloud::Catalog {
                schema: 1,
                targets: Vec::new(),
            }
        } else {
            return None; /* catalog request still in flight */
        };
        Some((self.device_info.take().unwrap(), catalog))
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
        b.waiting_since = None;
        b.device_info = None;
        b.catalog = None;
        b.catalog_failed = false;
        b.identification_error = None;
    });
}
pub fn connected() {
    with(|b| b.waiting_since = None);
    state::with(|s| {
        s.status.clear();
    });
}
pub fn probe_failed(_error: &str) -> bool {
    let (retrying, unsupported) = with(|b| {
        (
            b.waiting_since
                .is_some_and(|since| since.elapsed().as_secs() < 180),
            b.identification_error.is_some(),
        )
    });
    // Identification happens after the first service probe has already been
    // sent. If it fails, the bootstrap state machine was never armed; keep the
    // exact firmware/model error on screen instead of reporting a nonexistent
    // "Chem service did not reply" failure.
    if unsupported {
        return true;
    }
    if retrying {
        state::with(|s| {
            s.connection =
                Phase::WaitingForService("暂未连接 Chem，请在手环手动运行 Chem，正在重试…".into());
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
    cloud::identify_target(catalog, firmware, model)
        .ok_or_else(|| format!("暂不支持此手环固件：{model} {firmware}"))
}
pub async fn start(addr: &str) -> Result<(), String> {
    reset();
    let epoch = state::snapshot().connection_epoch;
    state::with(|s| {
        s.connection = Phase::Checking;
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
        // The request could not even be started, so no catalog_ready callback
        // will arrive. Mark it finished and keep local .chem import available.
        state::with(|s| {
            s.cloud_notice = format!("获取云端清单失败，本地 .chem 导入仍可用：{e}")
        });
        with(|b| b.catalog_failed = true);
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
    match result {
        Ok(catalog) => with(|b| b.catalog = Some(catalog)),
        Err(e) => {
            state::with(|s| {
                s.cloud_notice = format!("获取云端清单失败，本地 .chem 导入仍可用：{e}")
            });
            /* Do not return: prepare_ready() also fills target and the exact
             * firmware digest used to verify a local .chem receipt. An empty
             * catalog resolves that from the built-in identity table. */
            with(|b| b.catalog_failed = true);
        }
    }
    prepare_ready(addr).await?;
    ui::rerender();
    Ok(())
}
async fn prepare_ready(addr: &str) -> Result<(), String> {
    let Some((reply, catalog)) = with(Bootstrap::take_ready) else {
        return Ok(());
    };
    let target = match identify(&reply, &catalog) {
        Ok(t) => t,
        Err(e) => {
            with(|b| b.identification_error = Some(e.clone()));
            return Err(e);
        }
    };
    let local_only = target.local_only;
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
        /* The device's capability word, not the cloud lookup, decides whether
         * native .chem upload is possible. */
        s.native_delivery = s.resident_capabilities & 1 != 0;
        if local_only {
            s.cloud_notice = format!(
                "已识别 {}：云端清单不可用，已改用本地精确身份；可直接导入 .chem（需设备端启用原生传输）。",
                target.target
            );
        }
    });
    if s.manager_ready {
        return Ok(());
    }
    state::with(|s| {
        s.connection = Phase::Modules;
    });
    // Connection retries use the existing service loop, independent of watchfaces.
    with(|b| b.waiting_since = Some(Instant::now()));
    if !state::snapshot().manager_busy {
        modules::connect_device(addr).await?;
    }
    ui::rerender();
    Ok(())
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
    fn failed_catalog_still_resolves_builtin_identity_for_local_chem() {
        let mut b = Bootstrap::default();
        b.device_info = Some(packet("3.1.28", "M2456B1"));
        assert!(b.take_ready().is_none()); // catalog request still in flight
        b.catalog_failed = true;
        let (reply, catalog) = b.take_ready().unwrap();
        assert!(catalog.targets.is_empty());
        let target = identify(&reply, &catalog).expect("built-in o66nfc identity");
        assert_eq!(target.target, "xiaomi-band-10-nfc-3.1.28");
        assert_eq!(
            target.firmware_sha256,
            "fb726472416e99ba11d1c20ce8f0d4f0831a6e9a673d63bb5918e2c3921d8845"
        );
        assert!(target.local_only);
        assert!(b.take_ready().is_none());
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
    fn exact_device_and_firmware_gate_cloud_modules() {
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
    fn band10_nfc_is_identified_without_a_cloud_catalog_entry() {
        // Identification also works before the cloud target is available.
        let c = cloud::fixture().unwrap();
        for model in [
            "o66nfc",
            "O66NFC",
            "miwear.watch.o66nfc",
            "M2456B1",
            "m2456b1",
            "M2457B1",
            "m2457b1",
        ] {
            let target = identify(&packet("3.1.28", model), &c).unwrap();
            assert_eq!(target.target, "xiaomi-band-10-nfc-3.1.28");
            assert_eq!(target.firmware, "3.1.28");
            assert_eq!(
                target.firmware_sha256,
                "fb726472416e99ba11d1c20ce8f0d4f0831a6e9a673d63bb5918e2c3921d8845"
            );
            assert!(target.local_only);
            assert!(target.modules.is_empty());
        }
        // Identity stays exact: neither a wrong firmware nor a Band 9 model
        // may resolve to the Band 10 target.
        assert!(identify(&packet("3.1.35", "o66nfc"), &c).is_err());
        assert!(identify(&packet("3.1.28", "n66tc"), &c).is_err());
    }

    #[test]
    fn shared_firmware_hardware_models_are_supported() {
        let c = cloud::fixture().unwrap();
        for model in ["M2456B1", "M2457B1"] {
            let target = identify(&packet("3.1.28", model), &c).unwrap();
            assert_eq!(target.target, "xiaomi-band-10-nfc-3.1.28");
            assert!(target.local_only);
            assert!(identify(&packet("3.1.35", model), &c).is_err());
        }
        assert!(identify(&packet("3.1.28", "M2456B2"), &c).is_err());
    }

    #[test]
    fn cloud_catalog_still_wins_over_local_identity() {
        let c = cloud::fixture().unwrap();
        let target = identify(&packet("3.1.35", "n66tc"), &c).unwrap();
        assert_eq!(target.target, "xiaomi-band-9-3.1.35");
        assert!(!target.local_only);
        assert!(cloud::validate(&c).is_ok());
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

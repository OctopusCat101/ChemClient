//! Chem-owned management channel; independent of the music importer.
use crate::{
    astrobox::psys_host_v4::{timer, transport},
    connection, protocol, state, transfer, ui,
};
use base64::{Engine, engine::general_purpose::STANDARD};
use std::{
    sync::{Mutex, OnceLock},
    time::{Instant, SystemTime, UNIX_EPOCH},
};
const PACKAGE: &str = "com.chem.manager";
const ABI_MAJOR: u8 = 2;
const ABI_MINOR: u8 = 0;
pub(crate) const BULK_CHUNK: usize = 16 * 1024;
#[derive(Clone, Debug, Default)]
pub struct Module {
    pub id: String,
    pub state: u32,
    pub class: u32,
    pub version: u32,
    pub version_name: String,
    pub flags: u32,
    pub intent: u32,
    pub error: i32,
}
impl Module {
    pub fn name(&self) -> &str {
        if self.id == "bluetooth_audio" {
            "NoirMusic"
        } else if self.id == "com.chem.magic" {
            "Magic"
        } else {
            &self.id
        }
    }
    pub fn music(&self) -> bool {
        self.id == "bluetooth_audio"
    }
    pub fn importable(&self) -> bool {
        self.music() && self.intent == 1 && self.error == 0 && matches!(self.state, 9 | 13)
    }
    pub fn status(&self) -> String {
        if self.flags & 4 != 0 {
            return "更新已就绪 · 重启手环后生效".into();
        }
        if self.intent == 2 {
            return "等待重启手环后删除".into();
        }
        if self.error != 0 {
            return format!("加载异常 ({})", self.error);
        }
        if self.state == 14 {
            return "已设置关闭 · 重启手环后生效".into();
        }
        let resident = matches!(self.state, 8 | 9 | 13);
        if self.intent == 1 {
            if resident {
                "已启用"
            } else {
                "已设置启用 · 重启手环后生效"
            }
        } else {
            if resident {
                "已设置关闭 · 重启手环后生效"
            } else {
                "已关闭"
            }
        }
        .into()
    }
}
struct Pending {
    id: u32,
    op: u32,
    frame: Vec<u8>,
    start: Instant,
    sent: Instant,
    sending: bool,
    full_sent: Instant,
    polls: u8,
    background: bool,
    send_failed: bool,
}
#[derive(Default)]
struct Link {
    next: u32,
    reserved: u32,
    pending: Option<Pending>,
    timer: bool,
    rows: Vec<Module>,
    sequence: Option<u32>,
    restarts: u8,
    resyncs: u8,
    failures: u8,
}
static LINK: OnceLock<Mutex<Link>> = OnceLock::new();
fn link<R>(f: impl FnOnce(&mut Link) -> R) -> R {
    f(&mut LINK
        .get_or_init(|| Mutex::new(Link::default()))
        .lock()
        .unwrap_or_else(|e| e.into_inner()))
}
fn word(b: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(b.get(at..at + 4)?.try_into().ok()?))
}
fn resync_sequence(l: &mut Link, horizon: u32) -> bool {
    if l.resyncs >= 2 || horizon == 0 {
        return false;
    }
    l.resyncs += 1;
    l.next = horizon;
    l.reserved = 0;
    true
}
fn sequence_horizon(result: u32, data: &[u8]) -> Option<u32> {
    if result != 1 || data.len() != 8 || word(data, 0) != Some(0x31515343) {
        return None;
    }
    word(data, 4)
}
fn frame(id: u32, op: u32, data: &[u8]) -> Vec<u8> {
    let mut b = vec![0; 36];
    b[..4].copy_from_slice(&0x43504332u32.to_le_bytes());
    b[4] = 36;
    b[6] = 1;
    b[8] = ABI_MAJOR;
    b[10] = ABI_MINOR;
    for (at, n) in [
        (12, 36 + data.len() as u32),
        (16, op),
        (20, id),
        (32, data.len() as u32),
    ] {
        b[at..at + 4].copy_from_slice(&n.to_le_bytes());
    }
    b.extend_from_slice(data);
    b
}
fn bulk_frame(id: u32, op: u32, data: &[u8]) -> Vec<u8> {
    let mut b = Vec::with_capacity(16 + data.len());
    for n in [0x31424d43, id, op, data.len() as u32] {
        b.extend_from_slice(&n.to_le_bytes());
    }
    b.extend_from_slice(data);
    b
}
fn poll_frame(id: u32, op: u32) -> Vec<u8> {
    [0x31504d43u32, id, op]
        .into_iter()
        .flat_map(u32::to_le_bytes)
        .collect()
}
fn next_id(l: &mut Link) -> Result<u32, String> {
    next_id_at(l, std::path::Path::new("data/manager-sequence"))
}
fn next_id_at(l: &mut Link, store: &std::path::Path) -> Result<u32, String> {
    if l.next == 0 {
        l.next = std::fs::read(store)
            .ok()
            .and_then(|b| word(&b, 0))
            .unwrap_or_else(|| {
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs() as u32
            });
    }
    if l.reserved == 0 || l.next == l.reserved {
        let reserved = l.next.wrapping_add(4096).max(1);
        // Reserve before sending; reload skips every ID in this range.
        if let Some(parent) = store.parent() {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        std::fs::write(store, reserved.to_le_bytes()).map_err(|e| e.to_string())?;
        l.reserved = reserved;
    }
    l.next = l.next.wrapping_add(1).max(1);
    Ok(l.next)
}
pub(crate) async fn send(op: u32, data: Vec<u8>) -> Result<(), String> {
    send_inner(op, data, false).await
}
async fn send_inner(op: u32, data: Vec<u8>, background: bool) -> Result<(), String> {
    let bulk = matches!(op, 16 | 27 | 29) && state::snapshot().bulk_delivery;
    if data.len()
        > if bulk {
            BULK_CHUNK + if op == 29 { 32 } else { 4 }
        } else {
            512
        }
    {
        return Err("模块请求超过传输上限".into());
    }
    let s = state::snapshot();
    if !s.device_connected() {
        return Err("请先连接手环".into());
    }
    let addr = s.addr;
    let (id, b) = link(|l| {
        if !background && l.pending.as_ref().is_some_and(|p| p.background) {
            l.pending = None;
            l.rows.clear();
            l.sequence = None;
        }
        if l.pending.is_some() {
            return Err("模块操作尚未完成".to_string());
        }
        let id = next_id(l)?;
        let b = if bulk {
            bulk_frame(id, op, &data)
        } else {
            frame(id, op, &data)
        };
        l.pending = Some(Pending {
            id,
            op,
            frame: b.clone(),
            start: Instant::now(),
            sent: Instant::now(),
            sending: false,
            full_sent: Instant::now(),
            polls: 0,
            background,
            send_failed: false,
        });
        Ok((id, b))
    })?;
    state::with(|s| s.manager_busy = !background);
    transmit(addr, id, b).await
}
async fn transmit(addr: String, id: u32, b: Vec<u8>) -> Result<(), String> {
    let epoch = state::snapshot().connection_epoch;
    if !link(|l| {
        let Some(p) = l.pending.as_mut().filter(|p| p.id == id && !p.sending) else {
            return false;
        };
        p.sending = true;
        true
    }) {
        return Ok(());
    }
    let _write = crate::wire::acquire().await;
    if state::snapshot().connection_epoch != epoch
        || !link(|l| l.pending.as_ref().is_some_and(|p| p.id == id))
    {
        return Ok(());
    }
    let budget = if b.len() > 1024 { 30000 } else { 8000 };
    let result = connection::bounded(
        budget,
        transport::send(addr, protocol::encode_package(PACKAGE, &b)),
    )
    .await
    .and_then(|r| r.map_err(|e| e.to_string()));
    let current = link(|l| {
        if let Some(p) = l.pending.as_mut().filter(|p| p.id == id) {
            p.sending = false;
            p.sent = Instant::now();
            true
        } else {
            false
        }
    });
    if let Err(e) = result {
        if current {
            tracing::warn!("manager send {id} deferred after host error: {e}");
            link(|l| {
                if let Some(p) = l.pending.as_mut().filter(|p| p.id == id) {
                    p.send_failed = true;
                }
            });
        }
    }
    Ok(())
}
fn fail(error: String) {
    let retain = link(|l| {
        let background = l.pending.as_ref().is_some_and(|p| p.background);
        l.pending = None;
        l.rows.clear();
        l.failures = l.failures.saturating_add(1);
        background && l.failures < 3
    });
    if retain && state::snapshot().manager_ready {
        tracing::warn!("background manager probe missed, retaining session: {error}");
        return;
    }
    state::with(|s| {
        s.manager_busy = false;
        s.manager_ready = false;
        s.direct = false;
    });
    if crate::delivery::busy() {
        crate::delivery::fail(error.clone());
    }
    crate::magic::transport_failed(&error);
    if !crate::bootstrap::probe_failed(&error) {
        connection::failed(error);
    }
}
pub fn reset_connection() {
    link(|l| {
        l.pending = None;
        l.rows.clear();
        l.sequence = None;
        l.failures = 0;
    });
}
pub async fn connect_device(addr: &str) -> Result<(), String> {
    let s = state::snapshot();
    if s.active || s.manager_busy {
        return Err("请等待当前操作完成".into());
    }
    if !s.connected_devices.iter().any(|a| a == addr) {
        return Err("请先在宿主连接手环并刷新设备".into());
    }
    connection::attempted();
    state::with(|s| s.manager_busy = true);
    let receiver = transfer::ensure_receiver(addr).await;
    if state::snapshot().connection_epoch != s.connection_epoch {
        return Ok(());
    }
    state::with(|s| s.manager_busy = false);
    receiver?;
    state::with(|s| {
        if s.addr != addr {
            s.modules.clear();
            s.tab = "modules".into();
            s.module_detail.clear();
            s.delete_module.clear();
        }
        s.addr = addr.into();
        s.device_name = s
            .devices
            .iter()
            .find(|d| d.0 == addr)
            .map(|d| d.1.clone())
            .unwrap_or_default();
        s.direct = false;
        s.manager_ready = false;
    });
    if !link(|l| {
        let old = l.timer;
        l.timer = true;
        old
    }) {
        timer::set_interval(25, "chem-manager");
    }
    refresh().await
}
pub async fn refresh() -> Result<(), String> {
    let s = state::snapshot();
    if s.active || s.manager_busy {
        return Err("请等待当前操作完成".into());
    }
    link(|l| {
        l.rows.clear();
        l.sequence = None;
        l.restarts = 0;
        l.resyncs = 0;
    });
    send(9, 0x32534d43u32.to_le_bytes().to_vec()).await
}
pub async fn refresh_background() -> Result<(), String> {
    if link(|l| l.pending.is_some()) {
        return Ok(());
    }
    connection::attempted();
    link(|l| {
        l.rows.clear();
        l.sequence = None;
        l.restarts = 0;
        l.resyncs = 0;
    });
    send_inner(9, 0x32534d43u32.to_le_bytes().to_vec(), true).await
}
pub async fn mutate(id: &str, op: u32) -> Result<(), String> {
    let s = state::snapshot();
    if (s.discovering && !s.manager_ready) || s.connection.busy() {
        return Err("正在准备连接，请稍候".into());
    }
    if s.active || s.installing {
        return Err("请先完成或取消音乐导入".into());
    }
    if !s.manager_connected() || !s.modules.iter().any(|m| m.id == id && m.intent != 2) {
        return Err("模块列表已变化，请刷新".into());
    }
    if !matches!(op, 3 | 4 | 5 | 7) {
        return Err("不支持的模块操作".into());
    }
    let mut data = id.as_bytes().to_vec();
    data.push(0);
    send(op, data).await
}
pub async fn tick() {
    let pending = link(|l| {
        let p = l.pending.as_mut()?;
        if p.sending {
            return None;
        }
        let timeout = if p.op == 9 { 12 } else { 60 };
        if p.start.elapsed().as_secs() >= timeout {
            return Some(Err(()));
        }
        let delay = match p.polls {
            0 => 25,
            1 => 50,
            _ if matches!(p.op, 16 | 27) => 100,
            2 => 100,
            _ => 250,
        };
        if p.sending || p.sent.elapsed().as_millis() < delay {
            return None;
        }
        let retry_ms = if p.frame.len() > 1024 { 12000 } else { 3000 };
        let b = if p.full_sent.elapsed().as_millis() >= retry_ms
            || (p.send_failed && p.sent.elapsed().as_millis() >= 2000)
        {
            p.full_sent = Instant::now();
            p.send_failed = false;
            p.frame.clone()
        } else {
            p.polls = p.polls.saturating_add(1);
            poll_frame(p.id, p.op)
        };
        Some(Ok((p.id, b)))
    });
    match pending {
        Some(Err(())) => fail("暂未收到 Chem 服务回复，正在重新连接…".into()),
        Some(Ok((id, b))) => {
            let _ = transmit(state::snapshot().addr, id, b).await;
        }
        None => {}
    }
}
fn parse_response(b: &[u8], id: u32, op: u32) -> Option<(u32, &[u8])> {
    if b.len() < 36
        || b.len() > 548
        || word(b, 0) != Some(0x43504332)
        // CPC2 kind=2, followed by the SDK's ABI major/minor (2.0).
        || b[4..12] != [36, 0, 2, 0, ABI_MAJOR, 0, ABI_MINOR, 0]
        || word(b, 12) != Some(b.len() as u32)
        || word(b, 16) != Some(op)
        || word(b, 20) != Some(id)
        || word(b, 24) != Some(0)
        || word(b, 32) != Some((b.len() - 36) as u32)
    {
        return None;
    }
    Some((word(b, 28)?, &b[36..]))
}
pub async fn received(payload: &str) {
    if payload.len() > 12000 {
        return;
    }
    let Ok(packet) = STANDARD.decode(payload) else {
        return;
    };
    let Some(b) = protocol::decode_package(&packet, PACKAGE) else {
        return;
    };
    let pending = link(|l| l.pending.as_ref().map(|p| (p.id, p.op, p.background)));
    let Some((id, mut op, background)) = pending else {
        return;
    };
    let Some((result, mut data)) = parse_response(b, id, op) else {
        return;
    };
    link(|l| {
        l.pending = None;
        l.failures = 0;
    });
    connection::reply_received();
    state::with(|s| s.manager_busy = false);
    if let Some(horizon) = sequence_horizon(result, data).filter(|_| op == 9) {
        let retry = link(|l| resync_sequence(l, horizon));
        if retry {
            if let Err(e) = send_inner(9, 0x32534d43u32.to_le_bytes().to_vec(), background).await {
                fail(e);
            }
        } else {
            fail("连接序号同步失败，请关闭其他 Chem 客户端后重试".into());
        }
        return;
    }
    if op == 29 {
        crate::magic::management_response(result, data).await;
        return;
    }
    if crate::delivery::busy() {
        crate::delivery::received(op, result, data).await;
        return;
    }
    if matches!(op, 19 | 20 | 22) && result != 5 && result != 8 {
        let error = word(data, if op == 20 { 4 } else { 0 }).unwrap_or(0) as i32;
        state::with(|s| s.notice(state::NoticeOwner::Modules, setting_error(op, error)));
        ui::rerender();
        return;
    }
    if result != 5 && result != 8 {
        let error = format!(
            "设备拒绝模块操作（状态 {result}，错误 {}），请刷新检查模块状态",
            word(b, 36).unwrap_or(0) as i32
        );
        if state::snapshot().manager_ready {
            state::with(|s| s.notice(state::NoticeOwner::Modules, error));
            ui::rerender();
        } else {
            fail(error);
        }
        return;
    }
    if op == 19 || op == 20 {
        if data.len() != 8 {
            fail("自动启动状态格式不匹配".into());
            return;
        }
        state::with(|s| {
            s.boot_status = word(data, 0).unwrap_or(0);
            s.safe_exit_pending = s.boot_status & 4 != 0;
            if op == 20 {
                s.status_owner = state::NoticeOwner::Modules;
                s.status = if s.boot_status & 2 != 0 {
                    "自动启动已开启"
                } else {
                    "自动启动已关闭"
                }
                .into();
            }
        });
        if op == 19 {
            if let Err(e) = send_inner(0x100, 0u32.to_le_bytes().to_vec(), background).await {
                fail(e);
            }
        }
        ui::rerender();
        return;
    }
    if op == 9 {
        if data.len() < 32
            || word(data, 0) != Some(0x43514431)
            || word(data, 4) != Some(data.len() as u32)
        {
            fail("Chem 安装器版本不兼容，请更新后刷新".into());
            return;
        }
        if data.len() >= 88 {
            let target = String::from_utf8_lossy(&data[32..80])
                .trim_end_matches('\0')
                .to_string();
            if !state::snapshot().target.is_empty() && target != state::snapshot().target {
                fail("手环框架与当前固件目标不一致".into());
                return;
            }
            state::with(|s| {
                s.resident_target = target;
                s.resident_capabilities = word(data, 80).unwrap_or(0);
                /* Native upload capability is reported by the device itself.
                 * Requiring the cloud-derived firmware hash here disabled
                 * local .chem import whenever ChemCloud was unreachable. */
                s.native_delivery = s.resident_capabilities & 1 != 0;
                s.bulk_delivery = word(data, 80).unwrap_or(0) & 2 != 0;
            });
        }
        state::with(|s| {
            s.resident_revision = word(data, 8).unwrap_or(0);
            s.safe_mode = word(data, 12).unwrap_or(0) != 0;
            s.safe_reason = word(data, 20).unwrap_or(0);
        });
        if !background || !state::snapshot().manager_ready {
            crate::bootstrap::connected();
            state::with(|s| s.connection = connection::Phase::Modules);
        }
        if word(data, 88) == Some(0x32534d43) {
            if data.len() < 124 {
                fail("Chem 连接快照不完整".into());
                return;
            }
            state::with(|s| {
                s.boot_status = word(data, 92).unwrap_or(0);
                s.safe_exit_pending = s.boot_status & 4 != 0;
            });
            data = &data[100..];
            op = 0x100;
        } else {
            if let Err(e) = send_inner(19, vec![], background).await {
                fail(e);
            }
            return;
        }
    }
    if op == 0x100 {
        let Some((next, sequence, rows)) = parse_list(data) else {
            fail("模块列表响应格式不匹配".into());
            return;
        };
        let changed = link(|l| l.sequence.is_some_and(|s| s != sequence));
        if changed {
            let retry = link(|l| {
                l.rows.clear();
                l.sequence = None;
                l.restarts += 1;
                l.restarts <= 3
            });
            if retry {
                if let Err(e) = send_inner(0x100, 0u32.to_le_bytes().to_vec(), background).await {
                    fail(e);
                }
                return;
            }
            fail("设备模块状态持续变化，请稍后刷新".into());
            return;
        }
        let valid = link(|l| {
            let cursor = l.rows.len();
            if cursor + rows.len() > 16
                || rows.iter().any(|m| l.rows.iter().any(|old| old.id == m.id))
            {
                return false;
            }
            if next != u32::MAX && (rows.is_empty() || next <= cursor as u32) {
                return false;
            }
            l.sequence = Some(sequence);
            l.rows.extend(rows);
            true
        });
        if !valid {
            fail("设备返回了重复或超出范围的模块列表".into());
            return;
        }
        if next != u32::MAX {
            if let Err(e) = send_inner(0x100, next.to_le_bytes().to_vec(), background).await {
                fail(e);
            }
            return;
        }
        let rows = link(|l| std::mem::take(&mut l.rows));
        state::with(|s| {
            s.modules = rows;
            s.manager_ready = true;
            s.manager_busy = false;
        });
        connection::modules_ready().await;
    } else {
        state::with(|s| {
            s.manager_busy = false;
            s.delete_module.clear();
            s.status_owner = state::NoticeOwner::Modules;
            s.status = if op == 22 {
                "恢复设置已保存，请重启手环"
            } else if s.boot_status & 2 != 0 {
                "设置已保存，重启手环后生效"
            } else {
                "设置已保存，重启手环并运行安装器后生效"
            }
            .into();
        });
        if let Err(e) = refresh().await {
            fail(e);
        }
    }
    ui::rerender();
}
fn parse_list(b: &[u8]) -> Option<(u32, u32, Vec<Module>)> {
    if word(b, 0)? != 0x31534d43 || word(b, 4)? != 80 {
        return None;
    }
    let count = word(b, 20)? as usize;
    let next = word(b, 8)?;
    if count > 6 || b.len() != 24 + count * 80 || (next != u32::MAX && next >= 16) {
        return None;
    }
    let mut rows = Vec::new();
    for i in 0..count {
        let at = 24 + i * 80;
        let raw = b.get(at + 28..at + 60)?;
        let end = raw.iter().position(|c| *c == 0)?;
        let id = std::str::from_utf8(&raw[..end]).ok()?.to_string();
        if id.is_empty()
            || !id
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'_' | b'-' | b'.'))
        {
            return None;
        }
        rows.push(Module {
            id,
            state: word(b, at + 4)?,
            class: word(b, at + 8)?,
            version: word(b, at + 12)?,
            version_name: String::from_utf8_lossy(b.get(at + 60..at + 76)?)
                .trim_end_matches('\0')
                .to_string(),
            flags: word(b, at + 16)?,
            intent: word(b, at + 20)?,
            error: word(b, at + 24)? as i32,
        });
    }
    Some((next, word(b, 12)?, rows))
}
#[cfg(test)]
mod tests {
    #[test]
    fn ready_image_is_not_a_running_music_service() {
        let mut m = super::Module {
            id: "bluetooth_audio".into(),
            intent: 1,
            state: 8,
            ..Default::default()
        };
        assert!(!m.importable());
        for state in [9, 13] {
            m.state = state;
            assert!(m.importable());
        }
        m.error = -20;
        assert!(!m.importable());
    }
    #[test]
    fn lost_sequence_recovers_durably_across_wrap_and_is_bounded() {
        let hint = [0x43, 0x53, 0x51, 0x31, 0x50, 0xc3, 0, 0];
        assert_eq!(sequence_horizon(1, &hint), Some(50000));
        assert!(sequence_horizon(0, &hint).is_none());
        assert!(sequence_horizon(5, &hint).is_none());
        assert!(sequence_horizon(1, &hint[..7]).is_none());
        let store = std::env::temp_dir().join(format!("chem-resync-{}", std::process::id()));
        for horizon in [50000, u32::MAX] {
            let mut l = Link::default();
            assert!(resync_sequence(&mut l, horizon));
            let next = next_id_at(&mut l, &store).unwrap();
            assert!(next != 0 && next != horizon && next.wrapping_sub(horizon) < 0x80000000);
            let mut reloaded = Link::default();
            assert_ne!(next_id_at(&mut reloaded, &store).unwrap(), next);
            assert!(resync_sequence(&mut l, horizon));
            assert!(!resync_sequence(&mut l, horizon));
        }
        std::fs::remove_file(store).unwrap();
    }
    #[test]
    fn session_snapshot_connects_without_catalog_and_ignores_stale_replies() {
        use std::{
            future::Future,
            task::{Context, Poll, Waker},
        };
        fn complete(future: impl Future<Output = ()>) {
            assert!(matches!(
                std::pin::pin!(future)
                    .as_mut()
                    .poll(&mut Context::from_waker(Waker::noop())),
                Poll::Ready(())
            ));
        }
        let mut payload = vec![0; 124];
        for (at, n) in [
            (0, 0x43514431u32),
            (4, 124),
            (8, 0x26111303),
            (12, 1),
            (20, 2),
            (80, 3),
            (84, 1),
            (88, 0x32534d43),
            (92, 7),
            (100, 0x31534d43),
            (104, 80),
            (108, u32::MAX),
            (112, 4),
        ] {
            payload[at..at + 4].copy_from_slice(&n.to_le_bytes());
        }
        let target = b"xiaomi-band-9-3.1.35";
        payload[32..32 + target.len()].copy_from_slice(target);
        let mut wire = vec![0; 36];
        for (at, n) in [
            (0, 0x43504332u32),
            (12, 160),
            (16, 9),
            (20, 123),
            (28, 5),
            (32, 124),
        ] {
            wire[at..at + 4].copy_from_slice(&n.to_le_bytes());
        }
        wire[4..12].copy_from_slice(&[36, 0, 2, 0, 2, 0, 0, 0]);
        wire.extend(payload);
        let packet = STANDARD.encode(protocol::encode_package(PACKAGE, &wire));
        state::with(|s| {
            *s = state::State {
                addr: "test".into(),
                connected_devices: vec!["test".into()],
                manager_busy: true,
                ..Default::default()
            }
        });
        link(|l| {
            l.pending = Some(Pending {
                id: 124,
                op: 9,
                frame: vec![],
                start: Instant::now(),
                sent: Instant::now(),
                sending: false,
                full_sent: Instant::now(),
                polls: 0,
                background: false,
                send_failed: false,
            })
        });
        complete(received(&packet));
        assert!(!state::snapshot().manager_ready);
        link(|l| l.pending.as_mut().unwrap().id = 123);
        complete(received(&packet));
        let s = state::snapshot();
        assert!(s.manager_ready && !s.manager_busy && s.safe_mode && s.safe_exit_pending);
        assert_eq!(s.connection, connection::Phase::Ready);
        assert_eq!(s.resident_revision, 0x26111303);
        assert!(s.modules.is_empty() && s.firmware_sha256.is_empty());
        assert_eq!(s.native_delivery, s.resident_capabilities & 1 != 0);
        assert!(link(|l| l.pending.is_none()));
        state::with(|s| {
            s.expected_revision = 0x26111304;
            s.firmware_sha256 = "a".repeat(64);
            s.manager_ready = false;
            s.connection = connection::Phase::Modules;
        });
        let pending = || Pending {
            id: 123,
            op: 9,
            frame: vec![],
            start: Instant::now(),
            sent: Instant::now(),
            sending: false,
            full_sent: Instant::now(),
            polls: 0,
            background: true,
            send_failed: false,
        };
        link(|l| l.pending = Some(pending()));
        complete(received(&packet));
        let s = state::snapshot();
        assert!(s.needs_update() && s.manager_connected() && !s.manager_busy);
        assert_eq!(s.resident_revision, 0x26111303);
        assert_eq!(s.connection, connection::Phase::Ready);
        assert!(s.native_delivery && s.bulk_delivery);
        state::with(|s| {
            s.connection = connection::Phase::WaitingForService("等待用户运行 Chem".into());
        });
        link(|l| l.pending = Some(pending()));
        complete(received(&packet));
        assert!(state::snapshot().manager_connected() && state::snapshot().bulk_delivery);
        fail("temporary timeout".into());
        assert_eq!(
            state::snapshot().connection,
            connection::Phase::Failed("temporary timeout".into())
        );
        wire[44..48].copy_from_slice(&0x26111304u32.to_le_bytes());
        let updated = STANDARD.encode(protocol::encode_package(PACKAGE, &wire));
        link(|l| l.pending = Some(pending()));
        complete(received(&updated));
        let s = state::snapshot();
        assert!(!s.needs_update() && s.manager_ready && !s.manager_busy);
        assert_eq!(s.connection, connection::Phase::Ready);
        state::with(|s| *s = state::State::default());
    }
    #[test]
    fn newer_resident_build_is_not_an_update_requirement() {
        let mut s = state::State {
            expected_revision: 0x26111302,
            resident_revision: 0x26111301,
            ..Default::default()
        };
        assert!(s.needs_update());
        s.resident_revision = 0x26111302;
        assert!(!s.needs_update());
        s.resident_revision = 0x26111303;
        assert!(!s.needs_update());
    }
    use super::*;
    // Wire bytes from chem_transport_v2_encode_response (SDK ABI 2.0),
    // independent of this client's request encoder. Empty registry page.
    const EMPTY_LIST_REPLY: &[u8] = &[
        0x32, 0x43, 0x50, 0x43, 36, 0, 2, 0, 2, 0, 0, 0, 60, 0, 0, 0, 0, 1, 0, 0, 42, 0, 0, 0, 0,
        0, 0, 0, 5, 0, 0, 0, 24, 0, 0, 0, 0x43, 0x4d, 0x53, 0x31, 80, 0, 0, 0, 255, 255, 255, 255,
        7, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
    ];
    #[test]
    fn reserved_ids_survive_reload_and_wrap_without_per_packet_writes() {
        let store = std::env::temp_dir().join(format!("chem-sequence-{}", std::process::id()));
        for base in [100, u32::MAX - 10] {
            let mut l = Link {
                next: base,
                ..Default::default()
            };
            let first = next_id_at(&mut l, &store).unwrap();
            let durable = std::fs::read(&store).unwrap();
            for _ in 0..100 {
                next_id_at(&mut l, &store).unwrap();
            }
            assert_eq!(std::fs::read(&store).unwrap(), durable);
            let last = l.next;
            let mut reloaded = Link::default();
            let resumed = next_id_at(&mut reloaded, &store).unwrap();
            assert!(resumed.wrapping_sub(last) < 0x80000000 && resumed != last);
            assert_ne!(first, 0);
            assert_ne!(resumed, 0);
        }
        std::fs::remove_file(store).unwrap();
    }
    #[test]
    fn bulk_write_and_light_poll_keep_request_identity() {
        let payload = vec![0xa5; BULK_CHUNK + 4];
        let b = bulk_frame(123, 16, &payload);
        assert_eq!(b.len(), 16404);
        assert_eq!(word(&b, 0), Some(0x31424d43));
        assert_eq!(word(&b, 4), Some(123));
        assert_eq!(word(&b, 8), Some(16));
        assert_eq!(word(&b, 12), Some(16388));
        assert_eq!(&b[16..], &payload);
        let poll = poll_frame(123, 16);
        assert_eq!(poll.len(), 12);
        assert_eq!(word(&poll, 0), Some(0x31504d43));
        assert_eq!(word(&poll, 4), Some(123));
        assert_eq!(word(&poll, 8), Some(16));
    }
    #[test]
    fn sdk_abi2_reply_reaches_module_list_even_without_installed_modules() {
        let mut packet = protocol::encode_package(PACKAGE, EMPTY_LIST_REPLY);
        packet[3] = 9; // Xiaomi SEND_WEAR_MESSAGE.
        let b = protocol::decode_package(&packet, PACKAGE).unwrap();
        let (result, data) = parse_response(b, 42, 0x100).unwrap();
        assert_eq!(result, 5);
        let (next, sequence, rows) = parse_list(data).unwrap();
        assert_eq!((next, sequence), (u32::MAX, 7));
        assert!(rows.is_empty());
        assert_eq!(&frame(43, 9, &[])[8..12], &b[8..12]);
    }
    #[test]
    fn manager_response_rejects_wrong_abi_request_and_malformed_lengths() {
        for end in 0..EMPTY_LIST_REPLY.len() {
            assert!(parse_response(&EMPTY_LIST_REPLY[..end], 42, 0x100).is_none());
        }
        assert!(parse_response(EMPTY_LIST_REPLY, 43, 0x100).is_none());
        assert!(parse_response(EMPTY_LIST_REPLY, 42, 9).is_none());
        for (at, value) in [(6, 1), (8, 1), (10, 2), (12, 59), (24, 1), (32, 23)] {
            let mut b = EMPTY_LIST_REPLY.to_vec();
            b[at] = value;
            assert!(parse_response(&b, 42, 0x100).is_none(), "byte {at}");
        }
        let mut old_abi = EMPTY_LIST_REPLY.to_vec();
        old_abi[8..12].copy_from_slice(&[1, 0, 2, 0]);
        assert!(parse_response(&old_abi, 42, 0x100).is_none());
    }
    #[test]
    fn list_bounds_and_intent() {
        let mut b = vec![0; 104];
        for (at, n) in [
            (0, 0x31534d43u32),
            (4, 80),
            (8, u32::MAX),
            (20, 1),
            (24 + 20, 2),
        ] {
            b[at..at + 4].copy_from_slice(&n.to_le_bytes());
        }
        b[52..67].copy_from_slice(b"bluetooth_audio");
        let (_, _, rows) = parse_list(&b).unwrap();
        assert!(rows[0].music());
        assert_eq!(rows[0].intent, 2);
        for end in 0..b.len() {
            assert!(parse_list(&b[..end]).is_none());
        }
        b[20] = 7;
        assert!(parse_list(&b).is_none());
    }
}

pub async fn set_boot(enabled: bool) -> Result<(), String> {
    let s = state::snapshot();
    if !s.manager_connected()
        || s.manager_busy
        || s.active
        || s.installing
        || s.downloading_artifact()
    {
        return Err("请先完成当前操作并连接管理器".into());
    }
    if s.boot_status & 1 == 0 {
        return Err("当前固件不支持自动启动".into());
    }
    send(20, (enabled as u32).to_le_bytes().to_vec()).await
}
pub async fn exit_safe_mode(action: u32) -> Result<(), String> {
    let s = state::snapshot();
    if !s.manager_connected()
        || !s.safe_mode
        || s.safe_exit_pending
        || s.manager_busy
        || s.active
        || s.installing
        || action > 2
    {
        return Err("请刷新确认安全模式状态并完成当前操作".into());
    }
    send(22, action.to_le_bytes().to_vec()).await
}

fn setting_error(op: u32, error: i32) -> String {
    if op == 19 || op == 20 {
        let text = match error {
            -30 => "当前固件不支持自动启动",
            -31 => "暂无法读取手环电量，请稍后重试",
            -32 => "请保持手环充电或电量达到 30%",
            -33 => "启动文件未就绪，请重新运行最新版 Chem 安装器",
            -34 => "启动文件与固件不匹配，请更新并运行安装器",
            -35 => "固件启动区域校验不符，已停止修改",
            -36 => "启动设置写入失败，请重新运行安装器后重试",
            -37 => "手环内存不足，请重启后重试",
            -38 => "Flash 尚未就绪，请稍后重试",
            -39 => "启动探测写入被拒绝，已停止修改",
            -40 => "启动探测读回不符，未擦除启动区域",
            -41 => "启动区域擦除失败，请保留错误码",
            -42 => "启动脚本写入失败，请保留错误码",
            -43 => "Flash 同步失败，请保留错误码",
            -44 => "启动脚本读回校验失败，请保留错误码",
            -16 => "设备未就绪，请更新并运行最新版 Chem 安装器后重试",
            _ => return format!("设置失败（错误 {error}），请刷新确认当前状态"),
        };
        return text.into();
    }
    format!("恢复设置失败（错误 {error}），请刷新确认当前状态")
}

#[cfg(test)]
mod boot_error_tests {
    #[test]
    fn power_read_low_power_and_registry_errors_stay_distinct() {
        assert!(super::setting_error(20, -31).contains("读取"));
        assert!(super::setting_error(20, -32).contains("30%"));
        assert!(!super::setting_error(22, -16).contains("30%"));
        assert!(!super::setting_error(20, -36).contains("已保留"));
    }
}

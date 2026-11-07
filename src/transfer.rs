use crate::{
    astrobox::psys_host_v4::{device, register, timer, transport},
    protocol, state, ui,
};
use base64::{Engine, engine::general_purpose::STANDARD};
use serde_json::{Value, json};
use std::{
    collections::VecDeque,
    fs::{self, File},
    io::{Read, Seek, SeekFrom},
    sync::{Mutex, OnceLock},
    time::{Instant, SystemTime, UNIX_EPOCH},
};

struct Pending {
    id: u32,
    frame: Vec<u8>,
    since: Instant,
    last_send: Instant,
    attempts: u8,
    polls: u16,
    sending: bool,
}
struct Upload {
    file: File,
    path: String,
    temporary: bool,
    size: u64,
    offset: u64,
    crc: u32,
    started: Instant,
    chunk_len: u64,
    cancel_requested: bool,
    asset: u8,
}
#[derive(Default)]
struct Link {
    next: u32,
    pending: Option<Pending>,
    upload: Option<Upload>,
    command: u8,
    list_offset: u32,
    timer_id: u64,
    registered_addr: String,
    last_progress_render: Option<Instant>,
    assets: VecDeque<(String, Value, String)>,
    completed_assets: u8,
}
static LINK: OnceLock<Mutex<Link>> = OnceLock::new();
fn link<R>(f: impl FnOnce(&mut Link) -> R) -> R {
    f(&mut LINK
        .get_or_init(|| {
            Mutex::new(Link {
                next: (SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_millis() as u32)
                    | 1,
                ..Link::default()
            })
        })
        .lock()
        .unwrap_or_else(|e| e.into_inner()))
}

pub async fn discover_devices() -> Result<(), String> {
    if state::snapshot().discovering {
        return Ok(());
    }
    if state::snapshot().active {
        return Err("请先完成或取消导入".into());
    }
    state::with(|s| {
        s.discovering = true;
        s.status = "正在读取 AstroBox 设备".into();
    });
    ui::rerender();
    // Connected devices come from the host runtime directly. Historical devices
    // require a frontend round trip and must not delay the usable device list.
    let connected = device::get_connected_device_list().await;
    let addresses: Vec<String> = connected.iter().map(|d| d.addr.clone()).collect();
    let devices: Vec<(String, String)> = connected.into_iter().map(|d| (d.addr, d.name)).collect();
    state::with(|s| {
        s.devices = devices;
        s.connected_devices = addresses;
        s.discovering = false;
        s.discovered = true;
        if !s.connected_devices.iter().any(|a| a == &s.addr) {
            s.direct = false;
        }
        s.status = if s.direct {
            "音乐连接已就绪".into()
        } else if s.devices.is_empty() {
            "没有已连接的设备，请先在 AstroBox 的设备页连接手环，再刷新".into()
        } else {
            "点击已连接的手环，建立音乐连接".into()
        };
    });
    Ok(())
}

pub async fn connect_device(addr: &str) -> Result<(), String> {
    if state::snapshot().active {
        return Err("请先完成或取消导入".into());
    }
    if !state::snapshot()
        .connected_devices
        .iter()
        .any(|a| a == addr)
    {
        return Err("请先在 AstroBox 设备页连接这只手环，然后刷新设备".into());
    }
    let registered = link(|l| l.registered_addr.clone());
    if !registered.is_empty() && registered != addr {
        return Err("切换手环前请重新加载 ChemClient，以清除旧设备消息订阅".into());
    }
    state::with(|s| {
        s.addr = addr.into();
        s.device_name = s
            .devices
            .iter()
            .find(|d| d.0 == s.addr)
            .map(|d| d.1.clone())
            .unwrap_or_default();
        s.direct = false;
        s.status = "正在连接 NoirMusic…".into();
    });
    ui::rerender();
    let addr = state::snapshot().addr;
    if addr.is_empty() {
        return Err("请先在 AstroBox 连接手环".into());
    }
    if registered.is_empty() {
        register::register_transport_recv(
            addr.clone(),
            register::TransportRecvFiler {
                xiaomi_vela_v5_channel_id: 0,
                xiaomi_vela_v5_protobuf_typeid: 20,
            },
        )
        .await
        .map_err(|e| format!("注册消息接收失败：{e}"))?;
        link(|l| l.registered_addr = addr.clone());
    }
    let has_timer = link(|l| l.timer_id != 0);
    if !has_timer {
        let id = timer::set_interval(25, "music-link");
        link(|l| l.timer_id = id);
    }
    send_command(1, vec![]).await
}
pub async fn send_command(op: u8, args: Vec<u8>) -> Result<(), String> {
    let addr = state::with(|s| s.addr.clone());
    if addr.is_empty() {
        return Err("请先连接手环".into());
    }
    let frame = link(|l| {
        if l.pending.is_some() {
            return Err("上一操作尚未完成".to_string());
        }
        l.next = l.next.wrapping_add(1).max(1);
        let frame = protocol::frame(l.next, op, &args);
        l.command = op;
        l.pending = Some(Pending {
            id: l.next,
            frame: frame.clone(),
            since: Instant::now(),
            last_send: Instant::now(),
            attempts: 1,
            polls: 0,
            sending: true,
        });
        Ok((l.next, frame))
    })?;
    send_pending(addr, frame.0, frame.1).await
}
// Each request has at most one outstanding host send. A delayed failure from an
// older ID cannot clear a newer command (responses may arrive during await).
async fn send_pending(addr: String, id: u32, frame: Vec<u8>) -> Result<(), String> {
    let result = transport::send(addr, protocol::encode(&frame)).await;
    let current = link(|l| {
        let Some(p) = l.pending.as_mut().filter(|p| p.id == id) else {
            return false;
        };
        p.sending = false;
        p.last_send = Instant::now();
        if p.polls == 0 {
            p.since = Instant::now();
        }
        true
    });
    if let Err(e) = result {
        if current {
            let message = format!("发送失败：{e}");
            fail(message.clone());
            return Err(message);
        }
    }
    Ok(())
}
pub async fn tick() {
    let resend = link(|l| {
        let p = l.pending.as_mut()?;
        if p.sending {
            return None;
        }
        if p.since.elapsed().as_secs() > 30 {
            return Some(Err("手环响应超时，请确认启用了新版蓝牙音乐模块".to_string()));
        }
        // File ACKs are available only on the receive owner. Poll promptly
        // during transfer, backing off when flash writes take longer.
        let delay = if matches!(l.command, 3 | 4) {
            (25u128 << p.polls.min(3)).min(100)
        } else { 100 };
        if p.last_send.elapsed().as_millis() < delay {
            return None;
        }
        p.last_send = Instant::now();
        p.polls = p.polls.saturating_add(1);
        p.sending = true;
        // Retrying exactly the same ID is safe. Poll never performs a file write.
        if p.polls % 20 == 0 && p.since.elapsed().as_secs() >= u64::from(p.attempts) * 2 {
            p.attempts = p.attempts.saturating_add(1);
            Some(Ok((p.id, p.frame.clone())))
        } else {
            Some(Ok((p.id, protocol::frame(p.id, 0, &[]))))
        }
    });
    match resend {
        Some(Ok((id, frame))) => {
            let addr = state::with(|s| s.addr.clone());
            let _ = send_pending(addr, id, frame).await;
        }
        Some(Err(e)) => fail(e),
        None => {}
    }
}
pub async fn received(payload: &str) {
    if payload.len() > 12000 {
        return;
    }
    let Ok(packet) = STANDARD.decode(payload) else {
        return;
    };
    let Some((id, status, value)) = protocol::decode(&packet) else {
        return;
    };
    // A poll may arrive before the original command; resend the same ID.
    if status == -2 {
        let frame = link(|l| {
            let p = l.pending.as_mut().filter(|p| p.id == id && !p.sending)?;
            p.sending = true;
            Some(p.frame.clone())
        });
        if let Some(frame) = frame {
            let _ = send_pending(state::with(|s| s.addr.clone()), id, frame).await;
        }
        return;
    }
    let op = link(|l| {
        let p = l.pending.as_ref()?;
        if p.id != id {
            return None;
        }
        if status == 1 {
            return None;
        }
        let op = l.command;
        l.pending = None;
        Some(op)
    });
    let Some(op) = op else {
        return;
    };
    if link(|l| l.upload.as_ref().is_some_and(|u| u.cancel_requested))
        && matches!(op, 3 | 4)
        && status >= 0
    {
        if let Err(error) = send_command(6, vec![]).await {
            fail(error);
        }
        return;
    }
    if status < 0 {
        fail(
            value["error"]
                .as_str()
                .unwrap_or(if status == -16 {
                    "手环正在执行其他导入任务"
                } else {
                    "原生导入请求失败"
                })
                .to_string(),
        );
        return;
    }
    let result = match op {
        1 => {
            if value["direct"] != true {
                Err("手环直连协议不匹配".into())
            } else {
                state::with(|s| {
                    s.direct = true;
                    s.sbc_supported = value["sbc_predecode"] == true;
                    s.attachments_supported = value["media_assets"] == 1;
                    s.cover_side = value["cover_side"].as_u64().unwrap_or(0).min(u64::from(u32::MAX)) as u32;
                    s.status = "已直连 · 无需打开手环快应用".into();
                });
                Ok(())
            }
        }
        2 => {
            let rows = value["tracks"].as_array().cloned().unwrap_or_default();
            let count = rows.len() as u32;
            state::with(|s| s.library.extend(rows));
            let next = link(|l| {
                l.list_offset += count;
                l.list_offset
            });
            if next < (value["total"].as_u64().unwrap_or(0) as u32) && count > 0 {
                send_command(2, next.to_le_bytes().to_vec()).await
            } else {
                state::with(|s| s.status = format!("曲库 · {} 首歌曲", s.library.len()));
                Ok(())
            }
        }
        3 => send_next().await,
        4 => {
            let checked = link(|l| {
                let u = l.upload.as_mut().ok_or("导入状态丢失")?;
                let offset = value["offset"].as_u64().ok_or("手环未返回写入位置")?;
                if offset != u.offset + u.chunk_len {
                    return Err("手环写入位置不匹配");
                }
                u.offset = offset;
                state::with(|s| {
                    s.sent = offset;
                    s.speed = (offset as f64 / u.started.elapsed().as_secs_f64().max(0.01)) as u64;
                });
                Ok(())
            });
            match checked {
                Ok(()) => send_next().await,
                Err(e) => Err(e.into()),
            }
        }
        5 => {
            // Only count assets after the watch acknowledged durable commit.
            link(|l| { if let Some(u)=&l.upload { l.completed_assets |= u.asset; } });
            cleanup_upload();
            let asset=link(|l|l.assets.pop_front());
            if let Some((path,song,format))=asset {
                let cleanup=path.clone();
                let result=begin_upload(path,song,true,format).await;
                if result.is_err() { let _=fs::remove_file(cleanup); }
                result
            } else {
            let assets=link(|l|l.completed_assets);
            state::with(|s| {
                s.active = false;
                s.sent = s.total;
                s.status = format!("导入完成 · {} · {}",
                    if assets&1!=0 {"含封面"} else {"无封面"},
                    if assets&2!=0 {"含歌词"} else {"无歌词"});
            });
            Ok(())
            }
        }
        6 => {
            cleanup_upload();
            cleanup_assets();
            state::with(|s| {
                s.active = false;
                s.status = "已取消，临时音频已移除".into();
            });
            Ok(())
        }
        7 => {
            state::with(|s| s.status = "歌曲已删除".into());
            refresh_library().await
        }
        _ => Ok(()),
    };
    if let Err(e) = result {
        fail(e);
    }
    let render = link(|l| {
        if op != 4
            || l.last_progress_render
                .is_none_or(|t| t.elapsed().as_millis() >= 400)
        {
            l.last_progress_render = Some(Instant::now());
            true
        } else {
            false
        }
    });
    if render {
        ui::rerender();
    }
}
pub async fn refresh_library() -> Result<(), String> {
    if !state::snapshot().direct {
        return Err("请先连接 NoirMusic".into());
    }
    if busy() {
        return Err("上一操作尚未完成，请稍候".into());
    }
    state::with(|s| {
        s.library.clear();
        s.status = "正在读取手环曲库…".into();
    });
    ui::rerender();
    link(|l| l.list_offset = 0);
    send_command(2, 0u32.to_le_bytes().to_vec()).await
}
pub fn busy() -> bool {
    link(|l| l.pending.is_some() || l.upload.is_some())
}
pub async fn start(path: String, mut song: Value, temporary: bool) -> Result<(), String> {
    if busy() {
        return Err("请先完成当前操作".into());
    }
    if !state::snapshot().direct {
        return Err("请先连接原生音乐模块".into());
    }
    link(|l| l.completed_assets=0);
    let assets=song.as_object_mut().and_then(|s|s.remove("_attachments")).unwrap_or(Value::Null);
    if let Some(assets)=assets.as_array() {
        if !assets.is_empty() && !state::snapshot().attachments_supported { return Err("手环版本不支持封面与歌词".into()); }
        if assets.iter().any(|a|a["format"]=="cover1") && state::snapshot().cover_side<crate::attachments::SIDE {
            crate::attachments::cleanup(&json!({"_attachments":assets}));
            return Err("160px 封面需要先更新 NoirMusic 至 0.4.50 或更新版本".into());
        }
        link(|l|for item in assets {
            if let (Some(path),Some(format))=(item["path"].as_str(),item["format"].as_str()) {
                l.assets.push_back((path.into(),song.clone(),format.into()));
            }
        });
    }
    let result=begin_upload(path,song,temporary,String::new()).await;
    if result.is_err() { cleanup_assets(); }
    result
}
async fn begin_upload(path:String,song:Value,temporary:bool, mut format:String) -> Result<(),String> {
    let mut file = File::open(&path).map_err(|e| format!("打开文件失败：{e}"))?;
    let size = file.metadata().map_err(|e| e.to_string())?.len();
    let asset=!format.is_empty();
    let limit=if format=="cover1" {crate::attachments::COVER_BYTES} else if asset {96*1024} else {128*1024*1024};
    if size == 0 || size > limit {
        return Err(if asset {"封面或歌词大小无效"} else {"请选择 128 MiB 以内的音频"}.into());
    }
    if !asset {
    let mut header = [0u8; 65536];
    let n = file.read(&mut header).map_err(|e| e.to_string())?;
    let sbc = noirmusic_format::Header::parse(&header[..n]);
    if header[..n].starts_with(b"NSB1") && sbc.is_none() {
        return Err("SBC 文件头无效".into());
    }
    if let Some(h) = sbc {
        if !state::snapshot().sbc_supported || h.file_bytes() != size {
            return Err("SBC 文件不完整或手环模块版本过旧".into());
        }
    } else if !header[..n].starts_with(b"ID3")
        && !header[..n]
            .windows(2)
            .any(|p| p[0] == 255 && p[1] & 0xe0 == 0xe0)
    {
        return Err("文件不是有效 MP3".into());
    }
    file.seek(SeekFrom::Start(0)).map_err(|e| e.to_string())?;
    format=if sbc.is_some() {"nsbc1"} else {"mp3"}.into();
    }
    state::with(|s| {
        s.active = true;
        s.sent = 0;
        s.total = size;
        s.status = if asset { "正在导入封面与歌词" } else { "正在准备导入" }.into();
        s.speed = 0;
    });
    let args = serde_json::to_vec(
        &json!({"size":size,"song":song,"format":format}),
    )
    .map_err(|e| e.to_string())?;
    link(|l| {
        l.upload = Some(Upload {
            file,
            path,
            temporary,
            size,
            offset: 0,
            crc: !0,
            started: Instant::now(),
            chunk_len: 0,
            cancel_requested: false,
            asset: if format=="cover1" {1} else if asset {2} else {0},
        })
    });
    send_command(3, args).await
}
fn crc32(mut crc: u32, bytes: &[u8]) -> u32 {
    const NIBBLES: [u32;16] = [
        0x00000000,0x1db71064,0x3b6e20c8,0x26d930ac,
        0x76dc4190,0x6b6b51f4,0x4db26158,0x5005713c,
        0xedb88320,0xf00f9344,0xd6d6a3e8,0xcb61b38c,
        0x9b64c2b0,0x86d3d2d4,0xa00ae278,0xbdbdf21c,
    ];
    for b in bytes {
        crc ^= *b as u32;
        crc = (crc >> 4) ^ NIBBLES[(crc & 15) as usize];
        crc = (crc >> 4) ^ NIBBLES[(crc & 15) as usize];
    }
    crc
}
async fn send_next() -> Result<(), String> {
    let next = link(|l| {
        let u = l.upload.as_mut().ok_or("导入状态丢失")?;
        if u.offset == u.size {
            return Ok((5, serde_json::to_vec(&json!({"crc32":!u.crc})).unwrap()));
        }
        let n = (u.size - u.offset).min(protocol::CHUNK as u64) as usize;
        let mut chunk = vec![0u8; n + 4];
        chunk[..4].copy_from_slice(&(u.offset as u32).to_le_bytes());
        u.file
            .read_exact(&mut chunk[4..])
            .map_err(|e| format!("读取音频失败：{e}"))?;
        u.crc = crc32(u.crc, &chunk[4..]);
        u.chunk_len = n as u64;
        Ok::<_, String>((4, chunk))
    })?;
    state::with(|s| {
        s.status = if next.0 == 5 {
            "正在保存曲库".into()
        } else {
            "正在导入".into()
        }
    });
    send_command(next.0, next.1).await
}
fn cleanup_upload() {
    let upload = link(|l| l.upload.take());
    if let Some(u) = upload {
        drop(u.file);
        if u.temporary {
            let _ = fs::remove_file(&u.path);
            if let Some(p) = std::path::Path::new(&u.path).parent() {
                let _ = fs::remove_dir(p);
            }
        }
    }
}
fn fail(e: String) {
    link(|l| l.pending = None);
    cleanup_upload();
    cleanup_assets();
    state::error(e);
    ui::rerender();
}
fn cleanup_assets() {
    let assets=link(|l|std::mem::take(&mut l.assets));
    for (path,_,_) in assets { let _=fs::remove_file(path); }
}
pub async fn cancel() -> Result<(), String> {
    if link(|l| l.command == 5 && l.pending.is_some()) {
        return Err("正在保存歌曲，请稍候".into());
    }
    if link(|l| {
        if l.pending.is_some() {
            if let Some(u) = l.upload.as_mut() {
                u.cancel_requested = true;
                return true;
            }
        }
        false
    }) {
        state::with(|s| s.status = "正在取消，等待当前分块写入".into());
        return Ok(());
    }
    send_command(6, vec![]).await
}

#[cfg(test)]
mod tests {
    #[test]
    fn incremental_crc_matches_the_device_and_standard_vector() {
        assert_eq!(!super::crc32(!0,b"123456789"),0xcbf43926);
        let data=vec![0xa5;32777];
        let mut expected=!0u32;
        for b in &data {
            expected^=u32::from(*b);
            for _ in 0..8 { expected=(expected>>1)^(0xedb88320u32.wrapping_mul(expected&1)); }
        }
        let mut actual=!0;
        for chunk in data.chunks(16384) { actual=super::crc32(actual,chunk); }
        assert_eq!(actual,expected);
    }
}

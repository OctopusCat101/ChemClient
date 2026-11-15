use crate::{
    astrobox::psys_host_v4::{register, timer, transport},
    connection, protocol, state, ui,
};
use base64::{Engine, engine::general_purpose::STANDARD};
use serde_json::{Value, json};
use std::{
    collections::VecDeque,
    fs::{self, File},
    io::{Read, Seek, SeekFrom},
    sync::{Mutex, OnceLock},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

struct Pending {
    id: u32,
    frame: Vec<u8>,
    since: Instant,
    last_send: Instant,
    attempts: u8,
    polls: u16,
    sending: bool,
    progress: Instant,
    full_sent: Instant,
    needs_full: bool,
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
    chunk_crc: u32,
    cancel_requested: bool,
    asset: u8,
    song: Value,
    addr: String,
}
#[derive(Default)]
struct Link {
    next: u32,
    pending: Option<Pending>,
    upload: Option<Upload>,
    command: u8,
    list_offset: u32,
    timer_id: u64,
    registered_addrs: Vec<String>,
    last_progress_render: Option<Instant>,
    assets: VecDeque<(String, Value, String)>,
    completed_assets: u8,
    chunk_size: usize,
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

pub fn reset_connection() {
    let timer = link(|l| {
        l.pending = None;
        l.list_offset = 0;
        l.registered_addrs.clear();
        std::mem::take(&mut l.timer_id)
    });
    if timer != 0 {
        timer::clear_timer(timer);
    }
    state::with(|s| {
        s.direct = false;
        s.sbc_supported = false;
        s.attachments_supported = false;
        s.cover_side = 0;
        s.library.clear();
    });
}
pub fn disconnected() {
    if link(|l| l.upload.is_some() || l.pending.is_some()) {
        if !retain_upload() {
            fail("手环已断开，导入未完成，请重新连接后重试".into());
        }
    }
    reset_connection();
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
    state::with(|s| {
        s.addr = addr.into();
        s.device_name = s
            .devices
            .iter()
            .find(|d| d.0 == s.addr)
            .map(|d| d.1.clone())
            .unwrap_or_default();
        s.direct = false;
    });
    ui::rerender();
    let addr = state::snapshot().addr;
    if addr.is_empty() {
        return Err("请先在 AstroBox 连接手环".into());
    }
    let epoch = state::snapshot().connection_epoch;
    ensure_receiver(&addr).await?;
    if state::snapshot().connection_epoch != epoch {
        return Ok(());
    }
    send_command(1, vec![]).await
}
pub async fn ensure_receiver(addr: &str) -> Result<(), String> {
    if !link(|l| l.registered_addrs.iter().any(|a| a == addr)) {
        let epoch = state::snapshot().connection_epoch;
        connection::bounded(
            5000,
            register::register_transport_recv(
                addr.to_owned(),
                register::TransportRecvFiler {
                    xiaomi_vela_v5_channel_id: 0,
                    xiaomi_vela_v5_protobuf_typeid: 20,
                },
            ),
        )
        .await?
        .map_err(|e| format!("注册消息接收失败：{e}"))?;
        if state::snapshot().connection_epoch != epoch {
            return Err("连接已变化".into());
        }
        link(|l| l.registered_addrs.push(addr.to_owned()));
    }
    let has_timer = link(|l| l.timer_id != 0);
    if !has_timer {
        let id = timer::set_interval(25, "music-link");
        link(|l| l.timer_id = id);
    }
    Ok(())
}
pub async fn send_command(op: u8, args: Vec<u8>) -> Result<(), String> {
    let s = state::snapshot();
    if !s.device_connected() {
        return Err("请先连接手环".into());
    }
    let addr = s.addr;
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
            sending: false,
            progress: Instant::now(),
            full_sent: Instant::now(),
            needs_full: true,
        });
        Ok((l.next, frame))
    })?;
    send_pending(addr, frame.0, frame.1).await
}
// Each request has at most one outstanding host send. A delayed failure from an
// older ID cannot clear a newer command (responses may arrive during await).
async fn send_pending(addr: String, id: u32, frame: Vec<u8>) -> Result<(), String> {
    let epoch = state::snapshot().connection_epoch;
    if !link(|l| {
        let Some(p) = l.pending.as_mut().filter(|p| p.id == id) else {
            return false;
        };
        if p.sending {
            return false;
        }
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
    if frame.get(8) != Some(&0) {
        link(|l| {
            if let Some(p) = l.pending.as_mut() {
                p.full_sent = Instant::now();
                p.needs_full = false;
            }
        });
    }
    let budget = if frame.len() > 1024 { 30000 } else { 8000 };
    let result = connection::bounded(budget, transport::send(addr, protocol::encode(&frame)))
        .await
        .and_then(|r| r.map_err(|e| e.to_string()));
    let current = link(|l| {
        let Some(p) = l.pending.as_mut().filter(|p| p.id == id) else {
            return false;
        };
        p.sending = false;
        p.last_send = Instant::now();
        true
    });
    if let Err(e) = result {
        if current {
            tracing::warn!("music send {id} deferred after host error: {e}");
            link(|l| {
                if let Some(p) = l.pending.as_mut().filter(|p| p.id == id) {
                    p.needs_full = true;
                }
                l.chunk_size = (l.chunk_size / 2).max(2048);
            });
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
        if p.progress.elapsed().as_secs() >= if l.command == 1 { 10 } else { 45 }
            || p.since.elapsed().as_secs() >= 180
        {
            return Some(Err("音乐传输长时间未收到应答，请重新连接后重试".to_string()));
        }
        // File ACKs are available only on the receive owner. Poll promptly
        // during transfer, backing off when flash writes take longer.
        let delay = if matches!(l.command, 3 | 4) {
            (25u128 << p.polls.min(3)).min(100)
        } else {
            100
        };
        if p.last_send.elapsed().as_millis() < delay {
            return None;
        }
        p.last_send = Instant::now();
        p.polls = p.polls.saturating_add(1);
        // Retransmit only after explicit unknown-ID or a long lost-ACK window.
        if (p.needs_full && p.full_sent.elapsed() >= Duration::from_secs(2))
            || p.full_sent.elapsed() >= Duration::from_secs(12)
        {
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
        Some(Err(e)) => {
            if retain_upload() {
                state::with(|s| s.direct = false);
            } else {
                fail(e);
            }
        }
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
    if !link(|l| l.pending.as_ref().is_some_and(|p| p.id == id)) {
        return;
    }
    connection::reply_received();
    if status == -2 {
        link(|l| {
            if let Some(p) = l.pending.as_mut().filter(|p| p.id == id) {
                p.needs_full = true;
            }
        });
        return;
    }
    let op = link(|l| {
        let p = l.pending.as_mut()?;
        if p.id != id {
            return None;
        }
        if status == 1 {
            p.progress = Instant::now();
            return None;
        }
        if l.command == 4 {
            let elapsed = p.since.elapsed();
            l.chunk_size = if elapsed >= Duration::from_secs(4) {
                (l.chunk_size / 2).max(2048)
            } else if elapsed < Duration::from_secs(1) {
                (l.chunk_size * 2).clamp(2048, protocol::CHUNK)
            } else {
                l.chunk_size
            };
        }
        let op = l.command;
        l.pending = None;
        Some(op)
    });
    let Some(op) = op else {
        return;
    };
    connection::reply_received();
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
            } else if !state::with(|s| {
                s.manager_connected() && s.modules.iter().any(|m| m.importable())
            }) {
                Ok(())
            } else {
                state::with(|s| {
                    s.direct = true;
                    s.sbc_supported = value["sbc_predecode"] == true;
                    s.attachments_supported = value["media_assets"] == 1;
                    s.cover_side = value["cover_side"]
                        .as_u64()
                        .unwrap_or(0)
                        .min(u64::from(u32::MAX)) as u32;
                    s.connection = connection::Phase::Ready;
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
                state::with(|s| {
                    s.notice(
                        state::NoticeOwner::Library,
                        format!("曲库 · {} 首歌曲", s.library.len()),
                    )
                });
                Ok(())
            }
        }
        3 => send_next().await,
        4 => {
            let checked: Result<(), &str> = link(|l| {
                let u = l.upload.as_mut().ok_or("导入状态丢失")?;
                let offset = value["offset"].as_u64().ok_or("手环未返回写入位置")?;
                confirm_chunk(u, offset)?;
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
            link(|l| {
                if let Some(u) = &l.upload {
                    l.completed_assets |= u.asset;
                }
            });
            cleanup_upload();
            let asset = link(|l| l.assets.pop_front());
            if let Some((path, song, format)) = asset {
                let cleanup = path.clone();
                let result = begin_upload(path, song, true, format).await;
                if result.is_err() {
                    let _ = fs::remove_file(cleanup);
                }
                result
            } else {
                let assets = link(|l| l.completed_assets);
                state::with(|s| {
                    s.active = false;
                    s.sent = s.total;
                    s.status_owner = state::NoticeOwner::Music;
                    s.status = format!(
                        "导入完成 · {} · {}",
                        if assets & 1 != 0 {
                            "含封面"
                        } else {
                            "无封面"
                        },
                        if assets & 2 != 0 {
                            "含歌词"
                        } else {
                            "无歌词"
                        }
                    );
                });
                state::with(|s| {
                    if let Some(job) = &mut s.song_job {
                        job.message = s.status.clone();
                    }
                });
                Ok(())
            }
        }
        6 => {
            cleanup_upload();
            cleanup_assets();
            state::with(|s| {
                s.active = false;
                s.notice(state::NoticeOwner::Music, "已取消，临时音频已移除");
                if let Some(job) = &mut s.song_job {
                    job.message = s.status.clone();
                }
            });
            Ok(())
        }
        7 => {
            state::with(|s| s.notice(state::NoticeOwner::Library, "歌曲已删除"));
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
    if !state::snapshot().music_connected() {
        return Err("请先连接 NoirMusic".into());
    }
    if busy() {
        return Err("上一操作尚未完成，请稍候".into());
    }
    state::with(|s| {
        s.library.clear();
        s.notice(state::NoticeOwner::Library, "正在读取手环曲库…");
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
    if !state::snapshot().music_connected() {
        crate::prepared::retain(
            path,
            song,
            temporary,
            state::snapshot().addr,
            crate::prepared::Stage::Upload,
        );
        return Ok(());
    }
    link(|l| l.completed_assets = 0);
    let assets = song
        .as_object_mut()
        .and_then(|s| s.remove("_attachments"))
        .unwrap_or(Value::Null);
    if let Some(assets) = assets.as_array() {
        if !assets.is_empty() && !state::snapshot().attachments_supported {
            return Err("手环版本不支持封面与歌词".into());
        }
        if assets.iter().any(|a| a["format"] == "cover1")
            && state::snapshot().cover_side < crate::attachments::SIDE
        {
            crate::attachments::cleanup(&json!({"_attachments":assets}));
            return Err("160px 封面需要先更新 NoirMusic 至 0.4.50 或更新版本".into());
        }
        link(|l| {
            for item in assets {
                if let (Some(path), Some(format)) = (item["path"].as_str(), item["format"].as_str())
                {
                    l.assets
                        .push_back((path.into(), song.clone(), format.into()));
                }
            }
        });
    }
    let result = begin_upload(path, song, temporary, String::new()).await;
    if result.is_err() {
        cleanup_assets();
    }
    result
}
async fn begin_upload(
    path: String,
    song: Value,
    temporary: bool,
    mut format: String,
) -> Result<(), String> {
    let mut file = File::open(&path).map_err(|e| format!("打开文件失败：{e}"))?;
    let size = file.metadata().map_err(|e| e.to_string())?.len();
    let asset = !format.is_empty();
    let limit = if format == "cover1" {
        crate::attachments::COVER_BYTES
    } else if asset {
        96 * 1024
    } else {
        128 * 1024 * 1024
    };
    if size == 0 || size > limit {
        return Err(if asset {
            "封面或歌词大小无效"
        } else {
            "请选择 128 MiB 以内的音频"
        }
        .into());
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
        format = if sbc.is_some() { "nsbc1" } else { "mp3" }.into();
    }
    state::with(|s| {
        s.active = true;
        s.sent = 0;
        s.total = size;
        s.status_owner = state::NoticeOwner::Music;
        s.status = if asset {
            "正在导入封面与歌词"
        } else {
            "正在准备导入"
        }
        .into();
        s.speed = 0;
    });
    let args = serde_json::to_vec(&json!({"size":size,"song":song,"format":format}))
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
            chunk_crc: !0,
            cancel_requested: false,
            song,
            addr: state::snapshot().addr,
            asset: if format == "cover1" {
                1
            } else if asset {
                2
            } else {
                0
            },
        })
    });
    link(|l| {
        if l.chunk_size == 0 {
            l.chunk_size = 4096;
        }
    });
    let result = send_command(3, args).await;
    if result.is_err() {
        if retain_upload() {
            return Ok(());
        }
        cleanup_upload();
        state::with(|s| s.active = false);
    }
    result
}
fn crc32(mut crc: u32, bytes: &[u8]) -> u32 {
    const NIBBLES: [u32; 16] = [
        0x00000000, 0x1db71064, 0x3b6e20c8, 0x26d930ac, 0x76dc4190, 0x6b6b51f4, 0x4db26158,
        0x5005713c, 0xedb88320, 0xf00f9344, 0xd6d6a3e8, 0xcb61b38c, 0x9b64c2b0, 0x86d3d2d4,
        0xa00ae278, 0xbdbdf21c,
    ];
    for b in bytes {
        crc ^= *b as u32;
        crc = (crc >> 4) ^ NIBBLES[(crc & 15) as usize];
        crc = (crc >> 4) ^ NIBBLES[(crc & 15) as usize];
    }
    crc
}
fn confirm_chunk(u: &mut Upload, offset: u64) -> Result<(), &'static str> {
    if u.chunk_len == 0 || offset != u.offset + u.chunk_len {
        return Err("手环写入位置不匹配");
    }
    u.offset = offset;
    u.crc = u.chunk_crc;
    u.chunk_len = 0;
    Ok(())
}
fn prepare_chunk(u: &mut Upload, chunk_size: usize) -> Result<(u8, Vec<u8>), String> {
    if u.offset == u.size {
        return Ok((5, serde_json::to_vec(&json!({"crc32":!u.crc})).unwrap()));
    }
    let n = (u.size - u.offset).min(chunk_size.clamp(2048, protocol::CHUNK) as u64) as usize;
    let mut chunk = vec![0u8; n + 4];
    chunk[..4].copy_from_slice(&(u.offset as u32).to_le_bytes());
    u.file
        .seek(SeekFrom::Start(u.offset))
        .map_err(|e| e.to_string())?;
    u.file
        .read_exact(&mut chunk[4..])
        .map_err(|e| format!("读取音频失败：{e}"))?;
    u.chunk_crc = crc32(u.crc, &chunk[4..]);
    u.chunk_len = n as u64;
    Ok((4, chunk))
}
async fn send_next() -> Result<(), String> {
    let next = link(|l| {
        let u = l.upload.as_mut().ok_or("导入状态丢失")?;
        prepare_chunk(u, l.chunk_size)
    })?;
    state::with(|s| {
        s.status_owner = state::NoticeOwner::Music;
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
fn retain_upload() -> bool {
    let upload = link(|l| {
        if l.upload
            .as_ref()
            .is_none_or(|u| u.asset != 0 || u.cancel_requested)
        {
            return None;
        }
        l.pending = None;
        l.upload.take()
    });
    let Some(u) = upload else {
        return false;
    };
    let mut song = u.song;
    let assets = link(|l| std::mem::take(&mut l.assets));
    if !assets.is_empty() {
        song["_attachments"] = Value::Array(
            assets
                .into_iter()
                .map(|(path, _, format)| json!({"path":path,"format":format}))
                .collect(),
        );
    }
    drop(u.file);
    crate::prepared::retain(
        u.path,
        song,
        u.temporary,
        u.addr,
        crate::prepared::Stage::Upload,
    );
    true
}
fn fail(e: String) {
    let handshake = link(|l| {
        l.pending = None;
        l.command == 1
    });
    if handshake {
        state::with(|s| s.direct = false);
        tracing::warn!("music handshake: {e}");
        return;
    }
    cleanup_upload();
    cleanup_assets();
    state::error(e);
    ui::rerender();
}
fn cleanup_assets() {
    let assets = link(|l| std::mem::take(&mut l.assets));
    for (path, _, _) in assets {
        let _ = fs::remove_file(path);
    }
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
        state::with(|s| s.notice(state::NoticeOwner::Music, "正在取消，等待当前分块写入"));
        return Ok(());
    }
    send_command(6, vec![]).await
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rereading_unacknowledged_chunk_does_not_advance_offset_or_crc() {
        let path = std::env::temp_dir().join(format!("chem-crc-{}.bin", std::process::id()));
        let data: Vec<u8> = (0..5000).map(|n| (n % 251) as u8).collect();
        fs::write(&path, &data).unwrap();
        let mut u = Upload {
            file: File::open(&path).unwrap(),
            path: path.to_string_lossy().into(),
            temporary: false,
            size: data.len() as u64,
            offset: 0,
            crc: !0,
            started: Instant::now(),
            chunk_len: 0,
            chunk_crc: !0,
            cancel_requested: false,
            asset: 0,
            song: Value::Null,
            addr: "A".into(),
        };
        let first = prepare_chunk(&mut u, 2048).unwrap();
        assert_eq!(prepare_chunk(&mut u, 2048).unwrap(), first);
        assert_eq!((u.offset, u.crc), (0, !0));
        assert!(confirm_chunk(&mut u, 1024).is_err());
        assert_eq!((u.offset, u.crc), (0, !0));
        confirm_chunk(&mut u, 2048).unwrap();
        assert!(confirm_chunk(&mut u, 2048).is_err());
        assert_eq!(u.crc, crc32(!0, &data[..2048]));
        let (_, second) = prepare_chunk(&mut u, 4096).unwrap();
        assert_eq!(&second[..4], &2048u32.to_le_bytes());
        assert_eq!(&second[4..], &data[2048..]);
        confirm_chunk(&mut u, 5000).unwrap();
        let (op, payload) = prepare_chunk(&mut u, 4096).unwrap();
        assert_eq!(op, 5);
        let commit: Value = serde_json::from_slice(&payload).unwrap();
        assert_eq!(commit["crc32"], json!(!crc32(!0, &data)));
        drop(u);
        fs::remove_file(path).unwrap();
    }
    #[test]
    fn queued_music_send_is_discarded_after_session_changes() {
        use std::{
            future::Future,
            sync::Arc,
            task::{Context, Poll, Wake, Waker},
        };
        struct Noop;
        impl Wake for Noop {
            fn wake(self: Arc<Self>) {}
        }
        let w = Waker::from(Arc::new(Noop));
        let mut cx = Context::from_waker(&w);
        let mut acquire = Box::pin(crate::wire::acquire());
        let Poll::Ready(held) = acquire.as_mut().poll(&mut cx) else {
            panic!()
        };
        let frame = protocol::frame(88, 4, b"data");
        link(|l| {
            l.pending = Some(Pending {
                id: 88,
                frame: frame.clone(),
                since: Instant::now(),
                last_send: Instant::now(),
                attempts: 1,
                polls: 0,
                sending: false,
                progress: Instant::now(),
                full_sent: Instant::now(),
                needs_full: true,
            })
        });
        let mut send = Box::pin(send_pending("A".into(), 88, frame));
        assert!(send.as_mut().poll(&mut cx).is_pending());
        state::with(|s| s.connection_epoch = s.connection_epoch.wrapping_add(1));
        link(|l| l.pending = None);
        drop(held);
        // Reaching the host send here would panic in a native unit test.
        assert!(matches!(send.as_mut().poll(&mut cx), Poll::Ready(Ok(()))));
    }
    #[test]
    fn incremental_crc_matches_the_device_and_standard_vector() {
        assert_eq!(!super::crc32(!0, b"123456789"), 0xcbf43926);
        let data = vec![0xa5; 32777];
        let mut expected = !0u32;
        for b in &data {
            expected ^= u32::from(*b);
            for _ in 0..8 {
                expected = (expected >> 1) ^ (0xedb88320u32.wrapping_mul(expected & 1));
            }
        }
        let mut actual = !0;
        for chunk in data.chunks(16384) {
            actual = super::crc32(actual, chunk);
        }
        assert_eq!(actual, expected);
    }
}

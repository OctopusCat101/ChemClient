use crate::{modules, state, ui};
use std::{
    fs::File,
    io::{Read, Seek, SeekFrom},
    sync::{Mutex, OnceLock},
    time::{Instant, SystemTime, UNIX_EPOCH},
};

const ID: &str = "com.chem.magic";
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Pack {
    pub id: String,
    pub name: String,
    pub enabled: bool,
}
#[derive(Clone, Default)]
pub struct View {
    pub rows: Vec<Pack>,
    pub busy: bool,
    pub sent: u64,
    pub total: u64,
    pub message: String,
}
#[derive(Clone, Copy)]
enum Step {
    List(u32),
    Begin,
    Chunk(u32),
    Finish,
    Wait,
    Toggle(u32),
    Remove(u32),
    Cancel,
}
struct Pending {
    sequence: u32,
    ack: bool,
    reply: Option<Vec<u8>>,
    started: Instant,
}
#[derive(Default)]
struct Link {
    view: View,
    step: Option<Step>,
    chunk_size: usize,
    polled: Option<Instant>,
    rendered: Option<Instant>,
    pending: Option<Pending>,
    file: Option<File>,
    temporary: Option<String>,
    crc: u32,
    next: u32,
    epoch: u64,
    timer: bool,
    waiting: Option<Instant>,
    cancel: bool,
}
static LINK: OnceLock<Mutex<Link>> = OnceLock::new();
fn with<R>(f: impl FnOnce(&mut Link) -> R) -> R {
    f(&mut LINK
        .get_or_init(|| Mutex::new(Link::default()))
        .lock()
        .unwrap_or_else(|e| e.into_inner()))
}
pub fn snapshot() -> View {
    with(|l| l.view.clone())
}
#[cfg(test)]
pub fn test_view(view: View) {
    with(|l| l.view = view);
}
pub fn busy() -> bool {
    with(|l| l.view.busy)
}
fn word(b: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(b.get(at..at + 4)?.try_into().ok()?))
}
fn list_row(b: &[u8], index: u32) -> Result<Option<Pack>, String> {
    let count = word(b, 16).ok_or("资源包列表格式无效")?;
    if count > 16 {
        return Err("资源包列表格式无效".into());
    }
    if index >= count {
        return if b.len() == 28 {
            Ok(None)
        } else {
            Err("资源包列表格式无效".into())
        };
    }
    if b.len() != 159 || b[28] > 1 || b[93] != 0 || b[158] != 0 {
        return Err("资源包列表格式无效".into());
    }
    let name = &b[94..159];
    let end = name.iter().position(|b| *b == 0).ok_or("资源包名称无效")?;
    let text = std::str::from_utf8(&name[..end]).map_err(|_| "资源包名称无效")?;
    if text.is_empty() || text.chars().any(char::is_control) {
        return Err("资源包名称无效".into());
    }
    let folder = &b[29..94];
    let end = folder
        .iter()
        .position(|b| *b == 0)
        .ok_or("资源包标识无效")?;
    let id = std::str::from_utf8(&folder[..end]).map_err(|_| "资源包标识无效")?;
    if id.is_empty()
        || !id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-'))
    {
        return Err("资源包标识无效".into());
    }
    Ok(Some(Pack {
        id: id.into(),
        name: text.into(),
        enabled: b[28] != 0,
    }))
}
fn finish(message: String) {
    with(|l| {
        l.view.busy = false;
        l.view.message = message;
        l.file = None;
        if let Some(path) = l.temporary.take() {
            let _ = std::fs::remove_file(path);
        }
        l.step = None;
        l.pending = None;
        l.waiting = None;
        l.cancel = false;
    });
    state::with(|s| s.active = false);
    ui::rerender();
}
fn begin_session(step: Step) -> Result<(), String> {
    let s = state::snapshot();
    if busy()
        || s.active
        || s.manager_busy
        || s.installing
        || !s.manager_connected()
        || s.connection.busy()
        || !s
            .modules
            .iter()
            .any(|m| m.id == ID && m.intent == 1 && m.error == 0 && matches!(m.state, 9 | 13))
    {
        return Err("请先启用 Magic 并等待连接完成".into());
    }
    if s.resident_capabilities & 4 == 0 {
        return Err("当前 Chem 未提供资源包导入接口，请安装本地新版 Chem 安装器并重启".into());
    }
    with(|l| {
        if l.next == 0 {
            let seed = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos();
            l.next = ((seed as u32) ^ ((seed >> 32) as u32)).max(1);
        }
        l.view.busy = true;
        l.view.sent = 0;
        l.view.total = 0;
        l.view.message = "正在读取资源包…".into();
        l.step = Some(step);
        l.chunk_size = if s.bulk_delivery { 16384 } else { 448 };
        l.pending = None;
        l.epoch = s.connection_epoch;
        l.cancel = false;
        if !l.timer {
            crate::astrobox::psys_host_v4::timer::set_interval(25, "magic-link");
            l.timer = true;
        }
    });
    state::with(|s| s.active = true);
    Ok(())
}
pub async fn refresh() -> Result<(), String> {
    begin_session(Step::List(0))?;
    with(|l| {
        l.view.rows.clear();
        l.view.sent = 0;
        l.view.total = 0;
    });
    advance().await;
    Ok(())
}
pub async fn import(path: &str) -> Result<(), String> {
    let mut file = File::open(path).map_err(|e| e.to_string())?;
    let size = file.metadata().map_err(|e| e.to_string())?.len();
    if !(138..=16 * 1024 * 1024).contains(&size) {
        return Err("资源包大小无效，最大 16 MiB".into());
    }
    let mut header = [0u8; 138];
    file.read_exact(&mut header).map_err(|e| e.to_string())?;
    if &header[..4] != b"MGP1" {
        return Err("请选择 Magic 的 .magic 资源包".into());
    }
    file.rewind().map_err(|e| e.to_string())?;
    let mut crc = u32::MAX;
    let mut buffer = [0u8; 8192];
    loop {
        let n = file.read(&mut buffer).map_err(|e| e.to_string())?;
        if n == 0 {
            break;
        }
        for b in &buffer[..n] {
            crc ^= u32::from(*b);
            for _ in 0..8 {
                crc = (crc >> 1) ^ (0xedb88320u32 & 0u32.wrapping_sub(crc & 1));
            }
        }
    }
    file.rewind().map_err(|e| e.to_string())?;
    begin_session(Step::Begin)?;
    with(|l| {
        l.file = Some(file);
        l.temporary = Some(path.to_owned());
        l.crc = crc ^ u32::MAX;
        l.view.sent = 0;
        l.view.total = size;
        l.view.message = "正在导入资源包".into();
    });
    advance().await;
    Ok(())
}
pub async fn toggle(index: u32) -> Result<(), String> {
    begin_session(Step::Toggle(index))?;
    advance().await;
    Ok(())
}
pub async fn remove(index: u32) -> Result<(), String> {
    begin_session(Step::Remove(index))?;
    advance().await;
    Ok(())
}
pub fn cancel() {
    with(|l| l.cancel = true);
}
pub fn disconnected() {
    with(|l| l.view.rows.clear());
    if busy() {
        finish("手环已断开，请重新连接后重试".into());
    }
}
pub fn transport_failed(error: &str) {
    if busy() {
        finish(error.into());
    }
}

fn management_error(result: u32, data: &[u8]) -> Option<String> {
    if result == 5 && data == [0] {
        return None;
    }
    Some(match data {
        [1] => "Magic 尚未加载，请在模块列表启用后重试".into(),
        [2] => "Magic 通信监听未注册，请更新 Chem 与 Magic 后重启手环".into(),
        [3] => "通信会话已满，请重新连接手环".into(),
        [5] => "模块收到请求但未返回应答，请更新 Magic 模块并重启".into(),
        [4] => "Chem 不支持此模块的通信请求，请安装最新的 Chem 安装器并重启".into(),
        _ => format!("Magic 通信请求失败（结果 {result}，状态 {data:?}）"),
    })
}
fn exchange_reply(result: u32, data: &[u8]) -> Result<&[u8], String> {
    if result != 5 || data.first() != Some(&0) {
        return Err(management_error(result, data.get(..1).unwrap_or_default())
            .unwrap_or_else(|| "资源包请求失败".into()));
    }
    let reply = data.get(1..).ok_or("模块应答不完整")?;
    if reply.len() < 28 || word(reply, 0) != Some(0x314f474d) {
        return Err("模块应答格式无效，请更新 Magic 模块".into());
    }
    Ok(reply)
}
pub async fn management_response(result: u32, data: &[u8]) {
    if !busy() {
        return;
    }
    let reply = match exchange_reply(result, data) {
        Ok(reply) => reply,
        Err(e) => {
            finish(e);
            return;
        }
    };
    let matched = with(|l| {
        if let Some(p) = l
            .pending
            .as_mut()
            .filter(|p| Some(p.sequence) == word(reply, 4))
        {
            p.ack = true;
            p.reply = Some(reply.to_vec());
            true
        } else {
            false
        }
    });
    if !matched {
        finish("模块应答序号不匹配，请刷新重试".into());
        return;
    }
    advance().await;
}
pub async fn tick() {
    if !busy() {
        return;
    }
    let s = state::snapshot();
    if with(|l| l.epoch != s.connection_epoch) {
        finish("连接已改变，请重新导入".into());
        return;
    }
    if with(|l| l.waiting.is_some_and(|t| t.elapsed().as_secs() > 180)) {
        finish("应用资源包超时，请检查手环中的提示".into());
        return;
    }
    let timeout = with(|l| {
        l.pending
            .as_ref()
            .is_some_and(|p| p.started.elapsed().as_secs() > 75)
    });
    if timeout {
        finish("Magic 未返回数据，请刷新重试".into());
        return;
    }
    advance().await;
}
async fn advance() {
    let next = with(|l| -> Result<Option<(u32, Vec<u8>)>, String> {
        if !l.view.busy {
            return Ok(None);
        }
        if let Some(p) = l.pending.as_ref() {
            if !p.ack || p.reply.is_none() {
                return Ok(None);
            }
            let p = l.pending.take().unwrap();
            if let Some(b) = p.reply {
                let rc = word(&b, 8).unwrap() as i32;
                if rc < 0 {
                    return Err(format!("Magic 操作失败 ({rc})"));
                }
                match l.step {
                    Some(Step::List(i)) => {
                        let count = word(&b, 16).unwrap();
                        if let Some(row) = list_row(&b, i)? {
                            l.view.rows.push(row);
                        }
                        if i + 1 < count {
                            l.step = Some(Step::List(i + 1));
                        } else {
                            l.step = None;
                        }
                    }
                    Some(Step::Begin) => l.step = Some(Step::Chunk(0)),
                    Some(Step::Chunk(previous)) => {
                        let offset = word(&b, 12).unwrap();
                        if offset <= previous
                            || offset as u64 > l.view.total
                            || offset - previous > l.chunk_size as u32
                        {
                            return Err("导入进度无效".into());
                        }
                        l.view.sent = offset as u64;
                        l.step = Some(if l.view.sent == l.view.total {
                            Step::Finish
                        } else {
                            Step::Chunk(offset)
                        });
                    }
                    Some(Step::Finish) | Some(Step::Toggle(_)) | Some(Step::Remove(_)) => {
                        l.step = Some(Step::Wait);
                        l.waiting = Some(Instant::now());
                    }
                    Some(Step::Wait) => {
                        let error = word(&b, 24).unwrap() as i32;
                        if word(&b, 20) == Some(0) {
                            if error != 0 {
                                return Err(format!("资源包未能应用 ({error})"));
                            }
                            l.view.rows.clear();
                            l.step = Some(Step::List(0));
                            l.waiting = None;
                        }
                    }
                    Some(Step::Cancel) => l.step = None,
                    _ => {}
                }
            }
        }
        if l.cancel {
            l.step = Some(Step::Cancel);
            l.cancel = false;
        }
        let Some(step) = l.step.as_ref() else {
            return Ok(Some((0, vec![])));
        };
        if matches!(step, Step::Wait) {
            if l.polled.is_some_and(|p| p.elapsed().as_millis() < 250) {
                return Ok(None);
            }
            l.polled = Some(Instant::now());
        }
        let (op, args) = match *step {
            Step::List(i) => (1, i.to_le_bytes().to_vec()),
            Step::Begin => (
                2,
                [l.view.total as u32, l.crc]
                    .into_iter()
                    .flat_map(u32::to_le_bytes)
                    .collect(),
            ),
            Step::Chunk(offset) => {
                let file = l.file.as_mut().ok_or("导入文件已关闭")?;
                file.seek(SeekFrom::Start(offset as u64))
                    .map_err(|e| e.to_string())?;
                let mut chunk = vec![0; l.chunk_size.min((l.view.total - offset as u64) as usize)];
                file.read_exact(&mut chunk).map_err(|e| e.to_string())?;
                let mut args = offset.to_le_bytes().to_vec();
                args.extend(chunk);
                (3, args)
            }
            Step::Finish => (4, vec![]),
            Step::Toggle(i) => (5, i.to_le_bytes().to_vec()),
            Step::Remove(i) => (7, i.to_le_bytes().to_vec()),
            Step::Cancel => (6, vec![]),
            Step::Wait => (1, u32::MAX.to_le_bytes().to_vec()),
        };
        let mut frame = vec![2, ID.len() as u8];
        frame.extend_from_slice(ID.as_bytes());
        l.next = l.next.wrapping_add(1).max(1);
        {
            frame.extend_from_slice(b"MGI1");
            frame.extend_from_slice(&l.next.to_le_bytes());
            frame.push(op);
            frame.extend(args);
        }
        l.pending = Some(Pending {
            sequence: l.next,
            ack: false,
            reply: None,
            started: Instant::now(),
        });
        Ok(Some((29, frame)))
    });
    match next {
        Err(e) => finish(e),
        Ok(Some((0, _))) => finish("资源包列表已更新".into()),
        Ok(Some((op, frame))) => {
            if let Err(e) = modules::send(op, frame).await {
                finish(e);
            }
            let render = with(|l| {
                if l.rendered.is_none_or(|t| t.elapsed().as_millis() >= 125) {
                    l.rendered = Some(Instant::now());
                    true
                } else {
                    false
                }
            });
            if render {
                ui::rerender();
            }
        }
        Ok(None) => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn management_errors_distinguish_module_listener_and_protocol() {
        assert_eq!(management_error(5, &[0]), None);
        assert!(management_error(3, &[1]).unwrap().contains("尚未加载"));
        assert!(management_error(3, &[2]).unwrap().contains("通信监听"));
        assert!(management_error(1, &[4]).unwrap().contains("安装器"));
        assert!(management_error(5, &[]).is_some());
        assert!(management_error(1, &[0]).is_some());
    }
    fn put(b: &mut [u8], at: usize, value: u32) {
        b[at..at + 4].copy_from_slice(&value.to_le_bytes());
    }
    #[test]
    fn inline_reply_rejects_missing_truncated_and_failed_responses() {
        let mut data = vec![0; 29];
        put(&mut data, 1, 0x314f474d);
        assert_eq!(exchange_reply(5, &data).unwrap().len(), 28);
        for n in 0..data.len() {
            assert!(exchange_reply(5, &data[..n]).is_err());
        }
        assert!(exchange_reply(1, &data).is_err());
        data[1] ^= 1;
        assert!(exchange_reply(5, &data).is_err());
    }
    #[test]
    fn rows_require_full_bounded_utf8_names() {
        let mut b = vec![0; 159];
        put(&mut b, 16, 1);
        b[28] = 1;
        b[94..99].copy_from_slice(b"Alpha");
        b[29..34].copy_from_slice(b"alpha");
        assert_eq!(
            list_row(&b, 0).unwrap(),
            Some(Pack {
                id: "alpha".into(),
                name: "Alpha".into(),
                enabled: true
            })
        );
        for n in 0..b.len() {
            assert!(list_row(&b[..n], 0).is_err());
        }
        b[94] = 0xff;
        assert!(list_row(&b, 0).is_err());
        b[94] = b'\n';
        assert!(list_row(&b, 0).is_err());
        put(&mut b, 16, 17);
        assert!(list_row(&b, 0).is_err());
        let empty = vec![0; 28];
        assert!(list_row(&empty, 0).unwrap().is_none());
    }
}

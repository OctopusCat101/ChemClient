use crate::{modules, state, ui};
use std::{
    fs::File,
    io::Read,
    sync::{Mutex, OnceLock},
    time::{Instant, SystemTime, UNIX_EPOCH},
};

pub const ID: &str = "org.hbl2d";
pub const LEGACY_ID: &str = "org.hbl2d.watchface";
const MAX_BYTES: usize = 512 * 1024;
// CONNECT_EXCHANGE uses the Supervisor's 16-byte address field.
const _: () = assert!(!ID.is_empty() && ID.len() <= 16);

#[derive(Clone, Default)]
pub struct View {
    pub busy: bool,
    pub sent: usize,
    pub total: usize,
    pub message: String,
}

#[derive(Clone, Copy)]
enum Step {
    Begin,
    Chunk(usize),
    Finish,
    Cancel,
}

struct Upload {
    data: Vec<u8>,
    crc: u32,
    chunk_size: usize,
    epoch: u64,
    step: Step,
    cancel: bool,
}

#[derive(Default)]
struct Link {
    view: View,
    upload: Option<Upload>,
    sequence: u32,
    rendered: Option<Instant>,
}

static LINK: OnceLock<Mutex<Link>> = OnceLock::new();
fn with<R>(f: impl FnOnce(&mut Link) -> R) -> R {
    f(&mut LINK
        .get_or_init(Mutex::default)
        .lock()
        .unwrap_or_else(|e| e.into_inner()))
}

pub fn snapshot() -> View {
    with(|link| link.view.clone())
}

pub fn busy() -> bool {
    with(|link| link.upload.is_some())
}

pub fn module_ready(s: &state::State) -> bool {
    s.modules
        .iter()
        .any(|m| m.id == ID && m.intent == 1 && m.error == 0 && matches!(m.state, 9 | 13))
}

pub fn can_import(s: &state::State) -> bool {
    module_ready(s)
        && s.manager_connected()
        && s.resident_capabilities & 4 != 0
        && !s.active
        && !s.manager_busy
        && !s.installing
        && !s.connection.busy()
        && !s.downloading_artifact()
}

fn validate_model(data: &[u8]) -> Result<(), String> {
    if !(128..=MAX_BYTES).contains(&data.len()) || &data[..4] != b"HBM3" {
        return Err("请选择不超过 512 KiB 的 HBL2D .hbm 模型".into());
    }
    let word = |at| u16::from_le_bytes([data[at], data[at + 1]]);
    if word(4) != 3
        || word(6) != 1
        || !(1..=144).contains(&word(8))
        || !(1..=300).contains(&word(10))
        || u32::from_le_bytes(data[84..88].try_into().unwrap()) as usize != data.len()
    {
        return Err("模型须为可分离布局的 HBM3，画面最大 144×300".into());
    }
    Ok(())
}

fn crc32(data: &[u8]) -> u32 {
    let mut crc = u32::MAX;
    for &byte in data {
        crc ^= u32::from(byte);
        for _ in 0..8 {
            crc = (crc >> 1) ^ (0xedb88320 & 0u32.wrapping_sub(crc & 1));
        }
    }
    !crc
}

pub async fn import(path: &str) -> Result<(), String> {
    let mut data = Vec::new();
    File::open(path)
        .and_then(|f| f.take((MAX_BYTES + 1) as u64).read_to_end(&mut data))
        .map_err(|e| format!("读取模型失败（{e}）"))?;
    validate_model(&data)?;
    let crc = crc32(&data);
    let s = state::snapshot();
    if !can_import(&s) || busy() {
        return Err("请连接手环、启用 HBL2D 并等待当前操作完成".into());
    }
    let chunk_size = if s.bulk_delivery {
        modules::BULK_CHUNK
    } else {
        448
    };
    with(|link| {
        if link.sequence == 0 {
            let seed = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos();
            link.sequence = ((seed as u32) ^ ((seed >> 32) as u32)).max(1);
        }
        link.view = View {
            busy: true,
            sent: 0,
            total: data.len(),
            message: "正在导入模型".into(),
        };
        link.upload = Some(Upload {
            data,
            crc,
            chunk_size,
            epoch: s.connection_epoch,
            step: Step::Begin,
            cancel: false,
        });
        link.rendered = None;
    });
    state::with(|s| s.active = true);
    ui::rerender();
    send().await;
    Ok(())
}

pub fn cancel() {
    with(|link| {
        if let Some(upload) = &mut link.upload {
            upload.cancel = true;
            link.view.message = "正在取消导入…".into();
        }
    });
}

pub fn transport_failed(message: &str) {
    finish(message);
}

fn finish(message: &str) {
    if with(|link| {
        if link.upload.take().is_none() {
            return false;
        }
        link.view.busy = false;
        link.view.message = message.into();
        true
    }) {
        state::with(|s| s.active = false);
        ui::rerender();
    }
}

async fn send() {
    let request = with(|link| {
        let upload = link.upload.as_ref()?;
        let sequence = link.sequence.wrapping_add(1).max(1);
        link.sequence = sequence;
        let mut packet = vec![2, ID.len() as u8];
        packet.extend_from_slice(ID.as_bytes());
        packet.extend_from_slice(b"HMI1");
        packet.extend_from_slice(&sequence.to_le_bytes());
        match upload.step {
            Step::Begin => {
                packet.push(2);
                packet.extend_from_slice(&(upload.data.len() as u32).to_le_bytes());
                packet.extend_from_slice(&upload.crc.to_le_bytes());
            }
            Step::Chunk(offset) => {
                packet.push(3);
                packet.extend_from_slice(&(offset as u32).to_le_bytes());
                packet.extend_from_slice(
                    &upload.data[offset..(offset + upload.chunk_size).min(upload.data.len())],
                );
            }
            Step::Finish => packet.push(4),
            Step::Cancel => packet.push(6),
        }
        Some((sequence, packet))
    });
    if let Some((sequence, packet)) = request
        && let Err(error) = modules::send(29, packet).await
        && with(|link| link.sequence == sequence)
    {
        finish(&error);
    }
}

pub async fn received(result: u32, data: &[u8]) {
    let epoch = state::snapshot().connection_epoch;
    let outcome = with(|link| {
        let upload = link.upload.as_mut()?;
        if epoch != upload.epoch {
            return Some(Err("连接已改变，请重新导入".into()));
        }
        if let Err(error) = check_reply(result, data) {
            return Some(Err(error));
        }
        let value = |at| u32::from_le_bytes(data[at..at + 4].try_into().unwrap());
        if value(5) != link.sequence {
            return Some(Err("模型导入应答序号不匹配，请重试".into()));
        }
        let error = value(9) as i32;
        if error != 0 {
            return Some(Err(match error {
                -16 => "请退出手环上的 HBL2D 预览再导入".into(),
                -22 => "模型格式、大小或校验不符合手环要求".into(),
                -12 => "手环内存不足，无法验证模型".into(),
                _ => format!("手环无法保存模型（{error}）"),
            }));
        }
        let offset = value(13) as usize;
        let next = match upload.step {
            Step::Begin if offset == 0 => Step::Chunk(0),
            Step::Chunk(start) if offset == (start + upload.chunk_size).min(upload.data.len()) => {
                link.view.sent = offset;
                if offset == upload.data.len() {
                    Step::Finish
                } else {
                    Step::Chunk(offset)
                }
            }
            Step::Finish if offset == upload.data.len() => {
                return Some(Ok(Some("导入完成，请在手环上打开 HBL2D")));
            }
            Step::Cancel => return Some(Ok(Some("已取消导入"))),
            _ => return Some(Err("模型导入进度不一致，请重试".into())),
        };
        upload.step = if upload.cancel { Step::Cancel } else { next };
        Some(Ok(None))
    });
    match outcome {
        Some(Err(error)) => finish(&error),
        Some(Ok(Some(message))) => finish(message),
        Some(Ok(None)) => {
            if with(|link| {
                if link.rendered.is_some_and(|t| t.elapsed().as_millis() < 125) {
                    return false;
                }
                link.rendered = Some(Instant::now());
                true
            }) {
                ui::rerender();
            }
            send().await;
        }
        None => {}
    }
}

fn check_reply(result: u32, data: &[u8]) -> Result<(), String> {
    if result == 5 && data.len() == 17 && data[0] == 0 && &data[1..5] == b"HMO1" {
        return Ok(());
    }
    Err(match data {
        [1] => "手环上找不到 HBL2D，请确认已安装并启用新版模块".into(),
        [2] => "HBL2D 的导入服务尚未启动，请重启手环并运行 Chem".into(),
        [4] => "Chem 拒绝了模型导入请求（请求格式无效）".into(),
        [5] => "Chem 已转发请求，但 HBL2D 没有返回应答".into(),
        _ => format!("模型导入应答无效（结果 {result}，状态 {:?}）", data.first()),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exchange_reports_the_stage_that_rejected_the_request() {
        assert!(check_reply(1, &[4]).unwrap_err().contains("请求格式无效"));
        assert!(check_reply(7, &[2]).unwrap_err().contains("尚未启动"));
        assert!(check_reply(6, &[5]).unwrap_err().contains("已转发"));
        let mut reply = [0; 17];
        reply[1..5].copy_from_slice(b"HMO1");
        assert!(check_reply(5, &reply).is_ok());
        assert!(check_reply(5, &reply[..16]).is_err());
    }
}

//! Stop-and-wait native delivery. Retries use the manager's cached request ID.
use crate::{modules, package::Package, state, ui};
use std::{
    sync::{Mutex, OnceLock},
    time::Instant,
};
#[derive(Clone, Copy)]
enum Step {
    Begin,
    Elf(usize),
    Finish,
    ResourceBegin(usize),
    ResourceWrite(usize, usize),
    ResourceFinish(usize),
    Install,
    Abort,
}
struct Upload {
    package: Package,
    step: Step,
    cancel: bool,
    update: bool,
    chunk_size: usize,
    rendered: Instant,
}
static UPLOAD: OnceLock<Mutex<Option<Upload>>> = OnceLock::new();
fn with<R>(f: impl FnOnce(&mut Option<Upload>) -> R) -> R {
    f(&mut UPLOAD
        .get_or_init(|| Mutex::new(None))
        .lock()
        .unwrap_or_else(|e| e.into_inner()))
}
pub fn busy() -> bool {
    with(|u| u.is_some())
}
pub fn fail(message: String) {
    with(|u| *u = None);
    state::with(|s| {
        s.installing = false;
        let id = s.install_id.clone();
        s.module_message(&id, "", &message);
        s.manager_busy = false;
    });
    ui::rerender();
}
pub async fn start(path: &str, expected: Option<(&str, u32)>) -> Result<(), String> {
    let s = state::snapshot();
    if busy() || s.active || s.manager_busy || s.connection.busy() || !s.manager_connected() {
        return Err("请等待连接及当前操作完成".into());
    }
    if !s.native_delivery || s.target.is_empty() {
        return Err("请先更新并运行最新 Chem 安装器".into());
    }
    let package = Package::open(path, &s.target, &s.firmware_sha256)?;
    let m = &package.manifest;
    if expected.is_some_and(|(id, version)| m.id != id || m.version != version) {
        return Err("云端模块与清单不一致".into());
    }
    let current = s.modules.iter().find(|old| old.id == m.id);
    if let Some(old) = current {
        if old.intent == 2 || old.flags & 4 != 0 {
            return Err("此模块已有待生效的更改，请先重新加载框架".into());
        }
        if m.version <= old.version {
            return Err("已安装相同或更新版本".into());
        }
    }
    let update = current.is_some();
    state::with(|s| {
        s.installing = true;
        s.install_id = m.id.clone();
        s.install_sent = 0;
        s.install_total = package.elf.len() as u64
            + package
                .resources
                .iter()
                .map(|r| r.len() as u64)
                .sum::<u64>();
        s.module_message(
            &m.id,
            &m.name,
            if update {
                "正在更新…"
            } else {
                "正在安装…"
            },
        );
    });
    with(|u| {
        *u = Some(Upload {
            package,
            step: Step::Begin,
            cancel: false,
            update,
            chunk_size: if s.bulk_delivery {
                modules::BULK_CHUNK
            } else {
                508
            },
            rendered: Instant::now(),
        })
    });
    ui::rerender();
    send_next().await
}
fn step_op(step: Step) -> u32 {
    match step {
        Step::Begin => 15,
        Step::Elf(_) => 16,
        Step::Finish => 17,
        Step::ResourceBegin(_) => 26,
        Step::ResourceWrite(_, _) => 27,
        Step::ResourceFinish(_) => 28,
        Step::Install => 2,
        Step::Abort => 18,
    }
}
fn packet(u: &Upload) -> (u32, Vec<u8>) {
    fn chunk(bytes: &[u8], offset: usize, size: usize) -> Vec<u8> {
        let mut b = (offset as u32).to_le_bytes().to_vec();
        b.extend_from_slice(&bytes[offset..bytes.len().min(offset + size)]);
        b
    }
    match u.step {
        Step::Begin => (15, u.package.receipt.clone()),
        Step::Elf(offset) => (16, chunk(&u.package.elf, offset, u.chunk_size)),
        Step::Finish => (17, vec![]),
        Step::ResourceBegin(i) => {
            let r = &u.package.manifest.resources[i];
            let mut b = vec![r.name.len() as u8];
            b.extend_from_slice(r.name.as_bytes());
            b.extend_from_slice(&(r.size as u32).to_le_bytes());
            // Hash the exact validated resource bytes, not an untrusted string.
            use sha2::{Digest, Sha256};
            b.extend_from_slice(&Sha256::digest(&u.package.resources[i]));
            (26, b)
        }
        Step::ResourceWrite(i, offset) => {
            (27, chunk(&u.package.resources[i], offset, u.chunk_size))
        }
        Step::ResourceFinish(_) => (28, vec![]),
        Step::Install => {
            let mut b = u.package.manifest.id.as_bytes().to_vec();
            b.push(0);
            (2, b)
        }
        Step::Abort => (18, vec![]),
    }
}
async fn send_next() -> Result<(), String> {
    let next = with(|slot| slot.as_ref().map(packet));
    if let Some((op, data)) = next {
        if let Err(e) = modules::send(op, data).await {
            fail(e.clone());
            return Err(e);
        }
    }
    Ok(())
}
pub async fn cancel() -> Result<(), String> {
    if with(|u| {
        if let Some(u) = u {
            u.cancel = true;
            true
        } else {
            false
        }
    }) {
        state::with(|s| {
            let id = s.install_id.clone();
            s.module_message(&id, "", "正在取消安装…");
        });
        Ok(())
    } else {
        Err("当前没有模块导入".into())
    }
}
pub async fn received(op: u32, result: u32, data: &[u8]) {
    if !busy() {
        return;
    }
    if result != 5 && result != 8 {
        let error = data
            .get(if matches!(op,15..=18|26..=28) {
                4..8
            } else {
                0..4
            })
            .map(|b| i32::from_le_bytes(b.try_into().unwrap()))
            .unwrap_or(0);
        fail(format!(
            "模块导入失败（状态 {result}，错误 {error}），刷新后可重试"
        ));
        return;
    }
    let outcome = with(|slot| -> Result<Option<String>, String> {
        let u = slot.as_mut().ok_or("模块导入已结束")?;
        if step_op(u.step) != op {
            return Err("模块上传响应顺序不匹配".into());
        }
        if let Step::Abort = u.step {
            return Ok(Some("已取消模块导入".into()));
        }
        if let Step::Install = u.step {
            return Ok(Some(
                if u.update {
                    "更新已保存，重启手环后生效；原版本保留用于回退"
                } else {
                    "模块已导入，可在列表中启用；重启手环并运行安装器后生效"
                }
                .into(),
            ));
        }
        if matches!(u.step, Step::Elf(_) | Step::ResourceWrite(_, _)) {
            let sent = match u.step {
                Step::Elf(offset) => u.chunk_size.min(u.package.elf.len() - offset),
                Step::ResourceWrite(i, offset) => {
                    u.chunk_size.min(u.package.resources[i].len() - offset)
                }
                _ => unreachable!(),
            };
            let wanted = match u.step {
                Step::Elf(offset) | Step::ResourceWrite(_, offset) => offset + sent,
                _ => 0,
            };
            if data.len() != 8
                || u32::from_le_bytes(data[..4].try_into().unwrap()) as usize != wanted
            {
                return Err("手环写入进度不一致，已停止上传".into());
            }
            state::with(|s| s.install_sent += sent as u64);
        }
        u.step = if u.cancel {
            Step::Abort
        } else {
            match u.step {
                Step::Begin => Step::Elf(0),
                Step::Elf(offset) => {
                    if offset + u.chunk_size < u.package.elf.len() {
                        Step::Elf(offset + u.chunk_size)
                    } else {
                        Step::Finish
                    }
                }
                Step::Finish => {
                    if u.package.resources.is_empty() {
                        Step::Install
                    } else {
                        Step::ResourceBegin(0)
                    }
                }
                Step::ResourceBegin(i) => Step::ResourceWrite(i, 0),
                Step::ResourceWrite(i, offset) => {
                    if offset + u.chunk_size < u.package.resources[i].len() {
                        Step::ResourceWrite(i, offset + u.chunk_size)
                    } else {
                        Step::ResourceFinish(i)
                    }
                }
                Step::ResourceFinish(i) => {
                    if i + 1 < u.package.resources.len() {
                        Step::ResourceBegin(i + 1)
                    } else {
                        Step::Install
                    }
                }
                _ => unreachable!(),
            }
        };
        Ok(None)
    });
    match outcome {
        Err(e) => fail(e),
        Ok(Some(message)) => {
            with(|u| *u = None);
            state::with(|s| {
                s.installing = false;
                let id = s.install_id.clone();
                s.module_message(&id, "", &message);
            });
            if let Err(e) = modules::refresh().await {
                fail(e);
            }
        }
        Ok(None) => {
            let _ = send_next().await;
        }
    }
    let render = with(|slot| match slot {
        Some(u) if u.rendered.elapsed().as_millis() < 400 => false,
        Some(u) => {
            u.rendered = Instant::now();
            true
        }
        None => true,
    });
    if render {
        ui::rerender();
    }
}

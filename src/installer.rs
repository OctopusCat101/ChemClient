//! Chem watchface installation through the AstroBox 2.2.6 queue.
use crate::{
    astrobox::psys_host_v4::{queue, watchface},
    cloud, cloud_download, connection, state, ui,
};
use serde::Deserialize;

#[derive(Clone)]
pub struct Release {
    pub face_id: String,
    pub artifact: cloud::Artifact,
}

#[derive(Clone, Default)]
pub struct View {
    pub release: Option<Release>,
    step: Step,
    pub message: String,
    pub progress: Option<u64>,
}

#[derive(Clone, Default)]
enum Step {
    #[default]
    Idle,
    Checking,
    Downloading,
    Submitting(Vec<TaskEvent>),
    Queued {
        addr: String,
        face_id: String,
        task: String,
    },
    Uploaded {
        addr: String,
        face_id: String,
    },
    Opening,
    Run,
}

impl View {
    pub fn waiting_for_run(&self) -> bool {
        matches!(self.step, Step::Run)
    }

    pub fn busy(&self) -> bool {
        !matches!(self.step, Step::Idle | Step::Run)
    }

    pub fn owns_transport(&self) -> bool {
        matches!(
            self.step,
            Step::Checking | Step::Submitting(_) | Step::Queued { .. } | Step::Opening
        )
    }

    pub fn can_refresh(&self) -> bool {
        !matches!(
            self.step,
            Step::Checking | Step::Downloading | Step::Submitting(_) | Step::Opening
        )
    }
}

pub fn offered(s: &state::State) -> bool {
    s.device_connected()
        && s.installer.release.is_some()
        && (s.needs_update()
            || (!s.manager_ready
                && matches!(
                    s.connection,
                    connection::Phase::Failed(_) | connection::Phase::WaitingForService(_)
                )))
}

pub fn reset() {
    state::with(|s| {
        s.installer.release = None;
        let keep = match &s.installer.step {
            Step::Queued { addr, .. } | Step::Uploaded { addr, .. } => *addr == s.addr,
            _ => false,
        };
        if !keep {
            s.installer = View::default();
        }
    });
}

pub fn connected() {
    state::with(|s| {
        if s.installer.waiting_for_run() && s.resident_revision >= s.expected_revision {
            s.installer.step = Step::Idle;
            s.installer.message.clear();
        }
    });
}

fn same_session(s: &state::State, addr: &str, epoch: u64) -> bool {
    s.connection_epoch == epoch && s.addr == addr && s.device_connected()
}

fn fail(addr: &str, epoch: u64, error: String) {
    state::with(|s| {
        if same_session(s, addr, epoch) {
            s.installer.step = Step::Idle;
            s.installer.message = error;
            s.installer.progress = None;
        }
    });
    ui::rerender();
}

pub fn download_canceled(addr: &str, epoch: u64) {
    fail(addr, epoch, "已取消下载 Chem 安装器".into());
}

pub async fn start() -> Result<(), String> {
    let s = state::snapshot();
    if !offered(&s)
        || s.installer.busy()
        || s.discovering
        || s.downloading_artifact()
        || s.active
        || s.installing
        || crate::transfer::busy()
        || crate::magic::busy()
    {
        return Err("请等待当前操作完成并刷新手环".into());
    }
    let Some(release) = s.installer.release else {
        return Err("云端暂无适配的 Chem 安装器".into());
    };
    let (addr, epoch) = (s.addr, s.connection_epoch);
    state::with(|s| {
        s.installer.step = Step::Checking;
        s.installer.message = "正在检查手环上的安装器…".into();
        s.installer.progress = None;
    });
    ui::rerender();
    let faces = connection::bounded(30_000, watchface::get_watchface_list(addr.clone())).await;
    if !same_session(&state::snapshot(), &addr, epoch) {
        return Ok(());
    }
    match faces.and_then(|r| r) {
        Ok(faces) => {
            if let Some(face) = faces.iter().find(|f| f.id == release.face_id) {
                open(&addr, epoch, &release.face_id, face.is_current).await;
            } else {
                state::with(|s| {
                    s.installer.step = Step::Downloading;
                    s.installer.message = "正在下载 Chem 安装器…".into();
                });
                if let Err(e) = cloud_download::installer(&addr, &release.artifact) {
                    fail(&addr, epoch, e);
                }
            }
        }
        Err(e) => fail(&addr, epoch, format!("无法读取手环表盘，请重试。{e}")),
    }
    Ok(())
}

pub async fn downloaded(addr: &str, epoch: u64, result: Result<String, String>) {
    let s = state::snapshot();
    if !same_session(&s, addr, epoch) || !matches!(s.installer.step, Step::Downloading) {
        if let Ok(path) = result {
            let _ = std::fs::remove_file(path);
        }
        return;
    }
    let path = match result {
        Ok(path) => path,
        Err(e) => {
            fail(addr, epoch, e);
            return;
        }
    };
    let Some(release) = s.installer.release else {
        let _ = std::fs::remove_file(path);
        fail(addr, epoch, "安装器清单已变化，请刷新后重试".into());
        return;
    };
    state::with(|s| {
        s.installer.step = Step::Submitting(Vec::new());
        s.installer.message = "正在提交到 AstroBox…".into();
    });
    ui::rerender();
    let task = queue::enqueue_install(queue::InstallTask {
        name: "Chem 安装器".into(),
        description: None,
        icon: None,
        device_addr: addr.to_owned(),
        res_type: queue::ResourceType::Watchface,
        source: queue::InstallSource::File(path.clone()),
        watchface_id: None,
    })
    .await;
    match task {
        Ok(task) => {
            state::with(|s| {
                if !same_session(s, addr, epoch) {
                    return;
                }
                let Step::Submitting(events) = std::mem::take(&mut s.installer.step) else {
                    return;
                };
                s.installer.message =
                    "等待 AstroBox 上传。若未自动开始，请在安装队列启动任务。".into();
                s.installer.step = Step::Queued {
                    addr: addr.to_owned(),
                    face_id: release.face_id,
                    task: task.clone(),
                };
                for event in events.into_iter().filter(|e| e.task_id == task) {
                    apply_event(&mut s.installer, event);
                }
            });
            ui::rerender();
        }
        Err(e) => {
            let _ = std::fs::remove_file(path);
            fail(addr, epoch, format!("AstroBox 未能加入安装队列。{e}"));
        }
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct HostMessage {
    event_name: String,
    payload: String,
    host: bool,
}

#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct TaskEvent {
    task_id: String,
    status: String,
    progress: Option<f64>,
    error_detail: Option<String>,
}

fn parse_event(payload: &str) -> Option<TaskEvent> {
    if payload.len() > 8192 {
        return None;
    }
    let message = serde_json::from_str::<HostMessage>(payload).ok()?;
    if !message.host || message.event_name != "queue.task" {
        return None;
    }
    serde_json::from_str(&message.payload).ok()
}

fn apply_event(view: &mut View, event: TaskEvent) -> bool {
    if let Step::Submitting(events) = &mut view.step {
        // A queue notification can arrive before enqueue-install returns its ID.
        if events.len() == 8 {
            events.remove(0);
        }
        events.push(event);
        return false;
    }
    let Step::Queued {
        addr,
        face_id,
        task,
    } = &view.step
    else {
        return false;
    };
    if *task != event.task_id {
        return false;
    }
    let (message, progress) = match event.status.as_str() {
        "pending" => (
            "等待 AstroBox 上传。若未自动开始，请在安装队列启动任务。".into(),
            None,
        ),
        "waiting_device" => ("AstroBox 正在等待手环重新连接…".into(), None),
        "running" => {
            let progress = event.progress.map(|p| (p.clamp(0.0, 1.0) * 100.0) as u64);
            ("正在上传 Chem 安装器…".into(), progress)
        }
        "succeeded" => {
            view.step = Step::Uploaded {
                addr: addr.clone(),
                face_id: face_id.clone(),
            };
            ("安装器已上传，正在等待切换表盘…".into(), None)
        }
        "failed" => {
            view.step = Step::Idle;
            (
                format!(
                    "安装器上传失败。{}",
                    event.error_detail.as_deref().unwrap_or("请重试")
                ),
                None,
            )
        }
        "canceled" => {
            view.step = Step::Idle;
            ("安装器上传已取消".into(), None)
        }
        _ => return false,
    };
    view.message = message;
    view.progress = progress;
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_queue_event(task_id: &str, status: &str, host: bool) -> String {
        serde_json::json!({
            "host": host,
            "eventName": "queue.task",
            "payload": serde_json::json!({"taskId": task_id, "status": status, "progress": 0.42}).to_string(),
        }).to_string()
    }

    #[test]
    fn only_the_hosts_matching_upload_can_open_the_installer() {
        let mut view = View {
            step: Step::Queued {
                addr: "A".into(),
                face_id: "913211314".into(),
                task: "mine".into(),
            },
            ..View::default()
        };
        assert!(parse_event(&sample_queue_event("mine", "succeeded", false)).is_none());
        assert!(!apply_event(
            &mut view,
            parse_event(&sample_queue_event("other", "succeeded", true)).unwrap()
        ));
        assert!(apply_event(
            &mut view,
            parse_event(&sample_queue_event("mine", "running", true)).unwrap()
        ));
        assert_eq!(view.progress, Some(42));
        assert!(matches!(view.step, Step::Queued { .. }));
        assert!(apply_event(
            &mut view,
            parse_event(&sample_queue_event("mine", "succeeded", true)).unwrap()
        ));
        assert!(
            matches!(view.step, Step::Uploaded { addr, face_id } if addr == "A" && face_id == "913211314")
        );
    }
}

pub fn received(payload: &str) {
    if let Some(event) = parse_event(payload) {
        if state::with(|s| apply_event(&mut s.installer, event)) {
            ui::rerender();
        }
    }
}

pub async fn tick() {
    let s = state::snapshot();
    if s.discovering || !s.device_connected() {
        return;
    }
    let Step::Uploaded { addr, face_id } = s.installer.step else {
        return;
    };
    if s.addr != addr {
        return;
    }
    let Some(release) = s.installer.release else {
        if !s.target.is_empty() || !s.cloud_notice.is_empty() {
            fail(
                &addr,
                s.connection_epoch,
                "安装器已上传，请刷新清单或在手环手动打开".into(),
            );
        }
        return;
    };
    if release.face_id != face_id {
        fail(
            &addr,
            s.connection_epoch,
            "云端已有新版安装器，请点击安装更新".into(),
        );
        return;
    }
    open(&addr, s.connection_epoch, &face_id, false).await;
}

async fn open(addr: &str, epoch: u64, face_id: &str, already_current: bool) {
    state::with(|s| {
        s.installer.step = Step::Opening;
        s.installer.message = "正在打开 Chem 安装器…".into();
    });
    ui::rerender();
    if !already_current {
        let result = connection::bounded(
            30_000,
            watchface::set_current_watchface(addr.to_owned(), face_id.to_owned()),
        )
        .await;
        if let Err(e) = result.and_then(|r| r) {
            fail(addr, epoch, format!("切换 Chem 安装器失败。{e}"));
            return;
        }
    }
    if same_session(&state::snapshot(), addr, epoch) {
        state::with(|s| {
            s.installer.step = Step::Run;
            s.installer.message = if s.needs_update() {
                "请重启手环，再打开这个安装器并点击「运行」。"
            } else {
                "请在手环点击「运行」，正在等待 Chem 连接…"
            }
            .into();
        });
        connection::opened();
        ui::rerender();
    }
}

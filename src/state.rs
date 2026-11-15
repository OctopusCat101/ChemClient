use serde_json::Value;
use std::sync::{
    Mutex, OnceLock,
    atomic::{AtomicBool, Ordering},
};

static ACTION_BUSY: AtomicBool = AtomicBool::new(false);
pub struct ActionGuard;
pub fn try_action() -> Option<ActionGuard> {
    ACTION_BUSY
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
        .ok()
        .map(|_| ActionGuard)
}
pub fn action_busy() -> bool {
    ACTION_BUSY.load(Ordering::Acquire)
}
impl Drop for ActionGuard {
    fn drop(&mut self) {
        with(|s| {
            s.pending_action.clear();
            if let Some(job) = s.song_job.as_mut().filter(|j| j.network) {
                job.network = false;
                job.message = "下载已中断，可重新导入".into();
            }
        });
        ACTION_BUSY.store(false, Ordering::Release);
    }
}

#[derive(Clone, Debug, Default)]
pub struct CloudSong {
    pub id: u64,
    pub name: String,
    pub artists: Vec<String>,
    pub album: String,
    pub album_id: u64,
    pub duration_ms: u32,
    pub cover_url: String,
}
#[derive(Clone, Debug, Default)]
pub struct CloudPlaylist {
    pub id: u64,
    pub name: String,
    pub creator: String,
    pub track_count: u32,
}
#[derive(Clone, Default)]
pub struct ModuleJob {
    pub id: String,
    pub name: String,
    pub message: String,
}
#[derive(Clone, Default)]
pub struct SongJob {
    pub id: u64,
    pub name: String,
    pub received: u64,
    pub total: Option<u64>,
    pub speed: u64,
    pub elapsed: u64,
    pub network: bool,
    pub cancel_requested: bool,
    pub message: String,
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum NoticeOwner {
    #[default]
    Modules,
    Music,
    Local,
    Cloud,
    Library,
    Magic,
}
impl NoticeOwner {
    pub fn visible_in(self, tab: &str) -> bool {
        match self {
            Self::Modules => tab == "modules",
            Self::Music => matches!(tab, "local" | "cloud" | "library"),
            Self::Local => tab == "local",
            Self::Cloud => tab == "cloud",
            Self::Library => tab == "library",
            Self::Magic => tab == "magic",
        }
    }
}
#[derive(Clone, Default)]
pub struct State {
    pub root: Option<String>,
    pub addr: String,
    pub connection_epoch: u64,
    pub device_name: String,
    pub devices: Vec<(String, String)>,
    pub connected_devices: Vec<String>,
    pub discovering: bool,
    pub recheck_connection: bool,
    pub connection: crate::connection::Phase,
    pub direct: bool,
    pub tab: String,
    pub modules: Vec<crate::modules::Module>,
    pub module_detail: String,
    pub upload_options: bool,
    pub delete_module: String,
    pub manager_ready: bool,
    pub manager_busy: bool,
    pub safe_mode: bool,
    pub safe_reason: u32,
    pub boot_status: u32,
    pub safe_exit_pending: bool,
    pub target: String,
    pub firmware_sha256: String,
    pub native_delivery: bool,
    pub bulk_delivery: bool,
    pub expected_revision: u32,
    pub resident_revision: u32,
    pub resident_target: String,
    pub resident_capabilities: u32,
    pub cloud_modules: Vec<crate::cloud::Module>,
    pub cloud_notice: String,
    pub download: Option<crate::cloud_download::Progress>,
    pub installing: bool,
    pub install_sent: u64,
    pub install_total: u64,
    pub install_id: String,
    pub module_jobs: Vec<ModuleJob>,
    pub status: String,
    pub status_owner: NoticeOwner,
    pub pending_action: String,
    pub query: String,
    pub results: Vec<CloudSong>,
    pub song_job: Option<SongJob>,
    pub playlists: Vec<CloudPlaylist>,
    pub cookie: String,
    pub qr_key: String,
    pub qr_url: String,
    pub local_path: String,
    pub local_name: String,
    pub cover_path: String,
    pub lyric_path: String,
    pub attachments_supported: bool,
    pub cover_side: u32,
    pub library: Vec<Value>,
    pub active: bool,
    pub sent: u64,
    pub total: u64,
    pub speed: u64,
    pub bitrate: u32,
    pub predecode: bool,
    pub sbc_supported: bool,
    pub transcoding: bool,
}
impl State {
    pub fn notice(&mut self, owner: NoticeOwner, text: impl Into<String>) {
        self.status = text.into();
        self.status_owner = owner;
    }
    pub fn device_connected(&self) -> bool {
        !self.addr.is_empty() && self.connected_devices.contains(&self.addr)
    }
    pub fn manager_connected(&self) -> bool {
        self.device_connected()
            && !self.discovering
            && self.manager_ready
            && self.connection == crate::connection::Phase::Ready
    }
    pub fn music_connected(&self) -> bool {
        self.manager_connected() && self.direct && self.modules.iter().any(|m| m.importable())
    }
    pub fn needs_update(&self) -> bool {
        self.resident_revision != 0 && self.resident_revision < self.expected_revision
    }
    pub fn downloading_artifact(&self) -> bool {
        self.download
            .as_ref()
            .is_some_and(|p| !matches!(&p.owner, crate::cloud_download::Owner::Catalog))
    }
    pub fn module_download(&self, id: &str) -> Option<&crate::cloud_download::Progress> {
        self.download.as_ref().filter(
            |p| matches!(&p.owner, crate::cloud_download::Owner::Module(owner) if owner == id),
        )
    }
    pub fn module_message(&mut self, id: &str, name: &str, message: &str) {
        if let Some(job) = self.module_jobs.iter_mut().find(|j| j.id == id) {
            job.message = message.into();
            if !name.is_empty() {
                job.name = name.into();
            }
        } else {
            self.module_jobs.push(ModuleJob {
                id: id.into(),
                name: name.into(),
                message: message.into(),
            });
        }
    }
}
static STATE: OnceLock<Mutex<State>> = OnceLock::new();
pub fn with<R>(f: impl FnOnce(&mut State) -> R) -> R {
    f(&mut STATE
        .get_or_init(|| {
            Mutex::new(State {
                tab: "modules".into(),
                bitrate: 24000,
                ..State::default()
            })
        })
        .lock()
        .unwrap_or_else(|e| e.into_inner()))
}
pub fn snapshot() -> State {
    with(|s| s.clone())
}
pub fn error(e: String) {
    with(|s| {
        if let Some(job) = &mut s.song_job {
            job.message = e.clone();
            job.network = false;
        }
        s.notice(NoticeOwner::Music, e);
        s.active = false;
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cloud_download::{Owner, Progress};
    #[test]
    fn progress_and_errors_stay_with_their_module() {
        let mut s = State::default();
        s.module_message("one", "First", "正在下载…");
        s.module_message("two", "Second", "已安装");
        s.module_message("one", "", "下载超时");
        assert_eq!(s.module_jobs.len(), 2);
        assert_eq!(s.module_jobs[0].name, "First");
        assert_eq!(s.module_jobs[0].message, "下载超时");
        assert_eq!(s.module_jobs[1].message, "已安装");
        s.download = Some(Progress {
            owner: Owner::Module("one".into()),
            received: 30,
            total: Some(100),
            elapsed: 1,
            connecting: false,
        });
        assert_eq!(s.module_download("one").unwrap().percent(), Some(30));
        assert!(s.module_download("two").is_none());
        for owner in [Owner::Catalog] {
            s.download.as_mut().unwrap().owner = owner;
            assert!(s.module_download("one").is_none());
            assert!(s.module_download("loader").is_none());
            assert!(s.module_download("catalog").is_none());
        }
    }
}

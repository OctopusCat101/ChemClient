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
pub struct State {
    pub root: Option<String>,
    pub addr: String,
    pub device_name: String,
    pub devices: Vec<(String, String)>,
    pub connected_devices: Vec<String>,
    pub discovering: bool,
    pub discovered: bool,
    pub direct: bool,
    pub tab: String,
    pub status: String,
    pub query: String,
    pub results: Vec<CloudSong>,
    pub playlists: Vec<CloudPlaylist>,
    pub cookie: String,
    pub qr_key: String,
    pub qr_url: String,
    pub local_path: String,
    pub local_name: String,
    pub library: Vec<Value>,
    pub active: bool,
    pub sent: u64,
    pub total: u64,
    pub speed: u64,
    pub bitrate: u32,
    pub transcoding: bool,
}
static STATE: OnceLock<Mutex<State>> = OnceLock::new();
pub fn with<R>(f: impl FnOnce(&mut State) -> R) -> R {
    f(&mut STATE
        .get_or_init(|| {
            Mutex::new(State {
                tab: "device".into(),
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
        s.status = e;
        s.active = false;
    });
}

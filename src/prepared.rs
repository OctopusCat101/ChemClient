//! Keep completed local work when the Bluetooth session changes.
use crate::{state, transcode, transfer, ui};
use serde_json::Value;
use std::sync::{Mutex, OnceLock};

#[derive(Clone, Copy)]
pub enum Stage {
    Convert(u32, bool),
    Upload,
}
struct Job {
    path: String,
    song: Value,
    temporary: bool,
    addr: String,
    stage: Stage,
}
static JOB: OnceLock<Mutex<Option<Job>>> = OnceLock::new();
fn job() -> &'static Mutex<Option<Job>> {
    JOB.get_or_init(Default::default)
}
pub fn waiting() -> bool {
    job().lock().unwrap_or_else(|e| e.into_inner()).is_some()
}
fn dispose(j: Job) {
    crate::attachments::cleanup(&j.song);
    if j.temporary {
        let _ = std::fs::remove_file(j.path);
    }
}
pub fn cancel() -> bool {
    let old = job().lock().unwrap_or_else(|e| e.into_inner()).take();
    if let Some(j) = old {
        dispose(j);
        true
    } else {
        false
    }
}
pub fn retain(path: String, song: Value, temporary: bool, addr: String, stage: Stage) {
    let retained_path = path.clone();
    let old = job()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .replace(Job {
            path,
            song,
            temporary,
            addr,
            stage,
        });
    if let Some(j) = old.filter(|j| j.path != retained_path) {
        dispose(j);
    }
    state::with(|s| {
        s.active = false;
        s.notice(state::NoticeOwner::Music, "音频已保留，连接恢复后继续导入");
        if let Some(j) = &mut s.song_job {
            j.network = false;
            j.message = s.status.clone();
        }
    });
    ui::rerender();
}
pub async fn tick() {
    let s = state::snapshot();
    if !s.music_connected()
        || s.active
        || s.transcoding
        || s.installing
        || s.manager_busy
        || state::action_busy()
        || transfer::busy()
        || crate::magic::busy()
    {
        return;
    }
    let ready = {
        let mut slot = job().lock().unwrap_or_else(|e| e.into_inner());
        if slot.as_ref().is_some_and(|j| j.addr == s.addr) {
            slot.take()
        } else {
            None
        }
    };
    let Some(j) = ready else {
        return;
    };
    let result = match j.stage {
        Stage::Convert(quality, predecode) => {
            transcode::start_options(
                j.path.clone(),
                j.song.clone(),
                j.temporary,
                quality,
                predecode,
            )
            .await
        }
        Stage::Upload => transfer::start(j.path.clone(), j.song.clone(), j.temporary).await,
    };
    if let Err(e) = result {
        dispose(j);
        state::error(e);
        ui::rerender();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn completed_audio_survives_offline_wait_and_explicit_cancel_reclaims_it() {
        let path = std::env::temp_dir().join(format!("chem-prepared-{}.mp3", std::process::id()));
        std::fs::write(&path, b"ID3audio").unwrap();
        retain(
            path.to_string_lossy().into(),
            Value::Null,
            true,
            "Band A".into(),
            Stage::Upload,
        );
        assert!(path.exists());
        assert!(!state::snapshot().active && !transfer::busy());
        assert_eq!(job().lock().unwrap().as_ref().unwrap().addr, "Band A");
        retain(
            path.to_string_lossy().into(),
            Value::Null,
            true,
            "Band A".into(),
            Stage::Upload,
        );
        assert!(
            path.exists(),
            "retaining the same job must not delete its file"
        );
        assert!(cancel());
        assert!(!path.exists());
    }
}

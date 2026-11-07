//! Optional second conversion stage, after the user's selected MP3 compression.
use crate::{astrobox::psys_host_v4::timer, state, transfer, ui};
use noirmusic_sbc_codec::convert::Converter;
use serde_json::Value;
use std::{
    cell::RefCell,
    fs::{self, File},
    time::Instant,
};

struct Job {
    converter: Converter<File, File>,
    source: String,
    temporary: bool,
    output: String,
    song: Value,
    size: u64,
    timer: u64,
    rendered: Instant,
}
thread_local! { static JOB: RefCell<Option<Job>> = const { RefCell::new(None) }; }

pub fn active() -> bool {
    JOB.with(|j| j.borrow().is_some())
}

pub fn start(source: String, song: Value, temporary: bool) -> Result<(), String> {
    if active() || transfer::busy() || state::snapshot().active {
        return Err("请先完成当前导入".into());
    }
    let snapshot = state::snapshot();
    if !snapshot.direct || !snapshot.sbc_supported {
        return Err("请更新手环 NoirMusic 并重新连接后使用 SBC 预解码".into());
    }
    let input = File::open(&source).map_err(|e| e.to_string())?;
    let size = input.metadata().map_err(|e| e.to_string())?.len();
    if size > 128 * 1024 * 1024 {
        return Err("请选择 128 MiB 以内的音频".into());
    }
    fs::create_dir_all("media/predecoded").map_err(|e| e.to_string())?;
    let output = format!("media/predecoded/{}.nsbc", song["id"].as_u64().unwrap_or(0));
    let file = File::create(&output).map_err(|e| e.to_string())?;
    let converter = match Converter::new(input, file) {
        Ok(c) => c,
        Err(e) => {
            let _ = fs::remove_file(&output);
            return Err(e);
        }
    };
    JOB.with(|j| {
        *j.borrow_mut() = Some(Job {
            converter,
            source,
            temporary,
            output,
            song,
            size,
            timer: timer::set_interval(10, "music-compression"),
            rendered: Instant::now(),
        })
    });
    state::with(|s| {
        s.active = true;
        s.transcoding = true;
        s.sent = 0;
        s.total = size;
        s.speed = 0;
        s.status = "正在预解码为 SBC".into();
    });
    ui::rerender();
    Ok(())
}

fn dispose(j: Job, remove_output: bool) {
    if remove_output { crate::attachments::cleanup(&j.song); }
    timer::clear_timer(j.timer);
    let source = j.source.clone();
    let output = j.output.clone();
    let temporary = j.temporary;
    drop(j);
    if temporary {
        let _ = fs::remove_file(source);
    }
    if remove_output {
        let _ = fs::remove_file(output);
    }
}

pub fn cancel() -> bool {
    let Some(j) = JOB.with(|j| j.borrow_mut().take()) else {
        return false;
    };
    dispose(j, true);
    state::with(|s| {
        s.active = false;
        s.transcoding = false;
        s.status = "已取消预解码".into();
    });
    true
}

pub async fn tick() {
    let Some(mut j) = JOB.with(|j| j.borrow_mut().take()) else {
        return;
    };
    let started = Instant::now();
    let mut result = Ok(false);
    for _ in 0..4 {
        result = j.converter.step();
        if result != Ok(false) || started.elapsed().as_millis() >= 20 {
            break;
        }
    }
    if result == Ok(false) {
        let render = j.rendered.elapsed().as_millis() >= 400;
        if render {
            state::with(|s| {
                s.sent = j.converter.bytes_read();
                s.total = j.size;
            });
            j.rendered = Instant::now();
        }
        JOB.with(|slot| *slot.borrow_mut() = Some(j));
        if render {
            ui::rerender();
        }
        return;
    }
    let path = j.output.clone();
    let mut song = j.song.clone();
    song["duration_ms"] = Value::from(j.converter.header().duration_ms());
    dispose(j, result.is_err());
    state::with(|s| {
        s.active = false;
        s.transcoding = false;
    });
    match result {
        Ok(_) => {
            if let Err(e) = transfer::start(path.clone(), song, true).await {
                let _ = fs::remove_file(path);
                state::error(e);
            }
        }
        Err(e) => state::error(e),
    }
    ui::rerender();
}

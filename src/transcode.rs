//! Incremental conversion inside WASI. Audio stays on disk, never whole-file PCM.
use crate::{astrobox::psys_host_v4::timer, state, transfer, ui};
use nanomp3::{Decoder, MAX_SAMPLES_PER_FRAME};
use serde_json::Value;
use shine_rs::{Mp3Encoder, Mp3EncoderConfig, StereoMode};
use std::{
    cell::RefCell,
    fs::{self, File},
    io::{Read, Seek, SeekFrom, Write},
    time::Instant,
};

pub const EXTREME: u32 = 24_000;
pub const BALANCED: u32 = 64_000;
pub fn profile(value: u32) -> u32 {
    match value {
        0 | BALANCED => value,
        _ => EXTREME,
    }
}
struct Job {
    source: File,
    output: File,
    source_path: String,
    output_path: String,
    source_temporary: bool,
    song: Value,
    decoder: Box<Decoder>,
    encoder: Mp3Encoder,
    input: Vec<u8>,
    start: usize,
    end: usize,
    eof: bool,
    pcm: Box<[i16; MAX_SAMPLES_PER_FRAME]>,
    converted: Vec<i16>,
    channels: usize,
    source_rate: u32,
    phase: u32,
    sum: [i64; 2],
    count: u32,
    total_frames: u64,
    bytes: u64,
    size: u64,
    timer: u64,
    last_render: Instant,
}
// The WASI event driver owns codec state on its thread. Neither decoder nor
// encoder is Send; do not assert otherwise or hold a lock across host calls.
thread_local! { static JOB: RefCell<Option<Job>> = const { RefCell::new(None) }; }

pub async fn start(path: String, song: Value, temporary: bool, quality: u32) -> Result<(), String> {
    let quality = profile(quality);
    if quality == 0 {
        return transfer::start(path, song, temporary).await;
    }
    if !state::snapshot().direct {
        return Err("请先连接 NoirMusic".into());
    }
    if JOB.with(|j| j.borrow().is_some()) || transfer::busy() || state::snapshot().active {
        return Err("请先完成当前导入".into());
    }
    let mut source = File::open(&path).map_err(|e| format!("打开音频失败：{e}"))?;
    let size = source.metadata().map_err(|e| e.to_string())?.len();
    if size > 128 * 1024 * 1024 {
        return Err("请选择 128 MiB 以内的音频".into());
    }
    let mut header = [0u8; 10];
    let n = source.read(&mut header).map_err(|e| e.to_string())?;
    let offset = if n == 10 && &header[..3] == b"ID3" && header[6..10].iter().all(|b| b & 128 == 0)
    {
        10 + header[6..10]
            .iter()
            .fold(0u64, |v, b| (v << 7) | u64::from(*b))
            + if header[5] & 16 != 0 { 10 } else { 0 }
    } else {
        0
    };
    if offset >= size {
        return Err("音频标签不完整".into());
    }
    source
        .seek(SeekFrom::Start(offset))
        .map_err(|e| e.to_string())?;
    fs::create_dir_all("media/compressed").map_err(|e| e.to_string())?;
    let output_path = format!("media/compressed/{}.mp3", song["id"].as_u64().unwrap_or(0));
    if path == output_path {
        return Err("压缩目标与原文件冲突".into());
    }
    let channels = if quality == EXTREME { 1 } else { 2 };
    let encoder = Mp3Encoder::new(
        Mp3EncoderConfig::new()
            .sample_rate(24000)
            .bitrate(quality / 1000)
            .channels(channels as u8)
            .stereo_mode(if channels == 1 {
                StereoMode::Mono
            } else {
                StereoMode::Stereo
            }),
    )
    .map_err(|e| format!("编码器初始化失败：{e}"))?;
    let output = File::create(&output_path).map_err(|e| e.to_string())?;
    let timer = timer::set_interval(10, "music-compression");
    JOB.with(|j| {
        *j.borrow_mut() = Some(Job {
            source,
            output,
            source_path: path,
            output_path,
            source_temporary: temporary,
            song,
            decoder: Box::new(Decoder::new()),
            encoder,
            input: vec![0; 32768],
            start: 0,
            end: 0,
            eof: false,
            pcm: Box::new([0; MAX_SAMPLES_PER_FRAME]),
            converted: Vec::with_capacity(4608),
            channels,
            source_rate: 0,
            phase: 0,
            sum: [0; 2],
            count: 0,
            total_frames: 0,
            bytes: offset,
            size,
            timer,
            last_render: Instant::now(),
        })
    });
    state::with(|s| {
        s.transcoding = true;
        s.active = true;
        s.sent = offset;
        s.total = size;
        s.speed = 0;
        s.status = "正在压缩音频".into();
    });
    ui::rerender();
    Ok(())
}
impl Job {
    fn step(&mut self) -> Result<bool, String> {
        if !self.eof && self.end - self.start < 8192 {
            self.input.copy_within(self.start..self.end, 0);
            self.end -= self.start;
            self.start = 0;
            let n = self
                .source
                .read(&mut self.input[self.end..])
                .map_err(|e| e.to_string())?;
            self.end += n;
            self.bytes += n as u64;
            self.eof = n == 0;
        }
        if self.start == self.end && self.eof {
            return self.finish();
        }
        let (used, info) = self
            .decoder
            .decode(&self.input[self.start..self.end], self.pcm.as_mut());
        if used > self.end - self.start {
            return Err("音频解码位置异常".into());
        }
        self.start += used;
        let Some(info) = info else {
            if used == 0 {
                if self.eof {
                    return self.finish();
                }
                if self.end - self.start >= 8192 {
                    return Err("不是可读取的 MP3 音频".into());
                }
            }
            return Ok(false);
        };
        if self.source_rate != info.sample_rate {
            self.source_rate = info.sample_rate;
            self.phase = 0;
            self.sum = [0; 2];
            self.count = 0;
        }
        let source_channels = info.channels.num() as usize;
        if info.sample_rate == 0
            || !(1..=2).contains(&source_channels)
            || info.samples_produced > self.pcm.len() / source_channels
        {
            return Err("音频格式不受支持".into());
        }
        self.converted.clear();
        // Continuous-phase box filter averages source samples before downsampling.
        for frame in
            self.pcm[..info.samples_produced * source_channels].chunks_exact(source_channels)
        {
            let left = i64::from(frame[0]);
            let right = i64::from(frame[source_channels - 1]);
            self.sum[0] += if self.channels == 1 {
                (left + right) / 2
            } else {
                left
            };
            self.sum[1] += right;
            self.count += 1;
            self.phase += 24000;
            let mut emitted = false;
            while self.phase >= info.sample_rate {
                self.phase -= info.sample_rate;
                for ch in 0..self.channels {
                    self.converted
                        .push((self.sum[ch] / i64::from(self.count)) as i16);
                }
                self.total_frames += 1;
                emitted = true;
            }
            if emitted {
                self.sum = [0; 2];
                self.count = 0;
            }
        }
        if !self.converted.is_empty() {
            for bytes in self
                .encoder
                .encode_interleaved(&self.converted)
                .map_err(|e| format!("压缩失败：{e}"))?
            {
                self.output.write_all(&bytes).map_err(|e| e.to_string())?;
            }
        }
        Ok(false)
    }
    fn finish(&mut self) -> Result<bool, String> {
        if self.total_frames == 0 {
            return Err("音频未产生可播放数据".into());
        }
        let tail = self.encoder.finish().map_err(|e| e.to_string())?;
        self.output
            .write_all(&tail)
            .and_then(|()| self.output.flush())
            .map_err(|e| e.to_string())?;
        self.song["duration_ms"] = Value::from(self.total_frames.saturating_mul(1000) / 24000);
        Ok(true)
    }
}
pub fn cancel() -> bool {
    let Some(j) = JOB.with(|j| j.borrow_mut().take()) else {
        return false;
    };
    dispose(j, true);
    state::with(|s| {
        s.transcoding = false;
        s.active = false;
        s.status = "已取消压缩".into();
    });
    true
}
fn dispose(j: Job, remove_output: bool) {
    timer::clear_timer(j.timer);
    let path = j.output_path.clone();
    let source = j.source_path.clone();
    let temporary = j.source_temporary;
    drop(j);
    if remove_output {
        let _ = fs::remove_file(path);
    }
    if temporary {
        let _ = fs::remove_file(&source);
        if let Some(p) = std::path::Path::new(&source).parent() {
            let _ = fs::remove_dir(p);
        }
    }
}
pub async fn tick() {
    let Some(mut j) = JOB.with(|j| j.borrow_mut().take()) else {
        return;
    };
    let started = Instant::now();
    let mut result = Ok(false);
    for _ in 0..4 {
        result = j.step();
        if result != Ok(false) || started.elapsed().as_millis() >= 20 {
            break;
        }
    }
    if result == Ok(false) {
        let render = j.last_render.elapsed().as_millis() >= 400;
        if render {
            state::with(|s| {
                s.sent = j.bytes;
                s.total = j.size;
            });
            j.last_render = Instant::now();
        }
        JOB.with(|slot| *slot.borrow_mut() = Some(j));
        if render {
            ui::rerender();
        }
        return;
    }
    let path = j.output_path.clone();
    let song = j.song.clone();
    dispose(j, result.is_err());
    state::with(|s| {
        s.transcoding = false;
        s.active = false;
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

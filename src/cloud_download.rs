//! Nonblocking WASI HTTP: each timer reads a bounded amount and returns to UI.
use crate::{astrobox::psys_host_v4::timer, cloud, package, state, ui};
use std::{
    fs,
    io::Write,
    sync::{Mutex, OnceLock},
    time::{Duration, Instant},
};

#[derive(Clone, Debug)]
pub enum Owner {
    Catalog,
    Module(String),
}
#[derive(Clone, Debug)]
pub struct Progress {
    pub owner: Owner,
    pub received: u64,
    pub total: Option<u64>,
    pub elapsed: u64,
    pub connecting: bool,
}
impl Progress {
    pub fn percent(&self) -> Option<u64> {
        self.total.filter(|n| *n > 0).map(|n| {
            self.received
                .saturating_mul(100)
                .checked_div(n)
                .unwrap_or(0)
                .min(100)
        })
    }
}
enum Destination {
    Catalog,
    Module { id: String, version: u32 },
}
struct Job {
    destination: Destination,
    addr: String,
    artifact: Option<cloud::Artifact>,
    response_length: Option<u64>,
    progress: Progress,
    bytes: Vec<u8>,
    started: Instant,
    last_byte: Instant,
    http: Http,
    timer: u64,
}
static JOB: OnceLock<Mutex<Option<Job>>> = OnceLock::new();
fn with<R>(f: impl FnOnce(&mut Option<Job>) -> R) -> R {
    f(&mut JOB
        .get_or_init(|| Mutex::new(None))
        .lock()
        .unwrap_or_else(|e| e.into_inner()))
}
pub fn busy() -> bool {
    with(|j| j.is_some())
}

fn begin(
    addr: &str,
    destination: Destination,
    artifact: Option<&cloud::Artifact>,
) -> Result<(), String> {
    let s = state::snapshot();
    if s.addr != addr || !s.device_connected() {
        return Err("请先连接手环".into());
    }
    if busy() {
        return Err("请先完成或取消当前下载".into());
    }
    let base = cloud::base()?;
    if base.is_empty() {
        return Err("云端仓库尚未配置".into());
    }
    let path = artifact.map(|a| a.path.as_str()).unwrap_or("index.json");
    let mut url = format!("{base}/{path}");
    if artifact.is_none() {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(|_| "系统时间无效，无法刷新云端清单")?
            .as_millis();
        url.push_str(&format!("?refresh={now}"));
    }
    let http = Http::start(&url)?;
    let owner = match &destination {
        Destination::Catalog => Owner::Catalog,
        Destination::Module { id, .. } => Owner::Module(id.clone()),
    };
    let progress = Progress {
        owner,
        received: 0,
        total: artifact.map(|a| a.size),
        elapsed: 0,
        connecting: true,
    };
    let now = Instant::now();
    let timer = timer::set_interval(100, "chem-download");
    with(|j| {
        *j = Some(Job {
            destination,
            addr: addr.into(),
            artifact: artifact.cloned(),
            response_length: None,
            progress: progress.clone(),
            bytes: Vec::new(),
            started: now,
            last_byte: now,
            http,
            timer,
        })
    });
    state::with(|s| {
        s.download = Some(progress);
        s.status.clear();
    });
    ui::rerender();
    Ok(())
}
pub fn catalog(addr: &str) -> Result<(), String> {
    begin(addr, Destination::Catalog, None)
}
pub fn module(addr: &str, module: &cloud::Module) -> Result<(), String> {
    begin(
        addr,
        Destination::Module {
            id: module.id.clone(),
            version: module.version,
        },
        Some(&module.artifact),
    )?;
    state::with(|s| s.module_message(&module.id, &module.name, "正在下载…"));
    Ok(())
}
pub fn cancel(silent: bool) {
    if let Some(job) = with(|j| j.take()) {
        timer::clear_timer(job.timer);
        state::with(|s| s.download = None);
        if !silent {
            match job.destination {
                Destination::Catalog => {
                    crate::connection::failed("已取消检查，点击刷新重试".into())
                }
                Destination::Module { id, .. } => {
                    state::with(|s| s.module_message(&id, "", "已取消安装"))
                }
            }
        }
    }
}

fn check_deadline(started: Duration, idle: Duration) -> Result<(), String> {
    if started >= Duration::from_secs(120) {
        return Err("下载已超过 2 分钟，请重试".into());
    }
    if idle >= Duration::from_secs(20) {
        return Err("下载超时：云端 20 秒未返回数据，请重试".into());
    }
    Ok(())
}
fn append(bytes: &mut Vec<u8>, chunk: &[u8], limit: u64) -> Result<(), String> {
    if bytes.len() as u64 + chunk.len() as u64 > limit {
        return Err("云端文件超过声明大小".into());
    }
    bytes.extend_from_slice(chunk);
    Ok(())
}
fn response_length(
    declared: Option<u64>,
    header: Option<u64>,
    limit: u64,
) -> Result<Option<u64>, String> {
    if header.is_some_and(|n| n == 0 || n > limit) {
        return Err("云端响应长度无效".into());
    }
    if declared.zip(header).is_some_and(|(a, b)| a != b) {
        return Err("云端文件长度与目录不一致，请刷新后重试".into());
    }
    Ok(declared.or(header))
}
// A known length completes on the last byte. HTTP keep-alive/EOF must not
// prevent an already complete, subsequently hashed file from being installed.
fn receive(
    bytes: &mut Vec<u8>,
    chunk: &[u8],
    expected: Option<u64>,
    limit: u64,
) -> Result<bool, String> {
    append(bytes, chunk, expected.unwrap_or(limit).min(limit))?;
    Ok(expected.is_some_and(|n| bytes.len() as u64 == n))
}
fn ended(received: u64, expected: Option<u64>) -> Result<(), String> {
    if expected.is_some_and(|n| n != received) {
        return Err(format!(
            "下载中断：已接收 {received} / {} 字节，请重试",
            expected.unwrap()
        ));
    }
    Ok(())
}
fn save(bytes: &[u8], a: &cloud::Artifact, extension: &str) -> Result<String, String> {
    if bytes.len() as u64 != a.size || package::digest(bytes) != a.sha256 {
        return Err("下载文件校验失败，请重试".into());
    }
    fs::create_dir_all("data/downloads").map_err(|e| e.to_string())?;
    let path = format!("data/downloads/{}.{}", a.sha256, extension);
    let tmp = format!("{path}.tmp");
    let result = (|| {
        let mut file = fs::File::create(&tmp).map_err(|e| e.to_string())?;
        file.write_all(bytes).map_err(|e| e.to_string())?;
        drop(file);
        if fs::exists(&path).unwrap_or(false) {
            fs::remove_file(&path).map_err(|e| e.to_string())?;
        }
        fs::rename(&tmp, &path).map_err(|e| e.to_string())?;
        Ok(path)
    })();
    if result.is_err() {
        let _ = fs::remove_file(tmp);
    }
    result
}
pub async fn tick() {
    if !busy() {
        return;
    }
    let mut finished_timer = None;
    let outcome = with(|slot| {
        let job = slot.as_mut()?;
        // Device changes invalidate both the download and its future install action.
        let s = state::snapshot();
        if s.addr != job.addr || !s.connected_devices.contains(&job.addr) {
            return Some(Err("手环连接已变化，请刷新后重试".to_string()));
        }
        if let Err(e) = check_deadline(job.started.elapsed(), job.last_byte.elapsed()) {
            return Some(Err(e));
        }
        let limit = job.artifact.as_ref().map(|a| a.size).unwrap_or(262144);
        // A bounded tick keeps Render/Cancel events responsive even on fast links.
        for _ in 0..4 {
            match job.http.poll() {
                Ok(Read::Waiting) => break,
                Ok(Read::Headers(header)) => {
                    match response_length(job.artifact.as_ref().map(|a| a.size), header, limit) {
                        Ok(n) => job.response_length = n,
                        Err(e) => return Some(Err(e)),
                    }
                    job.progress.total = job.response_length;
                    job.progress.connecting = false;
                    job.last_byte = Instant::now();
                }
                Ok(Read::Bytes(chunk)) => {
                    let complete = match receive(&mut job.bytes, &chunk, job.response_length, limit)
                    {
                        Ok(complete) => complete,
                        Err(e) => return Some(Err(e)),
                    };
                    job.last_byte = Instant::now();
                    job.progress.received = job.bytes.len() as u64;
                    if complete {
                        return Some(Ok(()));
                    }
                }
                Ok(Read::Done) => return Some(ended(job.bytes.len() as u64, job.response_length)),
                Err(e) => return Some(Err(e)),
            }
        }
        job.progress.elapsed = job.started.elapsed().as_secs();
        state::with(|s| s.download = Some(job.progress.clone()));
        None
    });
    if let Some(result) = outcome {
        if let Some(job) = with(|j| j.take()) {
            finished_timer = Some(job.timer);
            state::with(|s| {
                s.download = None;
                s.status.clear();
            });
            // Render completion before verification and delivery.
            ui::rerender();
            if state::snapshot().addr != job.addr {
                ui::rerender();
                timer::clear_timer(job.timer);
                return;
            }
            drop(job.http);
            match job.destination {
                Destination::Catalog => {
                    let catalog = result
                        .and_then(|_| {
                            serde_json::from_slice::<cloud::Catalog>(&job.bytes)
                                .map_err(|e| format!("云端目录无效：{e}"))
                        })
                        .and_then(|c| {
                            cloud::validate(&c)?;
                            Ok(c)
                        });
                    if let Err(e) = crate::bootstrap::catalog_ready(&job.addr, catalog).await {
                        crate::connection::failed(e);
                    }
                }
                Destination::Module { id, version } => {
                    match result
                        .and_then(|_| save(&job.bytes, job.artifact.as_ref().unwrap(), "chem"))
                    {
                        Ok(path) => {
                            let result = crate::delivery::start(&path, Some((&id, version))).await;
                            let _ = fs::remove_file(path);
                            if let Err(error) = result {
                                state::with(|s| s.module_message(&id, "", &error));
                            }
                        }
                        Err(e) => state::with(|s| s.module_message(&id, "", &e)),
                    }
                }
            }
        }
    }
    ui::rerender();
    // Clearing a host timer aborts its dispatch task. Do it only after the
    // completed file, next stage and UI have been handed off.
    if let Some(id) = finished_timer {
        timer::clear_timer(id);
    }
}

#[cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]
enum Read {
    Waiting,
    Headers(Option<u64>),
    Bytes(Vec<u8>),
    Done,
}

#[cfg(target_arch = "wasm32")]
use waki::bindings::wasi::{
    http::{
        outgoing_handler,
        types::{
            Fields, FutureIncomingResponse, IncomingBody, Method, OutgoingBody, OutgoingRequest,
            RequestOptions, Scheme,
        },
    },
    io::{
        poll::Pollable,
        streams::{InputStream, StreamError},
    },
};
#[cfg(target_arch = "wasm32")]
enum Http {
    Headers {
        ready: Pollable,
        future: FutureIncomingResponse,
    },
    // Resource children must be dropped before their parent body.
    Body {
        ready: Pollable,
        stream: InputStream,
        _body: IncomingBody,
    },
}
#[cfg(target_arch = "wasm32")]
impl Http {
    fn start(url: &str) -> Result<Self, String> {
        let tail = url.strip_prefix("https://").ok_or("云端地址必须为 HTTPS")?;
        let (authority, path) = tail.split_once('/').ok_or("云端地址无效")?;
        let request = OutgoingRequest::new(Fields::new());
        request
            .set_method(&Method::Get)
            .map_err(|_| "无法创建下载请求")?;
        request
            .set_scheme(Some(&Scheme::Https))
            .map_err(|_| "下载协议无效")?;
        request
            .set_authority(Some(authority))
            .map_err(|_| "下载域名无效")?;
        request
            .set_path_with_query(Some(&format!("/{path}")))
            .map_err(|_| "下载路径无效")?;
        let body = request.body().map_err(|_| "无法创建请求体")?;
        let options = RequestOptions::new();
        options
            .set_connect_timeout(Some(10_000_000_000))
            .map_err(|_| "无法设置连接超时")?;
        options
            .set_first_byte_timeout(Some(20_000_000_000))
            .map_err(|_| "无法设置响应超时")?;
        options
            .set_between_bytes_timeout(Some(20_000_000_000))
            .map_err(|_| "无法设置读取超时")?;
        let future = outgoing_handler::handle(request, Some(options))
            .map_err(|e| format!("连接云端失败：{e:?}"))?;
        OutgoingBody::finish(body, None).map_err(|e| format!("下载请求失败：{e:?}"))?;
        let ready = future.subscribe();
        Ok(Self::Headers { ready, future })
    }
    fn poll(&mut self) -> Result<Read, String> {
        match self {
            Self::Headers { ready, future } => {
                // get() only inspects state; readiness polling advances the host
                // future without blocking UI events between timer callbacks.
                if !ready.ready() {
                    return Ok(Read::Waiting);
                }
                let Some(response) = future.get() else {
                    return Ok(Read::Waiting);
                };
                let response = response
                    .map_err(|_| "响应已被读取")?
                    .map_err(|e| format!("下载失败：{e:?}"))?;
                if response.status() != 200 {
                    return Err(format!("云端 HTTP {}，请重试", response.status()));
                }
                let values = response.headers().get(&"content-length".into());
                let length = if values.is_empty() {
                    None
                } else {
                    if values.len() != 1 {
                        return Err("云端响应长度重复".into());
                    }
                    Some(
                        std::str::from_utf8(&values[0])
                            .ok()
                            .and_then(|v| v.parse::<u64>().ok())
                            .ok_or("云端响应长度无效")?,
                    )
                };
                let body = response.consume().map_err(|_| "无法读取下载内容")?;
                let stream = body.stream().map_err(|_| "无法读取下载数据流")?;
                let ready = stream.subscribe();
                *self = Self::Body {
                    ready,
                    stream,
                    _body: body,
                };
                Ok(Read::Headers(length))
            }
            Self::Body { ready, stream, .. } => {
                if !ready.ready() {
                    return Ok(Read::Waiting);
                }
                match stream.read(16384) {
                    Ok(bytes) if bytes.is_empty() => Ok(Read::Waiting),
                    Ok(bytes) => Ok(Read::Bytes(bytes)),
                    Err(StreamError::Closed) => Ok(Read::Done),
                    Err(e) => Err(format!("读取下载失败：{e:?}")),
                }
            }
        }
    }
}
#[cfg(not(target_arch = "wasm32"))]
struct Http;
#[cfg(not(target_arch = "wasm32"))]
impl Http {
    fn start(_: &str) -> Result<Self, String> {
        Err("下载需要插件宿主".into())
    }
    fn poll(&mut self) -> Result<Read, String> {
        Ok(Read::Waiting)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn progress_and_payload_bounds_use_real_received_bytes() {
        let mut p = Progress {
            owner: Owner::Catalog,
            received: 0,
            total: Some(100),
            elapsed: 0,
            connecting: false,
        };
        assert_eq!(p.percent(), Some(0));
        let mut bytes = vec![];
        append(&mut bytes, &[0; 32], 100).unwrap();
        p.received = bytes.len() as u64;
        assert_eq!(p.percent(), Some(32));
        assert!(append(&mut bytes, &[0; 69], 100).is_err());
        assert_eq!(bytes.len(), 32);
        p.total = None;
        assert_eq!(p.percent(), None);
    }
    #[test]
    fn stalled_and_total_downloads_have_bounded_deadlines() {
        assert!(check_deadline(Duration::from_secs(19), Duration::from_secs(19)).is_ok());
        assert!(check_deadline(Duration::from_secs(20), Duration::from_secs(20)).is_err());
        assert!(check_deadline(Duration::from_secs(120), Duration::ZERO).is_err());
    }
    #[test]
    fn final_two_percent_completes_without_waiting_for_http_eof() {
        let length = 238312;
        let mut bytes = Vec::new();
        assert!(
            !receive(
                &mut bytes,
                &vec![1; length * 98 / 100],
                Some(length as u64),
                length as u64
            )
            .unwrap()
        );
        let tail = length - bytes.len();
        assert!(
            receive(
                &mut bytes,
                &vec![2; tail],
                Some(length as u64),
                length as u64
            )
            .unwrap()
        );
        assert_eq!(bytes.len(), length);
        assert!(receive(&mut bytes, &[3], Some(length as u64), length as u64).is_err());
        assert_eq!(bytes.len(), length);
    }
    #[test]
    fn truncated_response_is_not_treated_as_a_completed_download() {
        assert!(ended(98, Some(100)).unwrap_err().contains("98 / 100"));
        assert!(ended(100, Some(100)).is_ok());
        assert!(ended(98, None).is_ok());
    }
    #[test]
    fn response_length_must_match_the_catalog_and_stay_bounded() {
        assert_eq!(response_length(Some(100), None, 100).unwrap(), Some(100));
        assert_eq!(response_length(None, Some(100), 200).unwrap(), Some(100));
        assert_eq!(response_length(None, None, 200).unwrap(), None);
        assert!(response_length(Some(100), Some(99), 100).is_err());
        assert!(response_length(None, Some(201), 200).is_err());
        assert!(response_length(None, Some(0), 200).is_err());
    }
    #[test]
    fn complete_length_still_requires_the_correct_file_hash() {
        let payload = b"complete but corrupt";
        let a = cloud::Artifact {
            path: String::new(),
            size: payload.len() as u64,
            sha256: "0".repeat(64),
        };
        assert!(save(payload, &a, "face").unwrap_err().contains("校验失败"));
    }
}

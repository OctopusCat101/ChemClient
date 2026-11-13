//! Cooperative HTTP: socket waits yield to rendering, BLE and cancellation.
use std::time::Duration;
#[cfg(target_arch = "wasm32")]
use std::{sync::atomic::{AtomicU64, Ordering}, time::Instant};

pub struct Response { pub cookies: Vec<String>, #[cfg_attr(not(target_arch="wasm32"),allow(dead_code))] pub length: Option<u64> }

pub fn check_length(received: u64, length: Option<u64>, limit: u64, done: bool) -> Result<bool, String> {
    if received > limit || length.is_some_and(|n| n > limit || received > n) {
        return Err("下载内容超过声明大小或导入上限".into());
    }
    if done && (received == 0 || length.is_some_and(|n| n != received)) {
        return Err("下载内容为空或不完整，请重试".into());
    }
    Ok(done || length == Some(received))
}

#[cfg(target_arch = "wasm32")]
mod wasi {
    use super::*;
    use waki::bindings::wasi::{http::{outgoing_handler, types::{Fields, OutgoingRequest,
        OutgoingBody, RequestOptions, FutureIncomingResponse, IncomingBody, Scheme, Method}},
        io::{poll::Pollable, streams::{InputStream, OutputStream, StreamError}}};

    enum Http {
        Send { ready: Pollable, stream: OutputStream, body: OutgoingBody,
            future: FutureIncomingResponse, bytes: Vec<u8>, offset: usize },
        Headers { ready: Pollable, future: FutureIncomingResponse },
        Body { ready: Pollable, stream: InputStream, _body: IncomingBody },
        Empty,
    }
    enum Read { Waiting, Sent, Headers(Response), Bytes(Vec<u8>), Done, Redirect(String) }
    impl Http {
        fn start(url: &str, post: bool, headers: &[(String, String)], bytes: Vec<u8>) -> Result<Self, String> {
            let (scheme, tail) = if let Some(tail) = url.strip_prefix("https://") {
                (Scheme::Https, tail)
            } else if let Some(tail) = url.strip_prefix("http://") { (Scheme::Http, tail) }
            else { return Err("下载地址协议无效".into()); };
            let (authority, path) = tail.split_once('/').unwrap_or((tail, ""));
            let fields = Fields::new();
            for (name, value) in headers {
                fields.append(name, &value.as_bytes().to_vec()).map_err(|_| "HTTP 请求头无效")?;
            }
            let request = OutgoingRequest::new(fields);
            request.set_method(&if post {Method::Post} else {Method::Get}).map_err(|_| "HTTP 方法无效")?;
            request.set_scheme(Some(&scheme)).map_err(|_| "HTTP 协议无效")?;
            request.set_authority(Some(authority)).map_err(|_| "HTTP 域名无效")?;
            request.set_path_with_query(Some(&format!("/{path}"))).map_err(|_| "HTTP 路径无效")?;
            let body = request.body().map_err(|_| "无法创建请求体")?;
            let options = RequestOptions::new();
            options.set_connect_timeout(Some(10_000_000_000)).map_err(|_| "无法设置超时")?;
            options.set_first_byte_timeout(Some(20_000_000_000)).map_err(|_| "无法设置超时")?;
            options.set_between_bytes_timeout(Some(20_000_000_000)).map_err(|_| "无法设置超时")?;
            let future = outgoing_handler::handle(request, Some(options)).map_err(|e| format!("网络请求失败：{e:?}"))?;
            if bytes.is_empty() {
                OutgoingBody::finish(body, None).map_err(|e| format!("请求体提交失败：{e:?}"))?;
                let ready = future.subscribe();
                Ok(Self::Headers { ready, future })
            } else {
                let stream = body.write().map_err(|_| "无法写入请求体")?;
                let ready = stream.subscribe();
                Ok(Self::Send { ready, stream, body, future, bytes, offset: 0 })
            }
        }
        fn poll(&mut self) -> Result<Read, String> {
            match self {
                Self::Send { ready, stream, bytes, offset, .. } => {
                    if !ready.ready() { return Ok(Read::Waiting); }
                    let permit = stream.check_write().map_err(|e| format!("请求发送失败：{e:?}"))? as usize;
                    if permit == 0 { return Ok(Read::Waiting); }
                    let end = (*offset + permit.min(64 * 1024)).min(bytes.len());
                    stream.write(&bytes[*offset..end]).map_err(|e| format!("请求发送失败：{e:?}"))?;
                    *offset = end;
                    if end == bytes.len() {
                        let Self::Send { ready, stream, body, future, .. } = std::mem::replace(self, Self::Empty) else { unreachable!() };
                        drop(ready); drop(stream);
                        OutgoingBody::finish(body, None).map_err(|e| format!("请求提交失败：{e:?}"))?;
                        let ready = future.subscribe();
                        *self = Self::Headers { ready, future };
                    }
                    Ok(Read::Sent)
                }
                Self::Headers { ready, future } => {
                    if !ready.ready() { return Ok(Read::Waiting); }
                    let Some(response) = future.get() else { return Ok(Read::Waiting); };
                    let response = response.map_err(|_| "响应已读取")?.map_err(|e| format!("网络响应失败：{e:?}"))?;
                    let fields = response.headers();
                    if matches!(response.status(), 301 | 302 | 303 | 307 | 308) {
                        let url = fields.get(&"location".into()).first()
                            .and_then(|v| String::from_utf8(v.clone()).ok()).ok_or("下载重定向地址无效")?;
                        return Ok(Read::Redirect(url));
                    }
                    if !(200..300).contains(&response.status()) { return Err(format!("HTTP {}", response.status())); }
                    let length = fields.get(&"content-length".into());
                    let length = if length.is_empty() { None } else {
                        if length.len() != 1 { return Err("HTTP 长度重复".into()); }
                        Some(std::str::from_utf8(&length[0]).ok().and_then(|s|s.parse::<u64>().ok()).ok_or("HTTP 长度无效")?)
                    };
                    let cookies = fields.get(&"set-cookie".into()).iter().filter_map(|v|String::from_utf8(v.clone()).ok()).collect();
                    drop(fields);
                    let body = response.consume().map_err(|_| "无法读取响应")?;
                    let stream = body.stream().map_err(|_| "无法读取响应流")?;
                    let ready = stream.subscribe();
                    *self = Self::Body { ready, stream, _body: body };
                    Ok(Read::Headers(Response { cookies, length }))
                }
                Self::Body { ready, stream, .. } => {
                    if !ready.ready() { return Ok(Read::Waiting); }
                    match stream.read(64 * 1024) {
                        Ok(bytes) if bytes.is_empty() => Ok(Read::Waiting),
                        Ok(bytes) => Ok(Read::Bytes(bytes)),
                        Err(StreamError::Closed) => Ok(Read::Done),
                        Err(e) => Err(format!("读取响应失败：{e:?}")),
                    }
                }
                Self::Empty => Err("HTTP 状态无效".into()),
            }
        }
    }
    pub async fn fetch(url: &str, post: bool, headers: &[(String, String)], body: Vec<u8>, limit: u64,
        write: impl FnMut(&[u8]) -> Result<(), String>,
        progress: impl FnMut(u64, Option<u64>, Duration) -> Result<(), String>) -> Result<Response, String> {
        static NEXT: AtomicU64 = AtomicU64::new(1);
        let id = NEXT.fetch_add(1, Ordering::Relaxed);
        let started = Instant::now();
        let host = url.split("://").nth(1).unwrap_or("").split('/').next().unwrap_or("");
        eprintln!("[chem:http:{id}] start {} {host}", if post { "POST" } else { "GET" });
        let result = fetch_inner(id, url, post, headers, body, limit, write, progress).await;
        eprintln!("[chem:http:{id}] {} total={}ms", if result.is_ok() { "complete" } else { "failed" }, started.elapsed().as_millis());
        result
    }
    async fn fetch_inner(id: u64, url: &str, post: bool, headers: &[(String, String)], body: Vec<u8>, limit: u64,
        mut write: impl FnMut(&[u8]) -> Result<(), String>,
        mut progress: impl FnMut(u64, Option<u64>, Duration) -> Result<(), String>) -> Result<Response, String> {
        let mut current_url = url.to_string();
        let mut http = Http::start(url, post, headers, body)?;
        let mut response = Response { cookies: vec![], length: None };
        let mut redirects = 0;
        let (mut received, started, mut last_byte) = (0, Instant::now(), Instant::now());
        loop {
            if last_byte.elapsed() >= Duration::from_secs(20) || started.elapsed() >= Duration::from_secs(180) {
                return Err("网络响应超时，请重试".into());
            }
            progress(received, response.length, started.elapsed())?;
            let turn = Instant::now();
            let mut waiting = false;
            for _ in 0..16 {
                match http.poll()? {
                    Read::Waiting => { waiting = true; break; }
                    Read::Sent => {}
                    Read::Headers(value) => {
                        eprintln!("[chem:http:{id}] headers={}ms length={:?}", started.elapsed().as_millis(), value.length);
                        check_length(0, value.length, limit, false)?;
                        response = value; last_byte = Instant::now();
                    }
                    Read::Bytes(bytes) => {
                        received += bytes.len() as u64;
                        let complete = check_length(received, response.length, limit, false)?;
                        write(&bytes)?; last_byte = Instant::now();
                        progress(received, response.length, started.elapsed())?;
                        if complete { return Ok(response); }
                    }
                    Read::Done => { check_length(received, response.length, limit, true)?; return Ok(response); }
                    Read::Redirect(location) => {
                        if post || redirects >= 3 { return Err("HTTP 重定向过多或 API 地址无效".into()); }
                        current_url = redirect_url(&current_url, &location)?;
                        http = Http::start(&current_url, false, headers, vec![])?;
                        redirects += 1;
                    }
                }
                if turn.elapsed().as_millis() >= 8 { break; }
            }
            if waiting {
                // Native async wait stays on this component task. Astro timers
                // dispatch a different callback and cannot wake a sleeping task.
                crate::wasi::clocks::monotonic_clock::wait_for(5_000_000).await;
            } else {
                wit_bindgen::yield_async().await;
            }
        }
    }
}

pub fn redirect_url(base: &str, location: &str) -> Result<String, String> {
    if location.starts_with("https://") || location.starts_with("http://") { return Ok(location.into()); }
    let (scheme, tail) = base.split_once("://").ok_or("下载地址无效")?;
    if location.starts_with("//") { return Ok(format!("{scheme}:{location}")); }
    if location.starts_with('/') {
        return Ok(format!("{scheme}://{}{location}", tail.split('/').next().unwrap_or("")));
    }
    Err("下载重定向地址无效".into())
}

#[cfg(target_arch = "wasm32")]
pub use wasi::fetch;
#[cfg(not(target_arch = "wasm32"))]
pub async fn fetch(_: &str, _: bool, _: &[(String, String)], _: Vec<u8>, _: u64,
    _: impl FnMut(&[u8]) -> Result<(), String>,
    _: impl FnMut(u64, Option<u64>, Duration) -> Result<(), String>) -> Result<Response, String> {
    Err("网络请求需要插件宿主".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn exact_size_finishes_without_eof_and_truncation_is_rejected() {
        assert!(!check_length(98, Some(100), 200, false).unwrap());
        assert!(check_length(100, Some(100), 200, false).unwrap());
        assert!(check_length(98, Some(100), 200, true).is_err());
        assert!(check_length(0, None, 200, true).is_err());
        assert!(check_length(201, None, 200, false).is_err());
    }
    #[test]
    fn cdn_redirects_preserve_query_and_use_the_new_host() {
        assert_eq!(redirect_url("https://one/a", "//two/song.mp3?x=1").unwrap(), "https://two/song.mp3?x=1");
        assert_eq!(redirect_url("https://one/a", "/b?x=1").unwrap(), "https://one/b?x=1");
        assert!(redirect_url("https://one/a", "file:bad").is_err());
    }
}

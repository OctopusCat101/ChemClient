use std::{cell::Cell, future::{Future, poll_fn}, pin::pin, task::Poll, time::Instant};
use aes::Aes128;
use cipher::{BlockEncryptMut, KeyInit, block_padding::Pkcs7};
use md5::{Digest, Md5};
use serde_json::{Value, json};

#[path = "../../../src/network.rs"]
mod network;
#[path = "../../../src/deadline.rs"]
mod deadline;
wit_bindgen::generate!({ path: "wit", world: "network-check", generate_all });
struct Check;

async fn get(url: &str, limit: u64) -> Result<Vec<u8>, String> {
    let mut bytes = vec![];
    network::fetch(url, false, &[], vec![], limit,
        |b| { bytes.extend_from_slice(b); Ok(()) }, |_,_,_|Ok(())).await?;
    Ok(bytes)
}

async fn local(base: &str) -> Result<(), String> {
    assert_eq!(deadline::bounded(100, std::future::ready(42)).await?,42);
    let timeout=Instant::now();
    assert!(deadline::bounded(40, std::future::pending::<()>()).await.is_err());
    assert!(timeout.elapsed().as_millis()>=35 && timeout.elapsed().as_millis()<500);
    deadline::bounded(200,wasi::clocks::monotonic_clock::wait_for(20_000_000)).await?;
    println!("PASS host deadline, pending cancellation and clock wakeup");
    let started = Instant::now();
    let heartbeat = Cell::new(0);
    let request = async {
        let bytes = get(&format!("{base}/slow"), 100).await?;
        assert_eq!(bytes,b"ready");
        assert!(heartbeat.get() >= 5, "network must allow other tasks to run");
        Ok::<_,String>(())
    };
    let beat = async {
        loop {
            wasi::clocks::monotonic_clock::wait_for(10_000_000).await;
            heartbeat.set(heartbeat.get()+1);
        }
    };
    let (mut request, mut beat)=(pin!(request),pin!(beat));
    poll_fn(|cx| {
        if let Poll::Ready(result)=request.as_mut().poll(cx) { return Poll::Ready(result); }
        let _=beat.as_mut().poll(cx);
        Poll::Pending
    }).await?;
    println!("PASS async wait and heartbeat {}ms",started.elapsed().as_millis());

    let payload=vec![0x5a;256*1024];
    let mut echoed=vec![];
    let response=network::fetch(&format!("{base}/echo"),true,&[],payload.clone(),payload.len() as u64,
        |b| { echoed.extend_from_slice(b); Ok(()) }, |_,_,_|Ok(())).await?;
    assert_eq!(echoed,payload);
    assert_eq!(response.cookies,vec!["chem-test=ok; Path=/"]);
    println!("PASS streamed POST body and response cookies");
    let started=Instant::now();
    let bytes=get(&format!("{base}/large"),16*1024*1024).await?;
    assert_eq!(bytes.len(),16*1024*1024);
    assert!(bytes.iter().all(|b|*b==0x5a));
    println!("PASS 16MiB streaming {}ms",started.elapsed().as_millis());
    assert_eq!(get(&format!("{base}/redirect"),100).await?,b"ready");
    assert_eq!(get(&format!("{base}/chunked"),100).await?,b"ready");
    assert!(get(&format!("{base}/truncated"),100).await.is_err());
    assert!(get(&format!("{base}/large"),100).await.is_err());
    let started=Instant::now();
    assert_eq!(get(&format!("{base}/keepalive"),100).await?,b"ready");
    assert!(started.elapsed().as_secs()<1,"known length must not wait for EOF");
    let started=Instant::now();
    let error=network::fetch(&format!("{base}/slow"),false,&[],vec![],100,
        |_|Ok(()),|_,_,_|if started.elapsed().as_millis()>=40 { Err("cancelled".into()) } else { Ok(()) }).await;
    assert!(matches!(error,Err(e) if e=="cancelled"));
    println!("PASS redirect, chunked, truncation, bounds, keepalive and cancellation");
    Ok(())
}

async fn netease() -> Result<(),String> {
    for (path,data) in [
        ("/api/search/get",json!({"s":"hello","type":1,"limit":20,"offset":0})),
        ("/api/song/lyric",json!({"id":347230,"lv":-1,"kv":-1,"tv":-1})),
        ("/api/v3/song/detail",json!({"c":"[{\"id\":347230}]"})),
        ("/api/song/enhance/player/url",json!({"ids":"[\"347230\"]","br":192000})),
    ] {
        let mut data=data;
        data["header"]=json!({"os":"pc","appver":"3.1.17.204416"});
        data["e_r"]=json!(false);
        let data=data.to_string();
        let digest=Md5::digest(format!("nobody{path}use{data}md5forencrypt"));
        let signed=format!("{path}-36cd479b6b5-{data}-36cd479b6b5-{digest:x}");
        let mut buffer=signed.as_bytes().to_vec();
        buffer.resize(buffer.len()+16,0);
        let encrypted=Aes128::new(b"e82ckenh8dichen8".into()).encrypt_padded_mut::<Pkcs7>(&mut buffer,signed.len()).unwrap();
        let body=format!("params={}",encrypted.iter().map(|b|format!("{b:02X}")).collect::<String>());
        let url=format!("https://interfacepc.music.163.com{}",path.replacen("/api/","/eapi/",1));
        let mut bytes=vec![];
        let started=Instant::now();
        network::fetch(&url,true,&[
            ("Content-Type".into(),"application/x-www-form-urlencoded".into()),
            ("User-Agent".into(),"NeteaseMusic 9.0.90/5038 (iPhone; iOS 16.2; zh_CN)".into())],
            body.into_bytes(),2*1024*1024,|b|{bytes.extend_from_slice(b);Ok(())},|_,_,_|Ok(())).await?;
        let response:Value=serde_json::from_slice(&bytes).map_err(|e|e.to_string())?;
        assert_eq!(response["code"],200);
        println!("PASS {path} {}ms {} bytes",started.elapsed().as_millis(),bytes.len());
    }
    Ok(())
}

impl exports::wasi::cli::run::Guest for Check {
    async fn run() -> Result<(),()> {
        let args:Vec<_>=std::env::args().collect();
        let result=if args.get(1).map(String::as_str)==Some("netease") { netease().await }
            else { local(args.get(1).expect("local test server URL")).await };
        if let Err(e)=&result { eprintln!("FAIL {e}"); }
        result.map_err(|_|())
    }
}
export!(Check);

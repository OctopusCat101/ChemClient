//! Prepare small, device-ready artwork and bounded lyrics on the phone.
use image::{ImageReader, imageops::FilterType};
use material_colors::{color::Argb, quantize::{Quantizer, QuantizerCelebi}, score::Score, theme::ThemeBuilder};
use serde_json::{Value, json};
use std::{fs, io::{Read, Seek, SeekFrom}};

pub const SIDE: u32 = 96;
pub fn cover(path: &str, id: u64) -> Result<String, String> {
    if fs::metadata(path).map_err(|e| e.to_string())?.len() > 8 * 1024 * 1024 {
        return Err("封面请选择 8 MiB 以内的 JPG/PNG".into());
    }
    let mut reader = ImageReader::open(path).map_err(|e| e.to_string())?
        .with_guessed_format().map_err(|e| e.to_string())?;
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(4096); limits.max_image_height = Some(4096);
    limits.max_alloc = Some(32 * 1024 * 1024); reader.limits(limits);
    let decoded = reader.decode().map_err(|e| format!("读取封面失败：{e}"))?;
    let output=encode(&decoded);
    fs::create_dir_all("media/attachments").map_err(|e| e.to_string())?;
    let target=format!("media/attachments/{id}.ncv");
    fs::write(&target,output).map_err(|e| e.to_string())?;
    Ok(target)
}
fn encode(decoded: &image::DynamicImage) -> Vec<u8> {
    // Derive colors from the entire image, before square cropping artwork.
    let sampled = decoded.thumbnail(64, 64).to_rgba8();
    let pixels: Vec<_> = sampled.pixels().filter(|p| p[3] >= 128)
        .map(|p| Argb::new(255,p[0],p[1],p[2])).collect();
    let colors = QuantizerCelebi::quantize(&pixels, 32);
    let seed = Score::score(&colors.color_to_count, Some(1), None, Some(true))[0];
    let scheme = ThemeBuilder::with_source(seed).build().schemes.dark;
    let rgba = decoded.resize_to_fill(SIDE,SIDE,FilterType::Triangle).to_rgba8();
    let mut output = Vec::with_capacity(16 + (SIDE*SIDE*4) as usize);
    output.extend_from_slice(b"NCV1");
    for color in [scheme.primary, scheme.on_primary, scheme.secondary_container, scheme.on_secondary_container] {
        let rgb565 = ((u16::from(color.red)>>3)<<11) | ((u16::from(color.green)>>2)<<5) | (u16::from(color.blue)>>3);
        output.extend_from_slice(&rgb565.to_le_bytes());
    }
    output.extend_from_slice(&SIDE.to_le_bytes());
    for (x,y,p) in rgba.enumerate_pixels() { // Firmware format16 is BGRA8888, matching the committed launcher assets.
        let dx=if x<12 {12-x} else {x.saturating_sub(SIDE-13)};
        let dy=if y<12 {12-y} else {y.saturating_sub(SIDE-13)};
        let a=if dx*dx+dy*dy>144 {0} else {u16::from(p[3])};
        output.extend_from_slice(&[(u16::from(p[2])*a/255) as u8,(u16::from(p[1])*a/255) as u8,(u16::from(p[0])*a/255) as u8,255]);
    }
    output
}

pub fn lyrics(path: &str, id: u64) -> Result<String, String> {
    let mut bytes=Vec::new();
    fs::File::open(path).map_err(|e| e.to_string())?.take(96*1024+1)
        .read_to_end(&mut bytes).map_err(|e| e.to_string())?;
    if bytes.len()>96*1024 { return Err("歌词不能超过 96 KiB".into()); }
    let text=std::str::from_utf8(&bytes).map_err(|_| "歌词请使用 UTF-8 编码")?;
    fs::create_dir_all("media/attachments").map_err(|e| e.to_string())?;
    let target=format!("media/attachments/{id}.lrc");
    fs::write(&target,text.trim_start_matches('\u{feff}')).map_err(|e| e.to_string())?;
    Ok(target)
}
pub fn add(song: &mut Value, path: String, format: &str) {
    if !song["_attachments"].is_array() { song["_attachments"]=json!([]); }
    song["_attachments"].as_array_mut().unwrap().push(json!({"path":path,"format":format}));
}
pub fn local(song: &mut Value, cover_path: &str, lyric_path: &str, audio_path: &str) -> Result<(),String> {
    let id=song["id"].as_u64().ok_or("歌曲编号无效")?;
    if !cover_path.is_empty() { add(song,cover(cover_path,id)?,"cover1"); }
    else if let Some(path) = embedded_cover(audio_path,id) {
        add(song,path,"cover1");
    }
    if !lyric_path.is_empty() { add(song,lyrics(lyric_path,id)?,"lrc"); }
    Ok(())
}

/// Scan ID3 frame headers on disk; never read the audio or entire tag into RAM.
fn embedded_cover(path: &str, id: u64) -> Option<String> {
    let mut file=fs::File::open(path).ok()?;
    let mut header=[0u8;10]; file.read_exact(&mut header).ok()?;
    if &header[..3]!=b"ID3" || !matches!(header[3],3|4) || header[5]&0x80!=0 { return None; }
    fn sync_size(bytes:&[u8])->Option<u32> {
        if bytes.iter().any(|b|b&0x80!=0) { return None; }
        Some(bytes.iter().fold(0,|n,b|(n<<7)|u32::from(*b)))
    }
    let version=header[3];
    let end=10+u64::from(sync_size(&header[6..10])?);
    if end>file.metadata().ok()?.len() { return None; }
    if header[5]&0x40!=0 {
        let mut size=[0;4]; file.read_exact(&mut size).ok()?;
        let n=if version==4 { sync_size(&size)?.checked_sub(4)? } else { u32::from_be_bytes(size) };
        file.seek(SeekFrom::Current(i64::from(n))).ok()?;
    }
    // Bounded work even for a malformed tag containing many tiny frames.
    for _ in 0..256 {
        if file.stream_position().ok()?.checked_add(10)?>end { break; }
        file.read_exact(&mut header).ok()?;
        if header[..4]==[0;4] { break; }
        let n=if version==4 { sync_size(&header[4..8])? } else { u32::from_be_bytes(header[4..8].try_into().ok()?) };
        if n==0 || file.stream_position().ok()?.checked_add(u64::from(n))?>end { return None; }
        if &header[..4]!=b"APIC" || header[9]!=0 || n>512*1024 {
            file.seek(SeekFrom::Current(i64::from(n))).ok()?; continue;
        }
        let mut data=vec![0;n as usize]; file.read_exact(&mut data).ok()?;
        let encoding=*data.first()?;
        let mime_end=data[1..].iter().position(|b|*b==0)?+1;
        let description=mime_end+2; // MIME terminator, then picture type.
        let tail=data.get(description..)?;
        let skip=match encoding {
            0|3 => tail.iter().position(|b|*b==0)?+1,
            1|2 => tail.chunks_exact(2).position(|c|c==[0,0])?*2+2,
            _=>return None,
        };
        let image=tail.get(skip..)?;
        fs::create_dir_all("media/attachments").ok()?;
        let source=format!("media/attachments/{id}.embedded");
        fs::write(&source,image).ok()?;
        let result=cover(&source,id).ok();
        let _=fs::remove_file(source);
        return result;
    }
    None
}

/// Generated sidecars are disposable; original user-selected files are retained.
pub fn cleanup(song: &Value) {
    if let Some(items)=song["_attachments"].as_array() {
        for item in items { if let Some(path)=item["path"].as_str().filter(|p|p.starts_with("media/attachments/")) { let _=fs::remove_file(path); } }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn embedded_id3_covers_are_found_without_reading_audio() {
        use std::io::Cursor;
        let id=std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos() as u64;
        let source=std::env::temp_dir().join(format!("chemclient-apic-{id}.mp3"));
        let mut png=Cursor::new(Vec::new());
        image::DynamicImage::new_rgb8(8,8).write_to(&mut png,image::ImageFormat::Png).unwrap();
        fn sync(n:usize)->[u8;4] { [(n>>21) as u8&127,(n>>14) as u8&127,(n>>7) as u8&127,n as u8&127] }
        for version in [3,4] {
            let mut apic=b"\x03image/png\0\x03\0".to_vec(); apic.extend_from_slice(png.get_ref());
            let mut tag=b"APIC".to_vec();
            tag.extend_from_slice(&if version==4 {sync(apic.len())}else{(apic.len() as u32).to_be_bytes()});
            tag.extend_from_slice(&[0,0]); tag.extend_from_slice(&apic);
            let mut file=vec![b'I',b'D',b'3',version,0,0];
            file.extend_from_slice(&sync(tag.len())); file.extend_from_slice(&tag);
            // Deliberately no audio bytes: artwork extraction must not decode it.
            fs::write(&source,file).unwrap();
            let path=embedded_cover(source.to_str().unwrap(),id).unwrap();
            let bytes=fs::read(&path).unwrap();
            assert_eq!(bytes.len(),36880); assert_eq!(&bytes[..4],b"NCV1");
            fs::remove_file(path).unwrap();
        }
        fs::write(&source,b"ID3\x04\0\0\x7f\x7f\x7f\x7f").unwrap();
        assert!(embedded_cover(source.to_str().unwrap(),id).is_none());
        fs::remove_file(source).unwrap();
    }
    #[test]
    fn native_cover_has_bgra_pixels_opaque_black_corners_and_distinct_roles() {
        let source=image::DynamicImage::ImageRgba8(image::RgbaImage::from_pixel(200,100,image::Rgba([255,0,0,255])));
        let bytes=encode(&source);
        assert_eq!(bytes.len(),36880);
        assert_eq!(&bytes[..4],b"NCV1");
        assert_eq!(&bytes[12..16],&96u32.to_le_bytes());
        assert_ne!(&bytes[4..6],&bytes[6..8]);
        assert_eq!(&bytes[16..20],&[0,0,0,255]);
        let pixel=16+(48*96+48)*4;
        assert_eq!(&bytes[pixel..pixel+4],&[0,0,255,255]);
    }
    #[test]
    fn transparent_artwork_uses_a_valid_default_palette() {
        let source=image::DynamicImage::new_rgba8(8,8);
        let bytes=encode(&source);
        assert_ne!(&bytes[4..6],&bytes[6..8]);
        assert!(bytes[16..].chunks_exact(4).all(|p|p==[0,0,0,255]));
    }
}

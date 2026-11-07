//! Bounded binary envelope inside the documented Xiaomi ThirdpartyApp protobuf.
pub const PACKAGE: &str = "com.canopus.music";
pub const CHUNK: usize = 16 * 1024;
fn varint(mut n: u64, out: &mut Vec<u8>) {
    while n >= 128 {
        out.push((n as u8) | 128);
        n >>= 7;
    }
    out.push(n as u8);
}
#[cfg(test)]
fn field(tag: u8, data: &[u8], out: &mut Vec<u8>) {
    out.push(tag);
    varint(data.len() as u64, out);
    out.extend_from_slice(data);
}
pub fn frame(request: u32, op: u8, args: &[u8]) -> Vec<u8> {
    let mut v = Vec::with_capacity(9 + args.len());
    v.extend_from_slice(b"CMI1");
    v.extend_from_slice(&request.to_le_bytes());
    v.push(op);
    v.extend_from_slice(args);
    v
}
pub fn encode(data: &[u8]) -> Vec<u8> {
    fn field_size(n: usize) -> usize {
        let mut value = n;
        let mut prefix = 1;
        while value >= 128 { prefix += 1; value >>= 7; }
        1 + prefix + n
    }
    let info_size = field_size(PACKAGE.len()) + 2;
    let message_size = field_size(info_size) + field_size(data.len());
    let third_size = field_size(message_size);
    // Encode the nested envelope directly: no audio-sized intermediate Vecs.
    let mut packet = Vec::with_capacity(5 + field_size(third_size));
    packet.extend_from_slice(&[8, 20, 16, 8, 178, 1]);
    varint(third_size as u64, &mut packet);
    packet.push(74);
    varint(message_size as u64, &mut packet);
    packet.push(10);
    varint(info_size as u64, &mut packet);
    packet.push(10);
    varint(PACKAGE.len() as u64, &mut packet);
    packet.extend_from_slice(PACKAGE.as_bytes());
    packet.extend_from_slice(&[18, 0, 18]);
    varint(data.len() as u64, &mut packet);
    packet.extend_from_slice(data);
    packet
}
fn take_varint(b: &[u8], i: &mut usize) -> Option<u64> {
    let mut v = 0u64;
    for shift in (0..=63).step_by(7) {
        let c = *b.get(*i)?;
        *i += 1;
        if shift == 63 && c > 1 {
            return None;
        }
        v |= ((c & 127) as u64) << shift;
        if c < 128 {
            return Some(v);
        }
    }
    None
}
fn fields(b: &[u8], wanted: u64) -> Option<&[u8]> {
    let mut i = 0;
    while i < b.len() {
        let tag = take_varint(b, &mut i)?;
        match tag & 7 {
            0 => {
                take_varint(b, &mut i)?;
            }
            2 => {
                let n = usize::try_from(take_varint(b, &mut i)?).ok()?;
                let end = i.checked_add(n)?;
                let v = b.get(i..end)?;
                if tag >> 3 == wanted {
                    return Some(v);
                }
                i = end;
            }
            1 => {
                i = i.checked_add(8)?;
            }
            4 | 3 => return None,
            5 => {
                i = i.checked_add(4)?;
            }
            _ => return None,
        }
    }
    None
}
pub fn decode(packet: &[u8]) -> Option<(u32, i32, serde_json::Value)> {
    if packet.len() > 8192 {
        return None;
    }
    let message = fields(fields(packet, 22)?, 9)?;
    let info = fields(message, 1)?;
    if fields(info, 1)? != PACKAGE.as_bytes() {
        return None;
    }
    let b = fields(message, 2)?;
    if b.len() < 12 || &b[..4] != b"CMO1" {
        return None;
    }
    Some((
        u32::from_le_bytes(b[4..8].try_into().ok()?),
        i32::from_le_bytes(b[8..12].try_into().ok()?),
        serde_json::from_slice(&b[12..]).unwrap_or(serde_json::Value::Null),
    ))
}
#[cfg(test)]
mod tests {
    #[test]
    fn direct_encoding_matches_the_nested_wire_format_at_length_boundaries() {
        use super::*;
        for size in [0,1,127,128,16383,16384,16397] {
            let data=vec![0xa5;size];
            let mut info=Vec::new(); field(10,PACKAGE.as_bytes(),&mut info); field(18,&[],&mut info);
            let mut message=Vec::new(); field(10,&info,&mut message); field(18,&data,&mut message);
            let mut third=Vec::new(); field(74,&message,&mut third);
            let mut expected=vec![8,20,16,8,178,1];
            varint(third.len() as u64,&mut expected); expected.extend_from_slice(&third);
            assert_eq!(encode(&data),expected,"payload size {size}");
        }
    }
    use super::*;
    #[test]
    fn wire_and_bounds() {
        let raw = encode(&frame(42, 4, &[1, 2, 3]));
        assert_eq!(&raw[..4], &[8, 20, 16, 8]);
        assert!(fields(&raw, 22).is_some());
        assert!(fields(&[10, 255, 255], 1).is_none());
        assert!(decode(&[0; 12]).is_none());
    }
    #[test]
    fn reply_roundtrip_rejects_every_truncated_payload() {
        let mut reply = b"CMO1".to_vec();
        reply.extend_from_slice(&42u32.to_le_bytes());
        reply.extend_from_slice(&0i32.to_le_bytes());
        reply.extend_from_slice(br#"{"offset":16384}"#);
        let mut packet = encode(&reply);
        packet[3] = 9;
        let (id, status, value) = decode(&packet).unwrap();
        assert_eq!((id, status), (42, 0));
        assert_eq!(value["offset"], 16384);
        for end in 0..packet.len() {
            assert!(decode(&packet[..end]).is_none());
        }
        let position = packet
            .windows(PACKAGE.len())
            .position(|p| p == PACKAGE.as_bytes())
            .unwrap();
        packet[position] = b'x';
        assert!(decode(&packet).is_none());
    }
    #[test]
    fn chunk_length_and_offset_are_preserved() {
        let mut args = 7u32.to_le_bytes().to_vec();
        args.extend((0..CHUNK).map(|n| n as u8));
        let raw = frame(99, 4, &args);
        let encoded = encode(&raw);
        let message = fields(fields(&encoded, 22).unwrap(), 9).unwrap();
        assert_eq!(fields(message, 2).unwrap(), raw);
        assert_eq!(raw.len(), CHUNK + 13);
        assert!(raw.len() <= 16 * 1024 + 32);
    }
}

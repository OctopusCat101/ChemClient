//! One signed native package, shared by local import and cloud delivery.
use ed25519_dalek::{Signature, VerifyingKey};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeSet,
    fs::File,
    io::{Read, Seek},
};

pub const MAX_PACKAGE: u64 = 9 * 1024 * 1024;
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Resource {
    pub name: String,
    pub size: usize,
    pub sha256: String,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Manifest {
    pub schema: u32,
    pub id: String,
    pub name: String,
    pub version: u32,
    pub version_name: String,
    pub target: String,
    pub lifecycle: u32,
    pub firmware_sha256: String,
    pub artifact_size: usize,
    pub artifact_sha256: String,
    #[serde(default)]
    pub resources: Vec<Resource>,
}
pub struct Package {
    pub manifest: Manifest,
    pub receipt: Vec<u8>,
    pub elf: Vec<u8>,
    pub resources: Vec<Vec<u8>>,
}
pub fn digest(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}
pub fn safe_name(s: &str, max: usize) -> bool {
    !s.is_empty()
        && s.len() <= max
        && !s.starts_with('.')
        && !s.ends_with('.')
        && s.bytes()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'_' | b'-' | b'.'))
}
pub fn module_id(s: &str) -> bool {
    safe_name(s, 31)
        && !s.starts_with('-')
        && !s.ends_with('-')
        && !s.bytes().any(|c| c.is_ascii_uppercase())
}
fn word(b: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(b[at..at + 4].try_into().unwrap())
}
fn fixed(b: &[u8]) -> Result<&str, String> {
    let end = b.iter().position(|c| *c == 0).ok_or("签名字段缺少结束符")?;
    if b[end..].iter().any(|c| *c != 0) {
        return Err("签名字段无效".into());
    }
    std::str::from_utf8(&b[..end]).map_err(|_| "签名字段编码无效".into())
}
impl Package {
    pub fn open(path: &str, target: &str, firmware_hash: &str) -> Result<Self, String> {
        let f = File::open(path).map_err(|e| format!("读取模块失败：{e}"))?;
        if f.metadata().map_err(|e| e.to_string())?.len() > MAX_PACKAGE {
            return Err("模块包超过大小限制".into());
        }
        Self::read(
            f,
            target,
            firmware_hash,
            include_bytes!("../assets/module-key.bin"),
        )
    }
    fn read<R: Read + Seek>(
        reader: R,
        target: &str,
        firmware_hash: &str,
        key: &[u8; 32],
    ) -> Result<Self, String> {
        let mut zip = zip::ZipArchive::new(reader).map_err(|_| "不是有效的 .chem 模块包")?;
        if !(3..=11).contains(&zip.len()) {
            return Err("模块包文件数量无效".into());
        }
        let mut names = BTreeSet::new();
        let mut total = 0u64;
        for i in 0..zip.len() {
            let f = zip.by_index(i).map_err(|e| e.to_string())?;
            let name = f.name();
            let allowed = matches!(name, "manifest.json" | "receipt.bin" | "module.elf")
                || name
                    .strip_prefix("resources/")
                    .is_some_and(|n| safe_name(n, 48));
            if !allowed
                || !names.insert(name.to_string())
                || f.is_dir()
                || f.unix_mode().is_some_and(|m| m & 0o170000 == 0o120000)
            {
                return Err("模块包包含重复或不支持的文件".into());
            }
            let limit = match name {
                "manifest.json" => 16384,
                "receipt.bin" => 256,
                "module.elf" => 393216,
                _ => 1048576,
            };
            total = total.checked_add(f.size()).ok_or("模块包大小无效")?;
            if f.size() > limit || total > MAX_PACKAGE {
                return Err("模块文件超过大小限制".into());
            }
        }
        fn member<R: Read + Seek>(
            zip: &mut zip::ZipArchive<R>,
            name: &str,
        ) -> Result<Vec<u8>, String> {
            let mut f = zip
                .by_name(name)
                .map_err(|_| format!("模块包缺少 {name}"))?;
            let mut b = Vec::with_capacity(f.size() as usize);
            f.read_to_end(&mut b)
                .map_err(|e| format!("模块文件损坏：{e}"))?;
            Ok(b)
        }
        let manifest: Manifest = serde_json::from_slice(&member(&mut zip, "manifest.json")?)
            .map_err(|e| format!("模块清单无效：{e}"))?;
        let m = &manifest;
        if m.schema != 1
            || !module_id(&m.id)
            || m.name.is_empty()
            || m.name.len() > 96
            || m.version_name.len() > 32
            || m.resources.len() > 8
        {
            return Err("模块清单版本或标识无效".into());
        }
        let receipt = member(&mut zip, "receipt.bin")?;
        let r = &receipt;
        if r.len() != 256
            || word(r, 0) != 0x31494d43
            || word(r, 4) != 1
            || word(r, 8) != 256
            || word(r, 12) != 0
            || word(r, 28) != 0
        {
            return Err("模块签名凭据格式不支持".into());
        }
        let signature = Signature::from_slice(&r[192..]).map_err(|_| "模块签名无效")?;
        VerifyingKey::from_bytes(key)
            .map_err(|_| "公钥无效")?
            .verify_strict(&r[..192], &signature)
            .map_err(|_| "模块签名验证失败")?;
        if fixed(&r[32..64])? != m.id
            || fixed(&r[64..112])? != m.target
            || word(r, 16) != m.lifecycle
            || m.lifecycle > 3
            || word(r, 20) != m.version
            || word(r, 24) as usize != m.artifact_size
            || hex(&r[112..144]) != m.firmware_sha256
            || hex(&r[144..176]) != m.artifact_sha256
        {
            return Err("模块清单与签名不一致".into());
        }
        if m.target != target || m.firmware_sha256 != firmware_hash {
            return Err("模块不适用于当前手环或固件".into());
        }
        let elf = member(&mut zip, "module.elf")?;
        if !(512..=393216).contains(&elf.len())
            || elf.len() != m.artifact_size
            || digest(&elf) != m.artifact_sha256
            || &elf[..7] != b"\x7fELF\x01\x01\x01"
            || elf[16..20] != [1, 0, 40, 0]
        {
            return Err("模块 ELF 或校验值无效".into());
        }
        let mut resources = Vec::new();
        let mut expected = BTreeSet::from([
            "module.elf".into(),
            "manifest.json".into(),
            "receipt.bin".into(),
        ]);
        for resource in &m.resources {
            let name = format!("resources/{}", resource.name);
            if !safe_name(&resource.name, 48)
                || !expected.insert(name.clone())
                || !(1..=1048576).contains(&resource.size)
            {
                return Err("模块资源无效".into());
            }
            let bytes = member(&mut zip, &name)?;
            if bytes.len() != resource.size || digest(&bytes) != resource.sha256 {
                return Err("模块资源校验失败".into());
            }
            resources.push(bytes);
        }
        if expected != names {
            return Err("模块包中存在清单未声明的文件".into());
        }
        Ok(Self {
            manifest,
            receipt,
            elf,
            resources,
        })
    }
}
pub fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::{Signer, SigningKey};
    use std::io::{Cursor, Write};
    fn archive(
        mutator: impl FnOnce(&mut Vec<u8>, &mut Vec<u8>, &mut Manifest),
        extra: bool,
    ) -> (Vec<u8>, [u8; 32]) {
        let key = SigningKey::from_bytes(&[0x53; 32]);
        let mut elf = vec![0; 512];
        elf[..7].copy_from_slice(b"\x7fELF\x01\x01\x01");
        elf[16..20].copy_from_slice(&[1, 0, 40, 0]);
        let firmware = "01".repeat(32);
        let mut receipt = vec![0; 256];
        for (at, value) in [
            (0, 0x31494d43u32),
            (4, 1),
            (8, 256),
            (16, 1),
            (20, 7),
            (24, 512),
        ] {
            receipt[at..at + 4].copy_from_slice(&value.to_le_bytes());
        }
        receipt[32..36].copy_from_slice(b"test");
        receipt[64..70].copy_from_slice(b"target");
        receipt[112..144].fill(1);
        receipt[144..176].copy_from_slice(&Sha256::digest(&elf));
        let sig = key.sign(&receipt[..192]);
        receipt[192..].copy_from_slice(&sig.to_bytes());
        let mut manifest = Manifest {
            schema: 1,
            id: "test".into(),
            name: "Test".into(),
            version: 7,
            version_name: "1.0".into(),
            target: "target".into(),
            lifecycle: 1,
            firmware_sha256: firmware,
            artifact_size: 512,
            artifact_sha256: digest(&elf),
            resources: vec![],
        };
        mutator(&mut elf, &mut receipt, &mut manifest);
        let mut out = zip::ZipWriter::new(Cursor::new(Vec::new()));
        let options = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Stored);
        for (name, bytes) in [
            ("module.elf", elf),
            ("receipt.bin", receipt),
            ("manifest.json", serde_json::to_vec(&manifest).unwrap()),
        ] {
            out.start_file(name, options).unwrap();
            out.write_all(&bytes).unwrap();
        }
        if extra {
            out.start_file("resources/hidden.bin", options).unwrap();
            out.write_all(&[1]).unwrap();
        }
        (
            out.finish().unwrap().into_inner(),
            key.verifying_key().to_bytes(),
        )
    }
    #[test]
    fn signed_package_rejects_tampering_and_wrong_device() {
        let fw = "01".repeat(32);
        let (bytes, key) = archive(|_, _, _| {}, false);
        assert!(Package::read(Cursor::new(&bytes), "target", &fw, &key).is_ok());
        assert!(Package::read(Cursor::new(&bytes), "other", &fw, &key).is_err());
        assert!(Package::read(Cursor::new(&bytes), "target", &"02".repeat(32), &key).is_err());
        for kind in 0..3 {
            let (bytes, key) = archive(
                |elf, r, m| match kind {
                    0 => elf[100] ^= 1,
                    1 => r[192] ^= 1,
                    _ => m.version += 1,
                },
                false,
            );
            assert!(Package::read(Cursor::new(bytes), "target", &fw, &key).is_err());
        }
        let (bytes, key) = archive(|_, _, _| {}, true);
        assert!(Package::read(Cursor::new(bytes), "target", &fw, &key).is_err());
    }
    #[test]
    fn package_rejects_truncation_and_path_components() {
        let (bytes, key) = archive(|_, _, _| {}, false);
        for n in [0, 10, 100, bytes.len() - 1] {
            assert!(
                Package::read(Cursor::new(&bytes[..n]), "target", &"01".repeat(32), &key).is_err()
            );
        }
        for name in ["../x", "a/b", ".hidden", "x\\y", "x:", "x.", ""] {
            assert!(!safe_name(name, 48));
        }
    }
    #[test]
    fn current_release_matches_the_client_signing_key() {
        if let Ok(path) = std::env::var("CHEM_TEST_PACKAGE") {
            let catalog = crate::cloud::bundled().unwrap();
            let target = &catalog.targets[0];
            let p = Package::open(&path, &target.target, &target.firmware_sha256).unwrap();
            assert_eq!(p.manifest.id, "bluetooth_audio");
            assert!(!p.resources.is_empty());
        }
    }
}

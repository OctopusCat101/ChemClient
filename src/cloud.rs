//! Bounded repository catalog and cooperative cloud downloads.
use crate::package;
use serde::{Deserialize, Serialize};
use std::fs;

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct Artifact {
    pub path: String,
    pub size: u64,
    pub sha256: String,
}
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct Loader {
    pub version: u32,
    pub face_id: String,
    #[serde(flatten)]
    pub artifact: Artifact,
}
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct Module {
    pub id: String,
    pub name: String,
    pub version: u32,
    pub version_name: String,
    #[serde(flatten)]
    pub artifact: Artifact,
}
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct Target {
    pub model: String,
    pub firmware: String,
    pub target: String,
    pub firmware_sha256: String,
    pub device_ids: Vec<String>,
    pub loader: Loader,
    pub modules: Vec<Module>,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Catalog {
    pub schema: u32,
    pub targets: Vec<Target>,
}
#[derive(Deserialize)]
struct Config {
    repository_url: String,
}
pub fn base() -> Result<String, String> {
    let config: Config = serde_json::from_slice(
        &fs::read("cloud.json").map_err(|e| format!("读取云端配置失败：{e}"))?,
    )
    .map_err(|e| e.to_string())?;
    if config.repository_url.is_empty() {
        return Ok(String::new());
    }
    let base = config.repository_url.trim_end_matches('/');
    if !base.starts_with("https://")
        || base.len() > 1024
        || base[8..].is_empty()
        || base.contains(['?', '#', '@', '\\'])
        || base.chars().any(|c| c.is_whitespace() || c.is_control())
    {
        return Err("云端地址需为 HTTPS 仓库文件根目录".into());
    }
    Ok(base.to_string())
}
fn safe_path(path: &str) -> bool {
    path.len() <= 240 && path.split('/').all(|p| package::safe_name(p, 96))
}
pub fn validate(c: &Catalog) -> Result<(), String> {
    if c.schema != 1 || c.targets.len() > 128 {
        return Err("云端清单版本或大小不支持".into());
    }
    let mut targets = std::collections::BTreeSet::new();
    for t in &c.targets {
        if !package::safe_name(&t.model, 64)
            || !package::safe_name(&t.firmware, 32)
            || !package::safe_name(&t.target, 47)
            || !hash(&t.firmware_sha256)
            || t.device_ids.is_empty()
            || t.device_ids.len() > 16
            || !targets.insert(t.target.clone())
            || t.modules.len() > 64
            || t.loader.face_id.len() != 9
            || !t.loader.face_id.bytes().all(|c| c.is_ascii_digit())
        {
            return Err("云端目标信息无效".into());
        }
        let prefix = format!("{}/{}/", t.model, t.firmware);
        artifact(&t.loader.artifact, 16 * 1024 * 1024, &prefix, "face")?;
        let mut ids = std::collections::BTreeSet::new();
        for m in &t.modules {
            if !package::module_id(&m.id)
                || m.name.is_empty()
                || m.name.len() > 96
                || m.version_name.len() > 32
                || !ids.insert(&m.id)
            {
                return Err("云端模块信息无效".into());
            }
            artifact(&m.artifact, package::MAX_PACKAGE, &prefix, "chem")?;
        }
    }
    Ok(())
}
fn hash(s: &str) -> bool {
    s.len() == 64
        && s.bytes()
            .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
}
fn artifact(a: &Artifact, max: u64, prefix: &str, extension: &str) -> Result<(), String> {
    if !safe_path(&a.path)
        || !a.path.starts_with(prefix)
        || a.path.split('/').count() != 4
        || !a.path.ends_with(&format!(".{extension}"))
        || !hash(&a.sha256)
        || a.size == 0
        || a.size > max
    {
        return Err("云端文件路径或校验信息无效".into());
    }
    Ok(())
}
#[cfg(test)]
pub fn fixture() -> Result<Catalog, String> {
    let c: Catalog = serde_json::from_str(include_str!("../tests/fixtures/catalog.json"))
        .map_err(|e| e.to_string())?;
    validate(&c)?;
    Ok(c)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn catalog_checks_paths_hashes_and_unique_targets() {
        let catalog = fixture().unwrap();
        assert!(validate(&catalog).is_ok());
        for path in [
            "../module.chem",
            "/module.chem",
            "xiaomi-band-9/3.1.35/NoirMusic/../evil.chem",
            "https://evil/module.chem",
        ] {
            let mut c = catalog.clone();
            c.targets[0].modules[0].artifact.path = path.into();
            assert!(validate(&c).is_err());
        }
        let mut c = catalog.clone();
        c.targets.push(c.targets[0].clone());
        assert!(validate(&c).is_err());
        let mut c = catalog.clone();
        c.targets[0].modules[0].artifact.sha256 = "0".into();
        assert!(validate(&c).is_err());
    }
}

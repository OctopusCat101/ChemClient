//! Bounded repository catalog and cooperative cloud downloads.
use crate::package;
use serde::{Deserialize, Serialize};
use std::fs;

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(default)]
pub struct Artifact {
    pub path: String,
    pub size: u64,
    pub sha256: String,
}
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct FrameworkVersion {
    pub version: u32,
    #[serde(default)]
    pub face_id: String,
    #[serde(flatten)]
    pub artifact: Artifact,
}
impl Target {
    pub fn installer(&self) -> Option<crate::installer::Release> {
        let loader = &self.loader;
        if self.local_only
            || loader.face_id.len() != 9
            || !loader.face_id.bytes().all(|b| b.is_ascii_digit())
        {
            return None;
        }
        let prefix = format!("{}/{}/chem-loader/", self.model, self.firmware);
        artifact(&loader.artifact, 16 * 1024 * 1024, &prefix, "face").ok()?;
        Some(crate::installer::Release {
            face_id: loader.face_id.clone(),
            artifact: loader.artifact.clone(),
        })
    }
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
    pub loader: FrameworkVersion,
    pub modules: Vec<Module>,
    /// True for targets known locally but not published in the cloud catalog.
    /// Such a target can be identified and used with a locally imported
    /// `.chem`, but has no cloud modules. Never set by
    /// the repository catalog (see `validate`).
    #[serde(default, skip_serializing)]
    pub local_only: bool,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Catalog {
    pub schema: u32,
    pub targets: Vec<Target>,
}

/// A target the client can identify before ChemCloud publishes its artifacts.
/// Keeping identity separate from the catalog lets a newly ported device reach
/// the diagnostic/local-import path without inventing cloud download entries.
pub struct KnownTarget {
    pub model: &'static str,
    pub firmware: &'static str,
    pub target: &'static str,
    pub firmware_sha256: &'static str,
    pub device_ids: &'static [&'static str],
}

/// Exact-firmware identities shipped with the plugin. `firmware_sha256` is the
/// SHA-256 of the device's `vela_ap.bin`; it is what `.chem` receipts bind to.
pub const KNOWN_TARGETS: &[KnownTarget] = &[KnownTarget {
    model: "xiaomi-band-10-nfc",
    firmware: "3.1.28",
    target: "xiaomi-band-10-nfc-3.1.28",
    firmware_sha256: "fb726472416e99ba11d1c20ce8f0d4f0831a6e9a673d63bb5918e2c3921d8845",
    device_ids: &[
        "o66nfc",
        "O66NFC",
        "miwear.watch.o66nfc",
        "miwear.watch.O66NFC",
        "xiaomi.watch.o66nfc",
        // Both hardware variants use the same firmware.
        "M2456B1",
        "M2457B1",
    ],
}];

#[derive(Deserialize)]
struct LocalCatalog {
    targets: Vec<LocalTarget>,
}
#[derive(Deserialize)]
struct LocalTarget {
    model: Option<String>,
    firmware: String,
    target: String,
    firmware_sha256: String,
    device_ids: Vec<String>,
}

fn known_target(t: &KnownTarget) -> Target {
    Target {
        model: t.model.into(),
        firmware: t.firmware.into(),
        target: t.target.into(),
        firmware_sha256: t.firmware_sha256.into(),
        device_ids: t.device_ids.iter().map(|s| (*s).into()).collect(),
        loader: FrameworkVersion::default(),
        modules: Vec::new(),
        local_only: true,
    }
}

fn local_entry_ok(t: &Target) -> bool {
    package::safe_name(&t.model, 64)
        && package::safe_name(&t.firmware, 32)
        && package::safe_name(&t.target, 47)
        && hash(&t.firmware_sha256)
        && !t.device_ids.is_empty()
        && t.device_ids.len() <= 16
        && t.device_ids
            .iter()
            .all(|id| !id.is_empty() && id.len() <= 64)
}

/// Identity-only targets: built-ins first, then optional operator overrides
/// from `data/targets.local.json`. A malformed file is ignored rather than
/// breaking device discovery; the cloud catalog stays authoritative.
pub fn local_targets() -> Vec<Target> {
    let mut out: Vec<Target> = KNOWN_TARGETS.iter().map(known_target).collect();
    if let Ok(bytes) = fs::read("data/targets.local.json") {
        if bytes.len() <= 64 * 1024 {
            if let Ok(parsed) = serde_json::from_slice::<LocalCatalog>(&bytes) {
                for item in parsed.targets {
                    let candidate = Target {
                        model: item.model.unwrap_or_default(),
                        firmware: item.firmware,
                        target: item.target,
                        firmware_sha256: item.firmware_sha256,
                        device_ids: item.device_ids,
                        loader: FrameworkVersion::default(),
                        modules: Vec::new(),
                        local_only: true,
                    };
                    if !local_entry_ok(&candidate) {
                        continue;
                    }
                    out.retain(|t| t.target != candidate.target);
                    out.push(candidate);
                }
            }
        }
    }
    out
}

/// Resolve a reported (firmware, model) pair. Cloud targets win; local-only
/// targets are a fallback and never supply downloadable artifacts.
pub fn identify_target(catalog: &Catalog, firmware: &str, model: &str) -> Option<Target> {
    catalog
        .targets
        .iter()
        .find(|t| {
            t.firmware == firmware && t.device_ids.iter().any(|id| id.eq_ignore_ascii_case(model))
        })
        .cloned()
        .or_else(|| {
            local_targets().into_iter().find(|t| {
                t.firmware == firmware
                    && t.device_ids.iter().any(|id| id.eq_ignore_ascii_case(model))
            })
        })
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
            // local_only is a client-side identity fallback, never a cloud
            // catalog property. Reject it so a mirror cannot disable downloads.
            || t.local_only
            || !targets.insert(t.target.clone())
            || t.modules.len() > 64
            || t.loader.version == 0
        {
            return Err("云端目标信息无效".into());
        }
        let prefix = format!("{}/{}/", t.model, t.firmware);
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
    fn framework_version_checks_do_not_require_a_watchface_artifact() {
        let mut value: serde_json::Value =
            serde_json::from_str(include_str!("../tests/fixtures/catalog.json")).unwrap();
        let revision = value["targets"][0]["loader"]["version"].as_u64().unwrap();
        value["targets"][0]["loader"] = serde_json::json!({"version": revision});
        let catalog: Catalog = serde_json::from_value(value).unwrap();
        assert!(validate(&catalog).is_ok());
        let mut s = crate::state::State {
            expected_revision: catalog.targets[0].loader.version,
            resident_revision: revision as u32 - 1,
            ..Default::default()
        };
        assert!(s.needs_update());
        s.resident_revision = revision as u32;
        assert!(!s.needs_update());
    }
    #[test]
    fn local_only_is_rejected_in_cloud_catalogs() {
        let mut c = fixture().unwrap();
        c.targets[0].local_only = true;
        assert!(validate(&c).is_err());
    }
    #[test]
    fn builtin_band10_nfc_identity_is_exact_and_has_no_cloud_artifacts() {
        let t = local_targets()
            .into_iter()
            .find(|t| t.target == "xiaomi-band-10-nfc-3.1.28")
            .unwrap();
        assert_eq!(t.firmware, "3.1.28");
        assert!(t.local_only);
        assert!(t.modules.is_empty());
        assert_eq!(t.loader.version, 0);
        assert!(t.device_ids.iter().any(|id| id == "miwear.watch.o66nfc"));
        assert!(t.device_ids.iter().any(|id| id == "M2456B1"));
        assert!(t.device_ids.iter().any(|id| id == "M2457B1"));
        assert!(hash(&t.firmware_sha256));
    }
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

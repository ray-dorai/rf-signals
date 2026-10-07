//! Technologies and service packages for one operator.
//!
//! `packages.json` beside isp.ron holds two lists an administrator maintains:
//!   * technologies -- how to recognise an AP technology from its model and band
//!   * packages     -- a speed tier and, per technology allowed to deliver it,
//!                     the minimum predicted RSSI (and optionally maximum range)
//! Sales views then ask "which addresses can get package X" or "... on
//! technology Y". The rules are the administrator's policy; the planner does
//! not model throughput or sector capacity, and says so in the UI.
//!
//! Re-read when the file's modification time changes.
use crate::data_defs::AP;
use parking_lot::RwLock;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::time::SystemTime;

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct Technology {
    pub id: String,
    pub name: String,
    /// Case-insensitive substrings; any match on the AP model selects it.
    /// Empty = any model (band only).
    #[serde(default)]
    pub model_contains: Vec<String>,
    #[serde(default)]
    pub min_ghz: Option<f64>,
    #[serde(default)]
    pub max_ghz: Option<f64>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct PackageRule {
    pub tech: String,
    pub min_rssi_dbm: f64,
    #[serde(default)]
    pub max_km: Option<f64>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct Package {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub down_mbps: f64,
    #[serde(default)]
    pub up_mbps: f64,
    #[serde(default)]
    pub rules: Vec<PackageRule>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct Catalog {
    #[serde(default)]
    pub technologies: Vec<Technology>,
    #[serde(default)]
    pub packages: Vec<Package>,
    #[serde(default)]
    pub note: String,
}

pub const OTHER: &str = "other";

impl Catalog {
    /// Used when an operator has no packages.json: bands only, no packages.
    pub fn fallback() -> Self {
        let band = |id: &str, name: &str, lo: f64, hi: f64| Technology {
            id: id.into(), name: name.into(), model_contains: vec![], min_ghz: Some(lo), max_ghz: Some(hi) };
        Catalog {
            technologies: vec![band("60ghz", "60 GHz", 20.0, 100.0), band("6ghz", "6 GHz", 5.925, 7.2),
                               band("5ghz", "5 GHz", 4.9, 5.925), band("3ghz", "3 GHz", 3.0, 4.2),
                               band("2ghz", "2.4 GHz", 2.0, 3.0), band("900mhz", "900 MHz", 0.8, 1.0)],
            packages: vec![],
            note: "Built-in band list; no packages.json for this operator.".into(),
        }
    }

    /// First technology in list order that matches; order therefore matters
    /// (put specific models before band-only catch-alls).
    pub fn tech_of(&self, ap: &AP) -> &str {
        let model = ap.model.to_lowercase();
        for t in &self.technologies {
            if t.min_ghz.map_or(false, |v| ap.frequency_ghz < v) || t.max_ghz.map_or(false, |v| ap.frequency_ghz >= v) { continue; }
            if t.model_contains.is_empty() || t.model_contains.iter().any(|m| !m.is_empty() && model.contains(&m.to_lowercase())) {
                return &t.id;
            }
        }
        OTHER
    }

    pub fn validate(&self) -> Result<(), String> {
        let ok_id = |s: &str| !s.is_empty() && s.len() <= 40 && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
        let mut seen = std::collections::HashSet::new();
        if self.technologies.len() > 100 || self.packages.len() > 200 { return Err("too many entries".into()); }
        for t in &self.technologies {
            if !ok_id(&t.id) || t.id == OTHER { return Err(format!("bad technology id '{}'", t.id)); }
            if !seen.insert(format!("t:{}", t.id)) { return Err(format!("duplicate technology id '{}'", t.id)); }
            if t.name.trim().is_empty() || t.name.len() > 80 { return Err(format!("technology '{}' needs a name", t.id)); }
            if let (Some(a), Some(b)) = (t.min_ghz, t.max_ghz) { if !(a < b) { return Err(format!("technology '{}': min_ghz must be below max_ghz", t.id)); } }
        }
        for p in &self.packages {
            if !ok_id(&p.id) { return Err(format!("bad package id '{}'", p.id)); }
            if !seen.insert(format!("p:{}", p.id)) { return Err(format!("duplicate package id '{}'", p.id)); }
            if p.name.trim().is_empty() || p.name.len() > 80 { return Err(format!("package '{}' needs a name", p.id)); }
            if !(p.down_mbps >= 0.0 && p.up_mbps >= 0.0 && p.down_mbps.is_finite() && p.up_mbps.is_finite()) { return Err(format!("package '{}': bad speeds", p.id)); }
            for r in &p.rules {
                if r.tech != OTHER && !self.technologies.iter().any(|t| t.id == r.tech) { return Err(format!("package '{}': unknown technology '{}'", p.id, r.tech)); }
                if !(-110.0..=-20.0).contains(&r.min_rssi_dbm) { return Err(format!("package '{}' / {}: min RSSI must be between -110 and -20 dBm", p.id, r.tech)); }
                if r.max_km.map_or(false, |k| !(k > 0.0 && k <= 100.0)) { return Err(format!("package '{}' / {}: bad max range", p.id, r.tech)); }
            }
        }
        Ok(())
    }
}

struct State { path: PathBuf, mtime: Option<SystemTime>, catalog: Catalog }

lazy_static::lazy_static! {
    static ref STATE: RwLock<State> = RwLock::new(State { path: PathBuf::new(), mtime: None, catalog: Catalog::fallback() });
}

pub fn init(path: PathBuf) { STATE.write().path = path; current(); }

/// The catalog in force, reloading from disk if the file changed.
pub fn current() -> Catalog {
    let (path, known) = { let s = STATE.read(); (s.path.clone(), s.mtime) };
    let mtime = std::fs::metadata(&path).and_then(|m| m.modified()).ok();
    if mtime != known {
        let loaded = std::fs::read_to_string(&path).ok()
            .and_then(|t| serde_json::from_str::<Catalog>(&t).ok())
            .filter(|c| c.validate().is_ok());
        let mut s = STATE.write();
        s.mtime = mtime;
        match loaded {
            Some(c) => { println!("  catalog: {} technologies, {} packages from {}", c.technologies.len(), c.packages.len(), path.display()); s.catalog = c; }
            None => { if mtime.is_some() { eprintln!("WARNING: {} is not a valid catalog; using built-in bands", path.display()); } s.catalog = Catalog::fallback(); }
        }
    }
    STATE.read().catalog.clone()
}

/// Validate and write atomically.
pub fn save(c: &Catalog) -> Result<(), String> {
    c.validate()?;
    let path = STATE.read().path.clone();
    let tmp = path.with_extension("json.tmp");
    let text = serde_json::to_string_pretty(c).map_err(|e| e.to_string())?;
    if path.exists() { let _ = std::fs::copy(&path, path.with_extension("json.bak")); }
    std::fs::write(&tmp, text).map_err(|e| format!("cannot write {}: {}", tmp.display(), e))?;
    std::fs::rename(&tmp, &path).map_err(|e| e.to_string())?;
    current();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn ap(model: &str, ghz: f64) -> AP { AP { model: model.into(), frequency_ghz: ghz, ..Default::default() } }
    fn cat() -> Catalog {
        Catalog { technologies: vec![
            Technology { id: "wave".into(), name: "Wave".into(), model_contains: vec!["wave".into()], min_ghz: Some(20.0), max_ghz: None },
            Technology { id: "450-3g".into(), name: "450 3 GHz".into(), model_contains: vec!["450".into()], min_ghz: Some(3.0), max_ghz: Some(4.2) },
            Technology { id: "5ghz".into(), name: "5 GHz".into(), model_contains: vec![], min_ghz: Some(4.9), max_ghz: Some(7.2) }],
            packages: vec![Package { id: "100x20".into(), name: "100x20".into(), down_mbps: 100.0, up_mbps: 20.0,
                rules: vec![PackageRule { tech: "450-3g".into(), min_rssi_dbm: -72.0, max_km: None }] }], note: String::new() }
    }
    #[test]
    fn classifies_by_model_and_band_in_order() {
        let c = cat();
        assert_eq!(c.tech_of(&ap("Ubiquiti Wave AP Micro", 68.04)), "wave");
        assert_eq!(c.tech_of(&ap("Cambium-450m", 3.65)), "450-3g");
        assert_eq!(c.tech_of(&ap("Cambium-450m-5G", 5.8)), "5ghz");
        assert_eq!(c.tech_of(&ap("", 5.2)), "5ghz");
        assert_eq!(c.tech_of(&ap("Something", 2.4)), OTHER);
        assert_eq!(Catalog::fallback().tech_of(&ap("", 60.0)), "60ghz");
    }
    #[test]
    fn validation_rejects_bad_catalogs() {
        assert!(cat().validate().is_ok());
        let mut c = cat(); c.packages[0].rules[0].tech = "nope".into(); assert!(c.validate().is_err());
        let mut c = cat(); c.packages[0].rules[0].min_rssi_dbm = 5.0; assert!(c.validate().is_err());
        let mut c = cat(); c.technologies[1].id = "wave".into(); assert!(c.validate().is_err());
        let mut c = cat(); c.technologies[0].id = "has space".into(); assert!(c.validate().is_err());
    }
}

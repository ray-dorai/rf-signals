//! Pre-surveyed address layer: one row per parcel with EVERY AP the survey
//! offered there, written by an offline batch survey (see README) to
//! `address-layer-v2.json` beside isp.ron. Each request grades the rows for one
//! view -- all APs, one technology, or one service package -- from those links
//! and the current catalog, so changing a package rule needs no re-survey.
//! Optional: an instance without the file serves an empty layer. Re-read when
//! the file's modification time changes.
use crate::catalog::{Catalog, Package, OTHER};
use parking_lot::RwLock;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::PathBuf;
use std::time::SystemTime;

#[derive(Clone, Copy)]
struct Link { ap: u32, grade: u8, rssi: f32, mount: f32, km: f32 }   // mount NaN = unknown

struct Row { lat: f64, lon: f64, address: String, id: String, on_building: bool, links: Vec<Link> }

#[derive(Default)]
pub struct AddressLayer { path: PathBuf, mtime: Option<SystemTime>, meta: Value, aps: Vec<String>, rows: Vec<Row> }

lazy_static::lazy_static! {
    static ref LAYER: RwLock<AddressLayer> = RwLock::new(AddressLayer::default());
}

const GRADES: [&str; 4] = ["clear", "check_mount", "unverified", "obstructed"];

pub fn init(path: PathBuf) { LAYER.write().path = path; refresh(); }

fn parse_row(r: &Value) -> Option<Row> {
    let (lat, lon) = (r.get(0)?.as_f64()?, r.get(1)?.as_f64()?);
    if !lat.is_finite() || !lon.is_finite() { return None; }
    let links = r.get(5)?.as_array()?.iter().filter_map(|l| Some(Link {
        ap: l.get(0)?.as_u64()? as u32,
        grade: (l.get(1)?.as_u64()? as u8).min(3),
        rssi: l.get(2)?.as_f64()? as f32,
        mount: l.get(3).and_then(Value::as_f64).map_or(f32::NAN, |v| v as f32),
        km: l.get(4).and_then(Value::as_f64).unwrap_or(0.0) as f32,
    })).collect();
    Some(Row { lat, lon, address: r.get(2)?.as_str()?.to_string(), id: r.get(3)?.as_str()?.to_string(),
               on_building: r.get(4).and_then(Value::as_u64).unwrap_or(0) == 1, links })
}

fn refresh() {
    let (path, known) = { let l = LAYER.read(); (l.path.clone(), l.mtime) };
    let mtime = std::fs::metadata(&path).and_then(|m| m.modified()).ok();
    if mtime == known { return; }
    let mut l = LAYER.write();
    l.mtime = mtime; l.rows.clear(); l.aps.clear(); l.meta = Value::Null;
    let Some(doc) = std::fs::read_to_string(&path).ok().and_then(|s| serde_json::from_str::<Value>(&s).ok()) else { return };
    let mut meta = doc.get("meta").cloned().unwrap_or(Value::Null);
    l.aps = meta.get("aps").and_then(Value::as_array)
        .map(|a| a.iter().filter_map(|v| v.as_str().map(str::to_string)).collect()).unwrap_or_default();
    if let Some(m) = meta.as_object_mut() { m.remove("aps"); }   // large; clients do not need it
    l.meta = meta;
    if let Some(rows) = doc.get("rows").and_then(Value::as_array) { l.rows = rows.iter().filter_map(parse_row).collect(); }
    println!("  address layer: {} rows, {} APs from {}", l.rows.len(), l.aps.len(), path.display());
}

/// Rows inside the box, graded for the requested view.
///
/// `ap_info` maps "tower:ap" to (technology id, is 57-71 GHz). An AP that is no
/// longer in the network is ignored. Above `limit` only counts are returned.
/// Output row: lat, lon, grade, mount_m, lowest_mount_m, rssi, best_ap, aps,
/// wave, address, id, on_building, tech.
pub fn query(
    swlat: f64, swlon: f64, nelat: f64, nelon: f64, limit: usize,
    tech: Option<&str>, package: Option<&Package>, ap_info: &HashMap<String, (String, bool)>,
) -> Value {
    refresh();
    let l = LAYER.read();
    let info: Vec<Option<&(String, bool)>> = l.aps.iter().map(|n| ap_info.get(n)).collect();
    let mut by_grade = std::collections::BTreeMap::<&str, usize>::new();
    let mut out = Vec::new();
    let mut in_view = 0usize;
    for r in l.rows.iter().filter(|r| r.lat >= swlat && r.lat <= nelat && r.lon >= swlon && r.lon <= nelon) {
        in_view += 1;
        let ok = |k: &&Link| -> bool {
            let Some(Some((t, _))) = info.get(k.ap as usize) else { return false };
            if tech.map_or(false, |want| want != t) { return false; }
            match package {
                None => true,
                Some(p) => p.rules.iter().any(|rule| &rule.tech == t && k.rssi as f64 >= rule.min_rssi_dbm
                    && rule.max_km.map_or(true, |m| (k.km as f64) <= m)),
            }
        };
        let cands: Vec<&Link> = r.links.iter().filter(ok).collect();
        let Some(best_grade) = cands.iter().map(|k| k.grade).min() else {
            *by_grade.entry("none").or_default() += 1;
            if out.len() <= limit {
                out.push(json!([r.lat, r.lon, "none", null, null, null, "", 0, 0, r.address, r.id, r.on_building as u8, ""]));
            }
            continue;
        };
        *by_grade.entry(GRADES[best_grade as usize]).or_default() += 1;
        if out.len() > limit { continue; }
        let top: Vec<&&Link> = cands.iter().filter(|k| k.grade == best_grade).collect();
        let best = top.iter().max_by(|a, b| a.rssi.total_cmp(&b.rssi)).unwrap();
        let min_mount = |it: &mut dyn Iterator<Item = f32>| it.filter(|m| m.is_finite()).min_by(|a, b| a.total_cmp(b));
        let mount = min_mount(&mut top.iter().map(|k| k.mount));
        let lowest = min_mount(&mut cands.iter().map(|k| k.mount));
        let wave = cands.iter().any(|k| k.grade <= 1 && info[k.ap as usize].map_or(false, |i| i.1));
        let (t, _) = info[best.ap as usize].unwrap();
        out.push(json!([r.lat, r.lon, GRADES[best_grade as usize], mount, lowest, best.rssi, l.aps[best.ap as usize],
                        cands.len(), wave as u8, r.address, r.id, r.on_building as u8, t]));
    }
    let truncated = in_view > limit;
    json!({ "meta": l.meta, "total": l.rows.len(), "in_view": in_view, "by_grade": by_grade,
            "truncated": truncated, "rows": if truncated { vec![] } else { out } })
}

/// "tower:ap" -> (technology id, is V-band) for the current network and catalog.
pub fn ap_info(catalog: &Catalog) -> HashMap<String, (String, bool)> {
    let w = crate::WISP.read();
    w.towers.iter().flat_map(|t| t.access_points.iter().map(move |ap| {
        (format!("{}:{}", t.name, ap.name), (catalog.tech_of(ap).to_string(), ap.frequency_ghz > 20.0))
    })).collect()
}

#[allow(dead_code)]
pub const OTHER_TECH: &str = OTHER;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalog::PackageRule;
    #[test]
    fn grades_views_by_technology_and_package() {
        let dir = std::env::temp_dir().join(format!("addr-layer-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let f = dir.join("address-layer-v2.json");
        // parcel A: Wave obstructed (strong) + 450 clear (weak); parcel B: Wave clear; parcel C: nothing; D outside box
        std::fs::write(&f, r#"{"meta":{"version":2,"cpe_height_m":4,"aps":["T:W1","T:P1","T:GONE"]},"rows":[
            [39.0,-95.0,"1 A ST","p1",1,[[0,3,-55.0,22.0,1.0],[1,0,-74.0,4.5,3.0],[2,0,-40.0,3.0,1.0]]],
            [39.1,-95.1,"2 B ST","p2",0,[[0,0,-60.0,5.0,2.5]]],
            [39.05,-95.05,"3 C ST","p3",1,[]],
            [40.0,-94.0,"4 D ST","p4",1,[[1,0,-60.0,3.0,1.0]]]]}"#).unwrap();
        init(f);
        let mut info = HashMap::new();
        info.insert("T:W1".to_string(), ("wave".to_string(), true));
        info.insert("T:P1".to_string(), ("450-3g".to_string(), false));
        let q = |tech: Option<&str>, pkg: Option<&Package>, limit| query(38.9, -95.2, 39.2, -94.9, limit, tech, pkg, &info);
        let grade = |v: &Value, i: usize| v["rows"][i][2].as_str().unwrap().to_string();

        let all = q(None, None, 100);
        assert_eq!(all["in_view"], 3);
        assert_eq!(grade(&all, 0), "clear");                      // A: 450 clear beats Wave obstructed; removed AP ignored
        assert_eq!(all["rows"][0][6], "T:P1");
        assert_eq!(all["rows"][0][7], 2);
        assert_eq!(grade(&all, 2), "none");
        assert_eq!(all["by_grade"]["clear"], 2);

        let wave = q(Some("wave"), None, 100);
        assert_eq!(grade(&wave, 0), "obstructed");                // A on Wave only
        assert_eq!(grade(&wave, 1), "clear");

        let gig = Package { id: "g".into(), name: "g".into(), down_mbps: 500.0, up_mbps: 500.0,
            rules: vec![PackageRule { tech: "wave".into(), min_rssi_dbm: -58.0, max_km: Some(2.0) }] };
        let g = q(None, Some(&gig), 100);
        assert_eq!(grade(&g, 0), "obstructed");                   // A: Wave strong enough and in range, but blocked
        assert_eq!(grade(&g, 1), "none");                         // B: Wave too weak (-60) and too far (2.5 km)
        let basic = Package { id: "b".into(), name: "b".into(), down_mbps: 30.0, up_mbps: 10.0,
            rules: vec![PackageRule { tech: "450-3g".into(), min_rssi_dbm: -75.0, max_km: None }] };
        assert_eq!(grade(&q(None, Some(&basic), 100), 0), "clear");
        assert_eq!(grade(&q(Some("wave"), Some(&basic), 100), 0), "none");   // filters combine

        let t = q(None, None, 1);
        assert_eq!(t["truncated"], true);
        assert!(t["rows"].as_array().unwrap().is_empty());
        assert_eq!(t["by_grade"]["clear"], 2);
        std::fs::remove_dir_all(&dir).ok();
    }
}

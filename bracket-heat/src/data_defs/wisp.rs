use super::{LinkBudget, Tower};
use ron::de::from_reader;
use serde::{Deserialize, Serialize};
use std::fs::File;

#[derive(Clone, Serialize, Deserialize, Default)]
pub struct Wisp {
    pub listen_port: u16,
    pub name: String,
    pub center: (f64, f64),
    pub map_zoom: u32,
    pub towers: Vec<Tower>,
    pub heat_path: String,
    pub link_budgets: Vec<LinkBudget>,
}

/// Load the WISP definition. The path is supplied by the caller rather than
/// hardcoded to "resources/isp.ron", because three instances share one binary
/// and each needs its own tower list.
pub fn load_wisp(path: &str) -> Wisp {
    let f = match File::open(path) {
        Ok(f) => f,
        Err(e) => panic!("Cannot open WISP definition {}: {}", path, e),
    };
    match from_reader(f) {
        Ok(w) => w,
        Err(e) => panic!("Cannot parse WISP definition {}: {:?}", path, e),
    }
}

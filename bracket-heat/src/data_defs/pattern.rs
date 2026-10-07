use serde::Deserialize;
use std::collections::HashMap;

/// A normalised antenna pattern (see README for the JSON layout).
///
/// `az` and `el` are 360 entries each, in dB DOWN from the pattern peak, at one
/// degree steps clockwise off boresight. Index 0 is boresight and reads 0.0.
#[derive(Clone, Deserialize, Default)]
pub struct AntennaPattern {
    pub key: String,
    #[serde(default, deserialize_with = "null_as_zero")]
    pub gain_dbi: f64,
    #[serde(default, deserialize_with = "null_as_zero")]
    pub tilt_deg: f64,
    pub az: Vec<f64>,
    pub el: Vec<f64>,
    #[serde(default = "default_ceiling")]
    pub loss_ceiling_db: f64,
}

// A missing peak makes this a RELATIVE attenuation pattern. The config
// generator retains provisional EIRP in that case, rather than converting
// conducted SNMP power using an invented antenna gain.
fn null_as_zero<'de, D: serde::Deserializer<'de>>(d: D) -> Result<f64, D::Error> {
    Ok(Option::<f64>::deserialize(d)?.unwrap_or(0.0))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn optional_nulls_preserve_relative_pattern() {
        let p: AntennaPattern = serde_json::from_str(r#"{"key":"synthetic","gain_dbi":null,"tilt_deg":null,"az":[0,10],"el":[0,0]}"#).unwrap();
        assert_eq!(p.gain_toward(1.0,0.0),-10.0);
    }
    #[test]
    fn null_tilt_does_not_discard_known_gain() {
        let p: AntennaPattern = serde_json::from_str(r#"{"key":"horn","gain_dbi":16,"tilt_deg":null,"az":[0],"el":[0]}"#).unwrap();
        assert_eq!(p.gain_toward(0.0,0.0),16.0);
    }
}

fn default_ceiling() -> f64 {
    40.0
}

impl AntennaPattern {
    /// Gain in dBi toward a direction given as degrees off boresight in azimuth
    /// and degrees above/below the antenna's own boresight in elevation.
    ///
    /// Azimuth and elevation cuts are combined by summing their losses, which is
    /// the standard separable approximation. It overstates loss where both are
    /// far off boresight, so the total is clamped at the pattern's ceiling --
    /// real antennas have a noise floor of scattered energy and do not keep
    /// rolling off forever.
    pub fn gain_toward(&self, az_off_deg: f64, el_off_deg: f64) -> f64 {
        let a = lookup(&self.az, az_off_deg);
        let e = lookup(&self.el, el_off_deg);
        let loss = (a + e).min(self.loss_ceiling_db);
        self.gain_dbi - loss
    }
}

/// Loss in dB at an angle, from a 360-entry cut, wrapping and interpolating.
fn lookup(cut: &[f64], deg: f64) -> f64 {
    if cut.is_empty() {
        return 0.0;
    }
    let n = cut.len() as f64;
    let mut d = deg % 360.0;
    if d < 0.0 {
        d += 360.0;
    }
    let i = d.floor();
    let frac = d - i;
    let a = cut[(i as usize) % cut.len()];
    let b = cut[((i as usize) + 1) % cut.len()];
    let _ = n;
    a + (b - a) * frac
}

/// Load every `*.json` in the given directory into a key -> pattern map.
/// A missing directory is not an error: it just means no patterns are attached,
/// and every AP falls back to the omni behaviour it had before.
pub fn load_patterns(dir: &str) -> HashMap<String, AntennaPattern> {
    let mut out = HashMap::new();
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return out,
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|s| s.to_str()) != Some("json") {
            continue;
        }
        let stem = match path.file_stem().and_then(|s| s.to_str()) {
            Some(s) => s.to_string(),
            None => continue,
        };
        let text = match std::fs::read_to_string(&path) {
            Ok(t) => t,
            Err(_) => continue,
        };
        match serde_json::from_str::<AntennaPattern>(&text) {
            Ok(mut p) => {
                if p.key.is_empty() {
                    p.key = stem.clone();
                }
                out.insert(stem, p);
            }
            Err(e) => eprintln!("antenna pattern {}: {}", path.display(), e),
        }
    }
    out
}

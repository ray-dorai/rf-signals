use serde::{Deserialize, Serialize};

#[derive(Clone, Serialize, Deserialize, Default)]
pub struct Tower {
    pub name: String,
    pub lat: f64,
    pub lon: f64,
    pub height_meters: f64,
    pub max_range_km: f64,
    pub access_points: Vec<AP>,
}

#[derive(Clone, Serialize, Deserialize, Default)]
pub struct AP {
    pub name: String,
    #[serde(default)]
    pub equipment_id: Option<String>,
    #[serde(default)]
    pub model: String,
    #[serde(default)]
    pub height_meters: Option<f64>,
    #[serde(default)]
    pub channel_width_mhz: Option<f64>,
    #[serde(default)]
    pub budget_source: String,
    pub frequency_ghz: f64,
    pub max_range_km: f64,
    /// Budget EXCLUDING the AP's own antenna gain when `pattern` is set.
    ///
    /// Upstream defines this as transmit power + transmit antenna gain + CPE
    /// antenna gain, i.e. peak boresight gain is baked in. Once a pattern is
    /// attached, that gain comes from the pattern instead -- leaving it here as
    /// well would count it twice. `build-isp-ron.py` subtracts the pattern's
    /// `gain_dbi` when it emits a `pattern`, and leaves the budget untouched
    /// when it does not, so an isp.ron without patterns behaves exactly as before.
    pub link_budget: f64,

    /// Boresight bearing in degrees clockwise from true north.
    #[serde(default)]
    pub azimuth: Option<f64>,
    /// Mechanical plus electrical downtilt in degrees; positive points down.
    #[serde(default)]
    pub downtilt: Option<f64>,
    /// Nominal azimuth beamwidth, kept for reporting and for the synthetic
    /// pattern fallback even when a measured pattern is attached.
    #[serde(default)]
    pub beamwidth: Option<f64>,
    /// Key of a normalised pattern file in the `antenna-patterns` directory
    /// beside isp.ron, without the `.json`. `None` keeps the old omni behaviour.
    #[serde(default)]
    pub pattern: Option<String>,
}

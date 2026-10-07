//! 57-71 GHz (V-band) loss terms for Wave / cnWave links.
//!
//! ITWOM/Longley-Rice is defined for 20 MHz-20 GHz and must not be used here.
//! A 60 GHz link is line-of-sight or it is nothing, so the path model is
//!   free space + atmospheric gas + rain fade at a target availability,
//! and obstruction is decided separately from LiDAR geometry.
//!
//! * Gas: ITU-R P.676 line-by-line (oxygen + water vapour), tabulated from the
//!   ITU-Rpy reference implementation on the P.835 standard atmosphere
//!   (water vapour 7.5 g/m3 at sea level, 2 km scale height). The channel
//!   matters enormously: ~14 dB/km at 60.48 GHz, ~0.5 dB/km at 68 GHz at
//!   1.5 km elevation.
//! * Rain: ITU-R P.838 specific attenuation (45 deg polarisation tilt) with the
//!   ITU-R P.530 path reduction and percentage-of-time scaling.

const F0_GHZ: f64 = 57.0;
const F_STEP_GHZ: f64 = 0.5;
const ALT_STEP_KM: f64 = 0.5;

/// dB/km. Rows: altitude 0, 0.5 .. 3.5 km. Columns: 57.0, 57.5 .. 71.0 GHz.
const GAS_DB_PER_KM: [[f64; 29]; 8] = [
    [10.206, 11.473, 12.498, 13.251, 13.785, 14.274, 14.778, 15.127, 15.167, 14.883, 14.161, 12.847, 11.001, 8.954, 7.020, 5.352, 3.990, 2.933, 2.154, 1.604, 1.227, 0.974, 0.805, 0.691, 0.612, 0.555, 0.514, 0.482, 0.457],
    [9.854, 11.116, 12.147, 12.900, 13.419, 13.900, 14.411, 14.755, 14.770, 14.478, 13.765, 12.451, 10.586, 8.533, 6.623, 4.998, 3.686, 2.679, 1.945, 1.431, 1.083, 0.851, 0.697, 0.594, 0.522, 0.471, 0.433, 0.404, 0.380],
    [9.512, 10.769, 11.805, 12.560, 13.064, 13.537, 14.055, 14.394, 14.380, 14.079, 13.378, 12.068, 10.186, 8.130, 6.247, 4.667, 3.408, 2.450, 1.759, 1.281, 0.960, 0.748, 0.608, 0.515, 0.450, 0.404, 0.369, 0.342, 0.321],
    [9.179, 10.429, 11.472, 12.229, 12.717, 13.183, 13.709, 14.041, 13.994, 13.684, 12.997, 11.696, 9.799, 7.743, 5.889, 4.358, 3.151, 2.242, 1.593, 1.149, 0.854, 0.660, 0.534, 0.450, 0.391, 0.349, 0.318, 0.293, 0.273],
    [8.852, 10.093, 11.144, 11.906, 12.377, 12.835, 13.370, 13.696, 13.611, 13.291, 12.622, 11.334, 9.423, 7.368, 5.549, 4.067, 2.912, 2.052, 1.444, 1.032, 0.761, 0.585, 0.471, 0.395, 0.342, 0.304, 0.275, 0.253, 0.235],
    [8.531, 9.762, 10.822, 11.591, 12.042, 12.492, 13.036, 13.358, 13.229, 12.898, 12.251, 10.981, 9.056, 7.006, 5.223, 3.793, 2.690, 1.878, 1.309, 0.928, 0.680, 0.520, 0.417, 0.348, 0.301, 0.266, 0.241, 0.220, 0.204],
    [8.216, 9.434, 10.503, 11.281, 11.712, 12.154, 12.708, 13.026, 12.847, 12.506, 11.883, 10.636, 8.699, 6.656, 4.912, 3.534, 2.483, 1.717, 1.187, 0.835, 0.608, 0.463, 0.370, 0.309, 0.266, 0.235, 0.211, 0.193, 0.178],
    [7.906, 9.110, 10.188, 10.977, 11.385, 11.820, 12.384, 12.700, 12.466, 12.113, 11.517, 10.300, 8.350, 6.315, 4.613, 3.289, 2.290, 1.569, 1.075, 0.751, 0.544, 0.413, 0.329, 0.274, 0.236, 0.208, 0.187, 0.170, 0.156]
];

/// ITU-R P.838 (k, alpha) at 57.0, 57.5 .. 71.0 GHz.
const RAIN_K_ALPHA: [(f64, f64); 29] = [
    (0.79815, 0.76794),
    (0.80799, 0.76606),
    (0.81776, 0.76422),
    (0.82745, 0.76240),
    (0.83706, 0.76062),
    (0.84660, 0.75887),
    (0.85607, 0.75714),
    (0.86545, 0.75545),
    (0.87476, 0.75378),
    (0.88398, 0.75214),
    (0.89313, 0.75053),
    (0.90220, 0.74894),
    (0.91118, 0.74738),
    (0.92009, 0.74585),
    (0.92891, 0.74434),
    (0.93766, 0.74286),
    (0.94632, 0.74140),
    (0.95489, 0.73996),
    (0.96339, 0.73855),
    (0.97180, 0.73715),
    (0.98014, 0.73579),
    (0.98839, 0.73444),
    (0.99655, 0.73311),
    (1.00464, 0.73181),
    (1.01264, 0.73053),
    (1.02057, 0.72926),
    (1.02841, 0.72802),
    (1.03617, 0.72679),
    (1.04384, 0.72559)
];

fn f_index(f_ghz: f64) -> (usize, f64) {
    let x = ((f_ghz - F0_GHZ) / F_STEP_GHZ).clamp(0.0, 28.0);
    let i = (x.floor() as usize).min(27);
    (i, x - i as f64)
}

/// Whether the tables cover this frequency.
pub fn vband_supported(f_ghz: f64) -> bool {
    (57.0..=71.0).contains(&f_ghz)
}

/// Clear-air specific attenuation (oxygen + water vapour), dB/km.
pub fn vband_gas_db_per_km(f_ghz: f64, altitude_m: f64) -> f64 {
    let (i, ft) = f_index(f_ghz);
    let y = (altitude_m / 1000.0 / ALT_STEP_KM).clamp(0.0, 7.0);
    let j = (y.floor() as usize).min(6);
    let at = y - j as f64;
    let row = |r: usize| GAS_DB_PER_KM[r][i] * (1.0 - ft) + GAS_DB_PER_KM[r][i + 1] * ft;
    row(j) * (1.0 - at) + row(j + 1) * at
}

/// Rain fade (dB) exceeded `p_percent` of an average year over `d_km`, given
/// the rain rate exceeded 0.01% of the year. ITU-R P.530 section 2.4.1.
pub fn vband_rain_fade_db(f_ghz: f64, d_km: f64, r001_mm_h: f64, p_percent: f64) -> f64 {
    if d_km <= 0.0 || r001_mm_h <= 0.0 { return 0.0; }
    let (i, ft) = f_index(f_ghz);
    let k = RAIN_K_ALPHA[i].0 * (1.0 - ft) + RAIN_K_ALPHA[i + 1].0 * ft;
    let alpha = RAIN_K_ALPHA[i].1 * (1.0 - ft) + RAIN_K_ALPHA[i + 1].1 * ft;
    let gamma = k * r001_mm_h.powf(alpha);
    let r = (1.0 / (0.477 * d_km.powf(0.633) * r001_mm_h.powf(0.073 * alpha) * f_ghz.powf(0.123)
        - 10.579 * (1.0 - (-0.024 * d_km).exp()))).min(2.5);
    let a001 = gamma * d_km * r;
    let p = p_percent.clamp(0.001, 1.0);
    let c0 = 0.12 + 0.4 * ((f_ghz / 10.0).powf(0.8)).log10();
    let c1 = 0.07f64.powf(c0) * 0.12f64.powf(1.0 - c0);
    let c2 = 0.855 * c0 + 0.546 * (1.0 - c0);
    let c3 = 0.139 * c0 + 0.043 * (1.0 - c0);
    a001 * c1 * p.powf(-(c2 + c3 * p.log10()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gas_matches_p676_reference_and_falls_with_frequency_and_altitude() {
        // ITU-Rpy P.676 at 1.56 km: 60.48 -> 13.99, 66.96 -> 0.86, 69.12 -> 0.37 dB/km.
        assert!((vband_gas_db_per_km(60.48, 1560.0) - 13.99).abs() < 0.15);
        assert!((vband_gas_db_per_km(66.96, 1560.0) - 0.86).abs() < 0.05);
        assert!((vband_gas_db_per_km(69.12, 1560.0) - 0.37).abs() < 0.05);
        assert!(vband_gas_db_per_km(60.5, 0.0) > vband_gas_db_per_km(60.5, 2000.0));
        assert!(vband_gas_db_per_km(60.5, 1500.0) > 10.0 * vband_gas_db_per_km(68.0, 1500.0));
    }

    #[test]
    fn rain_fade_matches_p530_reference() {
        // ITU-Rpy itu530.rain_attenuation, R0.01 = 19 mm/h, tau = 45 deg.
        for (f, d, p, want) in [(66.96, 1.0, 0.1, 4.655), (66.96, 2.0, 0.1, 6.673),
                                (60.48, 1.5, 0.01, 14.481), (69.12, 3.0, 0.1, 8.498), (64.8, 0.5, 1.0, 0.824)] {
            let got = vband_rain_fade_db(f, d, 19.0, p);
            assert!((got - want).abs() < 0.1, "f={} d={} p={}: got {} want {}", f, d, p, got, want);
        }
    }

    #[test]
    fn degenerate_inputs_are_zero_and_range_is_bounded() {
        assert_eq!(vband_rain_fade_db(66.0, 0.0, 19.0, 0.1), 0.0);
        assert!(vband_supported(60.48) && !vband_supported(5.8) && !vband_supported(80.0));
    }
}

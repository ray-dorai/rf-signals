use super::{Distance, Frequency};
pub use super::itwom3_port::{ItWomDerived, ItWomWarning};

fn point_to_point(
    elev: &mut [f64],
    tht_m: f64,
    rht_m: f64,
    eps_dielect: f64,
    sgm_conductivity: f64,
    eno_ns_surfref: f64,
    frq_mhz: f64,
    radio_climate: ::std::os::raw::c_int,
    pol: ::std::os::raw::c_int,
    conf: f64,
    rel: f64,
    clutter_canopy_m: Option<f64>,
) -> PTPResult {
    let mut dbloss = 0.0f64;
    let mut mode = String::new();
    let mut errnum: std::os::raw::c_int = 0;

    use super::itwom3_port::ItWomState;

    let mut itm = ItWomState::default();
    itm.clutter_canopy_m = clutter_canopy_m;

    itm.point_to_point(
        elev,
        tht_m,
        rht_m,
        eps_dielect,
        sgm_conductivity,
        eno_ns_surfref,
        frq_mhz,
        radio_climate,
        pol,
        conf,
        rel,
        &mut dbloss,
        &mut mode,
        &mut errnum,
    );

    PTPResult {
        dbloss: dbloss,
        mode: mode,
        error_num: errnum,
        warnings: itm.warnings,
        derived: itm.derived,
    }
}

#[derive(Debug)]
pub struct PTPResult {
    pub dbloss: f64,
    pub mode: String,
    pub error_num: i32,
    /// Parameter-range checks that produced `error_num`.
    pub warnings: Vec<ItWomWarning>,
    pub derived: ItWomDerived,
}

impl PTPResult {
    /// Whether the result is usable: finite, and no check that NTIA ITM v1.4
    /// treats as an error. Non-blocking warnings remain in `warnings`.
    pub fn is_valid(&self) -> bool {
        self.dbloss.is_finite() && !self.warnings.iter().any(|w| w.blocking)
    }
}

#[derive(Debug, PartialEq)]
pub enum PTPError {
    //DistanceTooShort,
    DistanceTooLong,
    AltitudeTooHigh,
    AltitudeTooLow,
    TooFewPoints,
    InvalidElevation,
}

/// Describes terrain for an IWOM Point-To-Point path.
/// Elevations should be an array of altitudes along the path, starting at the transmitter and ending at the receiver.
/// The height of transmitters and receivers is height above the elevations specified.
#[derive(Debug)]
pub struct PTPPath {
    pub elevations: Vec<f64>,
    pub transmit_height: Distance,
    pub receive_height: Distance,
    /// Height of the vegetation/building canopy around the RECEIVER, in metres
    /// above local ground. ITWOM applies its `saalos` clutter attenuation
    /// whenever the receive antenna sits below this, so it decides whether a
    /// 4 m mount is in the open or under trees. `None` keeps the upstream
    /// preset of 22.5 m, which treats every subscriber as being under a
    /// canopy regardless of what is actually there.
    pub clutter_canopy_m: Option<f64>,
}

impl PTPPath {
    /// Construct a new PTP path for ITWOM evaluation. The constructor takes care of pre-pending the fields
    /// used by the C algorithm.
    pub fn new(
        elevations: Vec<f64>,
        transmit_height: Distance,
        receive_height: Distance,
        step_size: Distance,
    ) -> Result<Self, PTPError> {
        let total_distance: f64 = elevations.len() as f64 * step_size.as_meters();
        if total_distance > 2000000.0 {
            return Err(PTPError::DistanceTooLong);
        }

        // Elevations are terrain above sea level. ITM's 0.5-3000 m limits apply
        // to antenna heights above ground (checked inside the model), not to the
        // terrain: rejecting terrain above 3000 m made every high-mountain path
        // fail, and the caller's unwrap() turned that into an empty survey.
        if elevations.len() < 2 {
            return Err(PTPError::TooFewPoints);
        }
        if elevations.iter().any(|a| !a.is_finite()) {
            return Err(PTPError::InvalidElevation);
        }

        let mut path = Self {
            elevations,
            transmit_height,
            receive_height,
            clutter_canopy_m: None,
        };

        // Index 0 is the number of elements, next up is the distance per step
        // ITM profile header: [intervals, step, z0..zn]. n points are n-1
        // intervals; the old `len - 3` dropped the receiver-end sample.
        let intervals = path.elevations.len() as f64 - 1.0;
        path.elevations.insert(0, step_size.as_meters());
        path.elevations.insert(0, intervals);

        Ok(path)
    }
}

#[derive(Debug)]
pub struct PTPClimate {
    pub eps_dialect: f64,
    pub sgm_conductivity: f64,
    pub eno_ns_surfref: f64,
    pub radio_climate: i32,
}

pub enum GroundConductivity {
    SaltWater,
    GoodGround,
    FreshWater,
    MarshyLand,
    Farmland,
    Forest,
    AverageGround,
    Mountain,
    Sand,
    City,
    PoorGround,
}

pub enum RadioClimate {
    Equatorial,
    ContinentalSubtropical,
    MaritimeSubtropical,
    Desert,
    ContinentalTemperate,
    MaritimeTemperateLand,
    MaritimeTemperateSea,
}

impl PTPClimate {
    pub fn default() -> Self {
        Self {
            eps_dialect: 15.0,
            sgm_conductivity: 0.005,
            eno_ns_surfref: 301.0,
            radio_climate: 5,
        }
    }

    pub fn new(ground: GroundConductivity, climate: RadioClimate) -> Self {
        Self {
            eps_dialect: match ground {
                GroundConductivity::SaltWater => 80.0,
                GroundConductivity::GoodGround => 25.0,
                GroundConductivity::FreshWater => 80.0,
                GroundConductivity::MarshyLand => 12.0,
                GroundConductivity::Farmland => 15.0,
                GroundConductivity::Forest => 15.0,
                GroundConductivity::AverageGround => 15.0,
                GroundConductivity::Mountain => 13.0,
                GroundConductivity::Sand => 13.0,
                GroundConductivity::City => 5.0,
                GroundConductivity::PoorGround => 4.0,
            },
            sgm_conductivity: match ground {
                GroundConductivity::SaltWater => 5.0,
                GroundConductivity::GoodGround => 0.020,
                GroundConductivity::FreshWater => 0.010,
                GroundConductivity::MarshyLand => 0.007,
                GroundConductivity::Farmland => 0.005,
                GroundConductivity::Forest => 0.005,
                GroundConductivity::AverageGround => 0.005,
                GroundConductivity::Mountain => 0.002,
                GroundConductivity::Sand => 0.002,
                GroundConductivity::City => 0.001,
                GroundConductivity::PoorGround => 0.001,
            },
            eno_ns_surfref: 301.0,
            radio_climate: match climate {
                RadioClimate::Equatorial => 1,
                RadioClimate::ContinentalSubtropical => 2,
                RadioClimate::MaritimeSubtropical => 3,
                RadioClimate::Desert => 4,
                RadioClimate::ContinentalTemperate => 5,
                RadioClimate::MaritimeTemperateLand => 6,
                RadioClimate::MaritimeTemperateSea => 7,
            },
        }
    }
}

pub fn itwom_point_to_point(
    path: &mut PTPPath,
    climate: PTPClimate,
    frequency: Frequency,
    confidence: f64,
    rel: f64,
    polarity: i32,
) -> PTPResult {
    point_to_point(
        &mut path.elevations,
        path.transmit_height.as_meters(),
        path.receive_height.as_meters(),
        climate.eps_dialect,
        climate.sgm_conductivity,
        climate.eno_ns_surfref,
        frequency.as_mhz(),
        climate.radio_climate,
        polarity,
        confidence,
        rel,
        path.clutter_canopy_m,
    )
}

#[cfg(test)]
mod test {
    use super::*;

    /*#[test]
    fn test_too_short() {
        assert_eq!(
            PTPPath::new(
                vec![1.0; 2],
                Distance::with_meters(100.0),
                Distance::with_meters(100.0),
                Distance::with_meters(10.0)
            )
            .err(),
            Some(PTPError::DistanceTooShort)
        );
    }*/

    #[test]
    fn test_too_long() {
        assert_eq!(
            PTPPath::new(
                vec![1.0; 2000000],
                Distance::with_meters(100.0),
                Distance::with_meters(100.0),
                Distance::with_meters(10.0)
            )
            .err(),
            Some(PTPError::DistanceTooLong)
        );
    }

    #[test]
    fn mountain_terrain_is_accepted() {
        // Terrain above 3000 m AMSL is valid ITM input.
        let mut path = PTPPath::new(
            vec![3500.0; 200],
            Distance::with_meters(30.0),
            Distance::with_meters(6.0),
            Distance::with_meters(10.0),
        )
        .unwrap();
        let r = itwom_point_to_point(&mut path, PTPClimate::default(), Frequency::with_mhz(5500.0), 0.5, 0.5, 1);
        assert!(r.dbloss.is_finite());
        assert!(r.warnings.iter().all(|w| !w.blocking));
    }

    #[test]
    fn non_finite_or_degenerate_profiles_are_rejected() {
        let d = |m| Distance::with_meters(m);
        let mut e = vec![100.0; 50];
        e[10] = f64::NAN;
        assert_eq!(PTPPath::new(e, d(10.0), d(6.0), d(1.0)).err(), Some(PTPError::InvalidElevation));
        assert_eq!(PTPPath::new(vec![1.0], d(10.0), d(6.0), d(1.0)).err(), Some(PTPError::TooFewPoints));
    }

    #[test]
    fn profile_header_counts_intervals_and_keeps_receiver_sample() {
        let mut e = vec![100.0; 11];
        e[10] = 123.0;
        let p = PTPPath::new(e, Distance::with_meters(10.0), Distance::with_meters(6.0), Distance::with_meters(1.0)).unwrap();
        assert_eq!(p.elevations[0], 10.0);
        assert_eq!(p.elevations[1], 1.0);
        let np = p.elevations[0] as usize;
        assert_eq!(p.elevations[np + 2], 123.0);
    }

    fn flat_path(elevation_m: f64, points: usize) -> PTPResult {
        let mut terrain_path = PTPPath::new(
            vec![elevation_m; points],
            Distance::with_meters(30.0),
            Distance::with_meters(6.0),
            Distance::with_meters(10.0),
        )
        .unwrap();
        itwom_point_to_point(&mut terrain_path, PTPClimate::default(), Frequency::with_mhz(5500.0), 0.5, 0.5, 1)
    }

    #[test]
    fn high_elevation_reports_scaled_refractivity_warning() {
        // N0=301 scaled by exp(-2100/9460) is ~241 N, below the 250 N limit.
        let r = flat_path(2100.0, 400);
        assert_eq!(r.error_num, 4);
        assert!((r.derived.ens - 301.0 * (-r.derived.zsys_m / 9460.0).exp()).abs() < 1e-9);
        assert_eq!(r.warnings.len(), 1);
        assert_eq!(r.warnings[0].check, "surface_refractivity_below_250N");
        assert!(!r.warnings[0].blocking && r.is_valid());
        // Same path at 1400 m has no warning.
        let low = flat_path(1400.0, 400);
        assert_eq!(low.error_num, 0);
        assert!(low.warnings.is_empty());
    }

    #[test]
    fn out_of_model_frequency_blocks() {
        let mut path = PTPPath::new(vec![1400.0; 400], Distance::with_meters(30.0), Distance::with_meters(6.0), Distance::with_meters(10.0)).unwrap();
        let r = itwom_point_to_point(&mut path, PTPClimate::default(), Frequency::with_mhz(60_480.0), 0.5, 0.5, 1);
        assert!(!r.is_valid());
        assert!(r.warnings.iter().any(|w| w.check == "frequency_outside_20MHz_20GHz" && w.blocking));
    }

    #[test]
    fn short_path_reports_distance_warning() {
        let r = flat_path(1400.0, 60);
        assert_eq!(r.error_num, 4);
        assert!(r.warnings.iter().any(|w| w.check == "distance_under_1km" && w.value < 1000.0 && !w.blocking));
        assert!(r.is_valid());
    }

    #[test]
    fn basic_fspl_test() {
        let mut terrain_path = PTPPath::new(
            vec![1.0; 200],
            Distance::with_meters(100.0),
            Distance::with_meters(100.0),
            Distance::with_meters(10.0),
        )
        .unwrap();

        let itwom_test = itwom_point_to_point(
            &mut terrain_path,
            PTPClimate::default(),
            Frequency::with_mhz(5800.0),
            0.5,
            0.5,
            1,
        );

        assert_eq!(itwom_test.mode, "L-o-S");
        assert_eq!(itwom_test.error_num, 0);
        assert_eq!(itwom_test.dbloss.floor(), 113.0);
    }

    #[test]
    fn basic_one_obstruction() {
        let mut elevations = vec![1.0; 200];
        elevations[100] = 110.0;
        let mut terrain_path = PTPPath::new(
            elevations,
            Distance::with_meters(100.0),
            Distance::with_meters(100.0),
            Distance::with_meters(10.0),
        )
        .unwrap();

        let itwom_test = itwom_point_to_point(
            &mut terrain_path,
            PTPClimate::default(),
            Frequency::with_mhz(5800.0),
            0.5,
            0.5,
            1,
        );

        assert_eq!(itwom_test.mode, "1_Hrzn_Diff");
        assert_eq!(itwom_test.error_num, 0);
    }

    #[test]
    fn basic_two_obstructions() {
        let mut elevations = vec![1.0; 200];
        elevations[100] = 110.0;
        elevations[150] = 110.0;
        let mut terrain_path = PTPPath::new(
            elevations,
            Distance::with_meters(100.0),
            Distance::with_meters(100.0),
            Distance::with_meters(10.0),
        )
        .unwrap();

        let itwom_test = itwom_point_to_point(
            &mut terrain_path,
            PTPClimate::default(),
            Frequency::with_mhz(5800.0),
            0.5,
            0.5,
            1,
        );

        assert_eq!(itwom_test.mode, "2_Hrzn_Diff");
        assert_eq!(itwom_test.error_num, 0);
    }
}

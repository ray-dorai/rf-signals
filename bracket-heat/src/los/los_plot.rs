use rf_signal_algorithms::{
    bheat::heat_altitude, fresnel_radius, geometry::haversine_distance, itwom_point_to_point,
    lat_lon_path_1m, lat_lon_vec_to_ground_clutter, Distance, Frequency, LatLon, PTPClimate,
    PTPPath,
};
use serde::{Deserialize, Serialize};

#[derive(Clone, Serialize, Deserialize, Default)]
pub struct LineOfSightPlot {
    pub tower_base_height: f64,
    pub srtm: Vec<f64>,
    pub lidar: Vec<f64>,
    pub fresnel: Vec<f64>,
    pub dbloss: f64,
    pub mode: String,
    pub distance_m: f64,
    /// Metres of foliage the direct ray passes through, and the attenuation
    /// charged for it. Reported separately from `dbloss` (which includes it) so
    /// the vegetation model can be calibrated without re-running the server.
    pub vegetation_m: f64,
    pub vegetation_db: f64,
    /// Foliage depth in the last 150 m before the receiver, and before that.
    /// Diagnostic: rural foliage clusters around the house, not along the path.
    pub vegetation_near_m: f64,
    pub vegetation_far_m: f64,
}

pub fn los_plot(
    pos: &LatLon,
    tower_index: usize,
    cpe_height: f64,
    frequency: Frequency,
    heat_path: &str,
) -> LineOfSightPlot {
    let reader = crate::WISP.read();
    let t = &reader.towers[tower_index];
    let d = haversine_distance(pos, &LatLon::new(t.lat, t.lon));
    let base_tower_height = heat_altitude(t.lat, t.lon, heat_path)
        .unwrap_or((Distance::with_meters(0.0), Distance::with_meters(0.0)))
        .0
        .as_meters();
    let path = lat_lon_path_1m(&LatLon::new(t.lat, t.lon), pos); // Tower is 1st

    // Ground and canopy are kept apart on purpose. Diffraction geometry runs on
    // BARE EARTH; vegetation is charged afterwards as an attenuation. Feeding
    // canopy tops in as terrain turns every tree crown into a knife edge, which
    // is what produced 200 dB of modelled loss on links measuring -55 dBm.
    let profile = lat_lon_vec_to_ground_clutter(&path, heat_path);
    let (dbloss, mode, vegetation_m, vegetation_db, vegetation_near_m, vegetation_far_m) = {
        let ground: Vec<f64> = profile.iter().map(|p| p.0).collect();
        let canopy: Vec<f64> = profile.iter().map(|p| p.1).collect();
        if ground.iter().filter(|h| **h == 0.0).count() > 0 {
            (0.0, "Missing Data".to_string(), 0.0, 0.0, 0.0, 0.0)
        } else {
            let mut path_as_distances: Vec<f64> =
                if crate::calculators::profile_uses_clutter() {
                    canopy.clone()
                } else {
                    ground.clone()
                };
            path_as_distances[0] = base_tower_height;
            let veg_m = crate::calculators::vegetation_depth_m(
                &ground,
                &canopy,
                t.height_meters,
                cpe_height,
            );
            let near_far = crate::calculators::vegetation_depth_split(
                &ground,
                &canopy,
                t.height_meters,
                cpe_height,
                150.0,
            );
            // Fails only on non-finite or sub-2-point profiles; the route maps a
            // worker panic to 404, so this cannot blank another request.
            let mut terrain_path = PTPPath::new(
                path_as_distances,
                Distance::with_meters(t.height_meters),
                Distance::with_meters(cpe_height),
                Distance::with_meters(1.0),
            )
            .unwrap();
            terrain_path.clutter_canopy_m =
                crate::calculators::local_canopy_m(pos.lat(), pos.lon(), heat_path);

            let lr = itwom_point_to_point(
                &mut terrain_path,
                PTPClimate::default(),
                frequency,
                0.5,
                0.5,
                1,
            );

            (
                lr.dbloss + crate::calculators::vegetation_loss_db(veg_m, frequency),
                format!("{} ({})", lr.mode, lr.error_num),
                veg_m,
                crate::calculators::vegetation_loss_db(veg_m, frequency),
                near_far.0,
                near_far.1,
            )
        }
    };

    // Expand out the srtm, lidar and fresnel fields
    let mut srtm = Vec::new();
    let mut lidar = Vec::new();
    let mut fresnel = Vec::new();

    let mut walker = 0.0;
    path.iter().for_each(|loc| {
        let h = heat_altitude(loc.lat(), loc.lon(), heat_path)
            .unwrap_or((Distance::with_meters(0), Distance::with_meters(0)));
        srtm.push(h.0.as_meters());
        lidar.push(h.1.as_meters());
        fresnel.push(fresnel_radius(walker, d.as_meters() - walker, frequency.as_mhz()) * 0.6);
        walker += 1.0;
    });

    LineOfSightPlot {
        tower_base_height: base_tower_height,
        srtm,
        lidar,
        fresnel,
        dbloss,
        mode,
        distance_m: d.as_meters(),
        vegetation_m,
        vegetation_db,
        vegetation_near_m,
        vegetation_far_m,
    }
}
